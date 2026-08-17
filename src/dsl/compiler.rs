use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::{ToolCatalogSnapshot, canonical_sha256};

use super::{
    ArgumentAst, CallAst, CompiledCall, CompiledCallBinding, CompiledDefinition, CompiledOutcome,
    CompiledStatement, CompiledTask, CompiledToolBinding, CompiledType, CompiledValue,
    CompletionContract, ContextMode, ContextModeAst, DefinitionAst, DefinitionKind, Diagnostic,
    FieldAst, Limits, ModelRole, ModelRoleAst, SourceFile, Span, StatementAst, TaskAst, TypeAst,
    ValueRefAst, parse_report,
};

const RESERVED_NAMES: &[&str] = &["complete_task", "outcome"];

#[derive(Clone, Debug)]
pub struct Compilation {
    pub definition: CompiledDefinition,
    pub diagnostics: Vec<Diagnostic>,
}

/// Parses and compiles one author-authored definition against a principal-visible catalog.
pub fn compile(
    source: &str,
    catalog: &ToolCatalogSnapshot,
    ceilings: &Limits,
) -> Result<CompiledDefinition, Vec<Diagnostic>> {
    compile_report(source, catalog, ceilings).map(|value| value.definition)
}

pub fn compile_report(
    source: &str,
    catalog: &ToolCatalogSnapshot,
    ceilings: &Limits,
) -> Result<Compilation, Vec<Diagnostic>> {
    let parsed = parse_report(
        source,
        super::ParseLimits {
            max_source_bytes: 256 * 1024,
            max_tokens: 32 * 1024,
            max_identifier_bytes: 256,
            max_nesting: 64,
            max_diagnostics: 64,
        },
    )?;
    let definition = Compiler::new(source, &parsed.source, catalog, ceilings).compile()?;
    Ok(Compilation {
        definition,
        diagnostics: parsed.diagnostics,
    })
}

struct DefinitionView<'a> {
    kind: DefinitionKind,
    dsl_version: u8,
    name: &'a str,
    inputs: &'a [FieldAst],
    result: &'a TypeAst,
    entry: &'a CallAst,
    limits: &'a BTreeMap<String, u64>,
    events: Vec<&'a str>,
    tasks: &'a [TaskAst],
    span: Span,
}

struct Compiler<'a> {
    source: &'a str,
    ast: &'a SourceFile,
    view: DefinitionView<'a>,
    catalog: &'a ToolCatalogSnapshot,
    ceilings: &'a Limits,
    diagnostics: Vec<Diagnostic>,
    records: BTreeMap<String, BTreeMap<String, CompiledType>>,
    task_asts: BTreeMap<String, &'a super::TaskAst>,
    task_inputs: BTreeMap<String, BTreeMap<String, CompiledType>>,
    task_results: BTreeMap<String, CompiledType>,
    call_edges: BTreeMap<String, BTreeSet<String>>,
    reachability_edges: BTreeMap<String, BTreeSet<String>>,
    all_tool_bindings: BTreeMap<String, CompiledToolBinding>,
}

impl<'a> Compiler<'a> {
    fn new(
        source: &'a str,
        ast: &'a SourceFile,
        catalog: &'a ToolCatalogSnapshot,
        ceilings: &'a Limits,
    ) -> Self {
        let view = match &ast.definition {
            DefinitionAst::Workflow(workflow) => DefinitionView {
                kind: DefinitionKind::Workflow,
                dsl_version: 1,
                name: &workflow.name,
                inputs: &workflow.inputs,
                result: &workflow.result,
                entry: &workflow.entry,
                limits: &workflow.limits,
                events: Vec::new(),
                tasks: &workflow.tasks,
                span: workflow.span,
            },
            DefinitionAst::Agent(agent) => DefinitionView {
                kind: DefinitionKind::Agent,
                dsl_version: 2,
                name: &agent.name,
                inputs: &agent.inputs,
                result: &agent.result,
                entry: &agent.entry,
                limits: &agent.limits,
                events: agent
                    .events
                    .iter()
                    .map(|event| event.event_type.as_str())
                    .collect(),
                tasks: &agent.tasks,
                span: agent.span,
            },
        };
        Self {
            source,
            ast,
            view,
            catalog,
            ceilings,
            diagnostics: Vec::new(),
            records: BTreeMap::new(),
            task_asts: BTreeMap::new(),
            task_inputs: BTreeMap::new(),
            task_results: BTreeMap::new(),
            call_edges: BTreeMap::new(),
            reachability_edges: BTreeMap::new(),
            all_tool_bindings: BTreeMap::new(),
        }
    }

    fn compile(mut self) -> Result<CompiledDefinition, Vec<Diagnostic>> {
        self.collect_records();
        self.collect_tasks();
        let inputs = self.compile_fields(self.view.inputs, "definition input");
        let result = self.compile_type(self.view.result, self.view.span);
        self.check_limits();

        let entry = self.compile_call(self.view.entry, &inputs, None, self.view.span);
        if let Some(entry_task) = self.task_results.get(&self.view.entry.task)
            && *entry_task != result
        {
            self.error(
                "definition entry task result must match definition result",
                self.view.entry.span,
            );
        }

        let task_names = self.task_asts.keys().cloned().collect::<Vec<_>>();
        let mut tasks = BTreeMap::new();
        for name in task_names {
            if let Some(task) = self.compile_task(&name) {
                tasks.insert(name, task);
            }
        }

        self.reject_recursive_calls();
        self.reject_unreachable_tasks(&self.view.entry.task);

        if !self.diagnostics.is_empty() {
            return Err(self.diagnostics);
        }

        let tool_bindings = self.all_tool_bindings.into_values().collect::<Vec<_>>();
        let definition_hash = definition_hash(self.source, self.view.dsl_version, &tool_bindings)
            .map_err(|error| vec![Diagnostic::new(error, self.ast.span)])?;
        Ok(CompiledDefinition {
            dsl_version: self.view.dsl_version,
            definition_kind: self.view.kind,
            name: self.view.name.to_owned(),
            source: self.source.to_owned(),
            definition_hash,
            inputs,
            result,
            entry,
            limits: self.view.limits.clone(),
            events: self
                .view
                .events
                .iter()
                .map(|event| (*event).to_owned())
                .collect(),
            records: self.records,
            tasks,
            tool_bindings,
        })
    }

    fn collect_records(&mut self) {
        for record in &self.ast.records {
            self.reject_reserved(&record.name, record.span);
            if self.records.contains_key(&record.name) {
                self.error("record declaration is duplicated", record.span);
                continue;
            }
            self.records.insert(record.name.clone(), BTreeMap::new());
        }
        for record in &self.ast.records {
            if !self.records.contains_key(&record.name) {
                continue;
            }
            let fields = self.compile_fields(&record.fields, "record field");
            self.records.insert(record.name.clone(), fields);
        }
    }

    fn collect_tasks(&mut self) {
        for task in self.view.tasks {
            self.reject_reserved(&task.name, task.span);
            if self.task_asts.insert(task.name.clone(), task).is_some() {
                self.error("task declaration is duplicated", task.span);
            }
        }
        let tasks = self.task_asts.clone();
        for (name, task) in tasks {
            let inputs = self.compile_fields(&task.inputs, "task input");
            let result = self.compile_type(&task.result, task.span);
            self.task_inputs.insert(name.clone(), inputs);
            self.task_results.insert(name, result);
        }
    }

    fn compile_task(&mut self, name: &str) -> Option<CompiledTask> {
        let task = self.task_asts.get(name).copied()?;
        let inputs = self.task_inputs.get(name).cloned().unwrap_or_default();
        let result = self
            .task_results
            .get(name)
            .cloned()
            .unwrap_or(CompiledType::String);
        let tools = self.compile_tools(task);
        let mut outcomes = BTreeMap::new();
        let mut completion_outcomes = BTreeMap::new();
        let mut union_fields = BTreeMap::new();

        for outcome in &task.outcomes {
            self.reject_reserved(&outcome.name, outcome.span);
            if outcomes.contains_key(&outcome.name) {
                self.error("outcome handler is duplicated", outcome.span);
                continue;
            }
            let fields = self.compile_fields(&outcome.fields, "outcome field");
            for (field, ty) in &fields {
                if let Some(existing) = union_fields.get(field)
                    && existing != &nullable(ty.clone())
                {
                    self.error(
                        "outcome fields with the same name must have compatible types",
                        outcome.span,
                    );
                } else {
                    union_fields.insert(field.clone(), nullable(ty.clone()));
                }
            }
            let mut scope = inputs.clone();
            for (field, ty) in &fields {
                self.insert_binding(&mut scope, field, ty.clone(), outcome.span);
            }
            let statements =
                self.compile_statements(name, &result, &mut scope, &outcome.statements);
            completion_outcomes.insert(outcome.name.clone(), fields.clone());
            outcomes.insert(outcome.name.clone(), CompiledOutcome { fields, statements });
        }

        Some(CompiledTask {
            inputs,
            result,
            model_role: task.model_role.map(|role| match role {
                ModelRoleAst::Reasoning => ModelRole::Reasoning,
                ModelRoleAst::Coding => ModelRole::Coding,
            }),
            prompt: task.prompt.text.clone(),
            tools,
            outcomes,
            completion: CompletionContract {
                outcomes: completion_outcomes,
                required_nullable_fields: union_fields,
            },
        })
    }

    fn compile_tools(&mut self, task: &super::TaskAst) -> Vec<CompiledToolBinding> {
        let mut seen = BTreeSet::new();
        let mut tools = Vec::new();
        for declared in &task.tools {
            self.reject_reserved(declared, task.span);
            if !seen.insert(declared.clone()) {
                self.error("task tool declaration is duplicated", task.span);
                continue;
            }
            let mut matches = self
                .catalog
                .tools()
                .iter()
                .filter(|(qualified, _)| {
                    *qualified == declared
                        || qualified.replace('/', "_") == *declared
                        || qualified
                            .rsplit_once('/')
                            .is_some_and(|(_, short)| short == declared)
                })
                .collect::<Vec<_>>();
            matches.sort_by(|left, right| left.0.cmp(right.0));
            match matches.as_slice() {
                [] => self.error(
                    "declared tool is not in the visible tool catalog",
                    task.span,
                ),
                [(qualified_name, descriptor)] => {
                    if let Err(error) = self
                        .catalog
                        .resolve(qualified_name, descriptor.descriptor_digest())
                    {
                        self.error(
                            format!("visible tool contract is invalid: {error}"),
                            task.span,
                        );
                        continue;
                    }
                    let binding = CompiledToolBinding {
                        qualified_name: (*qualified_name).clone(),
                        catalog_epoch_id: self.catalog.epoch_id().clone(),
                        descriptor_digest: descriptor.descriptor_digest().to_owned(),
                        descriptor: (*descriptor).clone(),
                    };
                    self.all_tool_bindings
                        .entry(binding.qualified_name.clone())
                        .or_insert_with(|| binding.clone());
                    tools.push(binding);
                }
                _ => self.error(
                    "declared tool name is ambiguous in the visible tool catalog",
                    task.span,
                ),
            }
        }
        tools.sort_by(|left, right| left.qualified_name.cmp(&right.qualified_name));
        tools
    }

    fn compile_statements(
        &mut self,
        current_task: &str,
        current_result: &CompiledType,
        scope: &mut BTreeMap<String, CompiledType>,
        statements: &[StatementAst],
    ) -> Vec<CompiledStatement> {
        let mut compiled = Vec::new();
        for statement in statements {
            match statement {
                StatementAst::Call(call) => {
                    let value = self.compile_call(call, scope, Some(current_task), self.ast.span);
                    self.register_call_edge(current_task, &call.task);
                    let binding = call.binding.clone().unwrap_or_default();
                    self.reject_reserved(&binding, call.span);
                    if let Some(result) = self.task_results.get(&call.task).cloned() {
                        self.insert_binding(scope, &binding, result, call.span);
                    }
                    compiled.push(CompiledStatement::Call {
                        binding,
                        call: value,
                    });
                }
                StatementAst::Parallel(calls) => {
                    if calls.is_empty() {
                        self.error("parallel block must not be empty", self.ast.span);
                    }
                    if calls.len() as u64 > self.effective_limit("parallel_branches") {
                        self.error(
                            "parallel branch count exceeds definition limit",
                            self.ast.span,
                        );
                    }
                    let before_parallel = scope.clone();
                    let mut additions = BTreeMap::new();
                    let mut values = Vec::new();
                    for call in calls {
                        let value = self.compile_call(
                            call,
                            &before_parallel,
                            Some(current_task),
                            self.ast.span,
                        );
                        self.register_call_edge(current_task, &call.task);
                        let binding = call.binding.clone().unwrap_or_default();
                        self.reject_reserved(&binding, call.span);
                        if let Some(result) = self.task_results.get(&call.task).cloned() {
                            if additions.contains_key(&binding) || scope.contains_key(&binding) {
                                self.error("parallel result binding is duplicated", call.span);
                            } else {
                                additions.insert(binding.clone(), result);
                            }
                        }
                        values.push(CompiledCallBinding {
                            binding,
                            call: value,
                        });
                    }
                    scope.extend(additions);
                    compiled.push(CompiledStatement::Parallel { calls: values });
                }
                StatementAst::Ask {
                    binding,
                    answer_type,
                    question,
                } => {
                    self.reject_reserved(binding, self.ast.span);
                    let ty = self.compile_type(answer_type, self.ast.span);
                    self.expect_value_type(question, scope, &CompiledType::String, self.ast.span);
                    self.insert_binding(scope, binding, ty.clone(), self.ast.span);
                    compiled.push(CompiledStatement::Ask {
                        binding: binding.clone(),
                        answer_type: ty,
                        question: self.compiled_value(question),
                    });
                }
                StatementAst::Goto(call) => {
                    let value = self.compile_call(call, scope, Some(current_task), self.ast.span);
                    if let Some(target_result) = self.task_results.get(&call.task)
                        && target_result != current_result
                    {
                        self.error(
                            "goto target result must match the current task result",
                            call.span,
                        );
                    }
                    compiled.push(CompiledStatement::Goto { call: value });
                }
                StatementAst::Done(value) => {
                    self.expect_value_type(value, scope, current_result, self.ast.span);
                    compiled.push(CompiledStatement::Done {
                        value: self.compiled_value(value),
                    });
                }
                StatementAst::Fail { code, message } => {
                    self.expect_value_type(code, scope, &CompiledType::String, self.ast.span);
                    self.expect_value_type(message, scope, &CompiledType::String, self.ast.span);
                    compiled.push(CompiledStatement::Fail {
                        code: self.compiled_value(code),
                        message: self.compiled_value(message),
                    });
                }
            }
        }
        compiled
    }

    fn compile_call(
        &mut self,
        call: &CallAst,
        scope: &BTreeMap<String, CompiledType>,
        caller: Option<&str>,
        fallback_span: Span,
    ) -> CompiledCall {
        if let Some(caller) = caller {
            self.reachability_edges
                .entry(caller.to_owned())
                .or_default()
                .insert(call.task.clone());
        }
        let Some(expected) = self.task_inputs.get(&call.task).cloned() else {
            self.error("call target task does not exist", call.span);
            return CompiledCall {
                task: call.task.clone(),
                arguments: BTreeMap::new(),
                context: compile_context(call.context),
            };
        };
        let mut supplied = BTreeMap::new();
        for ArgumentAst { name, value, span } in &call.arguments {
            if supplied.insert(name.clone(), value).is_some() {
                self.error("call argument is duplicated", *span);
            }
        }
        for (name, ty) in &expected {
            match supplied.get(name) {
                Some(value) => self.expect_value_type(value, scope, ty, call.span),
                None if matches!(ty, CompiledType::Optional(_)) => {}
                None => self.error("call is missing a required argument", call.span),
            }
        }
        for name in supplied.keys() {
            if !expected.contains_key(name) {
                self.error("call contains an unknown argument", call.span);
            }
        }
        let arguments = expected
            .iter()
            .map(|(name, ty)| {
                let value = supplied
                    .get(name)
                    .map_or(CompiledValue::Null, |value| self.compiled_value(value));
                if !matches!(ty, CompiledType::Optional(_)) && matches!(value, CompiledValue::Null)
                {
                    self.error("call is missing a required argument", fallback_span);
                }
                (name.clone(), value)
            })
            .collect();
        CompiledCall {
            task: call.task.clone(),
            arguments,
            context: compile_context(call.context),
        }
    }

    fn compile_fields(
        &mut self,
        fields: &[FieldAst],
        kind: &str,
    ) -> BTreeMap<String, CompiledType> {
        let mut compiled = BTreeMap::new();
        for field in fields {
            self.reject_reserved(&field.name, field.span);
            let ty = self.compile_type(&field.ty, field.span);
            if compiled.insert(field.name.clone(), ty).is_some() {
                self.error(format!("{kind} is duplicated"), field.span);
            }
        }
        compiled
    }

    fn compile_type(&mut self, ty: &TypeAst, span: Span) -> CompiledType {
        match ty {
            TypeAst::String => CompiledType::String,
            TypeAst::Integer => CompiledType::Integer,
            TypeAst::Number => CompiledType::Number,
            TypeAst::Boolean => CompiledType::Boolean,
            TypeAst::Named(name) => {
                if !self.records.contains_key(name) {
                    self.error("type references an unknown closed record", span);
                }
                CompiledType::Record(name.clone())
            }
            TypeAst::Optional(inner) => {
                CompiledType::Optional(Box::new(self.compile_type(inner, span)))
            }
            TypeAst::List(inner) => CompiledType::List(Box::new(self.compile_type(inner, span))),
        }
    }

    fn expect_value_type(
        &mut self,
        value: &ValueRefAst,
        scope: &BTreeMap<String, CompiledType>,
        expected: &CompiledType,
        span: Span,
    ) {
        let actual = match value {
            ValueRefAst::Binding(name) => match scope.get(name) {
                Some(ty) => ty.clone(),
                None => {
                    self.error("value references an unbound name", span);
                    return;
                }
            },
            ValueRefAst::String(_) => CompiledType::String,
            ValueRefAst::Number(value) => {
                if value.contains('.') {
                    CompiledType::Number
                } else {
                    CompiledType::Integer
                }
            }
            ValueRefAst::Boolean(_) => CompiledType::Boolean,
            ValueRefAst::Null => CompiledType::Optional(Box::new(CompiledType::String)),
        };
        let optional_inner_is_valid = match expected {
            CompiledType::Optional(inner) => {
                matches!(value, ValueRefAst::Null) || actual == **inner || actual == *expected
            }
            _ => false,
        };
        if !optional_inner_is_valid && actual != *expected {
            self.error("value type does not match its declared type", span);
        }
    }

    fn compiled_value(&self, value: &ValueRefAst) -> CompiledValue {
        match value {
            ValueRefAst::Binding(value) => CompiledValue::Binding(value.clone()),
            ValueRefAst::String(value) => CompiledValue::String(value.clone()),
            ValueRefAst::Number(value) => CompiledValue::Number(value.clone()),
            ValueRefAst::Boolean(value) => CompiledValue::Boolean(*value),
            ValueRefAst::Null => CompiledValue::Null,
        }
    }

    fn insert_binding(
        &mut self,
        scope: &mut BTreeMap<String, CompiledType>,
        name: &str,
        ty: CompiledType,
        span: Span,
    ) {
        if scope.insert(name.to_owned(), ty).is_some() {
            self.error("binding name is duplicated", span);
        }
    }

    fn register_call_edge(&mut self, caller: &str, target: &str) {
        self.call_edges
            .entry(caller.to_owned())
            .or_default()
            .insert(target.to_owned());
    }

    fn check_limits(&mut self) {
        for (name, value) in self.view.limits {
            if *value > self.ceiling(name) {
                self.error(
                    "definition limit exceeds deployment ceiling",
                    self.view.span,
                );
            }
        }
        let compiled_limits = self.compiled_limits();
        if self.view.kind == DefinitionKind::Agent
            && compiled_limits.elapsed_seconds <= compiled_limits.active_execution_seconds
        {
            self.diagnostics.push(Diagnostic::error(
                "invalid_elapsed_limit",
                "elapsed_seconds must exceed active_execution_seconds",
                self.view.span,
            ));
        }
    }

    fn ceiling(&self, name: &str) -> u64 {
        match name {
            "steps" => self.ceilings.steps,
            "model_turns_per_task" => self.ceilings.model_turns_per_task,
            "tool_calls_per_response" => self.ceilings.tool_calls_per_response,
            "parallel_branches" => self.ceilings.parallel_branches,
            "response_item_bytes" => self.ceilings.response_item_bytes,
            "active_execution_seconds" => self.ceilings.active_execution_seconds,
            "elapsed_seconds" => self.ceilings.elapsed_seconds,
            _ => 0,
        }
    }

    fn effective_limit(&self, name: &str) -> u64 {
        self.view
            .limits
            .get(name)
            .copied()
            .unwrap_or_else(|| self.ceiling(name))
    }

    fn compiled_limits(&self) -> Limits {
        Limits {
            steps: self.effective_limit("steps"),
            model_turns_per_task: self.effective_limit("model_turns_per_task"),
            tool_calls_per_response: self.effective_limit("tool_calls_per_response"),
            parallel_branches: self.effective_limit("parallel_branches"),
            response_item_bytes: self.effective_limit("response_item_bytes"),
            active_execution_seconds: self.effective_limit("active_execution_seconds"),
            elapsed_seconds: self.effective_limit("elapsed_seconds"),
        }
    }

    fn reject_recursive_calls(&mut self) {
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        let tasks = self.task_asts.keys().cloned().collect::<Vec<_>>();
        for task in tasks {
            self.visit_call_edges(&task, &mut visiting, &mut visited);
        }
    }

    fn visit_call_edges(
        &mut self,
        task: &str,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
    ) {
        if visited.contains(task) {
            return;
        }
        if !visiting.insert(task.to_owned()) {
            self.error("recursive call chain is not allowed", self.ast.span);
            return;
        }
        let children = self.call_edges.get(task).cloned().unwrap_or_default();
        for child in children {
            self.visit_call_edges(&child, visiting, visited);
        }
        visiting.remove(task);
        visited.insert(task.to_owned());
    }

    fn reject_unreachable_tasks(&mut self, entry: &str) {
        let mut reachable = BTreeSet::new();
        let mut pending = vec![entry.to_owned()];
        while let Some(task) = pending.pop() {
            if !reachable.insert(task.clone()) {
                continue;
            }
            if let Some(children) = self.reachability_edges.get(&task) {
                pending.extend(children.iter().cloned());
            }
        }
        let all_tasks = self.task_asts.keys().cloned().collect::<Vec<_>>();
        for task in all_tasks {
            if !reachable.contains(&task) {
                self.error("task is unreachable from definition entry", self.ast.span);
            }
        }
    }

    fn reject_reserved(&mut self, name: &str, span: Span) {
        if RESERVED_NAMES.contains(&name) {
            self.error("reserved name is not allowed in definition source", span);
        }
    }

    fn error(&mut self, message: impl Into<String>, span: Span) {
        self.diagnostics.push(Diagnostic::new(message, span));
    }
}

fn nullable(ty: CompiledType) -> CompiledType {
    match ty {
        CompiledType::Optional(_) => ty,
        value => CompiledType::Optional(Box::new(value)),
    }
}

fn compile_context(context: ContextModeAst) -> ContextMode {
    match context {
        ContextModeAst::Legacy => ContextMode::Legacy,
        ContextModeAst::Isolated => ContextMode::Isolated,
        ContextModeAst::Kept => ContextMode::Kept,
    }
}

fn definition_hash(
    source: &str,
    dsl_version: u8,
    tool_bindings: &[CompiledToolBinding],
) -> Result<String, String> {
    #[derive(Serialize)]
    struct Identity<'a> {
        dsl_version: u8,
        source: &'a str,
        descriptors: BTreeMap<&'a str, serde_json::Value>,
    }

    let descriptors = tool_bindings
        .iter()
        .map(|binding| {
            (
                binding.qualified_name.as_str(),
                binding.descriptor.normalized_descriptor(),
            )
        })
        .collect();
    canonical_sha256(&Identity {
        dsl_version,
        source,
        descriptors,
    })
    .map_err(|error| error.to_string())
}
