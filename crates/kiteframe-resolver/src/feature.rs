use kiteframe_contract::{
    Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticStage, FeatureId, FeatureNegotiation,
    FeatureSet,
};

/// Negotiates only exact feature names with the same declared major version.
pub fn negotiate_features(
    required: &FeatureSet,
    optional: &FeatureSet,
    supported: &FeatureSet,
) -> Result<FeatureNegotiation, Vec<Diagnostic>> {
    let missing_required = difference_compatible(required, supported);
    if !missing_required.is_empty() {
        return Err(missing_required
            .into_iter()
            .map(required_feature_diagnostic)
            .collect());
    }

    Ok(FeatureNegotiation {
        enabled_optional: intersection_compatible(optional, supported),
        omitted_optional: difference_compatible(optional, supported),
    })
}

fn compatible(feature: &FeatureId, supported: &FeatureSet) -> bool {
    supported
        .iter()
        .any(|candidate| candidate.name() == feature.name() && candidate.major() == feature.major())
}

fn difference_compatible(left: &FeatureSet, right: &FeatureSet) -> FeatureSet {
    left.iter()
        .filter(|feature| !compatible(feature, right))
        .cloned()
        .collect()
}

fn intersection_compatible(left: &FeatureSet, right: &FeatureSet) -> FeatureSet {
    left.iter()
        .filter(|feature| compatible(feature, right))
        .cloned()
        .collect()
}

fn required_feature_diagnostic(feature: FeatureId) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::FeatureUnsupported,
        DiagnosticCategory::Feature,
        DiagnosticStage::Resolve,
        format!("runtime target does not support required feature {feature}"),
    )
}
