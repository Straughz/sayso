//! Microphone capture on a dedicated thread. The cpal `Stream` is not `Send`,
//! so it is created, played and dropped entirely on this thread.
//!
//! Push-to-talk semantics: the whole hotkey-held span is one utterance (no VAD
//! segmentation). Audio blocks are forwarded to the owner thread over a
//! channel, converted to 16 kHz mono int16 and wrapped in a WAV in memory when
//! capture stops.

use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::Arc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SampleRate, SizedSample, StreamConfig};

use crate::log;

/// Capture format requested first; fall back to the device default if the mic
/// cannot do 16 kHz (the samples are then resampled).
const TARGET_RATE: u32 = 16000;

pub enum AudioCmd {
    Start,
    Stop(Sender<Vec<u8>>),
}

#[derive(Clone)]
pub struct AudioController {
    tx: Sender<AudioCmd>,
}

impl AudioController {
    pub fn start(&self) {
        let _ = self.tx.send(AudioCmd::Start);
    }

    /// Stop capture and return the encoded WAV (empty if the mic never opened).
    pub fn stop(&self) -> Vec<u8> {
        let (tx, rx) = mpsc::channel();
        if self.tx.send(AudioCmd::Stop(tx)).is_err() {
            return Vec::new();
        }
        rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default()
    }
}

/// Reports a microphone failure to the state machine (start-up or mid-stream).
pub type OnError = Arc<dyn Fn(String) + Send + Sync>;

pub fn spawn(
    device_name: String,
    level_tx: SyncSender<f32>,
    on_error: OnError,
) -> AudioController {
    let (cmd_tx, cmd_rx) = mpsc::channel::<AudioCmd>();
    if let Err(e) = std::thread::Builder::new()
        .name("audio".into())
        .spawn(move || run(cmd_rx, device_name, level_tx, on_error))
    {
        log::error(&format!("audio thread spawn failed: {e}"));
    }
    AudioController { tx: cmd_tx }
}

fn run(
    cmd_rx: Receiver<AudioCmd>,
    device_name: String,
    level_tx: SyncSender<f32>,
    on_error: OnError,
) {
    let (block_tx, block_rx) = mpsc::channel::<Vec<f32>>();
    let mut stream: Option<cpal::Stream> = None;
    let mut samples: Vec<f32> = Vec::new();
    let mut stream_rate = TARGET_RATE;

    loop {
        match cmd_rx.recv_timeout(Duration::from_millis(10)) {
            Ok(AudioCmd::Start) => {
                samples.clear();
                match build_stream(&device_name, block_tx.clone(), level_tx.clone(), Arc::clone(&on_error)) {
                    Ok((s, rate)) => match s.play() {
                        Ok(()) => {
                            stream_rate = rate;
                            stream = Some(s);
                            log::info(&format!("mic stream started ({rate} Hz)"));
                        }
                        Err(e) => {
                            on_error(format!("mic error: {e}"));
                            log::error(&format!("audio play failed: {e}"));
                        }
                    },
                    Err(e) => {
                        on_error(format!("mic error: {e}"));
                        log::error(&format!("audio build failed: {e}"));
                    }
                }
            }
            Ok(AudioCmd::Stop(reply)) => {
                // Drop the stream first (stops the callback), then drain any
                // blocks still queued.
                drop(stream.take());
                while let Ok(b) = block_rx.try_recv() {
                    samples.extend(b);
                }
                let pcm = to_mono_i16(&samples, stream_rate);
                let wav = if pcm.is_empty() { Vec::new() } else { encode_wav(&pcm) };
                let _ = reply.send(wav);
                samples.clear();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        while let Ok(b) = block_rx.try_recv() {
            samples.extend(b);
        }
    }
}

fn pick_device(host: &cpal::Host, name: &str) -> Result<cpal::Device, String> {
    if name.trim().is_empty() {
        return host.default_input_device().ok_or_else(|| "no default input device".into());
    }
    let needle = name.trim().to_lowercase();
    if let Ok(devices) = host.input_devices() {
        for d in devices {
            if let Ok(n) = d.name() {
                if n.to_lowercase().contains(&needle) {
                    log::info(&format!("using mic device {n:?}"));
                    return Ok(d);
                }
            }
        }
    }
    log::warn(&format!("mic device {name:?} not found — using system default"));
    host.default_input_device().ok_or_else(|| "no default input device".into())
}

fn choose_config(device: &cpal::Device) -> Result<(StreamConfig, SampleFormat), String> {
    if let Ok(ranges) = device.supported_input_configs() {
        for r in ranges {
            if r.min_sample_rate().0 <= TARGET_RATE && TARGET_RATE <= r.max_sample_rate().0 {
                let cfg = StreamConfig {
                    channels: r.channels(),
                    sample_rate: SampleRate(TARGET_RATE),
                    buffer_size: cpal::BufferSize::Default,
                };
                return Ok((cfg, r.sample_format()));
            }
        }
    }
    let cfg = device
        .default_input_config()
        .map_err(|e| format!("default_input_config: {e}"))?;
    let format = cfg.sample_format();
    Ok((cfg.config(), format))
}

fn build_stream(
    device_name: &str,
    block_tx: Sender<Vec<f32>>,
    level_tx: SyncSender<f32>,
    on_error: OnError,
) -> Result<(cpal::Stream, u32), String> {
    let host = cpal::default_host();
    let device = pick_device(&host, device_name)?;
    let (config, format) = choose_config(&device)?;
    let rate = config.sample_rate.0;
    let channels = config.channels;
    // A device that fails mid-capture (unplugged) reaches the state machine
    // as an audio error; only the first error per stream is reported.
    let mut reported = false;
    let err_fn = move |e: cpal::StreamError| {
        if !reported {
            reported = true;
            log::error(&format!("audio stream error: {e}"));
            on_error(format!("mic error: {e}"));
        }
    };

    let stream = match format {
        SampleFormat::F32 => make::<f32>(&device, &config, channels, block_tx, level_tx, err_fn),
        SampleFormat::I16 => make::<i16>(&device, &config, channels, block_tx, level_tx, err_fn),
        SampleFormat::I32 => make::<i32>(&device, &config, channels, block_tx, level_tx, err_fn),
        SampleFormat::I8 => make::<i8>(&device, &config, channels, block_tx, level_tx, err_fn),
        SampleFormat::U16 => make::<u16>(&device, &config, channels, block_tx, level_tx, err_fn),
        SampleFormat::U8 => make::<u8>(&device, &config, channels, block_tx, level_tx, err_fn),
        SampleFormat::F64 => make::<f64>(&device, &config, channels, block_tx, level_tx, err_fn),
        other => return Err(format!("unsupported sample format {other}")),
    }?;
    Ok((stream, rate))
}

fn make<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    channels: u16,
    block_tx: Sender<Vec<f32>>,
    level_tx: SyncSender<f32>,
    err_fn: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let ch = channels.max(1) as usize;
    device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                let mut mono: Vec<f32> = Vec::with_capacity(data.len() / ch);
                let mut i = 0;
                while i + ch <= data.len() {
                    let mut sum = 0.0f32;
                    for k in 0..ch {
                        sum += f32::from_sample(data[i + k]);
                    }
                    mono.push(sum / ch as f32);
                    i += ch;
                }
                if mono.is_empty() {
                    return;
                }
                let mut sq = 0.0f64;
                for &v in &mono {
                    sq += (v as f64) * (v as f64);
                }
                let rms = (sq / mono.len() as f64).sqrt();
                // Speech sits far below full scale (measured RMS about 0.04 on
                // his mic), so a linear level left the bars almost flat. Map
                // 0.12 of full scale to a full bar and take the square root so
                // normal speech lands about two thirds up (0.25 left the bars
                // at under half height unless he spoke very loudly).
                let level = ((rms * std::f64::consts::SQRT_2 / 0.12).sqrt() as f32).clamp(0.0, 1.0);
                let _ = level_tx.try_send(level);
                let _ = block_tx.send(mono);
            },
            err_fn,
            None,
        )
        .map_err(|e| format!("build_input_stream: {e}"))
}

/// Convert captured mono f32 samples to 16 kHz mono int16.
fn to_mono_i16(samples: &[f32], rate: u32) -> Vec<i16> {
    let convert = |v: f64| (v * 32767.0).clamp(-32768.0, 32767.0) as i16;
    if rate == TARGET_RATE || samples.is_empty() {
        return samples.iter().map(|&v| convert(v as f64)).collect();
    }
    let ratio = TARGET_RATE as f64 / rate as f64;
    let out_len = (samples.len() as f64 * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src = i as f64 / ratio;
        let i0 = src.floor() as usize;
        let frac = src - i0 as f64;
        let s0 = samples.get(i0).copied().unwrap_or(0.0) as f64;
        let s1 = samples.get(i0 + 1).copied().unwrap_or(s0 as f32) as f64;
        out.push(convert(s0 + (s1 - s0) * frac));
    }
    out
}

/// Minimal 16-bit PCM mono RIFF/WAVE encoder.
pub fn encode_wav(samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&TARGET_RATE.to_le_bytes());
    out.extend_from_slice(&(TARGET_RATE * 2).to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}
