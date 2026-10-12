//! cef-rs' build script copies CEF's runtime files beside the executables
//! cargo builds. One of them is Chromium's own Vulkan loader, under the name
//! of the system's, in a directory cargo puts first on the library path of
//! everything it runs: an application started with `cargo run` would open it
//! for its own renderer instead of the system's. Chromium runs without the
//! GPU here and does not need it, so the copy does not stay.

use std::path::PathBuf;

fn main() {
    // Set by cef-dll-sys' script, which therefore runs before this one.
    println!("cargo::rerun-if-env-changed=DEP_CEF_DLL_WRAPPER_CEF_DIR");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    // target/<profile>/build/<package>/out
    if let Some(profile) = out.ancestors().nth(3) {
        let _ = std::fs::remove_file(profile.join("libvulkan.so.1"));
    }
}
