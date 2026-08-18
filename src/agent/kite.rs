use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::dsl::{
    CompiledCall, CompiledDefinition, CompiledStatement, CompiledTask, CompiledToolBinding,
    CompiledType, CompiledValue, CompletionContract, ContextMode, DefinitionKind,
};
use crate::{
    AcceptedUserTask, ClosedSchema, CriterionAssertion, EvidenceId, GoalContract, PrimitiveValue,
    TaskTarget, canonical_json,
};

use super::{
    AgentRuntime, AgentRuntimeError, BlockGoal, CompleteGoal, FailTask, GoalProjection,
    HarnessStructuralLimits, HarnessTool, InvokeHarness, KernelCommand, KernelCommandPayload,
    RequestApproval, RuntimeCeilings, RuntimeCommandKey, RuntimeDisposition, RuntimeEvent,
    RuntimeHandle, RuntimeHistoryItem, RuntimeHistoryKind, RuntimeStart, RuntimeStateSnapshot,
    RuntimeTransition,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct KiteFrameId(u32);

impl KiteFrameId {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state", deny_unknown_fields)]
pub enum KiteFrameState {
    Ready,
    WaitingForHarness {
        command_key: RuntimeCommandKey,
    },
    WaitingForApproval {
        command_key: RuntimeCommandKey,
        binding: String,
        answer_type: CompiledType,
    },
    WaitingForChildren {
        child_ids: Vec<KiteFrameId>,
    },
    Returned {
        result: Value,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KiteFrameSnapshot {
    pub id: KiteFrameId,
    pub parent_id: Option<KiteFrameId>,
    pub parent_binding: Option<String>,
    pub branch_index: u16,
    pub task_name: String,
    pub bindings: BTreeMap<String, Value>,
    pub outcome: Option<String>,
    pub statement_index: usize,
    pub history: Vec<RuntimeHistoryItem>,
    pub state: KiteFrameState,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KiteRuntimeState {
    pub definition_hash: String,
    pub root_frame_id: KiteFrameId,
    pub next_frame_id: u32,
    pub next_history_sequence: u64,
    pub steps_consumed: u64,
    pub frames: BTreeMap<KiteFrameId, KiteFrameSnapshot>,
}

pub struct KiteAgentRuntime {
    ceilings: RuntimeCeilings,
}

impl KiteAgentRuntime {
    pub const fn new(ceilings: RuntimeCeilings) -> Self {
        Self { ceilings }
    }

    pub fn decode_snapshot(
        &self,
        snapshot: &RuntimeStateSnapshot,
    ) -> Result<KiteRuntimeState, AgentRuntimeError> {
        snapshot.canonical_bytes(&self.ceilings)?;
        let state: KiteRuntimeState = serde_json::from_value(snapshot.payload.clone())
            .map_err(|error| AgentRuntimeError::InvalidState(error.to_string()))?;
        if state.definition_hash.is_empty()
            || state.root_frame_id != KiteFrameId(0)
            || state.next_frame_id == 0
            || state.frames.len() > self.ceilings.max_frames
            || !state.frames.contains_key(&state.root_frame_id)
            || state.frames.iter().any(|(id, frame)| {
                id != &frame.id
                    || id.0 >= state.next_frame_id
                    || frame.history.len() > self.ceilings.max_history_items_per_frame
                    || frame
                        .history
                        .windows(2)
                        .any(|items| items[0].sequence >= items[1].sequence)
                    || frame
                        .history
                        .iter()
                        .any(|item| item.sequence >= state.next_history_sequence)
            })
            || state.frames[&state.root_frame_id].parent_id.is_some()
        {
            return Err(AgentRuntimeError::InvalidState(
                "runtime snapshot counters are invalid".into(),
            ));
        }
        for frame in state.frames.values() {
            let invalid_wait = match &frame.state {
                KiteFrameState::WaitingForHarness { command_key }
                | KiteFrameState::WaitingForApproval { command_key, .. } => {
                    RuntimeCommandKey::new(command_key.as_str()).is_err()
                }
                _ => false,
            };
            let invalid_parent = match frame.parent_id {
                Some(parent_id) => {
                    frame.parent_binding.is_none()
                        || parent_id == frame.id
                        || !state.frames.contains_key(&parent_id)
                }
                None => frame.id != state.root_frame_id || frame.parent_binding.is_some(),
            };
            let invalid_children = match &frame.state {
                KiteFrameState::WaitingForChildren { child_ids } => {
                    let unique = child_ids.iter().copied().collect::<BTreeSet<_>>();
                    child_ids.is_empty()
                        || unique.len() != child_ids.len()
                        || child_ids.iter().any(|child_id| {
                            state
                                .frames
                                .get(child_id)
                                .is_none_or(|child| child.parent_id != Some(frame.id))
                        })
                }
                _ => false,
            };
            if invalid_wait || invalid_parent || invalid_children {
                return Err(AgentRuntimeError::InvalidState(
                    "runtime frame relationships are invalid".into(),
                ));
            }
        }
        Ok(state)
    }

    pub fn decode_snapshot_for_task_revision(
        &self,
        snapshot: &RuntimeStateSnapshot,
        current_task_revision: u64,
    ) -> Result<KiteRuntimeState, AgentRuntimeError> {
        let state = self.decode_snapshot(snapshot)?;
        validate_waiting_command_keys(&state, current_task_revision)?;
        Ok(state)
    }
}

impl AgentRuntime for KiteAgentRuntime {
    fn validate_agent(&self, definition: &CompiledDefinition) -> Result<(), AgentRuntimeError> {
        self.ceilings.validate()?;
        if definition.definition_kind != DefinitionKind::Agent || definition.dsl_version != 2 {
            return Err(AgentRuntimeError::InvalidDefinition(
                "built-in runtime requires a version-2 agent".into(),
            ));
        }
        if definition.entry.context == ContextMode::Legacy
            || !definition.tasks.contains_key(&definition.entry.task)
        {
            return Err(AgentRuntimeError::InvalidDefinition(
                "agent entry is invalid".into(),
            ));
        }
        validate_tools(&definition.tool_bindings)?;
        for task in definition.tasks.values() {
            if task.model_role.is_none() {
                return Err(AgentRuntimeError::InvalidDefinition(
                    "agent task has no model role".into(),
                ));
            }
            closed_object_schema(&task.inputs, definition)?
                .validate_definition()
                .map_err(|error| AgentRuntimeError::InvalidDefinition(error.to_string()))?;
            completion_result_schema(&task.completion, definition)?
                .validate_definition()
                .map_err(|error| AgentRuntimeError::InvalidDefinition(error.to_string()))?;
            validate_completion_contract(task)?;
            validate_tools(&task.tools)?;
            if task.tools.iter().any(|task_tool| {
                !definition
                    .tool_bindings
                    .iter()
                    .any(|frozen| same_tool_binding(task_tool, frozen))
            }) {
                return Err(AgentRuntimeError::InvalidDefinition(
                    "task tool does not match a frozen definition binding".into(),
                ));
            }
            for outcome in task.outcomes.values() {
                for statement in &outcome.statements {
                    validate_statement_context(statement)?;
                }
            }
        }
        Ok(())
    }

    fn start(&self, input: RuntimeStart<'_>) -> Result<RuntimeTransition, AgentRuntimeError> {
        self.validate_agent(input.definition)?;
        if input.execution.task.definition.definition_hash != input.definition.definition_hash {
            return Err(AgentRuntimeError::InvalidDefinition(
                "frozen definition hash does not match".into(),
            ));
        }
        let projected = accepted_input(&input.execution.task);
        closed_object_schema(&input.definition.inputs, input.definition)?
            .validate(&projected)
            .map_err(|error| AgentRuntimeError::InvalidValue(error.to_string()))?;
        let state = KiteRuntimeState {
            definition_hash: input.definition.definition_hash.clone(),
            root_frame_id: KiteFrameId(0),
            next_frame_id: 1,
            next_history_sequence: 0,
            steps_consumed: 0,
            frames: BTreeMap::from([(
                KiteFrameId(0),
                KiteFrameSnapshot {
                    id: KiteFrameId(0),
                    parent_id: None,
                    parent_binding: None,
                    branch_index: 0,
                    task_name: input.definition.entry.task.clone(),
                    bindings: resolve_call(&input.definition.entry, &projected, input.definition)?,
                    outcome: None,
                    statement_index: 0,
                    history: Vec::new(),
                    state: KiteFrameState::Ready,
                },
            )]),
        };
        let revisions = RuntimeRevisions {
            task: input.execution.task_revision,
            goal: input.execution.goal_revision,
        };
        let command = invoke_harness_for(
            KiteFrameId(0),
            &state,
            input.definition,
            &input.execution.task.goal,
            revisions,
        )?;
        let mut state = state;
        state.frames.get_mut(&KiteFrameId(0)).unwrap().state = KiteFrameState::WaitingForHarness {
            command_key: command.key.clone(),
        };
        transition(
            state,
            RuntimeDisposition::Running,
            vec![command],
            revisions,
            format!("start:{}:r0", input.execution.task.id.as_uuid()),
            &self.ceilings,
        )
    }

    fn handle(&self, input: RuntimeHandle<'_>) -> Result<RuntimeTransition, AgentRuntimeError> {
        self.validate_agent(input.definition)?;
        let snapshot =
            input.execution.runtime.as_ref().ok_or_else(|| {
                AgentRuntimeError::InvalidState("runtime snapshot is absent".into())
            })?;
        let mut state =
            self.decode_snapshot_for_task_revision(snapshot, input.execution.task_revision)?;
        if state.definition_hash != input.definition.definition_hash
            || input.execution.task.definition.definition_hash != input.definition.definition_hash
        {
            return Err(AgentRuntimeError::InvalidState(
                "runtime definition hash does not match".into(),
            ));
        }
        let revisions = RuntimeRevisions {
            task: input.execution.task_revision,
            goal: input.execution.goal_revision,
        };
        match input.event {
            RuntimeEvent::HarnessResult {
                command_key,
                outcome,
                fields,
                evidence_id,
                history,
                ..
            } => handle_harness_result(
                &mut state,
                input.definition,
                command_key,
                outcome,
                fields,
                evidence_id,
                history,
                &self.ceilings,
            )?,
            RuntimeEvent::ApprovalResult {
                command_key,
                answer,
                ..
            } => handle_approval_result(&mut state, input.definition, command_key, answer)?,
            RuntimeEvent::CommandFailed {
                command_key, code, ..
            } => {
                let frame_id = find_waiting_frame(&state, command_key, WaitingKind::Any)?;
                state.frames.get_mut(&frame_id).unwrap().state = KiteFrameState::Ready;
                return transition(
                    state,
                    RuntimeDisposition::Terminating,
                    vec![fail_task_command(
                        revisions,
                        code.clone(),
                        format!("runtime command `{}` failed", command_key.as_str()),
                    )?],
                    revisions,
                    format!("event:{}", input.event.event_id()),
                    &self.ceilings,
                );
            }
            _ => {
                return Err(AgentRuntimeError::InvalidTransition(
                    "runtime event does not match a waiting command".into(),
                ));
            }
        }
        reduce_to_transition(
            state,
            input.definition,
            &input.execution.task.goal,
            revisions,
            format!("event:{}", input.event.event_id()),
            &self.ceilings,
        )
    }
}

#[derive(Clone, Copy)]
struct RuntimeRevisions {
    task: u64,
    goal: u64,
}

#[derive(Clone, Copy)]
enum WaitingKind {
    Harness,
    Approval,
    Any,
}

fn validate_waiting_command_keys(
    state: &KiteRuntimeState,
    current_task_revision: u64,
) -> Result<(), AgentRuntimeError> {
    for frame in state.frames.values() {
        let waiting = match &frame.state {
            KiteFrameState::WaitingForHarness { command_key } => Some((command_key, "harness")),
            KiteFrameState::WaitingForApproval { command_key, .. } => {
                Some((command_key, "approval"))
            }
            _ => None,
        };
        let Some((command_key, expected_kind)) = waiting else {
            continue;
        };
        let Some((revision, suffix)) = command_key.as_str().strip_prefix('r').and_then(|value| {
            value.split_once("-f").and_then(|(revision, suffix)| {
                revision.parse::<u64>().ok().map(|value| (value, suffix))
            })
        }) else {
            return Err(AgentRuntimeError::InvalidState(
                "waiting command key is not canonical".into(),
            ));
        };
        let Some((encoded_frame, encoded_kind)) = suffix.split_once('-') else {
            return Err(AgentRuntimeError::InvalidState(
                "waiting command key is not canonical".into(),
            ));
        };
        let encoded_frame = encoded_frame.parse::<u32>().map_err(|_| {
            AgentRuntimeError::InvalidState("waiting command key is not canonical".into())
        })?;
        let canonical = format!("r{revision}-f{encoded_frame}-{encoded_kind}");
        if command_key.as_str() != canonical
            || revision == 0
            || revision > current_task_revision
            || encoded_frame != frame.id.0
            || encoded_kind != expected_kind
        {
            return Err(AgentRuntimeError::InvalidState(
                "waiting command key does not match its frame or task revision".into(),
            ));
        }
    }
    Ok(())
}

fn find_waiting_frame(
    state: &KiteRuntimeState,
    command_key: &RuntimeCommandKey,
    kind: WaitingKind,
) -> Result<KiteFrameId, AgentRuntimeError> {
    let matching = state
        .frames
        .iter()
        .filter_map(|(id, frame)| {
            let matches = match (&frame.state, kind) {
                (
                    KiteFrameState::WaitingForHarness {
                        command_key: waiting,
                    },
                    WaitingKind::Harness | WaitingKind::Any,
                ) => waiting == command_key,
                (
                    KiteFrameState::WaitingForApproval {
                        command_key: waiting,
                        ..
                    },
                    WaitingKind::Approval | WaitingKind::Any,
                ) => waiting == command_key,
                _ => false,
            };
            matches.then_some(*id)
        })
        .collect::<Vec<_>>();
    let [frame_id] = matching.as_slice() else {
        return Err(AgentRuntimeError::InvalidTransition(
            "event command key is not waiting exactly once".into(),
        ));
    };
    Ok(*frame_id)
}

#[allow(clippy::too_many_arguments)]
fn handle_harness_result(
    state: &mut KiteRuntimeState,
    definition: &CompiledDefinition,
    command_key: &RuntimeCommandKey,
    outcome: &str,
    fields: &Value,
    evidence_id: &EvidenceId,
    history: &[RuntimeHistoryItem],
    ceilings: &RuntimeCeilings,
) -> Result<(), AgentRuntimeError> {
    let frame_id = find_waiting_frame(state, command_key, WaitingKind::Harness)?;
    let frame = state
        .frames
        .get(&frame_id)
        .ok_or_else(|| AgentRuntimeError::InvalidState("waiting runtime frame is absent".into()))?;
    let task = definition
        .tasks
        .get(&frame.task_name)
        .ok_or_else(|| AgentRuntimeError::InvalidState("snapshot task is absent".into()))?;
    let fields_schema =
        task.completion.outcomes.get(outcome).ok_or_else(|| {
            AgentRuntimeError::InvalidValue("harness outcome is undeclared".into())
        })?;
    closed_object_schema(fields_schema, definition)?
        .validate(fields)
        .map_err(|error| AgentRuntimeError::InvalidValue(error.to_string()))?;
    let returned = fields.as_object().ok_or_else(|| {
        AgentRuntimeError::InvalidValue("harness fields must be an object".into())
    })?;
    let normalized = normalize_history(state, frame.history.len(), history, ceilings)?;
    let frame = state.frames.get_mut(&frame_id).unwrap();
    frame.bindings.remove("current_evidence");
    frame.bindings.extend(
        returned
            .iter()
            .map(|(name, value)| (name.clone(), value.clone())),
    );
    frame.bindings.insert(
        "current_evidence".into(),
        Value::String(evidence_id.as_uuid().to_string()),
    );
    frame.history.extend(normalized);
    frame.outcome = Some(outcome.to_owned());
    frame.statement_index = 0;
    frame.state = KiteFrameState::Ready;
    Ok(())
}

fn handle_approval_result(
    state: &mut KiteRuntimeState,
    definition: &CompiledDefinition,
    command_key: &RuntimeCommandKey,
    answer: &Value,
) -> Result<(), AgentRuntimeError> {
    let frame_id = find_waiting_frame(state, command_key, WaitingKind::Approval)?;
    let (binding, answer_type) = match &state.frames[&frame_id].state {
        KiteFrameState::WaitingForApproval {
            binding,
            answer_type,
            ..
        } => (binding.clone(), answer_type.clone()),
        _ => unreachable!("find_waiting_frame selected an approval wait"),
    };
    type_schema(&answer_type, definition)?
        .validate(answer)
        .map_err(|error| AgentRuntimeError::InvalidValue(error.to_string()))?;
    let frame = state.frames.get_mut(&frame_id).unwrap();
    frame.bindings.insert(binding, answer.clone());
    frame.state = KiteFrameState::Ready;
    Ok(())
}

fn normalize_history(
    state: &mut KiteRuntimeState,
    existing_frame_items: usize,
    history: &[RuntimeHistoryItem],
    ceilings: &RuntimeCeilings,
) -> Result<Vec<RuntimeHistoryItem>, AgentRuntimeError> {
    if existing_frame_items.saturating_add(history.len()) > ceilings.max_history_items_per_frame {
        return Err(AgentRuntimeError::CeilingExceeded(
            "history_items_per_frame",
        ));
    }
    if history
        .windows(2)
        .any(|items| items[0].sequence >= items[1].sequence)
    {
        return Err(AgentRuntimeError::InvalidValue(
            "history sequence is duplicated or out of order".into(),
        ));
    }
    let next = state
        .next_history_sequence
        .checked_add(history.len() as u64)
        .ok_or(AgentRuntimeError::CeilingExceeded("history_sequence"))?;
    let normalized = history
        .iter()
        .enumerate()
        .map(|(offset, item)| RuntimeHistoryItem {
            sequence: state.next_history_sequence + offset as u64,
            kind: item.kind,
            payload: item.payload.clone(),
        })
        .collect();
    state.next_history_sequence = next;
    Ok(normalized)
}

fn reduce_to_transition(
    mut state: KiteRuntimeState,
    definition: &CompiledDefinition,
    goal: &GoalContract,
    revisions: RuntimeRevisions,
    idempotency_key: String,
    ceilings: &RuntimeCeilings,
) -> Result<RuntimeTransition, AgentRuntimeError> {
    loop {
        if matches!(
            state
                .frames
                .get(&state.root_frame_id)
                .map(|frame| &frame.state),
            Some(KiteFrameState::Returned { .. })
        ) {
            return transition(
                state,
                RuntimeDisposition::Returned,
                Vec::new(),
                revisions,
                idempotency_key,
                ceilings,
            );
        }

        let ready = state
            .frames
            .values()
            .filter(|frame| frame.state == KiteFrameState::Ready)
            .min_by_key(|frame| (frame.branch_index, frame.id))
            .map(|frame| frame.id);
        let Some(frame_id) = ready else {
            return transition(
                state,
                RuntimeDisposition::Running,
                Vec::new(),
                revisions,
                idempotency_key,
                ceilings,
            );
        };
        let (task_name, outcome_name, statement_index) = {
            let frame = &state.frames[&frame_id];
            (
                frame.task_name.clone(),
                frame.outcome.clone(),
                frame.statement_index,
            )
        };
        let Some(outcome_name) = outcome_name else {
            let command = invoke_harness_for(frame_id, &state, definition, goal, revisions)?;
            state.frames.get_mut(&frame_id).unwrap().state = KiteFrameState::WaitingForHarness {
                command_key: command.key.clone(),
            };
            return transition(
                state,
                RuntimeDisposition::Running,
                vec![command],
                revisions,
                idempotency_key,
                ceilings,
            );
        };
        let task = definition
            .tasks
            .get(&task_name)
            .ok_or_else(|| AgentRuntimeError::InvalidState("snapshot task is absent".into()))?;
        let outcome = task
            .outcomes
            .get(&outcome_name)
            .ok_or_else(|| AgentRuntimeError::InvalidState("snapshot outcome is absent".into()))?;
        let statement = outcome
            .statements
            .get(statement_index)
            .cloned()
            .ok_or_else(|| {
                AgentRuntimeError::InvalidState(
                    "outcome exhausted without a terminal statement".into(),
                )
            })?;
        if state.steps_consumed >= limit(definition, "steps") {
            return Err(AgentRuntimeError::CeilingExceeded("steps"));
        }
        state.steps_consumed += 1;

        match statement {
            CompiledStatement::Call { binding, call } => {
                let child = create_child(
                    &mut state, frame_id, &binding, &call, 0, definition, ceilings,
                )?;
                let child_id = child.id;
                state.frames.insert(child_id, child);
                let parent = state.frames.get_mut(&frame_id).unwrap();
                parent.state = KiteFrameState::WaitingForChildren {
                    child_ids: vec![child_id],
                };
                parent.statement_index += 1;
                let command = invoke_harness_for(child_id, &state, definition, goal, revisions)?;
                state.frames.get_mut(&child_id).unwrap().state =
                    KiteFrameState::WaitingForHarness {
                        command_key: command.key.clone(),
                    };
                return transition(
                    state,
                    RuntimeDisposition::Running,
                    vec![command],
                    revisions,
                    idempotency_key,
                    ceilings,
                );
            }
            CompiledStatement::Parallel { calls } => {
                let mut child_ids = Vec::with_capacity(calls.len());
                for (branch, call) in calls.iter().enumerate() {
                    let branch = u16::try_from(branch)
                        .map_err(|_| AgentRuntimeError::CeilingExceeded("parallel_branches"))?;
                    let child = create_child(
                        &mut state,
                        frame_id,
                        &call.binding,
                        &call.call,
                        branch,
                        definition,
                        ceilings,
                    )?;
                    child_ids.push(child.id);
                    state.frames.insert(child.id, child);
                }
                let parent = state.frames.get_mut(&frame_id).unwrap();
                parent.state = KiteFrameState::WaitingForChildren {
                    child_ids: child_ids.clone(),
                };
                parent.statement_index += 1;
                child_ids.sort_by_key(|id| {
                    let child = &state.frames[id];
                    (child.branch_index, child.id)
                });
                let mut commands = Vec::with_capacity(child_ids.len());
                for child_id in child_ids {
                    let command =
                        invoke_harness_for(child_id, &state, definition, goal, revisions)?;
                    state.frames.get_mut(&child_id).unwrap().state =
                        KiteFrameState::WaitingForHarness {
                            command_key: command.key.clone(),
                        };
                    commands.push(command);
                }
                return transition(
                    state,
                    RuntimeDisposition::Running,
                    commands,
                    revisions,
                    idempotency_key,
                    ceilings,
                );
            }
            CompiledStatement::Ask {
                binding,
                answer_type,
                question,
            } => {
                let text = resolve_string(&question, &state.frames[&frame_id].bindings)?;
                let key = command_key(revisions.task + 1, frame_id, "approval")?;
                let answer_schema = type_schema(&answer_type, definition)?;
                let frame = state.frames.get_mut(&frame_id).unwrap();
                frame.state = KiteFrameState::WaitingForApproval {
                    command_key: key.clone(),
                    binding,
                    answer_type,
                };
                frame.statement_index += 1;
                return transition(
                    state,
                    RuntimeDisposition::Running,
                    vec![KernelCommand {
                        schema_version: 1,
                        key,
                        payload: KernelCommandPayload::RequestApproval(RequestApproval {
                            question: text,
                            answer_schema,
                        }),
                    }],
                    revisions,
                    idempotency_key,
                    ceilings,
                );
            }
            CompiledStatement::Goto { call } => {
                let next = resolve_call(
                    &call,
                    &serde_json::to_value(&state.frames[&frame_id].bindings)
                        .map_err(|error| AgentRuntimeError::InvalidValue(error.to_string()))?,
                    definition,
                )?;
                let frame = state.frames.get_mut(&frame_id).unwrap();
                frame.task_name = call.task;
                frame.bindings = next;
                frame.outcome = None;
                frame.statement_index = 0;
                if call.context == ContextMode::Isolated {
                    frame.history.clear();
                }
                frame.state = KiteFrameState::Ready;
            }
            CompiledStatement::Done { value } => {
                let result = resolve(&value, &state.frames[&frame_id].bindings)?;
                state.frames.get_mut(&frame_id).unwrap().statement_index += 1;
                return_frame(&mut state, frame_id, result)?;
            }
            CompiledStatement::Fail { code, message } => {
                let bindings = &state.frames[&frame_id].bindings;
                let code = resolve_string(&code, bindings)?;
                let message = resolve_string(&message, bindings)?;
                state.frames.get_mut(&frame_id).unwrap().statement_index += 1;
                return transition(
                    state,
                    RuntimeDisposition::Terminating,
                    vec![fail_task_command(revisions, code, message)?],
                    revisions,
                    idempotency_key,
                    ceilings,
                );
            }
            CompiledStatement::CompleteGoal { criterion_evidence } => {
                let expected = goal
                    .success
                    .iter()
                    .map(|criterion| criterion.id.as_str())
                    .collect::<BTreeSet<_>>();
                let supplied = criterion_evidence
                    .keys()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>();
                if expected != supplied {
                    return Err(AgentRuntimeError::InvalidValue(
                        "goal criterion assertions do not match the accepted goal".into(),
                    ));
                }
                let command = complete_goal_command(
                    &criterion_evidence,
                    &state.frames[&frame_id].bindings,
                    revisions,
                )?;
                state.frames.get_mut(&frame_id).unwrap().statement_index += 1;
                return transition(
                    state,
                    RuntimeDisposition::Terminating,
                    vec![command],
                    revisions,
                    idempotency_key,
                    ceilings,
                );
            }
            CompiledStatement::BlockGoal {
                blocked_when_index,
                reason_code,
                explanation,
                evidence,
                unlock_conditions,
            } => {
                if usize::from(blocked_when_index) >= goal.blocked_when.len() {
                    return Err(AgentRuntimeError::InvalidValue(
                        "goal blocker index is outside the accepted goal".into(),
                    ));
                }
                let command = block_goal_command(
                    blocked_when_index,
                    &reason_code,
                    &explanation,
                    &evidence,
                    &unlock_conditions,
                    &state.frames[&frame_id].bindings,
                    revisions,
                )?;
                state.frames.get_mut(&frame_id).unwrap().statement_index += 1;
                return transition(
                    state,
                    RuntimeDisposition::Blocked,
                    vec![command],
                    revisions,
                    idempotency_key,
                    ceilings,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn create_child(
    state: &mut KiteRuntimeState,
    parent_id: KiteFrameId,
    binding: &str,
    call: &CompiledCall,
    branch_index: u16,
    definition: &CompiledDefinition,
    ceilings: &RuntimeCeilings,
) -> Result<KiteFrameSnapshot, AgentRuntimeError> {
    if state.frames.len() >= ceilings.max_frames {
        return Err(AgentRuntimeError::CeilingExceeded("frames"));
    }
    let parent = state
        .frames
        .get(&parent_id)
        .ok_or_else(|| AgentRuntimeError::InvalidState("parent frame is absent".into()))?;
    let bindings = resolve_call(
        call,
        &serde_json::to_value(&parent.bindings)
            .map_err(|error| AgentRuntimeError::InvalidValue(error.to_string()))?,
        definition,
    )?;
    let history = match call.context {
        ContextMode::Isolated => Vec::new(),
        ContextMode::Kept => parent.history.clone(),
        ContextMode::Legacy => {
            return Err(AgentRuntimeError::InvalidDefinition(
                "agent call has legacy context".into(),
            ));
        }
    };
    let id = KiteFrameId(state.next_frame_id);
    state.next_frame_id = state
        .next_frame_id
        .checked_add(1)
        .ok_or(AgentRuntimeError::CeilingExceeded("frames"))?;
    Ok(KiteFrameSnapshot {
        id,
        parent_id: Some(parent_id),
        parent_binding: Some(binding.to_owned()),
        branch_index,
        task_name: call.task.clone(),
        bindings,
        outcome: None,
        statement_index: 0,
        history,
        state: KiteFrameState::Ready,
    })
}

fn return_frame(
    state: &mut KiteRuntimeState,
    frame_id: KiteFrameId,
    result: Value,
) -> Result<(), AgentRuntimeError> {
    let parent_id = state
        .frames
        .get(&frame_id)
        .ok_or_else(|| AgentRuntimeError::InvalidState("returning frame is absent".into()))?
        .parent_id;
    state.frames.get_mut(&frame_id).unwrap().state = KiteFrameState::Returned { result };
    let Some(parent_id) = parent_id else {
        return Ok(());
    };
    let child_ids = match &state
        .frames
        .get(&parent_id)
        .ok_or_else(|| AgentRuntimeError::InvalidState("parent frame is absent".into()))?
        .state
    {
        KiteFrameState::WaitingForChildren { child_ids } => child_ids.clone(),
        _ => {
            return Err(AgentRuntimeError::InvalidState(
                "parent is not waiting for returning children".into(),
            ));
        }
    };
    if child_ids.iter().any(|id| {
        !matches!(
            state.frames.get(id).map(|child| &child.state),
            Some(KiteFrameState::Returned { .. })
        )
    }) {
        return Ok(());
    }
    let returned = child_ids
        .iter()
        .map(|id| {
            let child = &state.frames[id];
            let binding = child.parent_binding.clone().ok_or_else(|| {
                AgentRuntimeError::InvalidState("child return binding is absent".into())
            })?;
            let KiteFrameState::Returned { result } = &child.state else {
                unreachable!("all joined children were checked as returned")
            };
            Ok((binding, result.clone()))
        })
        .collect::<Result<Vec<_>, AgentRuntimeError>>()?;
    let parent = state.frames.get_mut(&parent_id).unwrap();
    for (binding, result) in returned {
        parent.bindings.insert(binding, result);
    }
    parent.state = KiteFrameState::Ready;
    for child_id in child_ids {
        state.frames.remove(&child_id);
    }
    Ok(())
}

fn command_key(
    task_revision: u64,
    frame_id: KiteFrameId,
    kind: &str,
) -> Result<RuntimeCommandKey, AgentRuntimeError> {
    RuntimeCommandKey::new(format!("r{task_revision}-f{}-{kind}", frame_id.0))
}

fn complete_goal_command(
    values: &BTreeMap<String, CompiledValue>,
    bindings: &BTreeMap<String, Value>,
    revisions: RuntimeRevisions,
) -> Result<KernelCommand, AgentRuntimeError> {
    let assertions = values
        .iter()
        .map(|(criterion_id, evidence)| {
            Ok(CriterionAssertion {
                criterion_id: criterion_id.clone(),
                evidence_id: evidence_id(&resolve_string(evidence, bindings)?)?,
            })
        })
        .collect::<Result<_, AgentRuntimeError>>()?;
    Ok(KernelCommand {
        schema_version: 1,
        key: RuntimeCommandKey::new(format!("r{}-goal-complete", revisions.task + 1))?,
        payload: KernelCommandPayload::CompleteGoal(CompleteGoal { assertions }),
    })
}

#[allow(clippy::too_many_arguments)]
fn block_goal_command(
    index: u16,
    reason_code: &CompiledValue,
    explanation: &CompiledValue,
    evidence: &[CompiledValue],
    unlock: &[CompiledValue],
    bindings: &BTreeMap<String, Value>,
    revisions: RuntimeRevisions,
) -> Result<KernelCommand, AgentRuntimeError> {
    Ok(KernelCommand {
        schema_version: 1,
        key: RuntimeCommandKey::new(format!("r{}-goal-block", revisions.task + 1))?,
        payload: KernelCommandPayload::BlockGoal(BlockGoal {
            blocked_when_index: index,
            reason_code: resolve_string(reason_code, bindings)?,
            explanation: resolve_string(explanation, bindings)?,
            evidence_ids: evidence
                .iter()
                .map(|value| evidence_id(&resolve_string(value, bindings)?))
                .collect::<Result<_, _>>()?,
            unlock_conditions: unlock
                .iter()
                .map(|value| resolve_string(value, bindings))
                .collect::<Result<_, _>>()?,
        }),
    })
}

fn fail_task_command(
    revisions: RuntimeRevisions,
    code: String,
    message: String,
) -> Result<KernelCommand, AgentRuntimeError> {
    Ok(KernelCommand {
        schema_version: 1,
        key: RuntimeCommandKey::new(format!("r{}-task-fail", revisions.task + 1))?,
        payload: KernelCommandPayload::FailTask(FailTask { code, message }),
    })
}

fn evidence_id(value: &str) -> Result<EvidenceId, AgentRuntimeError> {
    uuid::Uuid::parse_str(value)
        .map(EvidenceId::new)
        .map_err(|error| AgentRuntimeError::InvalidValue(error.to_string()))
}

fn resolve_string(
    value: &CompiledValue,
    bindings: &BTreeMap<String, Value>,
) -> Result<String, AgentRuntimeError> {
    resolve(value, bindings)?
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| AgentRuntimeError::InvalidValue("resolved value is not a string".into()))
}

fn accepted_input(task: &AcceptedUserTask) -> Value {
    match &task.target {
        TaskTarget::GithubPullRequest { repository, number } => json!({
            "repository": repository,
            "pull_request": number,
        }),
    }
}

fn resolve_call(
    call: &CompiledCall,
    scope: &Value,
    definition: &CompiledDefinition,
) -> Result<BTreeMap<String, Value>, AgentRuntimeError> {
    let scope = scope
        .as_object()
        .ok_or_else(|| AgentRuntimeError::InvalidValue("call scope must be an object".into()))?;
    let resolved = call
        .arguments
        .iter()
        .map(|(name, value)| resolve(value, scope).map(|resolved| (name.clone(), resolved)))
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    let task = definition
        .tasks
        .get(&call.task)
        .ok_or_else(|| AgentRuntimeError::InvalidDefinition("call target is absent".into()))?;
    closed_object_schema(&task.inputs, definition)?
        .validate(
            &serde_json::to_value(&resolved)
                .map_err(|error| AgentRuntimeError::InvalidValue(error.to_string()))?,
        )
        .map_err(|error| AgentRuntimeError::InvalidValue(error.to_string()))?;
    Ok(resolved)
}

fn resolve(
    value: &CompiledValue,
    bindings: &impl BindingLookup,
) -> Result<Value, AgentRuntimeError> {
    match value {
        CompiledValue::Binding(name) => bindings
            .get_binding(name)
            .cloned()
            .ok_or_else(|| AgentRuntimeError::InvalidValue(format!("binding is absent: {name}"))),
        CompiledValue::String(value) => Ok(Value::String(value.clone())),
        CompiledValue::Number(value) => serde_json::from_str(value)
            .map_err(|error| AgentRuntimeError::InvalidValue(error.to_string())),
        CompiledValue::Boolean(value) => Ok(Value::Bool(*value)),
        CompiledValue::Null => Ok(Value::Null),
    }
}

trait BindingLookup {
    fn get_binding(&self, name: &str) -> Option<&Value>;
}

impl BindingLookup for BTreeMap<String, Value> {
    fn get_binding(&self, name: &str) -> Option<&Value> {
        self.get(name)
    }
}

impl BindingLookup for serde_json::Map<String, Value> {
    fn get_binding(&self, name: &str) -> Option<&Value> {
        self.get(name)
    }
}

fn type_schema(
    value: &CompiledType,
    definition: &CompiledDefinition,
) -> Result<ClosedSchema, AgentRuntimeError> {
    match value {
        CompiledType::String => Ok(ClosedSchema::string()),
        CompiledType::Integer => Ok(ClosedSchema::integer()),
        CompiledType::Number => Ok(ClosedSchema::number()),
        CompiledType::Boolean => Ok(ClosedSchema::boolean()),
        CompiledType::Optional(inner) => {
            Ok(ClosedSchema::nullable(type_schema(inner, definition)?))
        }
        CompiledType::List(inner) => Ok(ClosedSchema::array(type_schema(inner, definition)?)),
        CompiledType::Record(name) => {
            let fields = definition.records.get(name).ok_or_else(|| {
                AgentRuntimeError::InvalidDefinition(format!("record type is absent: {name}"))
            })?;
            Ok(ClosedSchema::object(
                name,
                fields
                    .iter()
                    .map(|(field, ty)| Ok((field.clone(), type_schema(ty, definition)?)))
                    .collect::<Result<_, AgentRuntimeError>>()?,
            ))
        }
    }
}

fn closed_object_schema(
    fields: &BTreeMap<String, CompiledType>,
    definition: &CompiledDefinition,
) -> Result<ClosedSchema, AgentRuntimeError> {
    Ok(ClosedSchema::object(
        "object",
        fields
            .iter()
            .map(|(name, value)| Ok((name.clone(), type_schema(value, definition)?)))
            .collect::<Result<_, AgentRuntimeError>>()?,
    ))
}

fn completion_result_schema(
    completion: &CompletionContract,
    definition: &CompiledDefinition,
) -> Result<ClosedSchema, AgentRuntimeError> {
    let mut fields = completion
        .required_nullable_fields
        .iter()
        .map(|(name, value)| Ok((name.clone(), type_schema(value, definition)?)))
        .collect::<Result<BTreeMap<_, _>, AgentRuntimeError>>()?;
    fields.insert(
        "outcome".into(),
        ClosedSchema::enumeration(
            completion
                .outcomes
                .keys()
                .cloned()
                .map(PrimitiveValue::String)
                .collect(),
        ),
    );
    Ok(ClosedSchema::object("complete_task_result", fields))
}

fn validate_completion_contract(task: &CompiledTask) -> Result<(), AgentRuntimeError> {
    if task.completion.outcomes.len() != task.outcomes.len()
        || task
            .outcomes
            .iter()
            .any(|(name, outcome)| task.completion.outcomes.get(name) != Some(&outcome.fields))
    {
        return Err(AgentRuntimeError::InvalidDefinition(
            "completion contract does not match executable outcomes".into(),
        ));
    }

    let mut expected_union = BTreeMap::new();
    for outcome in task.outcomes.values() {
        for (name, value) in &outcome.fields {
            let nullable = match value {
                CompiledType::Optional(_) => value.clone(),
                _ => CompiledType::Optional(Box::new(value.clone())),
            };
            if let Some(existing) = expected_union.insert(name.clone(), nullable.clone())
                && existing != nullable
            {
                return Err(AgentRuntimeError::InvalidDefinition(
                    "outcome field union has incompatible types".into(),
                ));
            }
        }
    }
    if task.completion.required_nullable_fields != expected_union {
        return Err(AgentRuntimeError::InvalidDefinition(
            "completion contract nullable field union does not match outcomes".into(),
        ));
    }
    Ok(())
}

fn validate_tools(bindings: &[CompiledToolBinding]) -> Result<(), AgentRuntimeError> {
    for binding in bindings {
        binding
            .descriptor
            .verify_descriptor_digest()
            .map_err(|error| AgentRuntimeError::InvalidDefinition(error.to_string()))?;
        if binding.qualified_name != binding.descriptor.qualified_name()
            || binding.descriptor_digest != binding.descriptor.descriptor_digest()
        {
            return Err(AgentRuntimeError::InvalidDefinition(
                "tool descriptor binding does not match".into(),
            ));
        }
    }
    Ok(())
}

fn same_tool_binding(left: &CompiledToolBinding, right: &CompiledToolBinding) -> bool {
    left.qualified_name == right.qualified_name
        && left.catalog_epoch_id == right.catalog_epoch_id
        && left.descriptor_digest == right.descriptor_digest
        && left.descriptor.normalized_descriptor() == right.descriptor.normalized_descriptor()
}

fn validate_statement_context(statement: &CompiledStatement) -> Result<(), AgentRuntimeError> {
    let calls = match statement {
        CompiledStatement::Call { call, .. } | CompiledStatement::Goto { call } => {
            std::slice::from_ref(call)
        }
        CompiledStatement::Parallel { calls } => {
            if calls
                .iter()
                .any(|binding| binding.call.context == ContextMode::Legacy)
            {
                return Err(AgentRuntimeError::InvalidDefinition(
                    "agent call has legacy context".into(),
                ));
            }
            return Ok(());
        }
        _ => return Ok(()),
    };
    if calls.iter().any(|call| call.context == ContextMode::Legacy) {
        return Err(AgentRuntimeError::InvalidDefinition(
            "agent call has legacy context".into(),
        ));
    }
    Ok(())
}

fn harness_tools(bindings: &[CompiledToolBinding]) -> Vec<HarnessTool> {
    let mut tools = bindings
        .iter()
        .map(|binding| HarnessTool {
            qualified_name: binding.qualified_name.clone(),
            descriptor_digest: binding.descriptor_digest.clone(),
            descriptor: binding.descriptor.normalized_descriptor(),
        })
        .collect::<Vec<_>>();
    tools.sort_by(|left, right| left.qualified_name.cmp(&right.qualified_name));
    tools
}

fn limit(definition: &CompiledDefinition, name: &str) -> u64 {
    definition.limits.get(name).copied().unwrap_or(u64::MAX)
}

fn remaining_limits(
    definition: &CompiledDefinition,
    frame: &KiteFrameSnapshot,
) -> Result<HarnessStructuralLimits, AgentRuntimeError> {
    let turns = frame
        .history
        .iter()
        .filter(|item| item.kind == RuntimeHistoryKind::HarnessOutput)
        .count() as u64;
    let model_turns = limit(definition, "model_turns_per_task");
    if turns >= model_turns {
        return Err(AgentRuntimeError::CeilingExceeded("model_turns_per_task"));
    }
    Ok(HarnessStructuralLimits {
        model_turns_remaining: model_turns - turns,
        tool_calls_per_response: limit(definition, "tool_calls_per_response"),
        response_item_bytes: limit(definition, "response_item_bytes"),
        active_execution_millis_remaining: limit(definition, "active_execution_seconds")
            .saturating_mul(1_000),
    })
}

fn invoke_harness_for(
    frame_id: KiteFrameId,
    state: &KiteRuntimeState,
    definition: &CompiledDefinition,
    goal: &GoalContract,
    revisions: RuntimeRevisions,
) -> Result<KernelCommand, AgentRuntimeError> {
    let frame = state
        .frames
        .get(&frame_id)
        .ok_or_else(|| AgentRuntimeError::InvalidState("runtime frame is absent".into()))?;
    let task = definition
        .tasks
        .get(&frame.task_name)
        .ok_or_else(|| AgentRuntimeError::InvalidDefinition("agent task is absent".into()))?;
    Ok(KernelCommand {
        schema_version: 1,
        key: RuntimeCommandKey::new(format!("r{}-f{}-harness", revisions.task + 1, frame.id.0))?,
        payload: KernelCommandPayload::InvokeHarness(InvokeHarness {
            role: task.model_role.ok_or_else(|| {
                AgentRuntimeError::InvalidDefinition("agent task has no model role".into())
            })?,
            prompt: task.prompt.clone(),
            goal: GoalProjection::from_contract(goal),
            input: serde_json::to_value(&frame.bindings)
                .map_err(|error| AgentRuntimeError::InvalidValue(error.to_string()))?,
            input_schema: closed_object_schema(&task.inputs, definition)?,
            result_schema: completion_result_schema(&task.completion, definition)?,
            tools: harness_tools(&task.tools),
            projected_history: frame.history.clone(),
            limits: remaining_limits(definition, frame)?,
        }),
    })
}

fn transition(
    state: KiteRuntimeState,
    disposition: RuntimeDisposition,
    commands: Vec<KernelCommand>,
    revisions: RuntimeRevisions,
    idempotency_key: String,
    ceilings: &RuntimeCeilings,
) -> Result<RuntimeTransition, AgentRuntimeError> {
    if commands.len() > ceilings.max_commands_per_transition {
        return Err(AgentRuntimeError::CeilingExceeded(
            "commands_per_transition",
        ));
    }
    for command in &commands {
        if canonical_json(command)
            .map_err(|error| AgentRuntimeError::Canonicalization(error.to_string()))?
            .len()
            > ceilings.max_command_bytes
        {
            return Err(AgentRuntimeError::CeilingExceeded("command_bytes"));
        }
    }
    Ok(RuntimeTransition {
        idempotency_key,
        expected_task_revision: revisions.task,
        expected_goal_revision: revisions.goal,
        snapshot: RuntimeStateSnapshot::new(
            disposition,
            serde_json::to_value(state)
                .map_err(|error| AgentRuntimeError::InvalidState(error.to_string()))?,
            ceilings,
        )?,
        commands,
    })
}
