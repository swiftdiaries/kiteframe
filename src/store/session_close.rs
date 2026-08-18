use serde_json::json;
use sqlx::Row;

use super::{AgentTaskEventKind, PostgresStore, StoreError, agent::append_agent_task_event};
use crate::{PrincipalId, SessionId, UserTaskId};

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

        // Global aggregate order: session, user tasks by id, goal states by
        // task id, then the legacy run/frame aggregates.
        let active_tasks = sqlx::query(
            "SELECT id, revision FROM user_tasks \
             WHERE session_id = $1 AND state NOT IN ('succeeded', 'failed', 'cancelled') \
             ORDER BY id FOR UPDATE",
        )
        .bind(*session_id.as_uuid())
        .fetch_all(&mut *tx)
        .await?;
        let task_ids = active_tasks
            .iter()
            .map(|row| row.get::<uuid::Uuid, _>("id"))
            .collect::<Vec<_>>();
        let goal_states = sqlx::query(
            "SELECT task_id, revision FROM goal_states \
             WHERE task_id = ANY($1) ORDER BY task_id FOR UPDATE",
        )
        .bind(&task_ids)
        .fetch_all(&mut *tx)
        .await?;
        if goal_states.len() != active_tasks.len() {
            return Err(StoreError::InvalidData("goal state is absent".to_owned()));
        }

        for task in &active_tasks {
            let task_uuid: uuid::Uuid = task.get("id");
            let task_id = UserTaskId::new(task_uuid);
            let old_revision = u64::try_from(task.get::<i64, _>("revision"))
                .map_err(|_| StoreError::InvalidData("user task revision is malformed".into()))?;
            let task_revision = old_revision
                .checked_add(1)
                .ok_or_else(|| StoreError::InvalidData("user task revision is malformed".into()))?;
            let goal_revision = goal_states
                .iter()
                .find(|row| row.get::<uuid::Uuid, _>("task_id") == task_uuid)
                .and_then(|row| u64::try_from(row.get::<i64, _>("revision")).ok())
                .ok_or_else(|| StoreError::InvalidData("goal revision is malformed".into()))?;

            sqlx::query(
                "UPDATE user_tasks SET state = 'cancelled', operational_status = 'finished', \
                 revision = $1, cancellation_reason = 'session closed', \
                 finished_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP WHERE id = $2",
            )
            .bind(
                i64::try_from(task_revision).map_err(|_| {
                    StoreError::InvalidData("user task revision is malformed".into())
                })?,
            )
            .bind(task_uuid)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE agent_runtime_snapshots SET task_revision = $1, \
                 updated_at = CURRENT_TIMESTAMP WHERE task_id = $2",
            )
            .bind(
                i64::try_from(task_revision).map_err(|_| {
                    StoreError::InvalidData("user task revision is malformed".into())
                })?,
            )
            .bind(task_uuid)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE agent_commands SET state = 'cancelled', revision = revision + 1, \
                 updated_at = CURRENT_TIMESTAMP \
                 WHERE task_id = $1 AND state IN ('pending', 'claimed')",
            )
            .bind(task_uuid)
            .execute(&mut *tx)
            .await?;
            append_agent_task_event(
                &mut tx,
                &task_id,
                AgentTaskEventKind::TaskCancelled,
                json!({"reason": "session closed"}),
                task_revision,
                goal_revision,
            )
            .await?;
        }

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
