# ymusic-rust

The YMusic desktop app in Rust: a Rust core under a [gpui-kit](https://gpui-kit.com) UI. Plan and
milestones: [`PLAN.md`](PLAN.md).

## Build

Needs a YMusicV4 checkout next to this one (or `YMUSIC_V4_DIR` pointing at it): the C sources
(QuickJS, MSE) and the engine bundle come from there.

```sh
cargo run -p ymusic
```

macOS needs Xcode's Metal toolchain (gpui compiles its shaders at build time).

## Package (macOS)

```sh
tools/bundle-macos.sh            # → target/bundle/YMusic.app
```

Builds `--release` and assembles `YMusic.app` (bundle id `io.ymusic.desktop.rust`): `ymusic://` links,
media files under Open With (rank Alternate), the icon from YMusicV4, and the libav dylibs
(`Contents/Frameworks`) plus the packed `ffmpeg` (`Contents/MacOS`, where downloads look first)
relinked to load only from the bundle. Signed ad-hoc unless `CODESIGN_IDENTITY` names an identity
(then with the hardened runtime); not notarized.

A release build keeps the cookie key in the login Keychain. An ad-hoc signature changes with every
build, so a rebuilt bundle stops at launch, before any window, on a Keychain access prompt for the item
an earlier build created. To try a bundle without touching your profile, give it a throwaway `HOME`
(release builds ignore `YMUSIC_DATA_DIR`), which also hides the login Keychain, and unregister it
afterwards so `ymusic://` links don't route to a stale copy (macOS still writes
`~/Library/Preferences/io.ymusic.desktop.rust.plist`):

```sh
open -n --env HOME=/tmp/ym-home target/bundle/YMusic.app
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -u target/bundle/YMusic.app
```
