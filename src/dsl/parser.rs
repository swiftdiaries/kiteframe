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

pub fn parse(source: &str, limits: ParseLimits) -> Result<SourceFile, Vec<Diagnostic>> {
    if source.len() > limits.max_source_bytes {
        return Err(if limits.max_diagnostics == 0 {
            Vec::new()
        } else {
            vec![Diagnostic::new(
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
        value.ok_or_else(Vec::new)
    }
}

struct Parser {
    tokens: Vec<Token>,
    position: usize,
    diagnostics: Vec<Diagnostic>,
    limits: ParseLimits,
    had_error: bool,
}

impl Parser {
    fn source_file(&mut self, source_len: usize) -> Option<SourceFile> {
        let mut records = Vec::new();
        let mut workflow = None;
        let mut saw_workflow = false;
        let mut tasks = Vec::new();
        while !self.at_eof() {
            if self.keyword("type") {
                if let Some(record) = self.record() {
                    records.push(record);
                }
            } else if self.keyword("workflow") {
                let declaration_span = self.previous_span();
                let item = self.workflow();
                if saw_workflow {
                    self.error(
                        "a source file contains exactly one workflow",
                        declaration_span,
                    );
                } else {
                    workflow = item;
                    saw_workflow = true;
                }
            } else if self.keyword("task") {
                if let Some(task) = self.task() {
                    tasks.push(task);
                }
            } else {
                self.error(
                    "expected type, workflow, or task declaration",
                    self.current_span(),
                );
                self.advance();
            }
        }
        let Some(workflow) = workflow else {
            if !saw_workflow {
                self.error(
                    "a source file contains exactly one workflow",
                    Span {
                        start: 0,
                        end: source_len,
                    },
                );
            }
            return None;
        };
        if tasks.is_empty() {
            self.error("a source file requires at least one task", workflow.span);
        }
        Some(SourceFile {
            span: Span {
                start: 0,
                end: source_len,
            },
            records,
            workflow,
            tasks,
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
                        "workflow has duplicate start declaration",
                        self.previous_span(),
                    );
                }
                entry = self.call(None);
            } else if self.keyword("limits") {
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
                    ) {
                        self.error("unsupported workflow limit", span);
                    }
                    if value == 0 {
                        self.error("workflow limit must be positive", span);
                    }
                    if limits.insert(key, value).is_some() {
                        self.error("workflow limit is duplicated", span);
                    }
                    self.optional_comma();
                }
                self.expect_rbrace();
            } else {
                self.error(
                    "workflow body accepts only start and limits",
                    self.current_span(),
                );
                self.advance();
            }
        }
        self.expect_rbrace();
        let Some(entry) = entry else {
            self.error(
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
            span: Span {
                start,
                end: self.previous_span().end,
            },
        })
    }

    fn task(&mut self) -> Option<TaskAst> {
        let start = self.previous_span().start;
        let name = self.identifier()?;
        let inputs = self.fields_in_parens()?;
        self.expect_arrow();
        let result = self.ty(0)?;
        self.expect_lbrace();
        let mut prompt = None;
        let mut tools = None;
        let mut outcomes = Vec::new();
        while !self.at_rbrace() && !self.at_eof() {
            if self.keyword("prompt") {
                let token = self.take_prompt()?;
                if prompt
                    .replace(PromptAst {
                        text: token.0,
                        span: token.1,
                    })
                    .is_some()
                {
                    self.error("task has duplicate prompt", token.1);
                }
            } else if self.keyword("tools") {
                let list = self.tool_list()?;
                if tools.replace(list).is_some() {
                    self.error("task has duplicate tools declaration", self.previous_span());
                }
            } else if self.keyword("on") {
                if let Some(outcome) = self.outcome() {
                    outcomes.push(outcome);
                }
            } else {
                self.error(
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
                self.error("task requires a prompt", span);
                PromptAst {
                    text: String::new(),
                    span,
                }
            }
        };
        let tools = match tools {
            Some(value) => value,
            None => {
                self.error("task requires a tools declaration", span);
                Vec::new()
            }
        };
        if outcomes.is_empty() {
            self.error("task requires at least one outcome", span);
        }
        Some(TaskAst {
            name,
            inputs,
            result,
            prompt,
            tools,
            outcomes,
            span,
        })
    }

    fn outcome(&mut self) -> Option<OutcomeAst> {
        let start = self.previous_span().start;
        let name = self.identifier()?;
        let fields = self.fields_in_parens()?;
        self.expect_lbrace();
        let mut statements = Vec::new();
        let mut terminal = false;
        while !self.at_rbrace() && !self.at_eof() {
            if terminal {
                self.error(
                    "statements are not allowed after a terminal control operation",
                    self.current_span(),
                );
                self.advance();
                continue;
            }
            let Some((statement, is_terminal)) = self.statement() else {
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
            self.error("outcome must not be empty", span);
        }
        if !terminal {
            self.error("outcome must end in goto, done, or fail", span);
        }
        Some(OutcomeAst {
            name,
            fields,
            statements,
            span,
        })
    }

    fn statement(&mut self) -> Option<(StatementAst, bool)> {
        if self.keyword("parallel") {
            return self
                .parallel()
                .map(|value| (StatementAst::Parallel(value), false));
        }
        if self.keyword("goto") {
            return self
                .call(None)
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
        let binding = self.identifier()?;
        self.expect_equals();
        if self.keyword("call") {
            self.call(Some(binding))
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
            self.error("binding accepts only call or ask", self.current_span());
            None
        }
    }

    fn parallel(&mut self) -> Option<Vec<CallAst>> {
        self.expect_lbrace();
        let mut calls = Vec::new();
        while !self.at_rbrace() && !self.at_eof() {
            let binding = self.identifier()?;
            self.expect_equals();
            if !self.keyword("call") {
                self.error(
                    "parallel contains only call statements",
                    self.current_span(),
                );
                return None;
            }
            calls.push(self.call(Some(binding))?);
        }
        self.expect_rbrace();
        if calls.is_empty() {
            self.error("parallel block must not be empty", self.previous_span());
        }
        Some(calls)
    }

    fn call(&mut self, binding: Option<String>) -> Option<CallAst> {
        let start = self.current_span().start;
        let task = self.identifier()?;
        self.expect_lparen();
        let mut arguments = Vec::new();
        while !self.at_rparen() && !self.at_eof() {
            let arg_start = self.current_span().start;
            let name = self.identifier()?;
            self.expect_colon();
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
                self.error("expected comma or closing parenthesis", self.current_span());
                return None;
            }
        }
        self.expect_rparen();
        Some(CallAst {
            binding,
            task,
            arguments,
            span: Span {
                start,
                end: self.previous_span().end,
            },
        })
    }

    fn tool_list(&mut self) -> Option<Vec<String>> {
        self.expect_lbracket();
        let mut tools = Vec::new();
        while !self.at_rbracket() && !self.at_eof() {
            tools.push(self.identifier()?);
            if !self.optional_comma() && !self.at_rbracket() {
                self.error("expected comma or closing bracket", self.current_span());
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
                self.error("expected comma or closing parenthesis", self.current_span());
                return None;
            }
        }
        self.expect_rparen();
        Some(fields)
    }

    fn ty(&mut self, depth: usize) -> Option<TypeAst> {
        if depth >= self.limits.max_nesting {
            self.error("type nesting exceeds configured limit", self.current_span());
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
                self.error("expected a binding or literal value", self.current_span());
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
                self.error("expected identifier", self.current_span());
                None
            }
        }
    }
    fn take_prompt(&mut self) -> Option<(String, Span)> {
        match self.current().kind.clone() {
            TokenKind::TripleString(value) => {
                let span = self.current_span();
                self.advance();
                Some((value, span))
            }
            _ => {
                self.error("expected triple-quoted prompt", self.current_span());
                None
            }
        }
    }
    fn number(&mut self) -> Option<(u64, Span)> {
        match self.current().kind {
            TokenKind::Number(ref value) => {
                let span = self.current_span();
                let Ok(value) = value.parse() else {
                    self.error("expected positive integer", span);
                    return None;
                };
                self.advance();
                Some((value, span))
            }
            _ => {
                self.error("expected positive integer", self.current_span());
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
            self.error(&format!("expected {expected}"), self.current_span());
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
    fn error(&mut self, message: &str, span: Span) {
        self.had_error = true;
        if self.diagnostics.len() < self.limits.max_diagnostics {
            self.diagnostics.push(Diagnostic::new(message, span));
        }
    }
}
