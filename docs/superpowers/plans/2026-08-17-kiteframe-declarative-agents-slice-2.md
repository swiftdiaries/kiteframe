# Superseded: Kiteframe Declarative Agents Slice 2 Plan

- **Status:** Superseded before implementation
- **Superseded by:**
  `docs/superpowers/plans/2026-08-17-kiteframe-declarative-agents-slice-1.md`
- **Reason:** The original Slice 1 and Slice 2 plans described one foundation
  through temporary intermediate states. Keeping them separate required agent
  publication to fail and then become available immediately afterward, and it
  extended and renamed the compiled IR in two passes.

Do not execute this file as an implementation plan. The replacement foundation
plan lands agent syntax, the final versioned `CompiledDefinition` shape,
immutable tagged publication, canonical `UserTask`, and immutable
`GoalContract` together. It preserves the real safety boundary: agent
definitions can be published and selected by inert accepted tasks, while the
legacy `StartRun` path rejects agents before creating execution state.

## Traceability

Every contract from the former plan remains in the replacement:

| Former Slice 2 work | Replacement foundation task |
|---|---|
| Canonical task request, validation ceilings, and reference fixture | Task 1 and Task 5 |
| Closed `GoalContract`, `UserTask`, target, preference, and selector types | Task 5 |
| `CompiledWorkflow` rename and tagged definition persistence | Task 4 and Task 6 |
| Immutable task and goal migration | Task 6 |
| Atomic exact-version selection and idempotent replay | Task 6 |
| Immutable agent publication and workflow-only run start | Task 7 |
| ACP v1/v2, NDJSON, and WebSocket submit/get methods | Task 8 |
| Restart, isolation, failure atomicity, and non-authority evidence | Task 9 |
| Rust, PostgreSQL, Python, TypeScript, and documentation gates | Task 9 |

The replacement retains all former normative limits, wire shapes, stable error
codes, persistence invariants, compatibility requirements, and acceptance
evidence. It removes only the temporary compile-only publication rejection,
the repeated IR transition, and duplicated plan prose.

The next active planning boundary after the replacement Slice 1 is
consolidated Slice 2: authoritative goal lifecycle and the agent-runtime
transition contract.
