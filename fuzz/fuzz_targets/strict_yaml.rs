#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    let _ = kiteframe_core::parse_manifest(bytes, kiteframe_core::PackageLimits::V1);
});
