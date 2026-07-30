use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use kiteframe_contract::{
    AuthorityRevisionSet, CapabilityIdentity, Diagnostic, DiagnosticCategory, DiagnosticCode,
    DiagnosticStage, EffectiveCapabilityGrant, LockedCapability, NormalizedResourceSelector,
    Sha256Digest, StableCapabilityError, TraceContext, resource_selector_is_subset_of,
};
use serde_json::Value;

use crate::{
    AuthenticatedInvocationContext, AuthorizationBackend, AuthorizationDecision,
    InvocationAuthorizationRequest, NarrowedAuthorizationConditions, require_current_authorization,
};

#[derive(Clone, Debug, PartialEq)]
pub struct Precondition {
    name: String,
    value: Value,
}

impl Precondition {
    pub fn try_new(name: impl Into<String>, value: Value) -> Result<Self, String> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err("precondition name is required".to_owned());
        }
        Ok(Self { name, value })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn value(&self) -> &Value {
        &self.value
    }
}

#[derive(Clone, Debug)]
pub struct InvocationContext {
    principals: AuthenticatedInvocationContext,
    capability: CapabilityIdentity,
    selected_resource: NormalizedResourceSelector,
    trace_context: TraceContext,
    locked_capability: LockedCapability,
    effective_grant: EffectiveCapabilityGrant,
    grant_digest: Sha256Digest,
    loaded_authority_revisions: AuthorityRevisionSet,
}

impl InvocationContext {
    #[allow(clippy::too_many_arguments)]
    pub fn try_new(
        principals: AuthenticatedInvocationContext,
        capability: CapabilityIdentity,
        selected_resource: NormalizedResourceSelector,
        trace_context: TraceContext,
        locked_capability: LockedCapability,
        effective_grant: EffectiveCapabilityGrant,
        grant_digest: Sha256Digest,
        loaded_authority_revisions: AuthorityRevisionSet,
    ) -> Result<Self, Diagnostic> {
        if locked_capability.identity() != &capability
            || effective_grant.capability() != &capability
            || !effective_grant.resources().iter().any(|granted| {
                resource_selector_is_subset_of(selected_resource.as_str(), granted.as_str())
            })
        {
            return Err(capability_error(
                "invocation does not match persisted exact capability state",
            ));
        }

        Ok(Self {
            principals,
            capability,
            selected_resource,
            trace_context,
            locked_capability,
            effective_grant,
            grant_digest,
            loaded_authority_revisions,
        })
    }

    pub fn principals(&self) -> &AuthenticatedInvocationContext {
        &self.principals
    }

    pub fn capability(&self) -> &CapabilityIdentity {
        &self.capability
    }

    pub fn selected_resource(&self) -> &NormalizedResourceSelector {
        &self.selected_resource
    }

    pub fn trace_context(&self) -> &TraceContext {
        &self.trace_context
    }

    pub fn locked_capability(&self) -> &LockedCapability {
        &self.locked_capability
    }

    pub fn effective_grant(&self) -> &EffectiveCapabilityGrant {
        &self.effective_grant
    }

    pub fn grant_digest(&self) -> &Sha256Digest {
        &self.grant_digest
    }

    pub fn loaded_authority_revisions(&self) -> &AuthorityRevisionSet {
        &self.loaded_authority_revisions
    }
}

fn condition_allows_resource(
    conditions: &NarrowedAuthorizationConditions,
    selected: &NormalizedResourceSelector,
) -> bool {
    conditions
        .resources()
        .iter()
        .any(|allowed| resource_selector_is_subset_of(selected.as_str(), allowed.as_str()))
}

#[derive(Clone, Debug)]
pub enum OperationFailure {
    Stable(StableCapabilityError),
    Diagnostic(Diagnostic),
}

impl OperationFailure {
    pub fn stable(error: StableCapabilityError) -> Self {
        Self::Stable(error)
    }

    pub fn diagnostic(&self) -> Diagnostic {
        match self {
            Self::Diagnostic(diagnostic) => diagnostic.clone(),
            Self::Stable(_) => capability_error("provider returned a stable capability error"),
        }
    }
}

impl From<Diagnostic> for OperationFailure {
    fn from(value: Diagnostic) -> Self {
        Self::Diagnostic(value)
    }
}

#[async_trait]
pub trait CapabilityOperation: Send + Sync {
    fn identity(&self) -> &CapabilityIdentity;

    async fn validate_preconditions(
        &self,
        context: &InvocationContext,
        preconditions: &[Precondition],
    ) -> Result<(), Diagnostic>;

    async fn execute(
        &self,
        context: &InvocationContext,
        arguments: Value,
    ) -> Result<Value, OperationFailure>;
}

pub struct OperationRegistry {
    operations: BTreeMap<CapabilityIdentity, Arc<dyn CapabilityOperation>>,
    frozen: bool,
}

impl OperationRegistry {
    pub fn new() -> Self {
        Self {
            operations: BTreeMap::new(),
            frozen: false,
        }
    }

    pub fn register<O>(&mut self, operation: O) -> Result<(), Diagnostic>
    where
        O: CapabilityOperation + 'static,
    {
        if self.frozen {
            return Err(runtime_error("operation registry is frozen"));
        }
        let identity = operation.identity().clone();
        if self.operations.contains_key(&identity) {
            return Err(runtime_error(
                "operation registry contains a duplicate exact capability identity",
            ));
        }
        self.operations.insert(identity, Arc::new(operation));
        Ok(())
    }

    pub fn freeze(mut self) -> Result<Self, Diagnostic> {
        self.frozen = true;
        Ok(self)
    }

    pub fn is_frozen(&self) -> bool {
        self.frozen
    }

    pub async fn execute(
        &self,
        backend: &dyn AuthorizationBackend,
        context: &InvocationContext,
        preconditions: &[Precondition],
        arguments: Value,
    ) -> Result<Value, OperationFailure> {
        if !self.frozen {
            return Err(runtime_error("operation registry must be frozen before use").into());
        }
        let operation = self.operations.get(context.capability()).ok_or_else(|| {
            OperationFailure::from(runtime_error(
                "no trusted operation is registered for the exact capability identity",
            ))
        })?;
        if operation.identity() != context.locked_capability().identity() {
            return Err(runtime_error(
                "registered operation does not match the locked capability identity",
            )
            .into());
        }

        let authorization_request = InvocationAuthorizationRequest::new(
            context.principals().clone(),
            context.capability().clone(),
            context.selected_resource().clone(),
            *context.grant_digest(),
            context.loaded_authority_revisions().clone(),
        );
        let authorization = require_current_authorization(backend, &authorization_request)
            .await
            .map_err(OperationFailure::from)?;
        validate_current_authorization(context, &authorization, preconditions)
            .map_err(OperationFailure::from)?;

        context
            .locked_capability()
            .descriptor()
            .validate_input(&arguments)
            .map_err(OperationFailure::from)?;
        operation
            .validate_preconditions(context, preconditions)
            .await
            .map_err(OperationFailure::from)?;
        match operation.execute(context, arguments).await {
            Ok(output) => {
                context
                    .locked_capability()
                    .descriptor()
                    .validate_output(&output)
                    .map_err(OperationFailure::from)?;
                Ok(output)
            }
            Err(OperationFailure::Stable(error)) => {
                context
                    .locked_capability()
                    .descriptor()
                    .validate_stable_error(&error)
                    .map_err(OperationFailure::from)?;
                Err(OperationFailure::Stable(error))
            }
            Err(error) => Err(error),
        }
    }
}

fn validate_current_authorization(
    context: &InvocationContext,
    authorization: &AuthorizationDecision,
    supplied_preconditions: &[Precondition],
) -> Result<(), Diagnostic> {
    let AuthorizationDecision::Allow {
        decided_at,
        narrowed_conditions,
        ..
    } = authorization
    else {
        return Err(authorization_error(
            "invocation has no current authorization",
        ));
    };
    if context.principals().expires_at() <= *decided_at
        || context.effective_grant().expires_at() <= *decided_at
        || narrowed_conditions.expires_at() <= *decided_at
        || !condition_allows_resource(narrowed_conditions, context.selected_resource())
    {
        return Err(authorization_error(
            "current authorization does not match persisted invocation state",
        ));
    }

    let all_required_present = context
        .effective_grant()
        .preconditions()
        .iter()
        .chain(narrowed_conditions.required_preconditions())
        .filter(|required| required.required)
        .all(|required| {
            supplied_preconditions
                .iter()
                .any(|supplied| supplied.name() == required.name)
        });
    if !all_required_present {
        return Err(precondition_error(
            "required authorization or grant precondition is missing",
        ));
    }
    Ok(())
}

impl Default for OperationRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn runtime_error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::ComponentUnresolved,
        DiagnosticCategory::Runtime,
        DiagnosticStage::Runtime,
        message.into(),
    )
}

fn capability_error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::ResultInvalid,
        DiagnosticCategory::Capability,
        DiagnosticStage::Invoke,
        message.into(),
    )
}

fn precondition_error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::PreconditionMissing,
        DiagnosticCategory::Capability,
        DiagnosticStage::Invoke,
        message.into(),
    )
}

fn authorization_error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::InvocationDenied,
        DiagnosticCategory::Authorization,
        DiagnosticStage::Invoke,
        message.into(),
    )
}
