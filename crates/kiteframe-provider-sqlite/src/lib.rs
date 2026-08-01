#![forbid(unsafe_code)]

use std::{
    path::Path,
    str::FromStr,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

#[cfg(test)]
use std::sync::Barrier;

use async_trait::async_trait;
use kiteframe_contract::{
    ActorRef, AdmissionId, CapabilityDescriptor, CapabilityIdentity, CapabilityName,
    CapabilityReleaseVersion, CatalogIdentity, Diagnostic, DiagnosticCategory, DiagnosticCode,
    DiagnosticStage, IdempotencyKey, InvocationId, NormalizedResourceSelector,
    ProtectedEvidenceRequestRef, Sha256Digest, StatusRequest, Timestamp,
};
use kiteframe_provider::{
    AbandonmentAuthorization, IdempotencyScopeValue, InvocationAuditLink, InvocationAuditLinkKind,
    InvocationReservation, InvocationReservationInput, InvocationState, InvocationStatus,
    InvocationStatusContext, InvocationStore, InvocationStoreClock, InvocationTransition,
    ReservationKind, StoredInvocation, SystemInvocationStoreClock, TransitionAuditRecord,
};
use sqlx::{
    Connection, Row, Sqlite, SqlitePool, Transaction,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow},
};

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub struct SqliteInvocationStore {
    pool: SqlitePool,
    clock: Arc<dyn InvocationStoreClock>,
    last_traceparent: Mutex<Option<String>>,
    #[cfg(test)]
    status_read_interlock: Mutex<Option<(Arc<Barrier>, Arc<Barrier>)>>,
}

impl SqliteInvocationStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, Diagnostic> {
        Self::open_with_clock(path, Arc::new(SystemInvocationStoreClock)).await
    }

    pub async fn open_with_clock(
        path: impl AsRef<Path>,
        clock: Arc<dyn InvocationStoreClock>,
    ) -> Result<Self, Diagnostic> {
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
        recover_incomplete_effects(&pool, clock.now()).await?;
        Ok(Self {
            pool,
            clock,
            last_traceparent: Mutex::new(None),
            #[cfg(test)]
            status_read_interlock: Mutex::new(None),
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

    #[cfg(test)]
    fn install_status_read_interlock(
        &self,
        invocation_read: Arc<Barrier>,
        writer_commit_attempted: Arc<Barrier>,
    ) {
        *self
            .status_read_interlock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some((invocation_read, writer_commit_attempted));
    }

    #[cfg(test)]
    fn take_status_read_interlock(&self) -> Option<(Arc<Barrier>, Arc<Barrier>)> {
        self.status_read_interlock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }
}

async fn recover_incomplete_effects(
    pool: &SqlitePool,
    recovered_at: Timestamp,
) -> Result<(), Diagnostic> {
    let recovered_at = as_i64(recovered_at)?;
    let mut connection = pool.acquire().await.map_err(|_| storage_error())?;
    let mut transaction = connection
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(|_| storage_error())?;
    sqlx::query(
        "UPDATE invocations
         SET state_kind = 'outcome_unknown',
             state_json = ?,
             updated_at = MAX(updated_at, ?)
         WHERE state_kind IN ('reserved', 'pending')",
    )
    .bind(encode_state(&InvocationState::OutcomeUnknown)?)
    .bind(recovered_at)
    .execute(&mut *transaction)
    .await
    .map_err(|_| storage_error())?;
    transaction.commit().await.map_err(|_| storage_error())
}

#[async_trait]
impl InvocationStore for SqliteInvocationStore {
    async fn reserve_or_get(
        &self,
        invocation: InvocationReservationInput,
        descriptor: &CapabilityDescriptor,
        retention_until: Timestamp,
    ) -> Result<InvocationReservation, Diagnostic> {
        let now = self.clock.now();
        let invocation =
            StoredInvocation::try_reserve(invocation, descriptor, now, retention_until)?;
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
        .bind(as_i64(now)?)
        .execute(&mut *transaction)
        .await
        .map_err(|_| storage_error())?;

        if let Some(row) = select_exact(&mut transaction, &invocation).await? {
            let mut existing = decode_row(&row)?;
            existing.audit_links =
                select_audit_links(&mut transaction, &existing.invocation_id).await?;
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
        transition: InvocationTransition,
    ) -> Result<(), Diagnostic> {
        let now = self.clock.now();
        let expected_json = encode_state(transition.expected())?;
        let next_json = encode_state(transition.next())?;
        let mut connection = self.pool.acquire().await.map_err(|_| storage_error())?;
        let mut transaction = connection
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|_| storage_error())?;
        let result = sqlx::query(
            "UPDATE invocations
             SET state_kind = ?,
                 state_json = ?,
                 updated_at = ?
             WHERE invocation_id = ?
               AND state_kind = ?
               AND state_json = ?
               AND updated_at <= ?",
        )
        .bind(transition.next().wire_name())
        .bind(next_json)
        .bind(as_i64(now)?)
        .bind(invocation_id.as_str())
        .bind(transition.expected().wire_name())
        .bind(expected_json)
        .bind(as_i64(now)?)
        .execute(&mut *transaction)
        .await
        .map_err(|_| storage_error())?;
        if result.rows_affected() != 1 {
            return Err(invalid("invocation state compare-and-swap failed"));
        }
        if let Some((kind, record_id)) = audit_record_parts(transition.audit_record()) {
            sqlx::query(
                "INSERT INTO invocation_audit_links (
                    invocation_id, sequence, kind, record_id, attached_at
                 )
                 SELECT ?, COALESCE(MAX(sequence), 0) + 1, ?, ?, ?
                 FROM invocation_audit_links
                 WHERE invocation_id = ?",
            )
            .bind(invocation_id.as_str())
            .bind(kind)
            .bind(record_id)
            .bind(as_i64(now)?)
            .bind(invocation_id.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(|_| storage_error())?;
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
        let mut connection = self.pool.acquire().await.map_err(|_| storage_error())?;
        let mut transaction = connection.begin().await.map_err(|_| storage_error())?;
        let row = sqlx::query("SELECT * FROM invocations WHERE invocation_id = ?")
            .bind(request.invocation_id().as_str())
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|_| storage_error())?
            .ok_or_else(status_unavailable)?;
        let mut stored = decode_row(&row)?;
        if &stored.status_context != context {
            return Err(status_unavailable());
        }
        #[cfg(test)]
        if let Some((invocation_read, writer_commit_attempted)) = self.take_status_read_interlock()
        {
            invocation_read.wait();
            writer_commit_attempted.wait();
        }
        stored.audit_links = select_audit_links(&mut transaction, &stored.invocation_id).await?;
        transaction.commit().await.map_err(|_| storage_error())?;
        Ok(status_from_existing(&stored))
    }

    async fn abandon(
        &self,
        invocation_id: &InvocationId,
        context: &InvocationStatusContext,
        authorization: AbandonmentAuthorization,
    ) -> Result<(), Diagnostic> {
        let now = self.clock.now();
        let result = sqlx::query(
            "UPDATE invocations
             SET state_kind = 'abandoned',
                 state_json = ?,
                 abandonment_authorization_record_id = ?,
                 abandoned_by = ?,
                 updated_at = ?
             WHERE invocation_id = ?
               AND state_kind = 'outcome_unknown'
               AND updated_at <= ?
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
        .bind(as_i64(now)?)
        .bind(invocation_id.as_str())
        .bind(as_i64(now)?)
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

async fn select_audit_links(
    transaction: &mut Transaction<'_, Sqlite>,
    invocation_id: &InvocationId,
) -> Result<Vec<InvocationAuditLink>, Diagnostic> {
    let rows = sqlx::query(
        "SELECT kind, record_id, attached_at
         FROM invocation_audit_links
         WHERE invocation_id = ?
         ORDER BY sequence",
    )
    .bind(invocation_id.as_str())
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| storage_error())?;
    rows.iter().map(decode_audit_link).collect()
}

fn decode_audit_link(row: &SqliteRow) -> Result<InvocationAuditLink, Diagnostic> {
    let kind = match text(row, "kind")?.as_str() {
        "authorization" => InvocationAuditLinkKind::Authorization,
        "outcome" => InvocationAuditLinkKind::Outcome,
        _ => return Err(storage_error()),
    };
    InvocationAuditLink::try_new(
        kind,
        text(row, "record_id")?,
        timestamp(row, "attached_at")?,
    )
    .map_err(|_| storage_error())
}

fn audit_record_parts(record: &TransitionAuditRecord) -> Option<(&'static str, &str)> {
    match record {
        TransitionAuditRecord::None => None,
        TransitionAuditRecord::Authorization(record_id) => {
            Some(("authorization", record_id.as_str()))
        }
        TransitionAuditRecord::Outcome(record_id) => Some(("outcome", record_id.as_str())),
    }
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
            protected_evidence_refs_json, created_at, updated_at, retention_until
         ) VALUES (
            ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
            ?, ?, ?
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
        audit_links: Vec::new(),
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use kiteframe_contract::{CatalogIdentity, TraceContext};

    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn status_reads_state_and_audit_links_from_one_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("status-snapshot.sqlite3");
        let store = Arc::new(SqliteInvocationStore::open(&path).await.unwrap());
        let invocation_id = InvocationId::new("inv-snapshot").unwrap();
        let context = InvocationStatusContext::try_new(
            "tenant-1",
            "human-1",
            "workload-1",
            "run-1",
            "actor-1",
            "agent-1",
            "task-1",
            "session-1",
            "admission-1",
        )
        .unwrap();
        let stored = StoredInvocation {
            invocation_id: invocation_id.clone(),
            status_id: "status-snapshot".to_owned(),
            scope: IdempotencyScopeValue::try_new(
                ActorRef::new("actor-1").unwrap(),
                CapabilityIdentity::try_new(
                    CapabilityName::new("cases.update").unwrap(),
                    CapabilityReleaseVersion::new("1.0.0").unwrap(),
                )
                .unwrap(),
                NormalizedResourceSelector::new("case:42").unwrap(),
                "cases.update",
            )
            .unwrap(),
            idempotency_key: IdempotencyKey::new("key-snapshot").unwrap(),
            request_digest: Sha256Digest::from_bytes([1; 32]),
            admission_id: AdmissionId::new("admission-1").unwrap(),
            grant_digest: Sha256Digest::from_bytes([2; 32]),
            catalog_identity: CatalogIdentity {
                name: "provider.catalog".to_owned(),
                revision: "1.0.0".to_owned(),
            },
            catalog_digest: Sha256Digest::from_bytes([3; 32]),
            descriptor_digest: Sha256Digest::from_bytes([4; 32]),
            authority_revision_digest: Sha256Digest::from_bytes([5; 32]),
            status_context: context.clone(),
            proposal_digest: Sha256Digest::from_bytes([6; 32]),
            protected_evidence_refs: Vec::new(),
            state: InvocationState::Pending,
            audit_links: Vec::new(),
            created_at: Timestamp::new(100),
            updated_at: Timestamp::new(101),
            retention_until: Timestamp::new(3_700),
            abandonment: None,
        };
        let mut connection = store.pool.acquire().await.unwrap();
        let mut transaction = connection.begin_with("BEGIN IMMEDIATE").await.unwrap();
        insert(&mut transaction, &stored).await.unwrap();
        sqlx::query(
            "INSERT INTO invocation_audit_links
             (invocation_id, sequence, kind, record_id, attached_at)
             VALUES (?, 1, 'authorization', 'audit-authz-initial', 101)",
        )
        .bind(invocation_id.as_str())
        .execute(&mut *transaction)
        .await
        .unwrap();
        transaction.commit().await.unwrap();
        drop(connection);

        let invocation_read = Arc::new(Barrier::new(2));
        let writer_commit_attempted = Arc::new(Barrier::new(2));
        store.install_status_read_interlock(
            invocation_read.clone(),
            writer_commit_attempted.clone(),
        );

        let writer_path = path.clone();
        let writer_invocation_id = invocation_id.clone();
        let writer = tokio::spawn(async move {
            invocation_read.wait();
            let url = format!("sqlite://{}", writer_path.display());
            let mut connection = sqlx::SqliteConnection::connect(&url).await.unwrap();
            sqlx::query("PRAGMA busy_timeout = 0")
                .execute(&mut connection)
                .await
                .unwrap();
            let committed = write_unknown_outcome(&mut connection, &writer_invocation_id)
                .await
                .is_ok();
            drop(connection);
            writer_commit_attempted.wait();
            if !committed {
                let mut connection = sqlx::SqliteConnection::connect(&url).await.unwrap();
                sqlx::query("PRAGMA busy_timeout = 5000")
                    .execute(&mut connection)
                    .await
                    .unwrap();
                write_unknown_outcome(&mut connection, &writer_invocation_id)
                    .await
                    .unwrap();
            }
        });

        let status_store = store.clone();
        let status_invocation_id = invocation_id.clone();
        let status_context = context.clone();
        let status_task = tokio::spawn(async move {
            status_store
                .status(
                    &StatusRequest::new(status_invocation_id, trace_context()),
                    &status_context,
                )
                .await
                .unwrap()
        });

        let snapshot = status_task.await.unwrap();
        writer.await.unwrap();
        assert_eq!(snapshot.state(), &InvocationState::Pending);
        assert_eq!(snapshot.audit_links().len(), 1);
        assert_eq!(snapshot.audit_links()[0].record_id(), "audit-authz-initial");

        let current = store
            .status(
                &StatusRequest::new(invocation_id, trace_context()),
                &context,
            )
            .await
            .unwrap();
        assert_eq!(current.state(), &InvocationState::OutcomeUnknown);
        assert_eq!(current.audit_links().len(), 2);
        assert_eq!(
            current.audit_links()[1].record_id(),
            "audit-outcome-unknown"
        );
    }

    fn trace_context() -> TraceContext {
        TraceContext::try_new(
            "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01",
            None,
            Default::default(),
        )
        .unwrap()
    }

    async fn write_unknown_outcome(
        connection: &mut sqlx::SqliteConnection,
        invocation_id: &InvocationId,
    ) -> Result<(), sqlx::Error> {
        let mut transaction = connection.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "UPDATE invocations
             SET state_kind = 'outcome_unknown', state_json = ?, updated_at = 102
             WHERE invocation_id = ?",
        )
        .bind(encode_state(&InvocationState::OutcomeUnknown).unwrap())
        .bind(invocation_id.as_str())
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO invocation_audit_links
             (invocation_id, sequence, kind, record_id, attached_at)
             VALUES (?, 2, 'outcome', 'audit-outcome-unknown', 102)",
        )
        .bind(invocation_id.as_str())
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await
    }
}
