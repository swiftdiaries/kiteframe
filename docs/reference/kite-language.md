# Kite language reference

This reference freezes the concrete Slice 1 source spelling for authoring
tools and the parser. It is a grammar and compile-time contract; it does not
make an `agent` executable.

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
                    identifier, "=", "ask", type, value ;
workflow-outcome  = (* existing workflow outcome grammar *) ;
```

In an `agent-definition`, the `outcome` reference in `task-definition` means
`agent-outcome`; legacy workflow tasks use `workflow-outcome`.

`workflow-definition` retains the pre-Slice 1 top-level-task form. It is
deprecated for new authoring but remains supported for publication and
execution. In a workflow, `start`, `goto`, and `parallel` retain their current
spellings and semantics. `return`, `fork ... join`, call context modes, event
declarations, and model roles are agent-only and are excluded from workflow
definitions and workflow tasks.

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

Commas remain optional where the current grammar already permits them: fields,
invocation arguments, tool lists, record fields, and event strings. Whitespace
separates adjacent items when a comma is omitted.

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

## Diagnostics and publication boundary

Diagnostics use stable snake-case codes and include a source span. A valid
legacy workflow produces the bounded warning code `deprecated_workflow`.
Both definition kinds compile in Slice 1: a workflow has `dsl_version = 1` and
an agent has `dsl_version = 2`. Agents are immutable and publishable in consolidated Slice 1, but not executable. `StartRun` rejects agents with `agent_runtime_unavailable` before creating any execution state. Goal lifecycle
and agent runtime authority begin in Slice 2.

Warnings and errors consume the same configured diagnostic budget. Existing
source-size, token, identifier, and type-nesting limits remain in force.

Compiled definition identity includes definition kind, normalized agent
metadata, exact source, and immutable tool descriptors. Compiled agents are
available to authoring, compile-time tooling, and publication in this slice;
legacy workflows remain publishable and executable.
