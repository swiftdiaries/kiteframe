use std::collections::BTreeMap;

use kiteframe_contract::{
    CompilationWarning, ComponentKind, ComponentMetadata, ComponentMetadataCatalog, Diagnostic,
    DiagnosticCategory, DiagnosticCode, DiagnosticStage, LatencyClass, ModelCapability,
    ModelLatencyClass, ModelModality, ModelRequirement, ModelRole, RegistrySymbol,
    ResolvedModelRequirement, RuntimeBinding,
};

pub(crate) struct ModelResolution {
    pub(crate) models: BTreeMap<ModelRole, ResolvedModelRequirement>,
    pub(crate) warnings: Vec<CompilationWarning>,
}

pub(crate) fn resolve_models(
    requirements: &BTreeMap<ModelRole, ModelRequirement>,
    binding: &RuntimeBinding,
    components: &ComponentMetadataCatalog,
) -> Result<ModelResolution, Vec<Diagnostic>> {
    let primary_role = requirements
        .keys()
        .find(|role| role.as_str() == "primary")
        .cloned();
    let mut resolved = BTreeMap::new();
    let mut warnings = Vec::new();
    let mut diagnostics = Vec::new();

    for (role, requirement) in requirements {
        let selected = binding.spec.models.get(role).and_then(|symbol| {
            model_satisfies(symbol, requirement, components).then(|| symbol.clone())
        });

        if let Some(symbol) = selected {
            resolved.insert(
                role.clone(),
                ResolvedModelRequirement::new(requirement.clone(), symbol),
            );
            continue;
        }

        let fallback = (!requirement.required)
            .then_some(())
            .and(primary_role.as_ref())
            .and_then(|primary| binding.spec.models.get(primary))
            .filter(|symbol| model_satisfies(symbol, requirement, components))
            .cloned();

        if let Some(symbol) = fallback {
            warnings.push(CompilationWarning {
                code: "KF-MODEL-OPTIONAL-FALLBACK".to_owned(),
                message: format!(
                    "optional model role {} fell back to primary model {}",
                    role,
                    symbol.as_str()
                ),
            });
            resolved.insert(
                role.clone(),
                ResolvedModelRequirement::new(requirement.clone(), symbol),
            );
        } else if requirement.required {
            diagnostics.push(component_unresolved(format!(
                "required model role {role} has no binding that satisfies every constraint"
            )));
        } else {
            warnings.push(CompilationWarning {
                code: "KF-MODEL-OPTIONAL-OMITTED".to_owned(),
                message: format!(
                    "optional model role {role} has no binding that satisfies every constraint"
                ),
            });
        }
    }

    if diagnostics.is_empty() {
        Ok(ModelResolution {
            models: resolved,
            warnings,
        })
    } else {
        diagnostics.sort();
        Err(diagnostics)
    }
}

fn model_satisfies(
    symbol: &RegistrySymbol,
    requirement: &ModelRequirement,
    components: &ComponentMetadataCatalog,
) -> bool {
    let Some(component) = components.components.get(symbol) else {
        return false;
    };
    if component.kind != ComponentKind::Model {
        return false;
    }
    let Some(model) = component.model.as_ref() else {
        return false;
    };

    requirement
        .capabilities
        .iter()
        .all(|capability| match capability {
            ModelCapability::Text => model.modalities.contains(&ModelModality::Text),
            ModelCapability::ToolCalling => model.tool_calling,
            ModelCapability::StructuredOutput => model.structured_output,
        })
        && requirement
            .min_context_tokens
            .is_none_or(|required| model.max_context_tokens >= required)
        && requirement
            .max_latency_class
            .is_none_or(|maximum| match maximum {
                LatencyClass::Interactive => model.latency_class == ModelLatencyClass::Interactive,
            })
        && requirement
            .residency
            .as_ref()
            .is_none_or(|required| &model.residency == required)
}

pub(crate) fn require_component_kind<'a>(
    components: &'a ComponentMetadataCatalog,
    symbol: &RegistrySymbol,
    expected: ComponentKind,
) -> Result<&'a ComponentMetadata, Diagnostic> {
    components
        .components
        .get(symbol)
        .filter(|component| component.kind == expected)
        .ok_or_else(|| {
            component_unresolved(format!(
                "component {} is absent or is not of expected kind {:?}",
                symbol.as_str(),
                expected
            ))
        })
}

pub(crate) fn component_unresolved(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::ComponentUnresolved,
        DiagnosticCategory::Runtime,
        DiagnosticStage::Resolve,
        message.into(),
    )
}
