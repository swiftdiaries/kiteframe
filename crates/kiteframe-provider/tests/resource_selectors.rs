use kiteframe_provider::{
    intersect_resource_selectors, resource_selector_is_subset, validate_concrete_resource_selector,
};

#[test]
fn selectors_are_segment_aware_and_do_not_accept_extra_segments() {
    assert!(resource_selector_is_subset(
        "tenant:t1/case:case-7",
        "tenant:t1/case:*"
    ));
    assert!(!resource_selector_is_subset(
        "tenant:t1/case:case-7/detail:private",
        "tenant:t1/case:*"
    ));
    assert!(!resource_selector_is_subset(
        "tenant:t1/case:case-7",
        "tenant:t1:case:*"
    ));
}

#[test]
fn invocation_selectors_reject_wildcards_in_every_segment() {
    for selector in [
        "tenant:*/case:case-7",
        "tenant:t1/case:*",
        "tenant:t1/*:case-7",
        "tenant:t1/case:case-*",
    ] {
        assert!(
            validate_concrete_resource_selector(selector).is_err(),
            "{selector} must not be a concrete invocation resource"
        );
    }
}

#[test]
fn authority_intersection_uses_the_same_exact_segment_model() {
    assert_eq!(
        intersect_resource_selectors("tenant:t1/case:*", "tenant:*/case:case-7").unwrap(),
        Some("tenant:t1/case:case-7".to_owned())
    );
    assert_eq!(
        intersect_resource_selectors("tenant:t1/case:*", "tenant:t1/case:case-7/detail:*").unwrap(),
        None
    );
}
