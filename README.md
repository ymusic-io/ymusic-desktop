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
