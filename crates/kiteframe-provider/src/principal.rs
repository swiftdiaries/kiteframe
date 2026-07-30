use async_trait::async_trait;
use kiteframe_contract::{
    ActorRef, AdmissionId, AgentRef, Diagnostic, DiagnosticCategory, DiagnosticCode,
    DiagnosticStage, SessionRef, TaskRef, Timestamp,
};

macro_rules! opaque_ref {
    ($name:ident, $message:literal) => {
        #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, String> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err($message.to_owned());
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

opaque_ref!(TenantRef, "verified tenant reference is required");
opaque_ref!(
    HumanPrincipalRef,
    "verified human principal reference is required"
);
opaque_ref!(
    WorkloadPrincipalRef,
    "verified workload principal reference is required"
);
opaque_ref!(RunRef, "verified workload run reference is required");

/// A human identity independently authenticated by a deployment-owned verifier.
///
/// This value intentionally accepts only opaque references and expiry metadata. Raw
/// credentials and claims cannot be retained in it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedHumanPrincipal {
    tenant_ref: TenantRef,
    human_ref: HumanPrincipalRef,
    mapped_actor: ActorRef,
    expires_at: Timestamp,
}

impl VerifiedHumanPrincipal {
    pub fn try_new(
        tenant_ref: impl Into<String>,
        human_ref: impl Into<String>,
        mapped_actor: ActorRef,
        expires_at: Timestamp,
    ) -> Result<Self, String> {
        Ok(Self {
            tenant_ref: TenantRef::new(tenant_ref)?,
            human_ref: HumanPrincipalRef::new(human_ref)?,
            mapped_actor,
            expires_at,
        })
    }

    pub fn tenant_ref(&self) -> &TenantRef {
        &self.tenant_ref
    }

    pub fn human_ref(&self) -> &HumanPrincipalRef {
        &self.human_ref
    }

    pub fn mapped_actor(&self) -> &ActorRef {
        &self.mapped_actor
    }

    pub fn expires_at(&self) -> Timestamp {
        self.expires_at
    }
}

/// A calling workload independently authenticated by a deployment-owned verifier.
///
/// The workload is provenance and a constrained caller identity. It never replaces
/// the human business actor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedWorkloadPrincipal {
    tenant_ref: TenantRef,
    workload_ref: WorkloadPrincipalRef,
    run_ref: RunRef,
    mapped_agent: AgentRef,
    task_ref: TaskRef,
    session_ref: SessionRef,
    admission_ref: AdmissionId,
    expires_at: Timestamp,
}

impl VerifiedWorkloadPrincipal {
    #[allow(clippy::too_many_arguments)]
    pub fn try_new(
        tenant_ref: impl Into<String>,
        workload_ref: impl Into<String>,
        run_ref: impl Into<String>,
        mapped_agent: AgentRef,
        task_ref: TaskRef,
        session_ref: SessionRef,
        admission_ref: AdmissionId,
        expires_at: Timestamp,
    ) -> Result<Self, String> {
        Ok(Self {
            tenant_ref: TenantRef::new(tenant_ref)?,
            workload_ref: WorkloadPrincipalRef::new(workload_ref)?,
            run_ref: RunRef::new(run_ref)?,
            mapped_agent,
            task_ref,
            session_ref,
            admission_ref,
            expires_at,
        })
    }

    pub fn tenant_ref(&self) -> &TenantRef {
        &self.tenant_ref
    }

    pub fn workload_ref(&self) -> &WorkloadPrincipalRef {
        &self.workload_ref
    }

    pub fn run_ref(&self) -> &RunRef {
        &self.run_ref
    }

    pub fn mapped_agent(&self) -> &AgentRef {
        &self.mapped_agent
    }

    pub fn task_ref(&self) -> &TaskRef {
        &self.task_ref
    }

    pub fn session_ref(&self) -> &SessionRef {
        &self.session_ref
    }

    pub fn admission_ref(&self) -> &AdmissionId {
        &self.admission_ref
    }

    pub fn expires_at(&self) -> Timestamp {
        self.expires_at
    }
}

/// The pair returned by a server-side verifier after independent verification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedProviderPrincipals {
    human: VerifiedHumanPrincipal,
    workload: VerifiedWorkloadPrincipal,
}

impl VerifiedProviderPrincipals {
    pub fn new(human: VerifiedHumanPrincipal, workload: VerifiedWorkloadPrincipal) -> Self {
        Self { human, workload }
    }

    pub fn human(&self) -> &VerifiedHumanPrincipal {
        &self.human
    }

    pub fn workload(&self) -> &VerifiedWorkloadPrincipal {
        &self.workload
    }

    pub fn into_parts(self) -> (VerifiedHumanPrincipal, VerifiedWorkloadPrincipal) {
        (self.human, self.workload)
    }
}

/// Deployment extension seam that authenticates both principals outside package code.
///
/// Implementations read credentials from their server request context. No credential
/// material is passed into or returned from this portable provider interface.
#[async_trait]
pub trait ProviderPrincipalVerifier: Send + Sync {
    async fn verify(&self) -> Result<VerifiedProviderPrincipals, Diagnostic>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortableInvocationRefs {
    actor_ref: ActorRef,
    agent_ref: AgentRef,
    task_ref: TaskRef,
    session_ref: SessionRef,
    admission_ref: AdmissionId,
    correlated_at: Timestamp,
}

impl PortableInvocationRefs {
    pub fn new(
        actor_ref: ActorRef,
        agent_ref: AgentRef,
        task_ref: TaskRef,
        session_ref: SessionRef,
        admission_ref: AdmissionId,
        correlated_at: Timestamp,
    ) -> Self {
        Self {
            actor_ref,
            agent_ref,
            task_ref,
            session_ref,
            admission_ref,
            correlated_at,
        }
    }
}

/// Fully correlated dual-principal context. This is the only principal context
/// accepted by authorization and operation interfaces.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthenticatedInvocationContext {
    tenant_ref: TenantRef,
    human_ref: HumanPrincipalRef,
    workload_ref: WorkloadPrincipalRef,
    run_ref: RunRef,
    actor_ref: ActorRef,
    agent_ref: AgentRef,
    task_ref: TaskRef,
    session_ref: SessionRef,
    admission_ref: AdmissionId,
    expires_at: Timestamp,
}

impl AuthenticatedInvocationContext {
    pub fn tenant_ref(&self) -> &TenantRef {
        &self.tenant_ref
    }

    pub fn human_ref(&self) -> &HumanPrincipalRef {
        &self.human_ref
    }

    pub fn workload_ref(&self) -> &WorkloadPrincipalRef {
        &self.workload_ref
    }

    pub fn run_ref(&self) -> &RunRef {
        &self.run_ref
    }

    pub fn actor_ref(&self) -> &ActorRef {
        &self.actor_ref
    }

    pub fn agent_ref(&self) -> &AgentRef {
        &self.agent_ref
    }

    pub fn task_ref(&self) -> &TaskRef {
        &self.task_ref
    }

    pub fn session_ref(&self) -> &SessionRef {
        &self.session_ref
    }

    pub fn admission_ref(&self) -> &AdmissionId {
        &self.admission_ref
    }

    pub fn expires_at(&self) -> Timestamp {
        self.expires_at
    }
}

pub fn correlate_principals(
    human: VerifiedHumanPrincipal,
    workload: VerifiedWorkloadPrincipal,
    refs: PortableInvocationRefs,
) -> Result<AuthenticatedInvocationContext, Diagnostic> {
    if human.tenant_ref != workload.tenant_ref
        || human.mapped_actor != refs.actor_ref
        || workload.mapped_agent != refs.agent_ref
        || workload.task_ref != refs.task_ref
        || workload.session_ref != refs.session_ref
        || workload.admission_ref != refs.admission_ref
        || human.expires_at <= refs.correlated_at
        || workload.expires_at <= refs.correlated_at
    {
        return Err(correlation_error());
    }

    Ok(AuthenticatedInvocationContext {
        tenant_ref: human.tenant_ref,
        human_ref: human.human_ref,
        workload_ref: workload.workload_ref,
        run_ref: workload.run_ref,
        actor_ref: refs.actor_ref,
        agent_ref: refs.agent_ref,
        task_ref: refs.task_ref,
        session_ref: refs.session_ref,
        admission_ref: refs.admission_ref,
        expires_at: std::cmp::min(human.expires_at, workload.expires_at),
    })
}

fn correlation_error() -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::InvocationDenied,
        DiagnosticCategory::Authorization,
        DiagnosticStage::Invoke,
        "authenticated principal correlation failed",
    )
}
