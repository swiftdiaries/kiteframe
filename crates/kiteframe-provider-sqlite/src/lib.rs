#![forbid(unsafe_code)]

use std::{
    path::Path,
    str::FromStr,
    sync::{Mutex, MutexGuard},
    time::Duration,
};

use async_trait::async_trait;
use kiteframe_contract::{
    ActorRef, AdmissionId, CapabilityIdentity, CapabilityName, CapabilityReleaseVersion,
    CatalogIdentity, Diagnostic, DiagnosticCategory, DiagnosticCode, DiagnosticStage,
    IdempotencyKey, InvocationId, NormalizedResourceSelector, ProtectedEvidenceRequestRef,
    Sha256Digest, StatusRequest, Timestamp,
};
use kiteframe_provider::{
    AbandonmentAuthorization, IdempotencyScopeValue, InvocationReservation, InvocationState,
    InvocationStatus, InvocationStatusContext, InvocationStore, ReservationKind, StoredInvocation,
};
use sqlx::{
    Connection, Row, Sqlite, SqlitePool, Transaction,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow},
};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub struct SqliteInvocationStore {
    pool: SqlitePool,
    last_traceparent: Mutex<Option<String>>,
}

impl SqliteInvocationStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, Diagnostic> {
        let options = SqliteConnectOptions::from_str("sqlite://")
            .map_err(|_| storage_error())?
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .map_err(|_| storage_error())?;
        MIGRATOR.run(&pool).await.map_err(|_| storage_error())?;
        Ok(Self {
            pool,
            last_traceparent: Mutex::new(None),
        })
    }

    pub fn last_traceparent(&self) -> Option<String> {
        self.lock_traceparent().clone()
    }

    fn lock_traceparent(&self) -> MutexGuard<'_, Option<String>> {
        self.last_traceparent
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[async_trait]
impl InvocationStore for SqliteInvocationStore {
    async fn reserve_or_get(
        &self,
        invocation: StoredInvocation,
    ) -> Result<InvocationReservation, Diagnostic> {
        invocation.validate_for_storage()?;
        validate_sqlite_timestamps(&invocation)?;
        let mut connection = self.pool.acquire().await.map_err(|_| storage_error())?;
        let mut transaction = connection
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|_| storage_error())?;

        sqlx::query(
            "DELETE FROM invocations
             WHERE retention_until <= ? AND state_kind != 'outcome_unknown'",
        )
        .bind(as_i64(invocation.created_at)?)
        .execute(&mut *transaction)
        .await
        .map_err(|_| storage_error())?;

        if let Some(row) = select_exact(&mut transaction, &invocation).await? {
            let existing = decode_row(&row)?;
            if existing.status_context != invocation.status_context {
                return Err(status_unavailable());
            }
            if existing.request_digest != invocation.request_digest {
                return Err(invalid(
                    "idempotency key is already bound to a different request",
                ));
            }
            transaction.commit().await.map_err(|_| storage_error())?;
            return Ok(existing_reservation(&existing));
        }

        if has_unknown_in_scope(&mut transaction, &invocation).await? {
            return Err(Diagnostic::outcome_unknown(
                "a prior invocation in this idempotency scope has an unknown outcome; query status first",
            ));
        }

        if invocation_id_exists(&mut transaction, &invocation.invocation_id).await? {
            return Err(invalid("invocation ID is already reserved"));
        }

        insert(&mut transaction, &invocation).await?;
        transaction.commit().await.map_err(|_| storage_error())?;
        Ok(new_reservation(&invocation))
    }

    async fn transition(
        &self,
        invocation_id: &InvocationId,
        expected: InvocationState,
        next: InvocationState,
    ) -> Result<(), Diagnostic> {
        if !expected.permits_transition_to(&next) {
            return Err(invalid("invocation state transition is not permitted"));
        }
        let expected_json = encode_state(&expected)?;
        let next_json = encode_state(&next)?;
        let mut connection = self.pool.acquire().await.map_err(|_| storage_error())?;
        let mut transaction = connection
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|_| storage_error())?;
        let result = sqlx::query(
            "UPDATE invocations
             SET state_kind = ?, state_json = ?, updated_at = updated_at + 1
             WHERE invocation_id = ? AND state_kind = ? AND state_json = ?",
        )
        .bind(next.wire_name())
        .bind(next_json)
        .bind(invocation_id.as_str())
        .bind(expected.wire_name())
        .bind(expected_json)
        .execute(&mut *transaction)
        .await
        .map_err(|_| storage_error())?;
        if result.rows_affected() != 1 {
            return Err(invalid("invocation state compare-and-swap failed"));
        }
        transaction.commit().await.map_err(|_| storage_error())?;
        Ok(())
    }

    async fn status(
        &self,
        request: &StatusRequest,
        context: &InvocationStatusContext,
    ) -> Result<InvocationStatus, Diagnostic> {
        *self.lock_traceparent() = Some(request.trace_context().traceparent().to_owned());
        let row = sqlx::query("SELECT * FROM invocations WHERE invocation_id = ?")
            .bind(request.invocation_id().as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| storage_error())?
            .ok_or_else(status_unavailable)?;
        let stored = decode_row(&row)?;
        if &stored.status_context != context {
            return Err(status_unavailable());
        }
        Ok(status_from_existing(&stored))
    }

    async fn abandon(
        &self,
        invocation_id: &InvocationId,
        context: &InvocationStatusContext,
        authorization: AbandonmentAuthorization,
    ) -> Result<(), Diagnostic> {
        let result = sqlx::query(
            "UPDATE invocations
             SET state_kind = 'abandoned',
                 state_json = ?,
                 abandonment_authorization_record_id = ?,
                 abandoned_by = ?,
                 updated_at = updated_at + 1
             WHERE invocation_id = ?
               AND state_kind = 'outcome_unknown'
               AND tenant_ref = ?
               AND human_ref = ?
               AND workload_ref = ?
               AND run_ref = ?
               AND actor_ref = ?
               AND agent_ref = ?
               AND task_ref = ?
               AND session_ref = ?
               AND admission_id = ?",
        )
        .bind(encode_state(&InvocationState::Abandoned)?)
        .bind(authorization.authorization_record_id())
        .bind(authorization.authorized_by())
        .bind(invocation_id.as_str())
        .bind(context.tenant_ref())
        .bind(context.human_ref())
        .bind(context.workload_ref())
        .bind(context.run_ref())
        .bind(context.actor_ref())
        .bind(context.agent_ref())
        .bind(context.task_ref())
        .bind(context.session_ref())
        .bind(context.admission_ref())
        .execute(&self.pool)
        .await
        .map_err(|_| storage_error())?;
        if result.rows_affected() != 1 {
            return Err(status_unavailable());
        }
        Ok(())
    }
}

async fn select_exact(
    transaction: &mut Transaction<'_, Sqlite>,
    invocation: &StoredInvocation,
) -> Result<Option<SqliteRow>, Diagnostic> {
    sqlx::query(
        "SELECT * FROM invocations
         WHERE actor_ref = ?
           AND capability_name = ?
           AND capability_version = ?
           AND normalized_resource = ?
           AND semantic_operation = ?
           AND idempotency_key = ?",
    )
    .bind(invocation.scope.actor().as_str())
    .bind(invocation.scope.capability().name().as_str())
    .bind(invocation.scope.capability().version().as_str())
    .bind(invocation.scope.normalized_resource().as_str())
    .bind(invocation.scope.semantic_operation())
    .bind(invocation.idempotency_key.as_str())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| storage_error())
}

async fn has_unknown_in_scope(
    transaction: &mut Transaction<'_, Sqlite>,
    invocation: &StoredInvocation,
) -> Result<bool, Diagnostic> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM invocations
         WHERE actor_ref = ?
           AND capability_name = ?
           AND capability_version = ?
           AND normalized_resource = ?
           AND semantic_operation = ?
           AND state_kind = 'outcome_unknown'",
    )
    .bind(invocation.scope.actor().as_str())
    .bind(invocation.scope.capability().name().as_str())
    .bind(invocation.scope.capability().version().as_str())
    .bind(invocation.scope.normalized_resource().as_str())
    .bind(invocation.scope.semantic_operation())
    .fetch_one(&mut **transaction)
    .await
    .map_err(|_| storage_error())?;
    Ok(count != 0)
}

async fn invocation_id_exists(
    transaction: &mut Transaction<'_, Sqlite>,
    invocation_id: &InvocationId,
) -> Result<bool, Diagnostic> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM invocations WHERE invocation_id = ?")
        .bind(invocation_id.as_str())
        .fetch_one(&mut **transaction)
        .await
        .map_err(|_| storage_error())?;
    Ok(count != 0)
}

async fn insert(
    transaction: &mut Transaction<'_, Sqlite>,
    invocation: &StoredInvocation,
) -> Result<(), Diagnostic> {
    let evidence = invocation
        .protected_evidence_refs
        .iter()
        .map(|reference| reference.as_str())
        .collect::<Vec<_>>();
    let evidence_json = serde_json::to_string(&evidence).map_err(|_| storage_error())?;
    sqlx::query(
        "INSERT INTO invocations (
            invocation_id, status_id, actor_ref, capability_name, capability_version,
            normalized_resource, semantic_operation, idempotency_key, request_digest,
            state_kind, state_json, admission_id, grant_digest, catalog_name, catalog_revision,
            catalog_digest, descriptor_digest, authority_revision_digest, tenant_ref, human_ref,
            workload_ref, run_ref, agent_ref, task_ref, session_ref, proposal_digest,
            protected_evidence_refs_json, audit_authorization_record_id, audit_outcome_record_id,
            created_at, updated_at, retention_until
         ) VALUES (
            ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
            ?, ?, ?, ?, ?
         )",
    )
    .bind(invocation.invocation_id.as_str())
    .bind(&invocation.status_id)
    .bind(invocation.scope.actor().as_str())
    .bind(invocation.scope.capability().name().as_str())
    .bind(invocation.scope.capability().version().as_str())
    .bind(invocation.scope.normalized_resource().as_str())
    .bind(invocation.scope.semantic_operation())
    .bind(invocation.idempotency_key.as_str())
    .bind(invocation.request_digest.to_string())
    .bind(invocation.state.wire_name())
    .bind(encode_state(&invocation.state)?)
    .bind(invocation.admission_id.as_str())
    .bind(invocation.grant_digest.to_string())
    .bind(&invocation.catalog_identity.name)
    .bind(&invocation.catalog_identity.revision)
    .bind(invocation.catalog_digest.to_string())
    .bind(invocation.descriptor_digest.to_string())
    .bind(invocation.authority_revision_digest.to_string())
    .bind(invocation.status_context.tenant_ref())
    .bind(invocation.status_context.human_ref())
    .bind(invocation.status_context.workload_ref())
    .bind(invocation.status_context.run_ref())
    .bind(invocation.status_context.agent_ref())
    .bind(invocation.status_context.task_ref())
    .bind(invocation.status_context.session_ref())
    .bind(invocation.proposal_digest.to_string())
    .bind(evidence_json)
    .bind(&invocation.audit_authorization_record_id)
    .bind(&invocation.audit_outcome_record_id)
    .bind(as_i64(invocation.created_at)?)
    .bind(as_i64(invocation.updated_at)?)
    .bind(as_i64(invocation.retention_until)?)
    .execute(&mut **transaction)
    .await
    .map_err(|_| storage_error())?;
    Ok(())
}

fn decode_row(row: &SqliteRow) -> Result<StoredInvocation, Diagnostic> {
    let actor_ref = text(row, "actor_ref")?;
    let admission_id = text(row, "admission_id")?;
    let abandonment_record = optional_text(row, "abandonment_authorization_record_id")?;
    let abandoned_by = optional_text(row, "abandoned_by")?;
    let abandonment = match (abandonment_record, abandoned_by) {
        (Some(record), Some(operator)) => {
            Some(AbandonmentAuthorization::try_new(record, operator).map_err(|_| storage_error())?)
        }
        (None, None) => None,
        _ => return Err(storage_error()),
    };
    let protected_references: Vec<String> =
        serde_json::from_str(&text(row, "protected_evidence_refs_json")?)
            .map_err(|_| storage_error())?;
    let state: InvocationState =
        serde_json::from_str(&text(row, "state_json")?).map_err(|_| storage_error())?;
    if state.wire_name() != text(row, "state_kind")?
        || (state == InvocationState::Abandoned) != abandonment.is_some()
    {
        return Err(storage_error());
    }
    Ok(StoredInvocation {
        invocation_id: InvocationId::new(text(row, "invocation_id")?)
            .map_err(|_| storage_error())?,
        status_id: text(row, "status_id")?,
        scope: IdempotencyScopeValue::try_new(
            ActorRef::new(actor_ref.clone()).map_err(|_| storage_error())?,
            CapabilityIdentity::try_new(
                CapabilityName::new(text(row, "capability_name")?).map_err(|_| storage_error())?,
                CapabilityReleaseVersion::new(text(row, "capability_version")?)
                    .map_err(|_| storage_error())?,
            )
            .map_err(|_| storage_error())?,
            NormalizedResourceSelector::new(text(row, "normalized_resource")?)
                .map_err(|_| storage_error())?,
            text(row, "semantic_operation")?,
        )
        .map_err(|_| storage_error())?,
        idempotency_key: IdempotencyKey::new(text(row, "idempotency_key")?)
            .map_err(|_| storage_error())?,
        request_digest: digest(row, "request_digest")?,
        admission_id: AdmissionId::new(admission_id.clone()).map_err(|_| storage_error())?,
        grant_digest: digest(row, "grant_digest")?,
        catalog_identity: CatalogIdentity {
            name: text(row, "catalog_name")?,
            revision: text(row, "catalog_revision")?,
        },
        catalog_digest: digest(row, "catalog_digest")?,
        descriptor_digest: digest(row, "descriptor_digest")?,
        authority_revision_digest: digest(row, "authority_revision_digest")?,
        status_context: InvocationStatusContext::try_new(
            text(row, "tenant_ref")?,
            text(row, "human_ref")?,
            text(row, "workload_ref")?,
            text(row, "run_ref")?,
            actor_ref,
            text(row, "agent_ref")?,
            text(row, "task_ref")?,
            text(row, "session_ref")?,
            admission_id,
        )
        .map_err(|_| storage_error())?,
        proposal_digest: digest(row, "proposal_digest")?,
        protected_evidence_refs: protected_references
            .into_iter()
            .map(|reference| {
                ProtectedEvidenceRequestRef::new(reference).map_err(|_| storage_error())
            })
            .collect::<Result<_, _>>()?,
        state,
        audit_authorization_record_id: optional_text(row, "audit_authorization_record_id")?,
        audit_outcome_record_id: optional_text(row, "audit_outcome_record_id")?,
        created_at: timestamp(row, "created_at")?,
        updated_at: timestamp(row, "updated_at")?,
        retention_until: timestamp(row, "retention_until")?,
        abandonment,
    })
}

fn new_reservation(invocation: &StoredInvocation) -> InvocationReservation {
    InvocationReservation::from_stored(ReservationKind::Reserved, invocation)
}

fn existing_reservation(invocation: &StoredInvocation) -> InvocationReservation {
    InvocationReservation::from_stored(ReservationKind::Existing, invocation)
}

fn status_from_existing(invocation: &StoredInvocation) -> InvocationStatus {
    InvocationStatus::from_stored(invocation)
}

fn encode_state(state: &InvocationState) -> Result<String, Diagnostic> {
    serde_json::to_string(state).map_err(|_| storage_error())
}

fn digest(row: &SqliteRow, column: &str) -> Result<Sha256Digest, Diagnostic> {
    serde_json::from_value(serde_json::Value::String(text(row, column)?))
        .map_err(|_| storage_error())
}

fn text(row: &SqliteRow, column: &str) -> Result<String, Diagnostic> {
    row.try_get(column).map_err(|_| storage_error())
}

fn optional_text(row: &SqliteRow, column: &str) -> Result<Option<String>, Diagnostic> {
    row.try_get(column).map_err(|_| storage_error())
}

fn timestamp(row: &SqliteRow, column: &str) -> Result<Timestamp, Diagnostic> {
    let value: i64 = row.try_get(column).map_err(|_| storage_error())?;
    let value = u64::try_from(value).map_err(|_| storage_error())?;
    Ok(Timestamp::new(value))
}

fn validate_sqlite_timestamps(invocation: &StoredInvocation) -> Result<(), Diagnostic> {
    as_i64(invocation.created_at)?;
    as_i64(invocation.updated_at)?;
    as_i64(invocation.retention_until)?;
    Ok(())
}

fn as_i64(value: Timestamp) -> Result<i64, Diagnostic> {
    i64::try_from(value.unix_seconds()).map_err(|_| invalid("timestamp exceeds SQLite range"))
}

fn invalid(message: &'static str) -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::ResultInvalid,
        DiagnosticCategory::Capability,
        DiagnosticStage::Invoke,
        message,
    )
}

fn status_unavailable() -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::InvocationDenied,
        DiagnosticCategory::Authorization,
        DiagnosticStage::Invoke,
        "invocation status is unavailable",
    )
}

fn storage_error() -> Diagnostic {
    Diagnostic::error(
        DiagnosticCode::RuntimeConstruction,
        DiagnosticCategory::Runtime,
        DiagnosticStage::Runtime,
        "durable invocation storage is unavailable",
    )
}
