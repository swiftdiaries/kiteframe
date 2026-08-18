# Kite language reference

This reference freezes the concrete Slice 2 source spelling for authoring
tools and the parser. It is a grammar and compile-time contract. Runtime
execution remains behind the validated `AgentRuntime` boundary.

## Definitions

```text
source              = { record-definition }, definition ;
definition          = workflow-definition | agent-definition ;
agent-definition    = "agent", identifier, fields, "->", type, "{",
                      entry, [ events ], [ limits ], task-definition,
                      { task-definition }, "}" ;
entry               = "entry", invocation ;
events              = "events", "[", string, { [ "," ], string }, "]" ;
task-definition     = "task", identifier, fields, "->", type, "{",
                      model-role, prompt, tools, outcome,
                      { outcome }, "}" ;
model-role          = "model", ( "reasoning" | "coding" ) ;
call-statement      = identifier, "=", "call", invocation,
                      [ context-mode ] ;
return-statement    = "return", invocation, [ context-mode ] ;
fork-statement      = "fork", "{", call-statement,
                      { call-statement }, "}", "join" ;
context-mode        = "context", ( "isolated" | "kept" ) ;
workflow-definition = (* existing grammar; deprecated *) ;
```

The existing productions used above retain their current meanings. The
`workflow-definition (deprecated)` production remains accepted for
compatibility:

```text
record-definition = "type", identifier, "{", { field }, "}" ;
fields            = "(", [ field, { [ "," ], field } ], ")" ;
field             = identifier, ":", type ;
invocation        = identifier, "(", [ argument, { [ "," ], argument } ], ")" ;
argument          = identifier, "=", value ;
prompt            = "prompt", triple-string ;
tools             = "tools", "[", { [ "," ], identifier }, "]" ;
agent-outcome     = "on", identifier, fields, "{", agent-statement,
                    { agent-statement }, "}" ;
agent-statement   = call-statement | return-statement | fork-statement |
                    "done", value | "fail", value, value |
                    identifier, "=", "ask", type, value |
                    complete-goal | block-goal ;
complete-goal     = "complete", "goal", "{", criterion-evidence,
                    { criterion-evidence }, "}" ;
criterion-evidence = criterion-id, ":", string-value ;
block-goal        = "block", "goal", "when", u16, "{",
                    block-field, { block-field }, "}" ;
block-field       = "code", ":", string-value |
                    "explanation", ":", string-value |
                    "evidence", ":", "[", string-value,
                    { ",", string-value }, "]" |
                    "unlock", ":", "[", string-value,
                    { ",", string-value }, "]" ;
string-value      = string | string-binding ;
workflow-outcome  = (* existing workflow outcome grammar *) ;
```

In an `agent-definition`, the `outcome` reference in `task-definition` means
`agent-outcome`; legacy workflow tasks use `workflow-outcome`.

`workflow-definition` retains the pre-Slice 1 top-level-task form. It is
deprecated for new authoring but remains supported for publication and
execution. In a workflow, `start`, `goto`, and `parallel` retain their current
spellings and semantics. `return`, `fork ... join`, call context modes, event
declarations, and model roles are agent-only and are excluded from workflow
definitions and workflow tasks. `complete goal` and `block goal` are also
agent-only and terminal. A statement after either operation is invalid.

## Names, words, and punctuation

Identifiers use the existing identifier rule: they begin with an ASCII letter
or underscore and continue with ASCII letters, digits, or underscores. Agent
names and task names use that same rule. The built-in scalar type names are
`string`, `integer`, `number`, and `boolean`.

The reserved language words are `agent`, `workflow`, `type`, `task`, `entry`,
`events`, `limits`, `model`, `reasoning`, `coding`, `prompt`, `tools`, `on`,
`call`, `return`, `fork`, `join`, `context`, `isolated`, `kept`, `done`,
`fail`, `ask`, `start`, `goto`, and `parallel`. `true`, `false`, and `null`
remain literal values rather than user bindings. Reserved compiler names remain
unavailable to source authors.

`complete` and `block` are contextual terminal-operation words at the start of
an outcome statement and therefore cannot be used as result bindings there.
`goal`, `when`, `code`, `explanation`, `evidence`, and `unlock` are contextual
inside those operations rather than general-purpose identifier extensions.

Goal criterion ids match `^[a-z][a-z0-9_]{0,63}$`. Hyphenated text such as
`Bad-ID` is tokenized only so the parser can emit `invalid_goal_criterion`; it
does not become a valid identifier anywhere else. `current_evidence` is an
immutable kernel-supplied `string` binding synthesized only inside an agent
outcome handler. Definition inputs, task inputs, and outcome fields cannot use
that name, and ordinary source/model bindings cannot shadow it.

Commas remain optional where the current grammar already permits them: fields,
invocation arguments, tool lists, record fields, and event strings. Whitespace
separates adjacent items when a comma is omitted.

Completion criteria and block fields are whitespace-separated and reject
commas. The `evidence` and `unlock` lists use commas between multiple values.

## Agent structure and validation

Each source contains exactly one definition after any record definitions. An
agent contains exactly one `entry`, optional single `events` and `limits`
blocks, and one or more nested tasks. Agent tasks occur only inside the agent
body; top-level tasks belong only to a legacy workflow. Agent tasks require one
`model`, one `prompt`, one `tools` declaration, and at least one outcome.

`events` accepts semantic event-name strings matching
`^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+\.v[1-9][0-9]*$`. Event names are
unique within an agent. Agent limits additionally admit `elapsed_seconds`;
workflow limits do not.

`context kept` retains a call's context, while `context isolated` starts the
target call with isolated context. When an agent call or return omits the
optional context mode, it defaults to `context isolated`. A `fork` contains one
or more bound call statements and ends with `join`. `done`, `fail`, and `ask`
retain their existing outcome semantics in both definition kinds.

`complete goal` maps one or more criterion ids to string evidence-id bindings.
The compiler normalizes this mapping into a `BTreeMap`; source order is not
semantic. `block goal when N` selects one immutable `blockedWhen[N]` condition
and requires exactly one each of `code`, `explanation`, `evidence`, and
`unlock`. Evidence and unlock list order is retained. Every goal-authority
value compiles against `CompiledType::String`. The compiler does not inspect a
`GoalContract`; the Rust kernel validates criteria, evidence identities, and
the blocked-condition index when it accepts the command.

The compiled IR shapes are exact:

```rust
CompleteGoal {
    criterion_evidence: BTreeMap<String, CompiledValue>,
}
BlockGoal {
    blocked_when_index: u16,
    reason_code: CompiledValue,
    explanation: CompiledValue,
    evidence: Vec<CompiledValue>,
    unlock_conditions: Vec<CompiledValue>,
}
```

## Diagnostics and publication boundary

Diagnostics use stable snake-case codes and include a source span. A valid
legacy workflow produces the bounded warning code `deprecated_workflow`.
Both definition kinds retain their established versions: a workflow has
`dsl_version = 1` and an agent has `dsl_version = 2`.
Agents are immutable and publishable. Legacy `StartRun` still rejects agents
with `agent_runtime_unavailable`; Slice 2 agent execution uses `StartAgent` and
the separate validated runtime contract. Corrupt legacy IR containing either
goal-authority statement fails closed rather than gaining workflow semantics.

Goal-authority diagnostics are stable:

| Code | Condition |
|---|---|
| `complete_goal_requires_agent` | completion appears outside an agent |
| `empty_goal_completion` | no criterion is asserted |
| `duplicate_goal_criterion` | a criterion id appears twice |
| `invalid_goal_criterion` | a criterion id violates its closed pattern |
| `block_goal_requires_agent` | blocking appears outside an agent |
| `invalid_blocked_when_index` | the index is not unsigned decimal `u16` |
| `duplicate_block_goal_field` | a block field appears twice |
| `missing_block_goal_field` | a required field is missing or unknown |
| `empty_block_evidence` | the evidence list is empty |
| `empty_unlock_conditions` | the unlock list is empty |
| `goal_authority_value_must_be_string` | a literal is not a string |
| `reserved_current_evidence_binding` | source tries to declare the kernel binding |

Warnings and errors consume the same configured diagnostic budget. Existing
source-size, token, identifier, and type-nesting limits remain in force.

Compiled definition identity includes definition kind, normalized agent
metadata, exact source, and immutable tool descriptors. Compiled agents are
available to authoring, compile-time tooling, and publication in this slice;
legacy workflows remain publishable and executable.

The reviewed [babysit PR example](../../examples/babysit_pr.kite) remains an
authoring and compiler example. This reference freezes the Slice 2 language
and runtime interface only; persistence, plugins, channels, workers, budgets,
connectors, Codex, Cursor, and VS Code integration land in later tasks.

Generic definition publication currently uses the private Kiteframe ACP
extension method `_kiteframe/definition/publish`. It is an application method,
not a stable plugin protocol.
