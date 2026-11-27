# Session policy reference

A session policy decides whether a tool call runs, is denied, or waits for a
person to approve it. This reference states what each rule means. The typed
rules in `SessionPolicy` are the source of truth. Kiteframe compiles them to
Cedar, and `TypedAuthorizer` evaluates the same rules directly as a reference
model. A differential test in `tests/cedar_policy.rs` keeps the two in step.

## Decisions

| Decision | When | Engine result |
|---|---|---|
| `Forbid` | At least one forbid rule applies | The call finishes with `policy_denied` |
| `Allow` | No forbid applies and at least one permit applies | The tool runs |
| `NeedsPermission` | No rule applies | A permission request waits for a person |

A forbid always wins over a permit. A rule that does not apply has no effect,
so a permit whose conditions are not met falls back to `NeedsPermission`.

## Tool rules

A tool **permit** applies to a call when the tool's qualified name is in
`names` and the call's descriptor digest equals the digest in the session's
current catalog. A permit written against one tool contract does not cover a
changed one.

A tool **forbid** applies when the tool's qualified name is in `names`,
whatever the digest. A forbid still applies after the tool's contract
changes.

## Capability rules

The tool's declared capabilities decide whether a capability rule applies.
Tools of unknown type carry `mcp_unknown`.

- A capability **forbid** applies when the tool has any of the named
  capabilities.
- A capability **permit** applies when the tool has at least one capability
  and every capability it has is named. A permit for `read_data` does not
  cover a tool that has `read_data` and `network`. A tool with no declared
  capabilities is never covered by a capability permit.

## Input constraints

`input` maps JSON pointers into the call's arguments to the values allowed
there. A rule with `input` applies only when every pointer's constraint holds.

The argument at a pointer is one of three kinds:

| Kind | Values |
|---|---|
| missing | The pointer resolves to nothing, or to `null` |
| comparable | A string, a boolean, or a JSON integer from -2^63 to 2^63 - 1, written without a decimal point |
| uncomparable | Anything else: a number with a decimal point such as `5.0`, a larger integer, an array, or an object |

| Argument | Permit constraint holds when | Forbid constraint holds when |
|---|---|---|
| missing | The constraint lists `null` | Always |
| comparable | The value is listed | The value is listed |
| uncomparable | Never | Always |

Forbids hold for missing and uncomparable arguments because arguments are
checked against the tool's input schema only after authorization. A forbid
must fail closed.

`input` is allowed only on tool rules, and each pointer must exist in every
named tool's input schema. Number constraints are rejected, because Cedar has
no floating-point type.

### Example

```json
{
  "rule_id": "push-to-api-only",
  "effect": "permit",
  "target": { "type": "tool", "names": ["git/push"] },
  "input": { "/repo": ["acme/api", "acme/web"] }
}
```

This rule allows `git/push` to `acme/api` and `acme/web`. A push to any other
repository needs a person's approval.

## Approval and re-authorization

When a person approves a waiting call with `allow_once` or
`allow_tool_for_session`, the engine authorizes the call again before it runs.
If a forbid was added while the call waited, the call finishes with
`policy_denied` and the tool does not run. `Allow` and `NeedsPermission` both
go ahead, because the approval answers `NeedsPermission`. If the second check
itself fails, the call finishes with `policy_evaluation_failed`. The approval
is already recorded, so the call cannot wait for a retry.

## Recorded revision

`PersistedAuthorizer`, the authorizer the server uses, reads the policy, its
revision, and the catalog from one database snapshot and reports that
revision. The tool call and any permission request record it. An in-memory
authorizer reports no revision, so the engine records the session's current
revision instead.

## Upgrading from earlier releases

Two kinds of stored rule change meaning with this release:

- A capability permit had no effect before. It now allows matching calls
  without approval. A permit naming `mcp_unknown` covers every
  MCP-discovered tool.
- A forbid with `input` used to block the whole tool. It now blocks only the
  listed values.

Before you deploy, list the affected rules in open sessions:

```sql
WITH latest AS (
  SELECT DISTINCT ON (session_id) session_id, revision, typed_source
  FROM session_policy_rules
  ORDER BY session_id, revision DESC
)
SELECT l.session_id, l.revision, r->>'rule_id' AS rule_id, r->>'effect' AS effect,
       r->'target' AS target, r->'input' AS input
FROM latest l
JOIN sessions s ON s.id = l.session_id AND s.state = 'active'
CROSS JOIN LATERAL jsonb_array_elements(l.typed_source->'rules') AS r
WHERE (r->>'effect' = 'permit' AND r->'target'->>'type' = 'capability')
   OR (r->>'effect' = 'forbid' AND COALESCE(r->'input', '{}'::jsonb) <> '{}'::jsonb);
```

A stored rule that limits an argument to a decimal number now fails
validation, and every tool call in its session returns an error until the
policy is replaced.

## Not covered

- Delegation chains for the principal, such as user to agent to sub-agent.
- Enforcement below the MCP boundary, such as system calls or network egress.
- Argument limits on `allow_tool_for_session` approvals, which permit the
  whole tool.
- Validating requests against the generated Cedar schema.
