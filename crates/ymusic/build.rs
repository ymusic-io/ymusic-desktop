use std::path::PathBuf;

fn main() {
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    // The C MSE host links libav by @rpath; point the executable at the ffmpeg build.
    if let Ok(lib) = std::env::var("DEP_MSEHOST_FFMPEG_LIB")
        && !windows
    {
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,{lib}");
    }
    if let Ok(dlls) = std::env::var("DEP_MSEHOST_FFMPEG_DLLS") {
        copy_dlls_beside_executable(&PathBuf::from(dlls));
    }
    if windows {
        embed_windows_resources();
    }
    if let Ok(binary) = std::env::var("DEP_MSEHOST_FFMPEG_BIN") {
        println!("cargo:rustc-env=YM_FFMPEG_BIN={binary}");
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
    // The app's version continues the JVM desktop line: YMusicV4's ymusic.versionName and
    // desktopVersionCode, which the update feed compares; YMUSIC_VERSION_NAME / YMUSIC_VERSION_CODE
    // override them for a release that runs ahead of that checkout.
    let gradle = v4.join("desktopApp/build.gradle.kts");
    let properties = v4.join("gradle.properties");
    for (variable, file, prefix) in [
        ("YMUSIC_VERSION_CODE", &gradle, "val desktopVersionCode = "),
        ("YMUSIC_VERSION_NAME", &properties, "ymusic.versionName="),
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
        println!("cargo:rerun-if-changed={}", file.display());
        let value = std::env::var(variable).ok().filter(|value| !value.is_empty()).unwrap_or_else(|| {
            let text = std::fs::read_to_string(file).unwrap_or_else(|_| panic!("{}", file.display()));
            text.lines()
                .find_map(|line| line.trim().strip_prefix(prefix).map(|value| value.trim().to_owned()))
                .unwrap_or_else(|| panic!("{prefix} not found in {}", file.display()))
        });
        if variable == "YMUSIC_VERSION_CODE" {
            value.parse::<i32>().unwrap_or_else(|_| panic!("{variable} is not a number: {value}"));
        }
        println!("cargo:rustc-env=YM_{}={value}", variable.trim_start_matches("YMUSIC_"));
    }
    // Private release inputs; empty leaves the app on its embedded bundles and defaults. The
    // environment wins over the gitignored secret.properties at the workspace root.
    for variable in [
        "YMUSIC_REMOTE_CONFIG_URL",
        "YMUSIC_BUNDLE_PUBLIC_KEY",
        "YMUSIC_UPDATE_PUBLIC_KEY",
        "YMUSIC_API_KEY",
        "YMUSIC_SENTRY_DSN",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    let secrets = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../secret.properties");
    println!("cargo:rerun-if-changed={}", secrets.display());
    let Ok(text) = std::fs::read_to_string(&secrets) else { return };
    for (key, variable) in [
        ("remote.config.url", "YMUSIC_REMOTE_CONFIG_URL"),
        ("bundle.signing.public.key", "YMUSIC_BUNDLE_PUBLIC_KEY"),
        ("update.public.key", "YMUSIC_UPDATE_PUBLIC_KEY"),
        ("api.key", "YMUSIC_API_KEY"),
        ("sentry.dsn", "YMUSIC_SENTRY_DSN"),
    ] {
        if std::env::var_os(variable).is_some() {
            continue;
        }
        let value = text.lines().find_map(|line| {
            let (name, value) = line.split_once('=')?;
            (name.trim() == key).then(|| unescape(value.trim()))
        });
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            println!("cargo:rustc-env={variable}={value}");
        }
    }
}

/// Java properties escapes (`\:`, `\=`, `\\`), which the JVM build's files may carry.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(char) = chars.next() {
        if char == '\\' {
            out.extend(chars.next());
        } else {
            out.push(char);
        }
    }
    out
}

/// Windows loads DLLs from the executable's folder: libav and the MinGW runtime go beside it in
/// the profile dir (`OUT_DIR` is `<profile>/build/<crate>/out`), copied only when they differ.
fn copy_dlls_beside_executable(source: &std::path::Path) {
    println!("cargo:rerun-if-changed={}", source.display());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let profile = out.ancestors().nth(3).expect("OUT_DIR inside the profile dir");
    for entry in std::fs::read_dir(source).unwrap_or_else(|_| panic!("{}", source.display())).flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|extension| !extension.eq_ignore_ascii_case("dll")) {
            continue;
        }
        let target = profile.join(entry.file_name());
        let same = |a: &std::path::Path, b: &std::path::Path| {
            let (Ok(a), Ok(b)) = (a.metadata(), b.metadata()) else { return false };
            a.len() == b.len() && a.modified().ok() == b.modified().ok()
        };
        if same(&path, &target) {
            continue;
        }
        std::fs::copy(&path, &target)
            .unwrap_or_else(|error| panic!("{} → {}: {error}", path.display(), target.display()));
    }
}

/// The JVM app's icon as the executable's (gpui's `windows-manifest` feature embeds the manifest).
fn embed_windows_resources() {
    let v4 = PathBuf::from(std::env::var("YMUSIC_V4_DIR").unwrap());
    let icon = v4.join("desktopApp/ymusic.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let rc = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("ymusic.rc");
    std::fs::write(&rc, format!("1 ICON \"{}\"\n", icon.display().to_string().replace('\\', "/"))).unwrap();
    embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().unwrap();
}
