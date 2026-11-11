# AGENTS.md

Guidance for AI coding agents in this repository. Plan and milestones: [`PLAN.md`](PLAN.md).

## Project

The YMusic desktop app rewritten in Rust (core + gpui-kit UI), replacing the Kotlin/Compose desktop
app in YMusicV4. It builds C sources and the `engine-js` bundle straight from a YMusicV4 checkout
(`YMUSIC_V4_DIR`, default `../YMusicV4`) — never copy them here; a C fix lands in YMusicV4.

## Crates

One sentence each; detail lives in the crate's own README.

- **`ym-quickjs`** — QuickJS binding: the YMusicV4 C (vendored engine + pipe transport) built with `cc`, the async `Pipe` layer on a dedicated engine thread, host timers.
- **`ym-common`** — shared primitives: the `Logger` crumb trail (+ `TeeSink`, `DeferredSink`) and the app's reqwest clients (no cookie store; following / non-following).
- **`ym-logstore`** — the app log (`logs.db`, the JVM schema): non-blocking enqueue, one writer, trim to a cap, claim lines for an error event, gzip export for feedback.
- **`ym-telemetry`** — Sentry: warnings/errors as breadcrumbs, `capture_error` as the job's one event (trail + error code tags, newest app-log lines attached; network category never sent), panics by the SDK.
- **`ym-engine`** — the engine host: boots `engine.js` (`DynamicEngine`), wire errors, logger grafting, host protocols (http, cookie, cache + `DiskCacheStore`, settings, remote-config, log/capture), `EngineManager` (boot the best bundle, swap a newer one without restart, retire with shutdown + drain), the typed `yt/*` / `ytmusic/*` command calls (`ym_engine::yt`), and the player bundle's `player/*` commands + `listen/player/*` streams (`ym_engine::player`; MSE attach and `host/main/*` proxy via `EngineConfig`).
- **`ym-store`** — the local library: `:media-store`'s schema (version 6) on SQLite and `scan_folder`, which reconciles a directory tree into buckets and items (in-process probe, unchanged files skipped, vanished ones soft-deleted, per-file errors in the report).
- **`ym-mse`** — YMusicV4's `:mse` C host (MSE over ffmpeg, audio DAC, video decode/presenters) built with `cc` against `ffmpeg-build/out/<slug>` (macOS, Linux: PulseAudio output via `aq_pulse.c`); owns one host, the presenters (`Presenter`: the VideoToolbox layer on macOS, CPU BGRA pictures to a callback elsewhere), the local file player (`LocalAv` over `file_av.c`), the scan probe (`probe`, `native/ym_probe.c` on libavformat), and the process-wide C→JS event and log hooks; `native/vt_overlay_stub.c` fills the JVM-only JAWT overlay symbols.
- **`ym-ytmedia`** — googlevideo SABR/UMP: schemaless protobuf, UMP framing streamed off the body, the SABR messages, `SabrSession` (appends each format's contiguous stream to its file until `clen`) and the SQLite segment index that resumes a session from the file length. The player media cache / local proxy are not ported yet.
- **`ym-download`** — the offline download manager: SQLite queue (`downloads.db`), `DownloadService` (slots, pause/cancel, startup recovery, broadcast changes), options probe, format selection, captions, cover art, ffmpeg outputs (MP3 + USLT lyrics, M4A, Opus + picture/lyrics comments, MP4/WebM mux) over the packed ffmpeg; the app supplies a `SpecResolver` over the player engine.
- **`ym-history`** — `play_history.db` (the JVM schema v5): watch-time rankings for local files (by playlist code) and YouTube videos, per-video display snapshots, and Play again sessions (whole queues with their play-head).
- **`ym-remote-config`** — the config service (`/v1/config`: features, published bundles, app update; cached, throttled, an outage changes nothing), ECDSA P-256 bundle signatures, and `BundleSource` (a verified cached remote bundle above the embedded one).
- **`ym-signin`** — signing in without a browser: the phone app's device-link handoff (QR `ymusic-link:2:`, HMAC keystream + MAC, confirmation code, relay polling), cookie pastes (cURL, Cookie header, cookies.txt, DevTools table, JSON), and the sealed cookie vault (the JVM `YMC1U`/`YMC1P` AES-GCM format, Keychain key in release builds).
- **`ym-settings`** — the `SettingsStore` trait shared by UI and engine (text values, typed reads), an in-memory store, and `AppDb`: `app.db` with the JVM app's `setting` / `secret` tables.
- **`ym-i18n`** — UI strings generated at build time from YMusicV4's compose-resources (`Str` / `Plural` keys, `t` / `tf` / `plural`, CLDR plurals, English fallback). Never hardcode UI text; add strings to every locale in YMusicV4.
- **`ymusic`** — the executable: boot (tokio + engine), gpui image client on reqwest, the shell (section sidebar, per-section back/forward stacks, search with suggestions) and feed surfaces. Playback over two engines (`Playback` + `QueueItem`: YouTube via the player engine + MSE host booted on first play, local files via `local_engine.rs` on `file_av`; radio / section / local queues), the Library (`library.rs`: scanned folder tree, files, Scan folder via the native picker) the mini player, Now Playing (video in an `AVSampleBufferDisplayLayer` over the window via `video_layer.rs`, audio-only switch, Up next queue), and the OS media session (souvlaki: Now Playing info + media keys). Debug builds take `YMUSIC_DEV_ROUTE` (`trending`, `music`, `search:<q>`, `play:<url>`, `nowplaying:<url>`, `download:<FORMAT>:<videoId>` with `YMUSIC_DEV_DOWNLOAD_DIR`, `playagain`, `playagain:resume`, `downloads`, `downloaddialog:<videoId>`, `settings`, `signin`, `sound`, `sleeptimer`, `feedback`, `librarytab:<n>`, `discovertab:<n>`, `searchinput`, `drawer`) to open a surface at start, floating, for screenshots, `YMUSIC_DEV_MUTE` to start muted, `YMUSIC_DEV_PLAYER` (`speed=,pitch=,preset=,loop=,shuffle=,sleep=,reload=<s>`) to set player state at start, `YMUSIC_DEV_WINDOW=WxH` for the window size, `YMUSIC_DEV_QUIT_AFTER=<s>` to quit normally (quit hooks run), and `YMUSIC_DATA_DIR` for a throwaway data dir. The YouTube player session (`youtube_session.rs`) is shared by playback and downloads (`downloads.rs`).

## Build inputs

`YMUSIC_REMOTE_CONFIG_URL`, `YMUSIC_BUNDLE_PUBLIC_KEY`, `YMUSIC_API_KEY` and `YMUSIC_SENTRY_DSN` are read at compile time (private release inputs; unset = embedded bundles and defaults only, and no QR sign-in). Locally they come from the gitignored `secret.properties` at the workspace root (keys in `secret.properties.example`, values from YMusicV4's `secrets.properties`/`local.properties`); an env var of the same name wins. Never commit `secret.properties`. The embedded bundle revisions come from YMusicV4's `Versions.engineJsVersion` / `playerJsVersion`.

Linux builds link `ffmpeg-build/out/linux-<arch>` and need the development packages `libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libxcb1-dev libfreetype-dev libfontconfig-dev libpulse-dev`.

## Rules

### Style
- `cargo fmt` and `cargo clippy --workspace -- -D warnings` must pass.
- Descriptive names (`engine_player`, `error`), never single letters.
- **Comments**: default to none. Only a genuine hack or a non-obvious constraint/invariant earns one,
  in one line. The story of a change goes in the commit message, never the code ("used to",
  "instead of", "previously" are banned). Doc comments state contracts, not the name restated.

### Accessibility (from day one)
Every UI change ships accessible; it is not a later pass.
- **Every control has a spoken name.** An icon-only `Button` needs `.accessibility_label(..)`: its tooltip is only the description, so without a label VoiceOver reads nothing. Sliders and switches take `.accessibility_label(..)` too. Names come from YMusicV4's `cd_*` / `a11y_*` strings, never hardcoded.
- **Clickable rows and cards are controls.** A clickable `div` gets `.role(Role::Button)` (or `ListItem` inside a list), one `.aria_label(..)` that says what it is (title · artist · duration · kind, the `a11y_kind_*` / `a11y_x_of_y` strings), and is keyboard reachable. Its children are not read one by one.
- **Focus goes through `a11y.rs`.** gpui's `.tab_stop(true)` alone does NOT make a div focusable (only `tab_index` / `track_focus` / `focusable` do), so it is never reached by Tab and never announced. Every list item, card and tab is `FocusList::item(index, ..)`; a `Button` that arrows should reach (chip, pill, tab-like toggle) is `FocusList::button(..)` (or `wrap` after `track_focus(&list.handle(..))` when it is a dropdown). Arrow keys then move geometrically to the nearest stop on screen (Compose / FocusFinder), across lists; `container(..)` adds Home/End. A single div control is `.keyboard_focusable(theme.ring)`. A focused element is announced only with an element id and a role.
- **State is exposed, not just drawn.** Selected rail item / tab, toggled shuffle / repeat / sleep, checked menu items: set the component's selected/toggled/checked state, not only a color.
- **Images are decorative unless they carry information.** Thumbnails beside a title stay unlabeled; an image that is the only content (QR code, album art alone) gets a label (`cd_device_link_qr`).
- **Keyboard first.** Everything reachable by mouse is reachable by Tab / arrows / Enter / Space / Esc, in visual order; dialogs trap focus and close on Esc; focus returns to the opener.
- **No meaning by color alone** (errors also say so in text); no motion (the app runs with reduce-motion on).

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

### macOS Keychain in development
- Debug builds keep the cookie key in `app.db`: every rebuild re-signs the binary, and a Keychain read from a re-signed or different binary stops on an access prompt. Never read the app's Keychain item from another binary.
