#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|bytes: &[u8]| {
    kiteframe_core_fuzz::exercise_strict_yaml_input(bytes);
});
