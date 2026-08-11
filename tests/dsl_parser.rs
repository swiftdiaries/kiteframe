use kiteframe::dsl::{ParseLimits, StatementAst, TypeAst, ValueRefAst, parse};

const SUPPORT: &str = include_str!("fixtures/dsl/support.kite");

fn limits() -> ParseLimits {
    ParseLimits {
        max_source_bytes: 32 * 1024,
        max_tokens: 4_096,
        max_identifier_bytes: 128,
        max_nesting: 32,
        max_diagnostics: 4,
    }
}

#[test]
fn parses_the_approved_support_workflow_with_every_control_operation() {
    let source = parse(SUPPORT, limits()).expect("the approved example parses");

    assert_eq!(source.records.len(), 1);
    assert_eq!(source.records[0].name, "Evidence");
    assert_eq!(source.records[0].fields[2].name, "references");
    assert_eq!(
        source.records[0].fields[2].ty,
        TypeAst::List(Box::new(TypeAst::String))
    );
    assert_eq!(source.workflow.name, "support");
    assert_eq!(source.workflow.entry.task, "triage");
    assert_eq!(source.workflow.limits["steps"], 100);
    assert_eq!(source.tasks.len(), 4);

    let triage = source
        .tasks
        .iter()
        .find(|task| task.name == "triage")
        .unwrap();
    assert_eq!(
        triage.inputs[1].ty,
        TypeAst::Optional(Box::new(TypeAst::String))
    );
    assert!(triage.prompt.text.contains("Understand the ticket"));
    assert!(triage.prompt.span.start < triage.prompt.span.end);
    assert_eq!(triage.tools, vec!["search_docs"]);
    assert!(
        matches!(triage.outcomes[0].statements.as_slice(), [StatementAst::Done(ValueRefAst::Binding(name))] if name == "answer")
    );
    assert!(
        matches!(triage.outcomes[1].statements.as_slice(), [StatementAst::Parallel(calls), StatementAst::Goto(_)] if calls.len() == 2)
    );
    assert!(
        matches!(triage.outcomes[2].statements.as_slice(), [StatementAst::Ask { binding, answer_type: TypeAst::String, question: ValueRefAst::Binding(question) }, StatementAst::Goto(_)] if binding == "answer" && question == "question")
    );

    let logs = source
        .tasks
        .iter()
        .find(|task| task.name == "inspect_logs")
        .unwrap();
    assert!(
        matches!(logs.outcomes[1].statements.as_slice(), [StatementAst::Fail { code: ValueRefAst::String(code), message: ValueRefAst::Binding(message) }] if code == "logs_unavailable" && message == "reason")
    );
}

#[test]
fn rejects_hostile_inputs_with_bounded_diagnostics() {
    let errors = parse(
        r#"workflow one() -> string { start task() }
workflow two() -> string { start task() }
task task() -> string { prompt \"\"\"unterminated tools [] on ok() { done \"x\" } }"#,
        ParseLimits {
            max_diagnostics: 2,
            ..limits()
        },
    )
    .expect_err("duplicate workflows and unterminated prompts are invalid");

    assert!(!errors.is_empty());
    assert!(errors.len() <= 2);
    assert!(
        errors
            .iter()
            .all(|error| error.span.start <= error.span.end)
    );

    assert!(parse(
        r#"workflow w() -> string { start t() } task t() -> string { prompt \"bad\\q\" tools [] on ok() { done \"x\" } }"#,
        limits(),
    )
    .is_err());

    let zero_budget = parse(
        "source that is longer than the configured limit",
        ParseLimits {
            max_source_bytes: 1,
            max_diagnostics: 0,
            ..limits()
        },
    )
    .expect_err("the diagnostic budget applies to source-size failures too");
    assert!(zero_budget.is_empty());
}

#[test]
fn enforces_source_token_identifier_and_nesting_limits() {
    let too_long = "workflow abcdef() -> string { start task() }";
    assert!(
        parse(
            too_long,
            ParseLimits {
                max_identifier_bytes: 3,
                ..limits()
            }
        )
        .is_err()
    );
    assert!(
        parse(
            too_long,
            ParseLimits {
                max_source_bytes: 8,
                ..limits()
            }
        )
        .is_err()
    );
    assert!(
        parse(
            too_long,
            ParseLimits {
                max_tokens: 2,
                ..limits()
            }
        )
        .is_err()
    );

    let deep = format!("{}x{}", "[".repeat(8), "]".repeat(8));
    let nested = format!(
        "type R {{ field: {deep} }} workflow w() -> string {{ start t() }} task t() -> string {{ prompt \"\"\"x\"\"\" tools [] on ok() {{ done \"x\" }} }}"
    );
    assert!(
        parse(
            &nested,
            ParseLimits {
                max_nesting: 4,
                ..limits()
            }
        )
        .is_err()
    );
}

#[test]
fn parses_decimal_numeric_literals_without_confusing_workflow_limits() {
    let source = parse(
        "workflow w() -> number { start t() } task t() -> number { prompt \"\"\"x\"\"\" tools [] on ok() { done -1.25 } }",
        limits(),
    )
    .expect("decimal number literals are part of the value grammar");

    assert!(matches!(
        source.tasks[0].outcomes[0].statements.as_slice(),
        [StatementAst::Done(ValueRefAst::Number(value))] if value == "-1.25"
    ));
}

#[test]
fn rejects_empty_or_incomplete_bodies_and_trailing_statements_after_terminal() {
    for source in [
        "workflow w() -> string { start t() } task t() -> string {}",
        "workflow w() -> string { start t() } task t() -> string { prompt \"\"\"x\"\"\" tools [] }",
        "workflow w() -> string { start t() } task t() -> string { prompt \"\"\"x\"\"\" tools [] on ok() {} }",
        "workflow w() -> string { start t() } task t() -> string { prompt \"\"\"x\"\"\" tools [] on ok() { parallel {} done \"x\" } }",
        "workflow w() -> string { start t() } task t() -> string { prompt \"\"\"x\"\"\" tools [] on ok() { done \"x\" call u() } }",
        "workflow w() -> string { start t() limits { unbounded: 1 } } task t() -> string { prompt \"\"\"x\"\"\" tools [] on ok() { done \"x\" } }",
        "workflow w() -> string { start t() } task t() -> string { prompt \"x\" tools [] on ok() { done \"x\" } }",
    ] {
        assert!(
            parse(source, limits()).is_err(),
            "invalid source was accepted: {source}"
        );
    }
}
