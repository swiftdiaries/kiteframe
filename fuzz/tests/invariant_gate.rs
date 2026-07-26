use kiteframe_core_fuzz::exercise_strict_yaml_input;

#[test]
fn deterministic_modes_observe_every_configured_parser_limit() {
    for seed in [
        include_bytes!("../seeds/strict_yaml/byte-limit").as_slice(),
        include_bytes!("../seeds/strict_yaml/nesting-limit").as_slice(),
        include_bytes!("../seeds/strict_yaml/collection-limit").as_slice(),
        include_bytes!("../seeds/strict_yaml/alias-limit").as_slice(),
    ] {
        exercise_strict_yaml_input(seed);
    }
}
