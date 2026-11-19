fn main() {
    // GPU builds link whisper.cpp's Vulkan backend against vulkan-1.dll. Delay-load
    // it so the app still starts (and records) on machines without a Vulkan driver;
    // transcription checks for the DLL before touching whisper.cpp.
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if windows && msvc && std::env::var_os("CARGO_FEATURE_GPU").is_some() {
        println!("cargo:rustc-link-arg=/DELAYLOAD:vulkan-1.dll");
        println!("cargo:rustc-link-arg=delayimp.lib");
    }
}
