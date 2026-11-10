use std::path::PathBuf;

fn main() {
    // The C MSE host links libav by @rpath; point the executable at the ffmpeg build.
    if let Ok(lib) = std::env::var("DEP_MSEHOST_FFMPEG_LIB") {
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,{lib}");
    }
    // The embedded bundles' revisions: YMusicV4's Versions.engineJsVersion / playerJsVersion.
    let v4 = PathBuf::from(std::env::var("YMUSIC_V4_DIR").expect("YMUSIC_V4_DIR (set in .cargo/config.toml)"));
    let versions = v4.join("buildSrc/src/main/kotlin/Dependencies.kt");
    println!("cargo:rerun-if-changed={}", versions.display());
    let text = std::fs::read_to_string(&versions).expect("YMusicV4 buildSrc Dependencies.kt");
    for (constant, variable) in
        [("engineJsVersion", "YM_ENGINE_JS_VERSION"), ("playerJsVersion", "YM_PLAYER_JS_VERSION")]
    {
        let value = text
            .lines()
            .find_map(|line| {
                line.split_once(&format!("const val {constant} = "))?.1.trim().strip_suffix('L')?.parse::<i64>().ok()
            })
            .unwrap_or_else(|| panic!("{constant} not found in {}", versions.display()));
        println!("cargo:rustc-env={variable}={value}");
    }
    // Private release inputs; empty leaves the app on its embedded bundles and defaults.
    for variable in ["YMUSIC_REMOTE_CONFIG_URL", "YMUSIC_BUNDLE_PUBLIC_KEY", "YMUSIC_API_KEY", "YMUSIC_SENTRY_DSN"] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
}
