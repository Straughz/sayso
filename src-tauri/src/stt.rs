//! Speech-to-text engines. Stage 1 ships the xAI batch implementation (port of
//! `src/vox/asr/xai.py`) and a no-key placeholder so the app still starts and
//! the pill can show "xAI API key required".
//!
//! The whole held span is uploaded as one WAV; no streaming. The only gate is
//! xAI's own `vad_threshold` (see `Config::vad_threshold`).

use std::time::Duration;
use std::sync::{Arc, RwLock};

pub const XAI_URL: &str = "https://api.x.ai/v1/stt";
pub const XAI_MODEL: &str = "grok-voice-transcribe-2.0";
const MAX_KEYTERMS: usize = 100;
const MAX_KEYTERM_CHARS: usize = 50;
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

pub trait SttEngine: Send + Sync {
    /// `hints` are the key terms for this request; the engine owns no fixed
    /// list, so the context engine can pick them per dictation.
    fn transcribe(&self, wav: &[u8], hints: &[String]) -> Result<String, String>;
    fn name(&self) -> String;
}

/// Swapping the engine leaves an in-flight dictation on its original client;
/// the next one sees the new key terms or API key.
pub struct LiveStt { inner: RwLock<Arc<dyn SttEngine>> }

impl LiveStt {
    pub fn new(engine: Arc<dyn SttEngine>) -> Self { Self { inner: RwLock::new(engine) } }
    pub fn replace(&self, engine: Arc<dyn SttEngine>) {
        *self.inner.write().unwrap_or_else(|p| p.into_inner()) = engine;
    }
    pub fn has_key(&self) -> bool {
        self.inner.read().unwrap_or_else(|p| p.into_inner()).name() != "xAI (API key required)"
    }
}

impl SttEngine for LiveStt {
    fn transcribe(&self, wav: &[u8], hints: &[String]) -> Result<String, String> {
        let engine = Arc::clone(&self.inner.read().unwrap_or_else(|p| p.into_inner()));
        engine.transcribe(wav, hints)
    }
    fn name(&self) -> String { self.inner.read().unwrap_or_else(|p| p.into_inner()).name() }
}

// ---------------------------------------------------------------------------
// xAI
// ---------------------------------------------------------------------------

pub struct XaiStt {
    client: reqwest::blocking::Client,
    api_key: String,
    vad_threshold: f32,
}

impl XaiStt {
    pub fn new(api_key: String, vad_threshold: f32) -> Result<Self, String> {
        if api_key.is_empty() {
            return Err("xAI API key is required".into());
        }
        let client = reqwest::blocking::Client::builder()
            // httpx drops idle connections after 5 s; keeping them warm for
            // 120 s avoids a fresh TLS handshake in front of nearly every
            // dictation (see src/vox/asr/xai.py).
            .timeout(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| format!("xAI client build failed: {e}"))?;
        Ok(XaiStt { client, api_key, vad_threshold: clamp_vad(vad_threshold) })
    }

    fn keyterms(hints: &[String]) -> Vec<String> {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out = Vec::new();
        for w in hints {
            let term = w.trim();
            if term.is_empty() || term.chars().count() > MAX_KEYTERM_CHARS {
                continue;
            }
            if out.len() >= MAX_KEYTERMS {
                break;
            }
            if seen.insert(term.to_lowercase()) {
                out.push(term.to_string());
            }
        }
        out
    }
}

impl SttEngine for XaiStt {
    fn transcribe(&self, wav: &[u8], hints: &[String]) -> Result<String, String> {
        // `mime_guess` (reqwest's multipart feature) is not in the local cargo
        // cache, so the multipart body is assembled by hand. Field order is
        // preserved by construction and `file` is written last, matching the
        // xAI rule in src/vox/asr/xai.py.
        let boundary = format!("----NVGVoiceBoundary{:x}", now_millis());
        let body = build_multipart(&boundary, wav, hints, self.vad_threshold);

        let response = self
            .client
            .post(XAI_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body)
            .send();

        let response = match response {
            Ok(r) => r,
            Err(_) => return Err("xAI request failed".to_string()),
        };
        let status = response.status();
        if !status.is_success() {
            return Err(format!("xAI transcription failed ({})", status.as_u16()));
        }
        // Read at most 1 MB; a transcript is a few KB, anything bigger is not
        // a transcript.
        let mut body = Vec::new();
        {
            use std::io::Read;
            response
                .take(MAX_RESPONSE_BYTES + 1)
                .read_to_end(&mut body)
                .map_err(|_| format!("xAI transcription failed ({}): read error", status.as_u16()))?;
        }
        if body.len() as u64 > MAX_RESPONSE_BYTES {
            return Err(format!("xAI transcription failed ({}): response too large", status.as_u16()));
        }
        let payload: serde_json::Value = serde_json::from_slice(&body)
            .map_err(|_| format!("xAI transcription failed ({}): bad JSON", status.as_u16()))?;
        let text = payload
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("xAI transcription failed ({}): missing text", status.as_u16()))?;
        Ok(text.trim().to_string())
    }

    fn name(&self) -> String {
        "xAI Grok Voice Transcribe 2.0".to_string()
    }
}

/// Values outside 0..=1 (or NaN) fall back to the ends of the range.
fn clamp_vad(v: f32) -> f32 {
    if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
}

/// Option fields first, `file` last (xAI ignores fields after `file`).
fn build_multipart(boundary: &str, wav: &[u8], hints: &[String], vad_threshold: f32) -> Vec<u8> {
    let mut body: Vec<u8> = Vec::with_capacity(wav.len() + 512);
    push_text_part(&mut body, boundary, "model", XAI_MODEL);
    push_text_part(&mut body, boundary, "language", "en");
    push_text_part(&mut body, boundary, "format", "true");
    push_text_part(&mut body, boundary, "vad_threshold", &format!("{vad_threshold}"));
    for term in XaiStt::keyterms(hints) {
        push_text_part(&mut body, boundary, "keyterm", &term);
    }
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: audio/wav\r\n\r\n");
    body.extend_from_slice(wav);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

fn push_text_part(body: &mut Vec<u8>, boundary: &str, name: &str, value: &str) {
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes());
    body.extend_from_slice(value.as_bytes());
    body.extend_from_slice(b"\r\n");
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Missing key placeholder
// ---------------------------------------------------------------------------

pub struct MissingKeyStt;

impl SttEngine for MissingKeyStt {
    fn transcribe(&self, _wav: &[u8], _hints: &[String]) -> Result<String, String> {
        Err("xAI API key required".to_string())
    }

    fn name(&self) -> String {
        "xAI (API key required)".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field_order(body: &[u8]) -> Vec<String> {
        let text = String::from_utf8_lossy(body);
        text.lines()
            .filter_map(|l| l.strip_prefix("Content-Disposition: form-data; name=\""))
            .map(|rest| rest.split('"').next().unwrap_or("").to_string())
            .collect()
    }

    #[test]
    fn multipart_sends_vad_threshold_before_file() {
        let hints = vec!["Cokpit".to_string(), "".to_string(), "cokpit".to_string()];
        let body = build_multipart("B", b"RIFFwav", &hints, 0.3);
        let order = field_order(&body);
        assert_eq!(order, ["model", "language", "format", "vad_threshold", "keyterm", "file"]);
        let text = String::from_utf8_lossy(&body);
        assert!(text.contains("name=\"vad_threshold\"\r\n\r\n0.3\r\n"));
        assert!(text.contains("name=\"model\"\r\n\r\ngrok-voice-transcribe-2.0\r\n"));
        assert!(text.ends_with("--B--\r\n"));
    }

    #[test]
    fn vad_threshold_is_clamped_and_key_is_required() {
        // `XaiStt::new` builds a TLS client, which needs the crypto provider
        // main() installs, so the clamp is tested directly.
        assert_eq!(clamp_vad(4.0), 1.0);
        assert_eq!(clamp_vad(-1.0), 0.0);
        assert_eq!(clamp_vad(f32::NAN), 0.0);
        assert_eq!(clamp_vad(0.3), 0.3);
        assert!(XaiStt::new(String::new(), 0.3).is_err());
    }
}
