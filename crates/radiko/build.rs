fn main() {
    println!("cargo:rerun-if-changed=src/player/alsa_error.c");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "linux" {
        return;
    }
    cc::Build::new()
        .file("src/player/alsa_error.c")
        .compile("radiko_alsa_error");
    println!("cargo:rustc-link-lib=asound");
}
