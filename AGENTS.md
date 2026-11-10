# AGENTS.md

Guidance for AI coding agents in this repository. Plan and milestones: [`PLAN.md`](PLAN.md).

## Project

The YMusic desktop app rewritten in Rust (core + gpui-kit UI), replacing the Kotlin/Compose desktop
app in YMusicV4. It builds C sources and the `engine-js` bundle straight from a YMusicV4 checkout
(`YMUSIC_V4_DIR`, default `../YMusicV4`) — never copy them here; a C fix lands in YMusicV4.

## Crates

One sentence each; detail lives in the crate's own README.

- **`ym-quickjs`** — QuickJS binding: the YMusicV4 C (vendored engine + pipe transport) built with `cc`, the async `Pipe` layer on a dedicated engine thread, host timers.
- **`ym-common`** — shared primitives: the `Logger` crumb trail and the app's reqwest clients (no cookie store; following / non-following).
- **`ym-engine`** — the engine host: boots `engine.js` (`DynamicEngine`), wire errors, logger grafting, host protocols (http, cookie, cache + `DiskCacheStore`, settings, remote-config, log/capture), the typed `yt/*` / `ytmusic/*` command calls (`ym_engine::yt`), and the player bundle's `player/*` commands + `listen/player/*` streams (`ym_engine::player`; MSE attach and `host/main/*` proxy via `EngineConfig`).
- **`ym-store`** — the local library: `:media-store`'s schema (version 6) on SQLite and `scan_folder`, which reconciles a directory tree into buckets and items (in-process probe, unchanged files skipped, vanished ones soft-deleted, per-file errors in the report).
- **`ym-mse`** — YMusicV4's `:mse` C host (MSE over ffmpeg, audio DAC, video decode/presenters) built with `cc` against `ffmpeg-build/out/<slug>` (macOS so far); owns one host, the VideoToolbox layer presenter (`Presenter`), the local file player (`LocalAv` over `file_av.c`), the scan probe (`probe`, `native/ym_probe.c` on libavformat), and the process-wide C→JS event and log hooks; `native/vt_overlay_stub.c` fills the JVM-only JAWT overlay symbols.
- **`ym-settings`** — the `SettingsStore` trait shared by UI and engine (text values, typed reads), an in-memory store, and `AppDb`: `app.db` with the JVM app's `setting` / `secret` tables.
- **`ym-i18n`** — UI strings generated at build time from YMusicV4's compose-resources (`Str` / `Plural` keys, `t` / `tf` / `plural`, CLDR plurals, English fallback). Never hardcode UI text; add strings to every locale in YMusicV4.
- **`ymusic`** — the executable: boot (tokio + engine), gpui image client on reqwest, the shell (section sidebar, per-section back/forward stacks, search with suggestions) and feed surfaces. YouTube playback (`Playback`: player engine + MSE host booted on first play, radio / section queues, status stream) the mini player, Now Playing (video in an `AVSampleBufferDisplayLayer` over the window via `video_layer.rs`, audio-only switch, Up next queue), and the OS media session (souvlaki: Now Playing info + media keys). Debug builds take `YMUSIC_DEV_ROUTE` (`trending`, `music`, `search:<q>`, `play:<url>`, `nowplaying:<url>`) to open a surface at start, floating, for screenshots, and `YMUSIC_DEV_MUTE` to start muted.

## Rules

### Style
- `cargo fmt` and `cargo clippy --workspace -- -D warnings` must pass.
- Descriptive names (`engine_player`, `error`), never single letters.
- **Comments**: default to none. Only a genuine hack or a non-obvious constraint/invariant earns one,
  in one line. The story of a change goes in the commit message, never the code ("used to",
  "instead of", "previously" are banned). Doc comments state contracts, not the name restated.

### Errors
- Never swallow an error. Propagate it (`?`), return it, or collect it into a result. Only
  documented fire-and-forget cleanup may ignore one.
- What the user sees is a well-known category (network, disk full, …) rendered localized at the UI,
  or "Something went wrong (E12345)". Technical detail stays in logs.
- Error codes: `R` + 5 digits (e.g. R12345), a unique literal at exactly one throw site, minted in
  [`error-codes.txt`](error-codes.txt) — grep for a free number in the crate's range, add a `-` row
  in the same commit as the throw site; on release flip the shipped rows to `D` (burned forever).
  The `R` set is disjoint from YMusicV4's `E` codes. E codes minted by engine-js pass through this
  app unchanged — wrap them, never re-mint.
- Log at the birth site with the caller's forked logger trail; `capture_error` once, where a job is
  declared dead.

### Threads and async
- A QuickJS engine thread is a main thread: accept handlers must never block. SQLite, files,
  keychain and other blocking calls go through `tokio::task::spawn_blocking`.
- The gpui main thread is UI only.
- Don't fake async: a purely blocking function is a plain `fn`; `async` only with real suspension
  points (network, channels, pipes).
- Never hold a `RefCell` borrow across an FFI call that can run JS (it re-enters synchronously).

### I/O
- Stream by default (`Bytes` chunks, readers, pipes). A whole payload in memory only when an API
  forces it, with a comment naming that API.

### Engine bundle compatibility
- `engine.js` / `player.js` are replaced remotely and run on every shipped host. Host protocols
  here are wire-identical to `quickjs/HOST_PROTOCOLS.md` in YMusicV4; never rename or drop a wire
  field or command.

### Moving app data
- When an on-disk location moves (including from the JVM app's layout), clean up the old one.

### Tests
- Don't add tests unless asked. Throwaway verification lives outside the tree and is deleted.
- Mute audio in any playback test (`YMUSIC_DEV_MUTE=1`). Never mouse-control the running desktop app; screenshot it via `YMUSIC_DEV_ROUTE` and `screencapture -l <window id>`.

### Git
- Commit each finished change, one logical change per commit, path-scoped `git add <paths>`.
- Never push; the user pushes.
- Never commit secrets, cookies, captured `base.js`, session dumps or personal data.
