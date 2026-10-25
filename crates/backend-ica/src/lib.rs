//! macOS backend over Apple's ImageCaptureCore (ICA). Covers USB scanners and
//! network scanners found through Bonjour, using whatever ICA driver the vendor
//! installed (Brother ships one for the ADS series).

#[cfg(target_os = "macos")]
mod imp;
#[cfg(target_os = "macos")]
pub use imp::{IcaBackend, run_with_main_loop};
