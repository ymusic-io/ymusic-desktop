fn main() {
    // The C MSE host links libav by @rpath; point the executable at the ffmpeg build.
    if let Ok(lib) = std::env::var("DEP_MSEHOST_FFMPEG_LIB") {
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,{lib}");
    }
}
