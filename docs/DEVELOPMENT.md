# Development notes

The app is a native tray process (Rust, Tauri v2) with a React flyout and
Settings window. The process starts with no webview. The flyout is created from
the tray icon and destroyed on close or focus loss. Settings is created when
opened and destroyed on close.

## Build and checks

From `ui`, run `pnpm install` and `pnpm build`. The package
uses React 19.2.7, TypeScript 6.0.3, and Vite 8.1.0. The Vite output is
`ui/dist`; Tauri embeds it through `frontendDist`.

From `src-tauri`, run `cargo test` and `cargo build --release`. The release
executable is `target/release/nvg-voice.exe` (the target directory sits next to
`src-tauri`, see `.cargo/config.toml`). Do not run it while another NVG Voice
instance is active: both would use the hotkey and shared user data. For visual
work, use `pnpm dev` in `ui`. Its development-only Tauri mock lives in
`ui/src/dev/` and is excluded from production by Vite's `DEV` branch.

## Installer

`installer/installer.nsi` is an NSIS 3 script (per-user, no admin). Build the
app first, then from `installer` (PowerShell, not Git Bash: bash rewrites
`/DVERSION` into a path):

```
mkdir out
"C:\Program Files (x86)\NSIS\makensis.exe" /DVERSION=0.1.0 installer.nsi
```

Output: `installer/out/NVG-Voice-Setup-<version>.exe` (git-ignored). It
installs the single exe to `%LOCALAPPDATA%\NVG Voice`, the same folder,
uninstall key and Run value name (`NVGVoice`) earlier releases used, so an old
install is upgraded in place. It closes a running copy before copying.

Silent install: `/S`, plus `/AUTOSTART=1` to start at sign-in and `/NORUN=1` to
not launch afterwards. Uninstall removes the exe, shortcuts, Run value and
uninstall key; it never touches `%APPDATA%\NVGVoice` or
`%LOCALAPPDATA%\NVGVoice` (settings, API key, history, logs).

The exe links the C runtime statically (`.cargo/config.toml`, `crt-static`).
Without that it needs `VCRUNTIME140.dll`, which a clean Windows does not have,
and it stops at a system error box before `main` runs.

## Windows data

Settings live in `%APPDATA%/NVGVoice/config.json`, the API key in
`xai_key.enc` next to it, and history in `%LOCALAPPDATA%/NVGVoice/history.jsonl`.
All writes go through `store.rs` (temp file + rename; a broken config is never
overwritten). The API key is encrypted by DPAPI and is never sent to
the webview or written to the log. The user dictionary lives at the configured
`dictionary_path`; the first correction creates
`%APPDATA%/NVGVoice/dictionary.json`, seeded with the bundled entries.

## Tauri commands

| Command | Use |
|---|---|
| `get_snapshot` | Status and nonsecret settings for both windows |
| `get_history` | Dictations and statistics |
| `set_setting` | Validated one-field config save; reloads the file first |
| `get_vocabulary` | Manual terms (source `manual`) then non-retired automatic terms |
| `edit_vocabulary` | Add/remove hot words; keeps the pool in step and rebuilds the xAI engine |
| `fix_word` | Add a case-insensitive dictionary correction and reload cleanup |
| `save_xai_key` | Encrypt a new key and rebuild the xAI engine |
| `list_microphones` | List cpal input devices |
| `copy_text` | Copy a dictation to the clipboard |
| `set_paused` | Pause or resume dictation |
| `open_settings`, `close_settings`, `close_flyout` | Window lifecycle |
| `open_log_folder` | Open the log folder in Explorer |
| `quit_app` | Exit the app |
| `report_ready` | Mark the flyout ready for focus-loss close behavior |

Recording mode, pause, vocabulary, API key, filler removal, capitalisation,
code mode, trailing space, and dictionary corrections apply to the next
dictation without restarting. Hotkey, shortcut suppression, and microphone
choice save immediately but require a restart; Settings marks those rows.
Launch at startup updates the current user's `NVGVoice` Run value immediately.

The flyout's ambient layer uses a slow transform while visible; reduced-motion
users see a still panel. History search waits briefly after typing, and the
history list mounts only visible rows plus a buffer.
