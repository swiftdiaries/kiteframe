use std::collections::BTreeMap;

use kiteframe::dsl::{Limits, compile};
use kiteframe::engine::{FrameSnapshot, TransitionError, next_command};
use kiteframe::{FrameCommand, SessionId, ToolCatalogSnapshot};
use serde_json::json;
use uuid::Uuid;

fn workflow() -> kiteframe::dsl::CompiledDefinition {
    let catalog = ToolCatalogSnapshot::new(SessionId::new(Uuid::nil()), Uuid::from_u128(1), [])
        .expect("empty catalog");
    compile(
        "workflow w(value: string) -> string { start t(value: value) } task t(value: string) -> string { prompt \"\"\"x\"\"\" tools [] on ok(answer: string) { done answer } on bad(reason: string) { fail \"bad\" reason } }",
        &catalog,
        &Limits::default(),
    )
    .expect("compile")
}

#[test]
fn resolves_terminal_outcomes_without_io() {
    let workflow = workflow();
    let done = next_command(
        &workflow,
        &FrameSnapshot {
            task_name: "t".to_owned(),
            outcome: "ok".to_owned(),
            statement_index: 0,
            bindings: BTreeMap::from([("answer".to_owned(), json!("done"))]),
        },
    )
    .expect("done command");
    assert!(matches!(done, FrameCommand::Complete(value) if value == json!("done")));
    let failed = next_command(
        &workflow,
        &FrameSnapshot {
            task_name: "t".to_owned(),
            outcome: "bad".to_owned(),
            statement_index: 0,
            bindings: BTreeMap::from([("reason".to_owned(), json!("nope"))]),
        },
    )
    .expect("failure command");
    assert!(
        matches!(failed, FrameCommand::Fail { code, message } if code == "bad" && message == "nope")
    );
}

#[test]
fn rejects_missing_bindings_and_corrupt_cursors() {
    let workflow = workflow();
    let missing = FrameSnapshot {
        task_name: "t".to_owned(),
        outcome: "ok".to_owned(),
        statement_index: 0,
        bindings: BTreeMap::new(),
    };
    assert!(matches!(
        next_command(&workflow, &missing),
        Err(TransitionError::MissingBinding(_))
    ));
    let corrupt = FrameSnapshot {
        statement_index: 1,
        ..missing
    };
    assert!(matches!(
        next_command(&workflow, &corrupt),
        Err(TransitionError::Corrupt(_))
    ));
}

#[test]
fn emits_call_parallel_and_ask_commands_in_source_order() {
    let catalog = ToolCatalogSnapshot::new(SessionId::new(Uuid::nil()), Uuid::from_u128(2), [])
        .expect("empty catalog");
    let workflow = compile(
        "workflow w(value: string) -> string { start t(value: value) } task t(value: string) -> string { prompt \"\"\"x\"\"\" tools [] on call_one() { child = call c(value: value) done child } on parallel_one() { parallel { left = call c(value: value) right = call c(value: value) } done value } on ask_one(question: string) { answer = ask string question done answer } } task c(value: string) -> string { prompt \"\"\"x\"\"\" tools [] on ok() { done value } }",
        &catalog,
        &Limits::default(),
    ).expect("compile");
    let bindings = BTreeMap::from([
        ("value".to_owned(), json!("v")),
        ("question".to_owned(), json!("q")),
    ]);
    let call = next_command(
        &workflow,
        &FrameSnapshot {
            task_name: "t".to_owned(),
            outcome: "call_one".to_owned(),
            statement_index: 0,
            bindings: bindings.clone(),
        },
    )
    .expect("call");
    assert!(
        matches!(call, FrameCommand::StartCall(child) if child.target == "c" && child.binding == "child")
    );
    let parallel = next_command(
        &workflow,
        &FrameSnapshot {
            task_name: "t".to_owned(),
            outcome: "parallel_one".to_owned(),
            statement_index: 0,
            bindings: bindings.clone(),
        },
    )
    .expect("parallel");
    assert!(
        matches!(parallel, FrameCommand::StartParallel(children) if children.iter().map(|child| child.binding.as_str()).collect::<Vec<_>>() == ["left", "right"])
    );
    let ask = next_command(
        &workflow,
        &FrameSnapshot {
            task_name: "t".to_owned(),
            outcome: "ask_one".to_owned(),
            statement_index: 0,
            bindings,
        },
    )
    .expect("ask");
    assert!(matches!(ask, FrameCommand::SuspendAsk(question) if question.text == "q"));
}

#[test]
fn emits_tail_goto_with_freshly_resolved_bindings() {
    let catalog = ToolCatalogSnapshot::new(SessionId::new(Uuid::nil()), Uuid::from_u128(6), [])
        .expect("empty catalog");
    let workflow = compile(
        "workflow w(value: string) -> string { start first(value: value) } task first(value: string) -> string { prompt \"\"\"x\"\"\" tools [] on redirect() { goto second(value: value) } } task second(value: string) -> string { prompt \"\"\"x\"\"\" tools [] on ok() { done value } }",
        &catalog,
        &Limits::default(),
    )
    .expect("compile");
    let command = next_command(
        &workflow,
        &FrameSnapshot {
            task_name: "first".to_owned(),
            outcome: "redirect".to_owned(),
            statement_index: 0,
            bindings: BTreeMap::from([("value".to_owned(), json!("carried"))]),
        },
    )
    .expect("goto command");
    assert!(
        matches!(command, FrameCommand::Goto { task_name, bindings } if task_name == "second" && bindings == BTreeMap::from([("value".to_owned(), json!("carried"))]))
    );
}

#[test]
fn preserves_large_numeric_literals() {
    let catalog = ToolCatalogSnapshot::new(SessionId::new(Uuid::nil()), Uuid::from_u128(3), [])
        .expect("empty catalog");
    let workflow = compile(
        "workflow w() -> integer { start t() } task t() -> integer { prompt \"\"\"x\"\"\" tools [] on ok() { done 9007199254740993 } }",
        &catalog,
        &Limits::default(),
    ).expect("compile");
    let command = next_command(
        &workflow,
        &FrameSnapshot {
            task_name: "t".to_owned(),
            outcome: "ok".to_owned(),
            statement_index: 0,
            bindings: BTreeMap::new(),
        },
    )
    .expect("number");
    assert!(
        matches!(command, FrameCommand::Complete(value) if value == json!(9_007_199_254_740_993_u64))
    );
}

#[test]
fn emits_closed_record_schema_for_typed_questions() {
    let catalog = ToolCatalogSnapshot::new(SessionId::new(Uuid::nil()), Uuid::from_u128(4), [])
        .expect("empty catalog");
    let workflow = compile(
        "type Choice { approved: boolean note: string? } workflow w() -> string { start t() } task t() -> string { prompt \"\"\"x\"\"\" tools [] on ask() { answer = ask Choice \"Continue?\" done \"done\" } }",
        &catalog,
        &Limits::default(),
    )
    .expect("compile");
    let command = next_command(
        &workflow,
        &FrameSnapshot {
            task_name: "t".to_owned(),
            outcome: "ask".to_owned(),
            statement_index: 0,
            bindings: BTreeMap::new(),
        },
    )
    .expect("ask command");
    assert!(
        matches!(command, FrameCommand::SuspendAsk(question) if question.schema.validate(&json!({"approved": true, "note": null})).is_ok() && question.schema.validate(&json!({"approved": true})).is_err())
    );
}

#[test]
fn accepts_required_nullable_scalar_answers_and_rejects_corrupt_record_ir() {
    let catalog = ToolCatalogSnapshot::new(SessionId::new(Uuid::nil()), Uuid::from_u128(5), [])
        .expect("empty catalog");
    let mut workflow = compile(
        "type Choice { value: string? } workflow w() -> string { start t() } task t() -> string { prompt \"\"\"x\"\"\" tools [] on ask_optional() { answer = ask string? \"Optional\" done \"done\" } on ask_record() { answer = ask Choice \"Record\" done \"done\" } }",
        &catalog,
        &Limits::default(),
    )
    .expect("compile");
    let optional = next_command(
        &workflow,
        &FrameSnapshot {
            task_name: "t".to_owned(),
            outcome: "ask_optional".to_owned(),
            statement_index: 0,
            bindings: BTreeMap::new(),
        },
    )
    .expect("nullable question");
    assert!(
        matches!(optional, FrameCommand::SuspendAsk(question) if question.schema.validate(&json!(null)).is_ok() && question.schema.validate(&json!("value")).is_ok())
    );

    workflow.records.remove("Choice");
    assert!(matches!(
        next_command(
            &workflow,
            &FrameSnapshot {
                task_name: "t".to_owned(),
                outcome: "ask_record".to_owned(),
                statement_index: 0,
                bindings: BTreeMap::new(),
            },
        ),
        Err(TransitionError::Corrupt(message)) if message.contains("record type is absent")
    ));
}
