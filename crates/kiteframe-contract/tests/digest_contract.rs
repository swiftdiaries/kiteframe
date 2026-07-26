use kiteframe_contract::Sha256Digest;

const LOWERCASE_DIGEST: &str = "abababababababababababababababababababababababababababababababab";

#[test]
fn sha256_digest_has_an_exact_lowercase_hex_wire_format() {
    let digest = Sha256Digest::from_bytes([0xab; Sha256Digest::BYTE_LENGTH]);

    assert_eq!(
        serde_json::to_string(&digest).unwrap(),
        format!("\"{LOWERCASE_DIGEST}\"")
    );
    assert_eq!(
        serde_json::from_str::<Sha256Digest>(&format!("\"{LOWERCASE_DIGEST}\"")).unwrap(),
        digest
    );

    for invalid in [
        "ABABABABABABABABABABABABABABABABABABABABABABABABABABABABABAB",
        "ababababababababababababababababababababababababababababababab",
        "gbababababababababababababababababababababababababababababababab",
    ] {
        assert!(
            serde_json::from_str::<Sha256Digest>(&format!("\"{invalid}\"")).is_err(),
            "{invalid:?} must not deserialize"
        );
    }
}

#[test]
fn sha256_digest_schema_matches_its_wire_contract() {
    let schema = serde_json::to_value(schemars::schema_for!(Sha256Digest)).unwrap();

    assert_eq!(schema["type"], "string");
    assert_eq!(schema["pattern"], "^[0-9a-f]{64}$");
}
