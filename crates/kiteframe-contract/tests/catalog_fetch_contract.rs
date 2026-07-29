use kiteframe_contract::{
    CapabilityCatalog, CatalogFetchResult, CatalogIdentity, Sha256Digest, Timestamp,
};

fn catalog_identity() -> CatalogIdentity {
    CatalogIdentity {
        name: "provider.test".to_owned(),
        revision: "revision-1".to_owned(),
    }
}

fn catalog() -> CapabilityCatalog {
    CapabilityCatalog::try_new(
        catalog_identity(),
        Timestamp::new(100),
        Some(Timestamp::new(200)),
        vec![],
    )
    .unwrap()
}

fn digest() -> Sha256Digest {
    Sha256Digest::from_bytes([9; Sha256Digest::BYTE_LENGTH])
}

#[test]
fn catalog_expiry_must_follow_issue_time() {
    let error = CapabilityCatalog::try_new(
        catalog_identity(),
        Timestamp::new(200),
        Some(Timestamp::new(200)),
        vec![],
    )
    .unwrap_err();

    assert_eq!(error, "catalog expiry must be after its issue time");
}

#[test]
fn modified_and_not_modified_are_disjoint_typed_results() {
    let modified = CatalogFetchResult::Modified { catalog: catalog() };
    let not_modified = CatalogFetchResult::NotModified {
        catalog_digest: digest(),
    };

    assert_ne!(
        serde_json::to_vec(&modified).unwrap(),
        serde_json::to_vec(&not_modified).unwrap()
    );
}

#[test]
fn catalog_digest_covers_issued_and_expiry_times() {
    let baseline = catalog();
    let changed_issue = CapabilityCatalog::try_new(
        catalog_identity(),
        Timestamp::new(101),
        Some(Timestamp::new(200)),
        vec![],
    )
    .unwrap();
    let changed_expiry = CapabilityCatalog::try_new(
        catalog_identity(),
        Timestamp::new(100),
        Some(Timestamp::new(201)),
        vec![],
    )
    .unwrap();

    assert_ne!(baseline.catalog_digest(), changed_issue.catalog_digest());
    assert_ne!(baseline.catalog_digest(), changed_expiry.catalog_digest());
}
