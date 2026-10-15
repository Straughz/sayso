# Contributing

Thanks for looking. This is a small, personal tool that happens to be open. Issues
and pull requests are welcome; please keep the scope of each one tight.

## Before you open a pull request

- Build and test as described in `docs/DEVELOPMENT.md` (`pnpm build` in `ui`,
  then `cargo test` and `cargo build --release` in `src-tauri`).
- Keep the two hard rules of this code base:
  - **Dictated text never reaches the log file.** Log counts, durations and
    state names, not what the user said.
  - **The API key never reaches the web view or the log.** It is read from
    DPAPI (or `XAI_API_KEY`) in Rust and used there only.
- Typing into other apps is paced on purpose (`inject.rs`); do not batch
  characters into one `SendInput` call, it corrupts text in slower apps.
- New settings get a default in `store.rs`, a row in the README's configuration
  table and, if the user should see them, a row in Settings.
- Dependencies are pinned exactly. Bump them in a separate pull request with a
  note on why, and check the exe still starts on a clean Windows install.

## Adding a speech-to-text provider

Implement the `SttEngine` trait in `src-tauri/src/stt.rs` (one method:
`transcribe(wav_bytes, hints) -> Result<String, String>`), add its key storage
and Settings row, and select it in `commands.rs` where the engine is rebuilt.
The rest of the app does not care who transcribes. Please keep each provider in
its own file.

## Reporting a bug

Include the Windows version, the app version (Settings > System), the log file
(`%LOCALAPPDATA%\Sayso\logs\sayso.log`; it contains no dictated text and
no key) and the app you were dictating into.

## Licence

By contributing you agree that your contribution is licensed under the
Apache License 2.0, the same licence as the project (see `LICENSE` and
`NOTICE`).
