use std::env;

fn main() {
    // OSAKit is required for in-process OSA (JavaScript-for-Automation) script
    // execution on macOS. `Foundation` is linked transitively by
    // `objc2-foundation`, but OSAKit must be requested explicitly.
    //
    // Gated on the *target* OS (via CARGO_CFG_TARGET_OS, not host `cfg!`) so
    // cross-compiling to non-macOS targets does not attempt to link it.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-lib=framework=OSAKit");
    }
}
