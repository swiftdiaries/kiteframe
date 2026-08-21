use std::collections::BTreeSet;
use std::fs;

use kiteframe::harness::{HarnessCapability, HarnessProfile, HarnessRouteTable};
use kiteframe::{HarnessRole, ModelRole};

fn profile(path: &str) -> HarnessProfile {
    HarnessProfile::from_json(&fs::read(path).expect("profile file")).expect("valid profile")
}

fn profile_id(value: &str) -> kiteframe::harness::HarnessProfileId {
    kiteframe::harness::HarnessProfileId::new(value).expect("profile id")
}

#[test]
fn v1_profiles_pin_exact_packages_and_share_only_the_portable_core() {
    let codex = profile("config/harness-profiles/codex-acp-v1.json");
    let deepseek = profile("config/harness-profiles/deepseek-harness-acp-v1.json");
    let core = BTreeSet::from([
        HarnessCapability::FreshSession,
        HarnessCapability::TextPrompt,
        HarnessCapability::PermissionOnce,
        HarnessCapability::Cancel,
        HarnessCapability::CommittedOutput,
    ]);
    assert_eq!(
        codex.source_revision,
        "296069e841634cd4bb9bc4515602d836e49231ec"
    );
    assert_eq!(
        deepseek.source_revision,
        "141eb6fef83422698aef7a981029e843e8161534"
    );
    assert_eq!(codex.required, core);
    assert_eq!(deepseek.required, core);
    assert!(deepseek.fresh_sessions_only);
    assert!(!deepseek.optional.contains(&HarnessCapability::SessionLoad));
}

#[test]
fn absent_optional_capability_fails_closed_when_requested() {
    let deepseek = profile("config/harness-profiles/deepseek-harness-acp-v1.json");
    let error = deepseek
        .require(HarnessCapability::SessionLoad)
        .unwrap_err();
    assert_eq!(error.code, "harness_capability_unsupported");
}

#[test]
fn deployment_route_table_selects_profiles_without_changing_kite_source() {
    let routes = HarnessRouteTable::new(profile_id("builtin-v1"), profile_id("codex-acp-v1"));
    assert_eq!(
        routes
            .select(Some(HarnessRole::Coding), ModelRole::Reasoning)
            .as_str(),
        "codex-acp-v1"
    );
    assert_eq!(
        routes.select(None, ModelRole::Reasoning).as_str(),
        "builtin-v1"
    );
}
