use serde_json::json;
use sqlx::Row;

use super::{PostgresStore, StoreError};
use crate::{PrincipalId, SessionId};

impl PostgresStore {
    pub async fn close_session_and_cancel_work(
        &self,
        session_id: SessionId,
        principal: &PrincipalId,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool().begin().await?;
        let session = sqlx::query("SELECT principal, state FROM sessions WHERE id = $1 FOR UPDATE")
            .bind(*session_id.as_uuid())
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StoreError::SessionNotFound)?;
        if session.get::<String, _>("principal") != principal.as_str() {
            return Err(StoreError::PrincipalMismatch);
        }
        if session.get::<String, _>("state") != "active" {
            return Err(StoreError::SessionClosed);
        }

        // The session row blocks new runs. Existing runs are the aggregate
        // mutexes, so lock them before the frames they own.
        let active_runs = sqlx::query(
            "SELECT id FROM runs WHERE session_id = $1 AND state NOT IN ('completed', 'failed', 'cancelled') ORDER BY id FOR UPDATE",
        )
        .bind(*session_id.as_uuid())
        .fetch_all(&mut *tx)
        .await?;
        sqlx::query(
            "SELECT f.id FROM frames f JOIN runs r ON r.id = f.run_id WHERE r.session_id = $1 AND r.state NOT IN ('completed', 'failed', 'cancelled') ORDER BY f.id FOR UPDATE OF f",
        )
        .bind(*session_id.as_uuid())
        .fetch_all(&mut *tx)
        .await?;

        for run in &active_runs {
            let run_id: uuid::Uuid = run.get("id");
            let sequence: i64 = sqlx::query(
                "SELECT COALESCE(MAX(sequence), 0) + 1 AS sequence FROM events WHERE run_id = $1",
            )
            .bind(run_id)
            .fetch_one(&mut *tx)
            .await?
            .get("sequence");
            sqlx::query(
                "INSERT INTO events (run_id, sequence, kind, metadata) VALUES ($1, $2, 'run.cancelled', $3)",
            )
            .bind(run_id)
            .bind(sequence)
            .bind(json!({"reason": "session closed"}))
            .execute(&mut *tx)
            .await?;
        }

        let cancellation_key = format!("session-close:{}", session_id.as_uuid());
        sqlx::query(
            "UPDATE frames SET state = 'cancelled', revision = revision + 1, updated_at = CURRENT_TIMESTAMP WHERE run_id IN (SELECT id FROM runs WHERE session_id = $1 AND state NOT IN ('completed', 'failed', 'cancelled')) AND state NOT IN ('completed', 'failed', 'cancelled')",
        )
        .bind(*session_id.as_uuid())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE runs SET state = 'cancelled', revision = revision + 1, cancellation_idempotency_key = $1, cancellation_reason = $2, updated_at = CURRENT_TIMESTAMP WHERE session_id = $3 AND state NOT IN ('completed', 'failed', 'cancelled')",
        )
        .bind(cancellation_key)
        .bind(json!("session closed"))
        .bind(*session_id.as_uuid())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE questions SET state = 'cancelled' WHERE run_id IN (SELECT id FROM runs WHERE session_id = $1) AND state = 'pending'",
        )
        .bind(*session_id.as_uuid())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE permission_requests SET state = 'cancelled' WHERE run_id IN (SELECT id FROM runs WHERE session_id = $1) AND state = 'pending'",
        )
        .bind(*session_id.as_uuid())
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE sessions SET state = 'closed', updated_at = CURRENT_TIMESTAMP WHERE id = $1",
        )
        .bind(*session_id.as_uuid())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
}
