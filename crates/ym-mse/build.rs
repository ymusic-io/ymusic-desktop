//! Builds YMusicV4's `:mse` C host (MSE over ffmpeg, the audio DAC, video decode + presenters)
//! against the shared libav in `ffmpeg-build/out/<slug>`. Exports the lib dir as
//! `DEP_MSEHOST_FFMPEG_LIB` so executables can add it as an rpath.

use std::path::PathBuf;

const APPLE_C: &[&str] = &[
    "mse_host.c",
    "mse_ffmpeg.c",
    "mse_caps.c",
    "mse_vt.c",
    "mse_vt_blit.c",
    "audio_fx.c",
    "vpresent.c",
    "vpresent_cpu.c",
    "vpresent_vt.c",
    "vpipe.c",
    "vpipe_hw_vt.c",
    "file_av.c",
];
const APPLE_OBJC: &[&str] = &["mse_vt_layer.m", "mse_vt_dec.m"];
const APPLE_FRAMEWORKS: &[&str] = &[
    "AudioToolbox",
    "CoreAudio",
    "VideoToolbox",
    "CoreMedia",
    "CoreFoundation",
    "CoreVideo",
    "AVFoundation",
    "QuartzCore",
];
const LIBAV: &[&str] = &["avformat", "avcodec", "swscale", "swresample", "avutil"];

fn main() {
    println!("cargo:rerun-if-env-changed=YMUSIC_V4_DIR");
    println!("cargo:rerun-if-env-changed=FFMPEG_ROOT");
    let v4 = PathBuf::from(std::env::var("YMUSIC_V4_DIR").expect("YMUSIC_V4_DIR (set in .cargo/config.toml)"));
    let native = v4.join("mse/native");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    assert_eq!(target_os, "macos", "ym-mse builds for macOS only so far");
    let slug = format!("macos-{}", if target_arch == "aarch64" { "arm64" } else { "x86_64" });
    let ffmpeg = std::env::var("FFMPEG_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| v4.parent().unwrap().join("ffmpeg-build/out").join(&slug));
    assert!(ffmpeg.join("include").is_dir(), "no ffmpeg build at {} (set FFMPEG_ROOT)", ffmpeg.display());

    let production = if std::env::var("PROFILE").as_deref() == Ok("release") { "1" } else { "0" };
    let mut c = cc::Build::new();
    c.include(&native)
        .include(v4.join("quickjs/native"))
        .include(ffmpeg.join("include"))
        .opt_level(2)
        .warnings(false)
        .flag_if_supported("-w")
        .define("PRODUCTION", production);
    for source in APPLE_C {
        c.file(native.join(source));
    }
    for glue in ["native/vt_overlay_stub.c", "native/ym_probe.c"] {
        c.file(glue);
        println!("cargo:rerun-if-changed={glue}");
    }
    c.compile("msehost_c");
    let mut objc = cc::Build::new();
    objc.include(&native)
        .opt_level(2)
        .warnings(false)
        .flag("-fobjc-arc")
        .flag_if_supported("-w")
        .define("PRODUCTION", production);
    for source in APPLE_OBJC {
        objc.file(native.join(source));
    }
    objc.compile("msehost_objc");
    for entry in std::fs::read_dir(&native).unwrap().flatten() {
        println!("cargo:rerun-if-changed={}", entry.path().display());
    }

    let lib = ffmpeg.join("lib");
    println!("cargo:rustc-link-search=native={}", lib.display());
    for name in LIBAV {
        println!("cargo:rustc-link-lib=dylib={name}");
    }
    for framework in APPLE_FRAMEWORKS {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
    for name in ["z", "bz2", "iconv"] {
        println!("cargo:rustc-link-lib=dylib={name}");
    }
    println!("cargo:ffmpeg_lib={}", lib.display());
}
