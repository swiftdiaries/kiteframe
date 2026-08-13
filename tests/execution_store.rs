use std::collections::{BTreeMap, BTreeSet};

use kiteframe::dsl::{Limits, compile};
use kiteframe::{
    CatalogTool, ClosedSchema, CreateSession, EffectKind, FrameCommand, IdempotencyKey, NewChild,
    NewPermissionRequest, PermissionRequestId, PolicyRevision, PostgresStore, PrincipalId,
    Question, RecoverySupport, StartRun, StoreError, StoreLimits, ToolCatalogSnapshot,
};
use serde_json::json;
use sqlx::Row;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

#[tokio::test]
async fn start_run_snapshots_definition_and_replays_only_identical_requests() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping execution store test: DATABASE_URL is not configured");
        return;
    };
    let store = PostgresStore::connect(&database_url)
        .await
        .expect("connect store");
    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("alice-{}", Uuid::now_v7())).unwrap(),
            working_directory: "/work".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .expect("create session");
    let catalog =
        ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), [tool("docs/search")])
            .expect("catalog");
    store
        .append_catalog_epoch(&catalog)
        .await
        .expect("append catalog");

    let workflow_name = format!("w{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {workflow_name}(query: string) -> string {{ start t(query: query) }} task t(query: string) -> string {{ prompt \"\"\"x\"\"\" tools [search] on ok() {{ done query }} }}"
    );
    let workflow = compile(&source, &catalog, &Limits::default()).expect("compile workflow");
    let definition = store
        .publish(session.id.clone(), &workflow, &source)
        .await
        .expect("publish");
    let key = IdempotencyKey::new(format!("start-{}", Uuid::now_v7())).unwrap();
    let request = StartRun {
        session_id: session.id.clone(),
        workflow_name: definition.name.clone(),
        version: Some(definition.version),
        input: json!({"query": "durable"}),
        idempotency_key: key.clone(),
    };

    let started = store.start_run(request.clone()).await.expect("start run");
    assert_eq!(started.definition, definition);
    assert_eq!(started.session_id, session.id);
    assert_eq!(started.state, kiteframe::RunState::Runnable);
    assert_eq!(started.revision, 0);
    assert_eq!(
        store
            .get_run(session.id.clone(), started.id.clone())
            .await
            .expect("get run"),
        started
    );
    let events = store
        .events_after(session.id.clone(), started.id.clone(), 0, 8)
        .await
        .expect("replay events");
    assert_eq!(
        events
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(events[0].kind, "run.started");
    let runnable = store
        .runnable_frames_for_run(started.id.clone(), 8)
        .await
        .expect("list this run's runnable frames");
    assert!(
        runnable
            .iter()
            .any(|frame| frame.id == started.root_frame_id)
    );
    assert_eq!(
        store.start_run(request.clone()).await.expect("replay"),
        started
    );
    let attempt = store
        .begin_model_attempt(started.root_frame_id.clone(), 0, json!({"prompt": "x"}))
        .await
        .expect("persist model intent");
    assert_eq!(attempt.attempt, 1);
    assert_eq!(attempt.revision, 1);
    let event_count_before_stale = store
        .events_after(session.id.clone(), started.id.clone(), 0, 8)
        .await
        .expect("events before stale completion")
        .len();
    assert!(matches!(
        store
            .commit_model_attempt(
                started.root_frame_id.clone(),
                attempt.attempt,
                attempt.revision - 1,
                vec![json!({"type": "stale"})],
            )
            .await,
        Err(StoreError::RevisionConflict)
    ));
    assert_eq!(
        store
            .events_after(session.id.clone(), started.id.clone(), 0, 8)
            .await
            .expect("stale completion must not append an event")
            .len(),
        event_count_before_stale
    );
    assert_eq!(
        store
            .model_attempt_count(started.root_frame_id.clone())
            .await
            .expect("count attempts"),
        1
    );
    store
        .commit_model_attempt(
            started.root_frame_id.clone(),
            attempt.attempt,
            attempt.revision,
            vec![json!({"type": "reasoning"}), json!({"type": "tool_call"})],
        )
        .await
        .expect("persist ordered model result");
    let tool_call = store
        .begin_tool_call(
            started.root_frame_id.clone(),
            2,
            "provider-call-1".to_owned(),
            "docs/search".to_owned(),
            catalog.tools()["docs/search"]
                .descriptor_digest()
                .to_owned(),
            json!({"query": "durable"}),
            "tool-attempt-1".to_owned(),
        )
        .await
        .expect("persist tool intent");
    assert_eq!(tool_call.revision, 3);
    assert_eq!(
        store
            .begin_tool_call(
                started.root_frame_id.clone(),
                2,
                "provider-call-1".to_owned(),
                "docs/search".to_owned(),
                catalog.tools()["docs/search"]
                    .descriptor_digest()
                    .to_owned(),
                json!({"query": "durable"}),
                "tool-attempt-1".to_owned(),
            )
            .await
            .expect("replay tool intent"),
        tool_call
    );
    let event_count_before_conflict = store
        .events_after(session.id.clone(), started.id.clone(), 0, 16)
        .await
        .expect("events before conflicting tool replay")
        .len();
    assert!(matches!(
        store
            .begin_tool_call(
                started.root_frame_id.clone(),
                2,
                "provider-call-1".to_owned(),
                "docs/search".to_owned(),
                catalog.tools()["docs/search"]
                    .descriptor_digest()
                    .to_owned(),
                json!({"query": "changed"}),
                "tool-attempt-1".to_owned(),
            )
            .await,
        Err(StoreError::IdempotencyConflict)
    ));
    assert_eq!(
        store
            .events_after(session.id.clone(), started.id.clone(), 0, 16)
            .await
            .expect("conflicting tool replay must not append an event")
            .len(),
        event_count_before_conflict
    );
    let permission = NewPermissionRequest {
        id: PermissionRequestId::new(Uuid::now_v7()),
        provider_call_id: tool_call.provider_call_id.clone(),
        qualified_tool: "docs/search".to_owned(),
        descriptor_digest: catalog.tools()["docs/search"]
            .descriptor_digest()
            .to_owned(),
        input_digest: kiteframe::canonical_sha256(&json!({"query": "durable"})).unwrap(),
        policy_revision: PolicyRevision::new(0),
    };
    store
        .create_permission_request(
            started.root_frame_id.clone(),
            tool_call.revision,
            permission.clone(),
        )
        .await
        .expect("persist permission request");
    store
        .resolve_permission(
            started.root_frame_id.clone(),
            permission.id,
            4,
            json!({"decision": "allow"}),
            "approve-1".to_owned(),
        )
        .await
        .expect("resolve permission request");
    store
        .finish_tool_call(
            started.root_frame_id.clone(),
            tool_call.id,
            5,
            Ok(json!({"results": []})),
        )
        .await
        .expect("persist tool result");
    let question = Question {
        id: Uuid::now_v7(),
        binding: "confirmation".to_owned(),
        text: "Continue?".to_owned(),
        schema: ClosedSchema::boolean(),
    };
    store
        .create_question(started.root_frame_id.clone(), 6, question.clone())
        .await
        .expect("persist question");
    store
        .answer(
            started.root_frame_id.clone(),
            question.id,
            7,
            json!(true),
            "answer-1".to_owned(),
        )
        .await
        .expect("persist answer");
    let resumed = store
        .begin_model_attempt(
            started.root_frame_id.clone(),
            8,
            json!({"prompt": "resume"}),
        )
        .await
        .expect("resume frame");
    store
        .apply_frame_command(
            started.root_frame_id.clone(),
            resumed.revision,
            FrameCommand::StartCall(NewChild {
                target: "t".to_owned(),
                binding: "child_result".to_owned(),
                arguments: BTreeMap::from([("query".to_owned(), json!("child"))]),
                branch_index: 0,
            }),
        )
        .await
        .expect("start child frame");
    let child_id = sqlx::query("SELECT id FROM frames WHERE parent_frame_id = $1")
        .bind(*started.root_frame_id.as_uuid())
        .fetch_one(store.pool())
        .await
        .expect("find child")
        .get("id");
    let child = kiteframe::FrameId::new(child_id);
    let child_attempt = store
        .begin_model_attempt(child.clone(), 0, json!({"prompt": "child"}))
        .await
        .expect("start child");
    store
        .apply_frame_command(
            child,
            child_attempt.revision,
            FrameCommand::Complete(json!("child done")),
        )
        .await
        .expect("complete child");
    let final_attempt = store
        .begin_model_attempt(
            started.root_frame_id.clone(),
            11,
            json!({"prompt": "finish"}),
        )
        .await
        .expect("resume parent");
    store
        .apply_frame_command(
            started.root_frame_id.clone(),
            final_attempt.revision,
            FrameCommand::Complete(json!("done")),
        )
        .await
        .expect("complete root frame");
    let cancellable = store
        .start_run(StartRun {
            session_id: session.id.clone(),
            workflow_name: definition.name.clone(),
            version: Some(definition.version),
            input: json!({"query": "cancel"}),
            idempotency_key: IdempotencyKey::new(format!("cancel-{}", Uuid::now_v7())).unwrap(),
        })
        .await
        .expect("start cancellable run");
    let late_attempt = store
        .begin_model_attempt(
            cancellable.root_frame_id.clone(),
            0,
            json!({"prompt": "cancelled while waiting"}),
        )
        .await
        .expect("persist model intent before cancellation");
    store
        .cancel_run(
            cancellable.id.clone(),
            0,
            Some("operator request".to_owned()),
            "cancel-1".to_owned(),
        )
        .await
        .expect("cancel run");
    store
        .cancel_run(
            cancellable.id.clone(),
            0,
            Some("operator request".to_owned()),
            "cancel-1".to_owned(),
        )
        .await
        .expect("replay cancel");
    assert!(
        !store
            .commit_model_attempt(
                cancellable.root_frame_id.clone(),
                late_attempt.attempt,
                late_attempt.revision,
                vec![json!({"type": "late_model_result"})],
            )
            .await
            .expect("late model result is recorded without reviving the cancelled run")
    );
    let cancelled = store
        .get_run(session.id.clone(), cancellable.id.clone())
        .await
        .expect("cancelled run");
    assert_eq!(cancelled.state, kiteframe::RunState::Cancelled);
    let late_items: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM model_items WHERE frame_id = $1 AND attempt = $2 AND direction = 'output'",
    )
    .bind(*cancellable.root_frame_id.as_uuid())
    .bind(late_attempt.attempt as i32)
    .fetch_one(store.pool())
    .await
    .expect("count late model items");
    assert_eq!(late_items, 1);
    assert!(
        store
            .events_after(session.id.clone(), cancellable.id, 0, 16)
            .await
            .expect("cancelled run events")
            .iter()
            .any(|event| event.kind == "model.late_result")
    );
    let recoverable = store
        .start_run(StartRun {
            session_id: session.id.clone(),
            workflow_name: definition.name.clone(),
            version: Some(definition.version),
            input: json!({"query": "recover"}),
            idempotency_key: IdempotencyKey::new(format!("recover-{}", Uuid::now_v7())).unwrap(),
        })
        .await
        .expect("start recoverable run");
    store
        .begin_model_attempt(
            recoverable.root_frame_id.clone(),
            0,
            json!({"prompt":"recover"}),
        )
        .await
        .expect("interrupted model intent");
    assert!(
        store
            .requeue_uncommitted_model_attempts(256)
            .await
            .expect("recover model intent")
            >= 1
    );
    assert!(
        store
            .runnable_frames_for_run(recoverable.id.clone(), 8)
            .await
            .expect("requeued frame")
            .iter()
            .any(|frame| frame.id == recoverable.root_frame_id)
    );
    let completable = store
        .start_run(StartRun {
            session_id: session.id.clone(),
            workflow_name: definition.name.clone(),
            version: Some(definition.version),
            input: json!({"query": "complete"}),
            idempotency_key: IdempotencyKey::new(format!("complete-{}", Uuid::now_v7())).unwrap(),
        })
        .await
        .expect("start completable run");
    let completion_attempt = store
        .begin_model_attempt(
            completable.root_frame_id.clone(),
            0,
            json!({"prompt": "complete"}),
        )
        .await
        .expect("completion intent");
    store
        .commit_model_attempt(
            completable.root_frame_id.clone(),
            completion_attempt.attempt,
            completion_attempt.revision,
            vec![json!({"type": "function_call"})],
        )
        .await
        .expect("completion output");
    store
        .record_completion(
            completable.root_frame_id.clone(),
            2,
            "ok".to_owned(),
            json!({}),
        )
        .await
        .expect("record completion");
    assert!(
        store
            .runnable_frames_for_run(completable.id.clone(), 8)
            .await
            .expect("this run's runnable frames")
            .iter()
            .any(|frame| frame.id == completable.root_frame_id
                && frame.outcome_name.as_deref() == Some("ok"))
    );
    assert!(matches!(
        store
            .start_run(StartRun {
                input: json!({"query": "changed"}),
                ..StartRun {
                    session_id: session.id,
                    workflow_name: definition.name,
                    version: Some(definition.version),
                    input: json!({"query": "ignored"}),
                    idempotency_key: key,
                }
            })
            .await,
        Err(StoreError::IdempotencyConflict)
    ));
}

#[tokio::test]
async fn active_run_quota_rejects_new_work_but_replays_the_original_request() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let store = PostgresStore::connect_with_limits(
        &database_url,
        StoreLimits {
            max_active_runs_per_session: 1,
            ..StoreLimits::default()
        },
    )
    .await
    .expect("quota store");
    let session = store
        .create_session(CreateSession {
            principal: PrincipalId::new(format!("run-quota-{}", Uuid::now_v7())).unwrap(),
            working_directory: "/quota".to_owned(),
            retention_until: OffsetDateTime::now_utc() + Duration::days(1),
        })
        .await
        .expect("session");
    let catalog =
        ToolCatalogSnapshot::new(session.id.clone(), Uuid::now_v7(), []).expect("catalog");
    store
        .append_catalog_epoch(&catalog)
        .await
        .expect("catalog epoch");
    let name = format!("quota{}", Uuid::now_v7().simple());
    let source = format!(
        "workflow {name}() -> string {{ start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [] on ok() {{ done \"ok\" }} }}"
    );
    let workflow = compile(&source, &catalog, &Limits::default()).expect("workflow");
    let definition = store
        .publish(session.id.clone(), &workflow, &source)
        .await
        .expect("publish");
    let request = StartRun {
        session_id: session.id.clone(),
        workflow_name: definition.name.clone(),
        version: Some(definition.version),
        input: json!({}),
        idempotency_key: IdempotencyKey::new(format!("quota-start-{}", Uuid::now_v7())).unwrap(),
    };
    let first = store.start_run(request.clone()).await.expect("first run");
    assert_eq!(
        store.start_run(request).await.expect("idempotent replay"),
        first
    );
    assert!(matches!(
        store
            .start_run(StartRun {
                session_id: session.id,
                workflow_name: definition.name,
                version: Some(definition.version),
                input: json!({}),
                idempotency_key: IdempotencyKey::new(format!("quota-start-{}", Uuid::now_v7()))
                    .unwrap(),
            })
            .await,
        Err(StoreError::QuotaExceeded("active_runs_per_session"))
    ));
}

fn tool(name: &str) -> CatalogTool {
    let alias = name.split_once('/').expect("qualified name").0;
    CatalogTool::new(
        name,
        alias,
        "test tool",
        ClosedSchema::object("input", Default::default()),
        ClosedSchema::object("output", Default::default()),
        BTreeSet::new(),
        EffectKind::ReadOnly,
        RecoverySupport::NotRequired,
    )
    .expect("tool")
}
