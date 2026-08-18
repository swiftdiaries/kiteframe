use std::collections::BTreeMap;

use super::ast::*;
use super::lexer::{Token, TokenKind, lex};
use super::{Diagnostic, Span};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseLimits {
    pub max_source_bytes: usize,
    pub max_tokens: usize,
    pub max_identifier_bytes: usize,
    pub max_nesting: usize,
    pub max_diagnostics: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParseReport {
    pub source: SourceFile,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn parse(source: &str, limits: ParseLimits) -> Result<SourceFile, Vec<Diagnostic>> {
    parse_report(source, limits).map(|report| report.source)
}

pub fn parse_report(source: &str, limits: ParseLimits) -> Result<ParseReport, Vec<Diagnostic>> {
    if source.len() > limits.max_source_bytes {
        return Err(if limits.max_diagnostics == 0 {
            Vec::new()
        } else {
            vec![Diagnostic::error(
                "source_byte_limit",
                "source exceeds configured byte limit",
                Span {
                    start: limits.max_source_bytes,
                    end: source.len(),
                },
            )]
        });
    }
    let tokens = lex(
        source,
        limits.max_tokens,
        limits.max_identifier_bytes,
        limits.max_diagnostics,
    )?;
    let mut parser = Parser {
        tokens,
        position: 0,
        diagnostics: Vec::new(),
        limits,
        had_error: false,
    };
    let value = parser.source_file(source.len());
    if parser.had_error {
        Err(parser.diagnostics)
    } else {
        value
            .map(|source| ParseReport {
                source,
                diagnostics: parser.diagnostics,
            })
            .ok_or_else(Vec::new)
    }
}

struct Parser {
    tokens: Vec<Token>,
    position: usize,
    diagnostics: Vec<Diagnostic>,
    limits: ParseLimits,
    had_error: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DefinitionSyntax {
    Workflow,
    Agent,
}

impl Parser {
    fn source_file(&mut self, source_len: usize) -> Option<SourceFile> {
        let mut records = Vec::new();
        let mut definition = None;
        let mut saw_definition = false;
        let mut workflow_tasks = Vec::new();
        while !self.at_eof() {
            if self.keyword("type") {
                if let Some(record) = self.record() {
                    records.push(record);
                }
            } else if self.keyword("workflow") {
                let declaration_span = self.previous_span();
                self.warning(
                    "deprecated_workflow",
                    "workflow definitions are deprecated",
                    declaration_span,
                );
                let item = self.workflow();
                if saw_definition {
                    self.error(
                        "workflow_definition_count",
                        "a source file contains exactly one workflow",
                        declaration_span,
                    );
                } else {
                    definition = item.map(DefinitionAst::Workflow);
                    saw_definition = true;
                }
            } else if self.keyword("agent") {
                let declaration_span = self.previous_span();
                let item = self.agent();
                if saw_definition {
                    self.error(
                        "definition_count",
                        "a source file contains exactly one definition",
                        declaration_span,
                    );
                } else {
                    definition = item.map(DefinitionAst::Agent);
                    saw_definition = true;
                }
            } else if self.keyword("task") {
                if let Some(task) = self.task(false) {
                    workflow_tasks.push(task);
                }
            } else {
                self.error(
                    "expected_top_level_declaration",
                    "expected type, workflow, or task declaration",
                    self.current_span(),
                );
                self.advance();
            }
        }
        let Some(definition) = definition else {
            if !saw_definition {
                self.error(
                    "workflow_definition_count",
                    "a source file contains exactly one workflow",
                    Span {
                        start: 0,
                        end: source_len,
                    },
                );
            }
            return None;
        };
        let definition = match definition {
            DefinitionAst::Workflow(mut workflow) => {
                if workflow_tasks.is_empty() {
                    self.error(
                        "workflow_task_required",
                        "a source file requires at least one task",
                        workflow.span,
                    );
                }
                workflow.tasks = workflow_tasks;
                DefinitionAst::Workflow(workflow)
            }
            DefinitionAst::Agent(agent) => {
                if !workflow_tasks.is_empty() {
                    self.error(
                        "unexpected_top_level_task",
                        "top-level tasks are only allowed with workflow definitions",
                        workflow_tasks[0].span,
                    );
                }
                DefinitionAst::Agent(agent)
            }
        };
        Some(SourceFile {
            span: Span {
                start: 0,
                end: source_len,
            },
            records,
            definition,
        })
    }

    fn record(&mut self) -> Option<RecordAst> {
        let start = self.previous_span().start;
        let name = self.identifier()?;
        self.expect_lbrace();
        let mut fields = Vec::new();
        while !self.at_rbrace() && !self.at_eof() {
            let field_start = self.current_span().start;
            let field_name = self.identifier()?;
            self.expect_colon();
            let ty = self.ty(0)?;
            fields.push(FieldAst {
                name: field_name,
                ty,
                span: Span {
                    start: field_start,
                    end: self.previous_span().end,
                },
            });
            self.optional_comma();
        }
        self.expect_rbrace();
        Some(RecordAst {
            name,
            fields,
            span: Span {
                start,
                end: self.previous_span().end,
            },
        })
    }

    fn workflow(&mut self) -> Option<WorkflowAst> {
        let start = self.previous_span().start;
        let name = self.identifier()?;
        let inputs = self.fields_in_parens()?;
        self.expect_arrow();
        let result = self.ty(0)?;
        self.expect_lbrace();
        let mut entry = None;
        let mut limits = BTreeMap::new();
        while !self.at_rbrace() && !self.at_eof() {
            if self.keyword("start") {
                if entry.is_some() {
                    self.error(
                        "duplicate_workflow_start",
                        "workflow has duplicate start declaration",
                        self.previous_span(),
                    );
                }
                entry = self.call(None, DefinitionSyntax::Workflow);
            } else if self.keyword("limits") {
                self.expect_lbrace();
                while !self.at_rbrace() && !self.at_eof() {
                    let key = self.identifier()?;
                    self.expect_colon();
                    let (value, span) = self.number()?;
                    if key == "elapsed_seconds" {
                        self.error(
                            "elapsed_seconds_requires_agent",
                            "unsupported workflow limit",
                            span,
                        );
                    } else if !matches!(
                        key.as_str(),
                        "steps"
                            | "model_turns_per_task"
                            | "tool_calls_per_response"
                            | "parallel_branches"
                            | "response_item_bytes"
                            | "active_execution_seconds"
                    ) {
                        self.error(
                            "unsupported_workflow_limit",
                            "unsupported workflow limit",
                            span,
                        );
                    }
                    if value == 0 {
                        self.error(
                            "non_positive_workflow_limit",
                            "workflow limit must be positive",
                            span,
                        );
                    }
                    if limits.insert(key, value).is_some() {
                        self.error(
                            "duplicate_workflow_limit",
                            "workflow limit is duplicated",
                            span,
                        );
                    }
                    self.optional_comma();
                }
                self.expect_rbrace();
            } else {
                self.error(
                    "invalid_workflow_body",
                    "workflow body accepts only start and limits",
                    self.current_span(),
                );
                self.advance();
            }
        }
        self.expect_rbrace();
        let Some(entry) = entry else {
            self.error(
                "missing_workflow_start",
                "workflow requires a start declaration",
                Span {
                    start,
                    end: self.previous_span().end,
                },
            );
            return None;
        };
        Some(WorkflowAst {
            name,
            inputs,
            result,
            entry,
            limits,
            tasks: Vec::new(),
            span: Span {
                start,
                end: self.previous_span().end,
            },
        })
    }

    fn agent(&mut self) -> Option<AgentAst> {
        let start = self.previous_span().start;
        let name = self.identifier()?;
        let inputs = self.fields_in_parens()?;
        self.expect_arrow();
        let result = self.ty(0)?;
        self.expect_lbrace();
        let mut entry = None;
        let mut events = None;
        let mut limits = BTreeMap::new();
        let mut saw_limits = false;
        let mut tasks = Vec::new();
        while !self.at_rbrace() && !self.at_eof() {
            if self.keyword("entry") {
                if entry.is_some() {
                    self.error(
                        "duplicate_agent_entry",
                        "agent has duplicate entry declaration",
                        self.previous_span(),
                    );
                }
                entry = self.call(None, DefinitionSyntax::Agent);
            } else if self.keyword("events") {
                let value = self.events()?;
                if events.replace(value).is_some() {
                    self.error(
                        "duplicate_agent_events",
                        "agent has duplicate events declaration",
                        self.previous_span(),
                    );
                }
            } else if self.keyword("limits") {
                if saw_limits {
                    self.error(
                        "duplicate_agent_limits",
                        "agent has duplicate limits declaration",
                        self.previous_span(),
                    );
                }
                saw_limits = true;
                self.agent_limits(&mut limits)?;
            } else if self.keyword("task") {
                if let Some(task) = self.task(true) {
                    tasks.push(task);
                }
            } else if self.keyword("agent") {
                self.error(
                    "nested_agent_not_allowed",
                    "agent definitions cannot be nested",
                    self.previous_span(),
                );
                self.skip_declaration_body();
            } else {
                self.error(
                    "invalid_agent_body",
                    "agent body accepts only entry, events, limits, and task declarations",
                    self.current_span(),
                );
                self.advance();
            }
        }
        self.expect_rbrace();
        let span = Span {
            start,
            end: self.previous_span().end,
        };
        let Some(entry) = entry else {
            self.error(
                "missing_agent_entry",
                "agent requires an entry declaration",
                span,
            );
            return None;
        };
        if tasks.is_empty() {
            self.error(
                "missing_agent_task",
                "agent requires at least one task",
                span,
            );
        }
        Some(AgentAst {
            name,
            inputs,
            result,
            entry,
            events: events.unwrap_or_default(),
            limits,
            tasks,
            span,
        })
    }

    fn task(&mut self, is_agent: bool) -> Option<TaskAst> {
        let start = self.previous_span().start;
        let name = self.identifier()?;
        let inputs = self.fields_in_parens()?;
        self.expect_arrow();
        let result = self.ty(0)?;
        self.expect_lbrace();
        let mut model_role = None;
        let mut prompt = None;
        let mut tools = None;
        let mut outcomes = Vec::new();
        while !self.at_rbrace() && !self.at_eof() {
            if self.keyword("model") {
                let role_span = self.current_span();
                let role = self.model_role()?;
                if is_agent {
                    if model_role.replace(role).is_some() {
                        self.error(
                            "duplicate_agent_task_model",
                            "agent task has duplicate model declaration",
                            role_span,
                        );
                    }
                } else {
                    self.error(
                        "model_role_requires_agent",
                        "workflow tasks do not accept model declarations",
                        role_span,
                    );
                }
            } else if self.keyword("prompt") {
                let token = self.take_prompt()?;
                if prompt
                    .replace(PromptAst {
                        text: token.0,
                        span: token.1,
                    })
                    .is_some()
                {
                    self.error(
                        "duplicate_task_prompt",
                        "task has duplicate prompt",
                        token.1,
                    );
                }
            } else if self.keyword("tools") {
                let list = self.tool_list()?;
                if tools.replace(list).is_some() {
                    self.error(
                        "duplicate_task_tools",
                        "task has duplicate tools declaration",
                        self.previous_span(),
                    );
                }
            } else if self.keyword("on") {
                let syntax = if is_agent {
                    DefinitionSyntax::Agent
                } else {
                    DefinitionSyntax::Workflow
                };
                if let Some(outcome) = self.outcome(syntax) {
                    outcomes.push(outcome);
                }
            } else {
                self.error(
                    "invalid_task_body",
                    "task body accepts only prompt, tools, and outcomes",
                    self.current_span(),
                );
                self.advance();
            }
        }
        self.expect_rbrace();
        let span = Span {
            start,
            end: self.previous_span().end,
        };
        let prompt = match prompt {
            Some(value) => value,
            None => {
                self.error("missing_task_prompt", "task requires a prompt", span);
                PromptAst {
                    text: String::new(),
                    span,
                }
            }
        };
        let tools = match tools {
            Some(value) => value,
            None => {
                self.error(
                    "missing_task_tools",
                    "task requires a tools declaration",
                    span,
                );
                Vec::new()
            }
        };
        if outcomes.is_empty() {
            self.error(
                "task_outcome_required",
                "task requires at least one outcome",
                span,
            );
        }
        if is_agent && model_role.is_none() {
            self.error(
                "missing_model_role",
                "agent task requires a model declaration",
                span,
            );
        }
        Some(TaskAst {
            name,
            inputs,
            result,
            model_role,
            prompt,
            tools,
            outcomes,
            span,
        })
    }

    fn outcome(&mut self, syntax: DefinitionSyntax) -> Option<OutcomeAst> {
        let start = self.previous_span().start;
        let name = self.identifier()?;
        let fields = self.fields_in_parens()?;
        self.expect_lbrace();
        let mut statements = Vec::new();
        let mut terminal = false;
        while !self.at_rbrace() && !self.at_eof() {
            if terminal {
                self.error(
                    "statement_after_terminal",
                    "statements are not allowed after a terminal control operation",
                    self.current_span(),
                );
                self.advance();
                continue;
            }
            let Some((statement, is_terminal)) = self.statement(syntax) else {
                self.advance();
                continue;
            };
            terminal = is_terminal;
            statements.push(statement);
        }
        self.expect_rbrace();
        let span = Span {
            start,
            end: self.previous_span().end,
        };
        if statements.is_empty() {
            self.error("empty_outcome", "outcome must not be empty", span);
        }
        if !terminal {
            self.error(
                "missing_terminal_statement",
                "outcome must end in goto, done, or fail",
                span,
            );
        }
        Some(OutcomeAst {
            name,
            fields,
            statements,
            span,
        })
    }

    fn statement(&mut self, syntax: DefinitionSyntax) -> Option<(StatementAst, bool)> {
        if self.keyword("parallel") {
            if syntax == DefinitionSyntax::Agent {
                self.error(
                    "agent_parallel_unsupported",
                    "agent outcomes use fork instead of parallel",
                    self.previous_span(),
                );
            }
            return self
                .parallel(syntax)
                .map(|value| (StatementAst::Parallel(value), false));
        }
        if self.keyword("goto") {
            if syntax == DefinitionSyntax::Agent {
                self.error(
                    "goto_requires_workflow",
                    "agent outcomes use return instead of goto",
                    self.previous_span(),
                );
            }
            return self
                .call(None, syntax)
                .map(|value| (StatementAst::Goto(value), true));
        }
        if self.keyword("fork") {
            if syntax == DefinitionSyntax::Workflow {
                self.error(
                    "workflow_fork_unsupported",
                    "workflow outcomes use parallel instead of fork",
                    self.previous_span(),
                );
                return self
                    .fork()
                    .map(|value| (StatementAst::Parallel(value), false));
            }
            return self
                .fork()
                .map(|value| (StatementAst::Parallel(value), false));
        }
        if self.keyword("return") {
            if syntax == DefinitionSyntax::Workflow {
                self.error(
                    "return_requires_agent",
                    "workflow outcomes use goto instead of return",
                    self.previous_span(),
                );
            }
            return self
                .call(None, syntax)
                .map(|value| (StatementAst::Goto(value), true));
        }
        if self.keyword("done") {
            return self.value().map(|value| (StatementAst::Done(value), true));
        }
        if self.keyword("fail") {
            let code = self.value()?;
            let message = self.value()?;
            return Some((StatementAst::Fail { code, message }, true));
        }
        if self.keyword("complete") {
            return self
                .complete_goal(syntax)
                .map(|value| (StatementAst::CompleteGoal(value), true));
        }
        if self.keyword("block") {
            return self
                .block_goal(syntax)
                .map(|value| (StatementAst::BlockGoal(value), true));
        }
        let binding = self.identifier()?;
        self.expect_equals();
        if self.keyword("call") {
            self.call(Some(binding), syntax)
                .map(|value| (StatementAst::Call(value), false))
        } else if self.keyword("ask") {
            let answer_type = self.ty(0)?;
            let question = self.value()?;
            Some((
                StatementAst::Ask {
                    binding,
                    answer_type,
                    question,
                },
                false,
            ))
        } else {
            self.error(
                "invalid_binding_statement",
                "binding accepts only call or ask",
                self.current_span(),
            );
            None
        }
    }

    fn complete_goal(&mut self, syntax: DefinitionSyntax) -> Option<Vec<CriterionEvidenceAst>> {
        let start = self.previous_span().start;
        if syntax != DefinitionSyntax::Agent {
            self.error(
                "complete_goal_requires_agent",
                "complete goal is available only in agent outcomes",
                self.previous_span(),
            );
        }
        if !self.keyword("goal") {
            self.error(
                "complete_goal_requires_agent",
                "complete must be followed by goal",
                self.current_span(),
            );
            return None;
        }
        self.expect_lbrace();
        let mut criteria = Vec::new();
        while !self.at_rbrace() && !self.at_eof() {
            let criterion_start = self.current_span().start;
            let criterion_id = self.goal_criterion_identifier()?;
            self.expect_colon();
            let evidence = self.goal_authority_value()?;
            let span = Span {
                start: criterion_start,
                end: self.previous_span().end,
            };
            if criteria
                .iter()
                .any(|criterion: &CriterionEvidenceAst| criterion.criterion_id == criterion_id)
            {
                self.error(
                    "duplicate_goal_criterion",
                    "goal completion criterion is duplicated",
                    span,
                );
            }
            criteria.push(CriterionEvidenceAst {
                criterion_id,
                evidence,
                span,
            });
            if self.optional_comma() {
                self.error(
                    "invalid_goal_criterion",
                    "goal completion criteria are separated by whitespace",
                    self.previous_span(),
                );
            }
        }
        self.expect_rbrace();
        if criteria.is_empty() {
            self.error(
                "empty_goal_completion",
                "complete goal requires at least one criterion",
                Span {
                    start,
                    end: self.previous_span().end,
                },
            );
        }
        Some(criteria)
    }

    fn block_goal(&mut self, syntax: DefinitionSyntax) -> Option<BlockGoalAst> {
        let start = self.previous_span().start;
        if syntax != DefinitionSyntax::Agent {
            self.error(
                "block_goal_requires_agent",
                "block goal is available only in agent outcomes",
                self.previous_span(),
            );
        }
        if !self.keyword("goal") {
            self.error(
                "block_goal_requires_agent",
                "block must be followed by goal",
                self.current_span(),
            );
            return None;
        }
        if !self.keyword("when") {
            self.error(
                "invalid_blocked_when_index",
                "block goal requires a blocked-when index",
                self.current_span(),
            );
            return None;
        }
        let blocked_when_index = self.blocked_when_index()?;
        self.expect_lbrace();

        let mut reason_code = None;
        let mut explanation = None;
        let mut evidence = None;
        let mut unlock_conditions = None;
        while !self.at_rbrace() && !self.at_eof() {
            let field_span = self.current_span();
            let field = self.identifier()?;
            self.expect_colon();
            match field.as_str() {
                "code" => {
                    let value = self.goal_authority_value()?;
                    if reason_code.replace(value).is_some() {
                        self.error(
                            "duplicate_block_goal_field",
                            "block goal field is duplicated",
                            field_span,
                        );
                    }
                }
                "explanation" => {
                    let value = self.goal_authority_value()?;
                    if explanation.replace(value).is_some() {
                        self.error(
                            "duplicate_block_goal_field",
                            "block goal field is duplicated",
                            field_span,
                        );
                    }
                }
                "evidence" => {
                    let value = self.goal_authority_values("empty_block_evidence")?;
                    if evidence.replace(value).is_some() {
                        self.error(
                            "duplicate_block_goal_field",
                            "block goal field is duplicated",
                            field_span,
                        );
                    }
                }
                "unlock" => {
                    let value = self.goal_authority_values("empty_unlock_conditions")?;
                    if unlock_conditions.replace(value).is_some() {
                        self.error(
                            "duplicate_block_goal_field",
                            "block goal field is duplicated",
                            field_span,
                        );
                    }
                }
                _ => {
                    self.error(
                        "missing_block_goal_field",
                        "block goal contains an unknown field",
                        field_span,
                    );
                    self.skip_block_goal_field_value()?;
                }
            }
            if self.optional_comma() {
                self.error(
                    "missing_block_goal_field",
                    "block goal fields are separated by whitespace",
                    self.previous_span(),
                );
            }
        }
        self.expect_rbrace();
        let span = Span {
            start,
            end: self.previous_span().end,
        };
        let reason_code = reason_code.unwrap_or_else(|| {
            self.error("missing_block_goal_field", "block goal requires code", span);
            ValueRefAst::String(String::new())
        });
        let explanation = explanation.unwrap_or_else(|| {
            self.error(
                "missing_block_goal_field",
                "block goal requires explanation",
                span,
            );
            ValueRefAst::String(String::new())
        });
        let evidence = evidence.unwrap_or_else(|| {
            self.error(
                "missing_block_goal_field",
                "block goal requires evidence",
                span,
            );
            Vec::new()
        });
        let unlock_conditions = unlock_conditions.unwrap_or_else(|| {
            self.error(
                "missing_block_goal_field",
                "block goal requires unlock",
                span,
            );
            Vec::new()
        });
        Some(BlockGoalAst {
            blocked_when_index,
            reason_code,
            explanation,
            evidence,
            unlock_conditions,
            span,
        })
    }

    fn blocked_when_index(&mut self) -> Option<u16> {
        let span = self.current_span();
        let TokenKind::Number(value) = self.current().kind.clone() else {
            self.error(
                "invalid_blocked_when_index",
                "blocked-when index must be an unsigned decimal u16",
                span,
            );
            self.advance();
            return None;
        };
        self.advance();
        match value.parse::<u16>() {
            Ok(index) => Some(index),
            _ => {
                self.error(
                    "invalid_blocked_when_index",
                    "blocked-when index must be an unsigned decimal u16",
                    span,
                );
                None
            }
        }
    }

    fn goal_authority_value(&mut self) -> Option<ValueRefAst> {
        let span = self.current_span();
        let value = self.value()?;
        if !matches!(value, ValueRefAst::Binding(_) | ValueRefAst::String(_)) {
            self.error(
                "goal_authority_value_must_be_string",
                "goal authority values must be strings or string bindings",
                span,
            );
        }
        Some(value)
    }

    fn goal_authority_values(&mut self, empty_code: &'static str) -> Option<Vec<ValueRefAst>> {
        let span = self.current_span();
        self.expect_lbracket();
        let mut values = Vec::new();
        while !self.at_rbracket() && !self.at_eof() {
            values.push(self.goal_authority_value()?);
            if !self.optional_comma() && !self.at_rbracket() {
                self.error(
                    "expected_goal_authority_value_separator",
                    "expected comma or closing bracket",
                    self.current_span(),
                );
                return None;
            }
        }
        self.expect_rbracket();
        if values.is_empty() {
            self.error(empty_code, "goal authority list must not be empty", span);
        }
        Some(values)
    }

    fn skip_block_goal_field_value(&mut self) -> Option<()> {
        if self.at_lbracket() {
            self.advance();
            while !self.at_rbracket() && !self.at_eof() {
                self.value()?;
                self.optional_comma();
            }
            self.expect_rbracket();
        } else {
            self.value()?;
        }
        Some(())
    }

    fn parallel(&mut self, syntax: DefinitionSyntax) -> Option<Vec<CallAst>> {
        self.expect_lbrace();
        let mut calls = Vec::new();
        while !self.at_rbrace() && !self.at_eof() {
            let binding = self.identifier()?;
            self.expect_equals();
            if !self.keyword("call") {
                self.error(
                    "invalid_parallel_statement",
                    "parallel contains only call statements",
                    self.current_span(),
                );
                return None;
            }
            calls.push(self.call(Some(binding), syntax)?);
        }
        self.expect_rbrace();
        if calls.is_empty() {
            self.error(
                "empty_parallel_block",
                "parallel block must not be empty",
                self.previous_span(),
            );
        }
        Some(calls)
    }

    fn fork(&mut self) -> Option<Vec<CallAst>> {
        self.expect_lbrace();
        let mut calls = Vec::new();
        while !self.at_rbrace() && !self.at_eof() {
            if self.keyword("call") {
                self.error(
                    "fork_call_requires_binding",
                    "fork calls require a result binding",
                    self.previous_span(),
                );
                calls.push(self.call(None, DefinitionSyntax::Agent)?);
                continue;
            }
            let binding = self.identifier()?;
            self.expect_equals();
            if !self.keyword("call") {
                self.error(
                    "invalid_fork_statement",
                    "fork contains only bound call statements",
                    self.current_span(),
                );
                return None;
            }
            calls.push(self.call(Some(binding), DefinitionSyntax::Agent)?);
        }
        self.expect_rbrace();
        if calls.is_empty() {
            self.error(
                "empty_fork",
                "fork block must not be empty",
                self.previous_span(),
            );
        }
        if !self.keyword("join") {
            self.error(
                "missing_fork_join",
                "fork block must end with join",
                self.current_span(),
            );
        }
        Some(calls)
    }

    fn call(&mut self, binding: Option<String>, syntax: DefinitionSyntax) -> Option<CallAst> {
        let start = self.current_span().start;
        let task = self.identifier()?;
        self.expect_lparen();
        let mut arguments = Vec::new();
        while !self.at_rparen() && !self.at_eof() {
            let arg_start = self.current_span().start;
            let name = self.identifier()?;
            match syntax {
                DefinitionSyntax::Workflow => self.expect_colon(),
                DefinitionSyntax::Agent => self.expect_equals(),
            }
            let value = self.value()?;
            arguments.push(ArgumentAst {
                name,
                value,
                span: Span {
                    start: arg_start,
                    end: self.previous_span().end,
                },
            });
            if !self.optional_comma() && !self.at_rparen() {
                self.error(
                    "expected_argument_separator",
                    "expected comma or closing parenthesis",
                    self.current_span(),
                );
                return None;
            }
        }
        self.expect_rparen();
        let context = match syntax {
            DefinitionSyntax::Workflow => {
                if self.keyword("context") {
                    let span = self.previous_span();
                    self.identifier()?;
                    self.error(
                        "workflow_context_unsupported",
                        "workflow calls do not accept context modes",
                        span,
                    );
                }
                ContextModeAst::Legacy
            }
            DefinitionSyntax::Agent => {
                if !self.keyword("context") {
                    ContextModeAst::Isolated
                } else {
                    let span = self.current_span();
                    match self.identifier()?.as_str() {
                        "isolated" => ContextModeAst::Isolated,
                        "kept" => ContextModeAst::Kept,
                        _ => {
                            self.error(
                                "invalid_context_mode",
                                "agent context mode must be isolated or kept",
                                span,
                            );
                            ContextModeAst::Isolated
                        }
                    }
                }
            }
        };
        Some(CallAst {
            binding,
            task,
            arguments,
            context,
            span: Span {
                start,
                end: self.previous_span().end,
            },
        })
    }

    fn events(&mut self) -> Option<Vec<EventDeclarationAst>> {
        self.expect_lbracket();
        let mut events = Vec::new();
        while !self.at_rbracket() && !self.at_eof() {
            let span = self.current_span();
            let TokenKind::String(event_type) = self.current().kind.clone() else {
                self.error("expected_event_name", "expected event-name string", span);
                return None;
            };
            self.advance();
            if !valid_event_type(&event_type) {
                self.error(
                    "invalid_agent_event",
                    "event name must be a versioned semantic event string",
                    span,
                );
            }
            if events
                .iter()
                .any(|event: &EventDeclarationAst| event.event_type == event_type)
            {
                self.error(
                    "duplicate_agent_event",
                    "agent event name is duplicated",
                    span,
                );
            }
            events.push(EventDeclarationAst { event_type, span });
            if !self.optional_comma() && !self.at_rbracket() {
                self.error(
                    "expected_event_separator",
                    "expected comma or closing bracket",
                    self.current_span(),
                );
                return None;
            }
        }
        self.expect_rbracket();
        if events.is_empty() {
            self.error(
                "empty_agent_events",
                "events declaration requires at least one event name",
                self.previous_span(),
            );
        }
        Some(events)
    }

    fn agent_limits(&mut self, limits: &mut BTreeMap<String, u64>) -> Option<()> {
        self.expect_lbrace();
        while !self.at_rbrace() && !self.at_eof() {
            let key = self.identifier()?;
            self.expect_colon();
            let (value, span) = self.number()?;
            if !matches!(
                key.as_str(),
                "steps"
                    | "model_turns_per_task"
                    | "tool_calls_per_response"
                    | "parallel_branches"
                    | "response_item_bytes"
                    | "active_execution_seconds"
                    | "elapsed_seconds"
            ) {
                self.error("unsupported_agent_limit", "unsupported agent limit", span);
            }
            if value == 0 {
                self.error(
                    "non_positive_agent_limit",
                    "agent limit must be positive",
                    span,
                );
            }
            if limits.insert(key, value).is_some() {
                self.error("duplicate_agent_limit", "agent limit is duplicated", span);
            }
            self.optional_comma();
        }
        self.expect_rbrace();
        Some(())
    }

    fn model_role(&mut self) -> Option<ModelRoleAst> {
        let span = self.current_span();
        match self.identifier()?.as_str() {
            "reasoning" => Some(ModelRoleAst::Reasoning),
            "coding" => Some(ModelRoleAst::Coding),
            _ => {
                self.error(
                    "invalid_agent_model_role",
                    "agent model role must be reasoning or coding",
                    span,
                );
                None
            }
        }
    }

    fn skip_declaration_body(&mut self) {
        while !self.at_lbrace() && !self.at_eof() {
            self.advance();
        }
        if !self.at_lbrace() {
            return;
        }
        let mut depth = 0usize;
        while !self.at_eof() {
            if self.at_lbrace() {
                depth += 1;
            } else if self.at_rbrace() {
                depth -= 1;
                if depth == 0 {
                    self.advance();
                    return;
                }
            }
            self.advance();
        }
    }

    fn tool_list(&mut self) -> Option<Vec<String>> {
        self.expect_lbracket();
        let mut tools = Vec::new();
        while !self.at_rbracket() && !self.at_eof() {
            tools.push(self.identifier()?);
            if !self.optional_comma() && !self.at_rbracket() {
                self.error(
                    "expected_tool_separator",
                    "expected comma or closing bracket",
                    self.current_span(),
                );
                return None;
            }
        }
        self.expect_rbracket();
        Some(tools)
    }

    fn fields_in_parens(&mut self) -> Option<Vec<FieldAst>> {
        self.expect_lparen();
        let mut fields = Vec::new();
        while !self.at_rparen() && !self.at_eof() {
            let start = self.current_span().start;
            let name = self.identifier()?;
            self.expect_colon();
            let ty = self.ty(0)?;
            fields.push(FieldAst {
                name,
                ty,
                span: Span {
                    start,
                    end: self.previous_span().end,
                },
            });
            if !self.optional_comma() && !self.at_rparen() {
                self.error(
                    "expected_field_separator",
                    "expected comma or closing parenthesis",
                    self.current_span(),
                );
                return None;
            }
        }
        self.expect_rparen();
        Some(fields)
    }

    fn ty(&mut self, depth: usize) -> Option<TypeAst> {
        if depth >= self.limits.max_nesting {
            self.error(
                "type_nesting_limit",
                "type nesting exceeds configured limit",
                self.current_span(),
            );
            return None;
        }
        let mut ty = if self.at_lbracket() {
            self.advance();
            let inner = self.ty(depth + 1)?;
            self.expect_rbracket();
            TypeAst::List(Box::new(inner))
        } else {
            match self.identifier()?.as_str() {
                "string" => TypeAst::String,
                "integer" => TypeAst::Integer,
                "number" => TypeAst::Number,
                "boolean" => TypeAst::Boolean,
                other => TypeAst::Named(other.to_owned()),
            }
        };
        if self.at_question() {
            self.advance();
            ty = TypeAst::Optional(Box::new(ty));
        }
        Some(ty)
    }

    fn value(&mut self) -> Option<ValueRefAst> {
        match self.current().kind.clone() {
            TokenKind::String(value) => {
                self.advance();
                Some(ValueRefAst::String(value))
            }
            TokenKind::Number(value) => {
                self.advance();
                Some(ValueRefAst::Number(value))
            }
            TokenKind::Identifier(value) => {
                self.advance();
                match value.as_str() {
                    "true" => Some(ValueRefAst::Boolean(true)),
                    "false" => Some(ValueRefAst::Boolean(false)),
                    "null" => Some(ValueRefAst::Null),
                    _ => Some(ValueRefAst::Binding(value)),
                }
            }
            _ => {
                self.error(
                    "expected_value",
                    "expected a binding or literal value",
                    self.current_span(),
                );
                None
            }
        }
    }

    fn identifier(&mut self) -> Option<String> {
        match self.current().kind.clone() {
            TokenKind::Identifier(value) => {
                self.advance();
                Some(value)
            }
            _ => {
                self.error(
                    "expected_identifier",
                    "expected identifier",
                    self.current_span(),
                );
                None
            }
        }
    }
    fn goal_criterion_identifier(&mut self) -> Option<String> {
        let span = self.current_span();
        let (value, identifier_candidate) = match self.current().kind.clone() {
            TokenKind::Identifier(value) | TokenKind::GoalCriterionCandidate(value) => {
                (value, true)
            }
            TokenKind::String(value)
            | TokenKind::TripleString(value)
            | TokenKind::Number(value) => (value, false),
            _ => (String::new(), false),
        };
        self.advance();
        if !identifier_candidate || !valid_goal_criterion_id(&value) {
            self.error(
                "invalid_goal_criterion",
                "goal criterion id is invalid",
                span,
            );
        }
        Some(value)
    }
    fn take_prompt(&mut self) -> Option<(String, Span)> {
        match self.current().kind.clone() {
            TokenKind::TripleString(value) => {
                let span = self.current_span();
                self.advance();
                Some((value, span))
            }
            _ => {
                self.error(
                    "expected_triple_quoted_prompt",
                    "expected triple-quoted prompt",
                    self.current_span(),
                );
                None
            }
        }
    }
    fn number(&mut self) -> Option<(u64, Span)> {
        match self.current().kind {
            TokenKind::Number(ref value) => {
                let span = self.current_span();
                let Ok(value) = value.parse() else {
                    self.error(
                        "expected_positive_integer",
                        "expected positive integer",
                        span,
                    );
                    return None;
                };
                self.advance();
                Some((value, span))
            }
            _ => {
                self.error(
                    "expected_positive_integer",
                    "expected positive integer",
                    self.current_span(),
                );
                None
            }
        }
    }
    fn keyword(&mut self, expected: &str) -> bool {
        if matches!(&self.current().kind, TokenKind::Identifier(value) if value == expected) {
            self.advance();
            true
        } else {
            false
        }
    }
    fn expect_arrow(&mut self) {
        self.expect("->", |kind| matches!(kind, TokenKind::Arrow));
    }
    fn expect_lbrace(&mut self) {
        self.expect("{", |kind| matches!(kind, TokenKind::LBrace));
    }
    fn expect_rbrace(&mut self) {
        self.expect("}", |kind| matches!(kind, TokenKind::RBrace));
    }
    fn expect_lparen(&mut self) {
        self.expect("(", |kind| matches!(kind, TokenKind::LParen));
    }
    fn expect_rparen(&mut self) {
        self.expect(")", |kind| matches!(kind, TokenKind::RParen));
    }
    fn expect_lbracket(&mut self) {
        self.expect("[", |kind| matches!(kind, TokenKind::LBracket));
    }
    fn expect_rbracket(&mut self) {
        self.expect("]", |kind| matches!(kind, TokenKind::RBracket));
    }
    fn expect_colon(&mut self) {
        self.expect(":", |kind| matches!(kind, TokenKind::Colon));
    }
    fn expect_equals(&mut self) {
        self.expect("=", |kind| matches!(kind, TokenKind::Equals));
    }
    fn expect(&mut self, expected: &str, matches: impl FnOnce(&TokenKind) -> bool) {
        if matches(&self.current().kind) {
            self.advance();
        } else {
            self.error(
                "expected_token",
                &format!("expected {expected}"),
                self.current_span(),
            );
        }
    }
    fn optional_comma(&mut self) -> bool {
        if matches!(self.current().kind, TokenKind::Comma) {
            self.advance();
            true
        } else {
            false
        }
    }
    fn at_lbracket(&self) -> bool {
        matches!(self.current().kind, TokenKind::LBracket)
    }
    fn at_lbrace(&self) -> bool {
        matches!(self.current().kind, TokenKind::LBrace)
    }
    fn at_rbracket(&self) -> bool {
        matches!(self.current().kind, TokenKind::RBracket)
    }
    fn at_question(&self) -> bool {
        matches!(self.current().kind, TokenKind::Question)
    }
    fn at_rbrace(&self) -> bool {
        matches!(self.current().kind, TokenKind::RBrace)
    }
    fn at_rparen(&self) -> bool {
        matches!(self.current().kind, TokenKind::RParen)
    }
    fn at_eof(&self) -> bool {
        matches!(self.current().kind, TokenKind::Eof)
    }
    fn current(&self) -> &Token {
        &self.tokens[self.position.min(self.tokens.len() - 1)]
    }
    fn current_span(&self) -> Span {
        self.current().span
    }
    fn previous_span(&self) -> Span {
        self.tokens[self.position.saturating_sub(1)].span
    }
    fn advance(&mut self) {
        if !self.at_eof() {
            self.position += 1;
        }
    }
    fn error(&mut self, code: &'static str, message: &str, span: Span) {
        self.had_error = true;
        if self.diagnostics.len() < self.limits.max_diagnostics {
            self.diagnostics
                .push(Diagnostic::error(code, message, span));
        }
    }
    fn warning(&mut self, code: &'static str, message: &str, span: Span) {
        if self.diagnostics.len() < self.limits.max_diagnostics {
            self.diagnostics
                .push(Diagnostic::warning(code, message, span));
        }
    }
}

fn valid_event_type(value: &str) -> bool {
    let mut components = value.split('.').collect::<Vec<_>>();
    let Some(version) = components.pop() else {
        return false;
    };
    if components.len() < 2
        || !version.starts_with('v')
        || version[1..].is_empty()
        || version[1..].starts_with('0')
        || !version[1..].bytes().all(|byte| byte.is_ascii_digit())
    {
        return false;
    }
    components.into_iter().all(valid_event_component)
}

fn valid_goal_criterion_id(value: &str) -> bool {
    let mut bytes = value.bytes();
    value.len() <= 64
        && matches!(bytes.next(), Some(b'a'..=b'z'))
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn valid_event_component(component: &str) -> bool {
    let mut bytes = component.bytes();
    matches!(bytes.next(), Some(byte) if byte.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}
