# UserTask and GoalContract reference

This reference freezes the consolidated Slice 1 submission and acceptance
contract for durable user tasks.

GoalContract is immutable after atomic task acceptance.
The authenticated connection supplies the principal.
The accepted task freezes one exact published agent definition.
Harness completion is invocation evidence, not goal completion authority.
Slice 2 owns goal phases, evidence assertions, blockers, and completion commands.

## Validation

| Field | Limit |
|---|---:|
| agent name | 256 UTF-8 bytes |
| repository | 255 characters |
| objective | 4,096 characters |
| success criteria | 1-32 |
| criterion id | 64 ASCII characters |
| criterion statement | 2,048 characters |
| evidence type | 128 ASCII characters |
| constraints | 0-32 entries, 2,048 characters each |
| blocked conditions | 0-32 entries, 2,048 characters each |

Agent names match `^[A-Za-z_][A-Za-z0-9_]*$`. Criterion ids match
`^[a-z][a-z0-9_]{0,63}$`. Evidence types match
`^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$`.

The repository is exactly two non-empty GitHub path segments using ASCII
letters, digits, `.`, `_`, or `-`, with no leading or trailing slash. Text is
trimmed only for validation and stored byte-for-byte. Every JSON object denies
unknown fields.

The kernel allocates the UUID v7 task id. An explicit agent version freezes
that version. An omitted version selects the latest visible version inside the
acceptance transaction. An idempotent replay returns the original selection.
