pub mod dirs;

#[cfg(feature = "download-binaries")]
include!(concat!(env!("OUT_DIR"), "/downloaded_version.rs"));
