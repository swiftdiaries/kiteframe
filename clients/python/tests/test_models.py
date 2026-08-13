from __future__ import annotations

from kiteframe import (
    CapabilityTarget,
    OneOfPrimitive,
    PolicyRule,
    SessionPolicy,
    ToolTarget,
)


def test_policy_serializes_to_the_rust_tagged_dto() -> None:
    policy = SessionPolicy(
        version=1,
        rules=(
            PolicyRule(
                rule_id="allow-reads",
                effect="permit",
                target=CapabilityTarget(names=("read_data",)),
            ),
            PolicyRule(
                rule_id="deny-shell-outside-project",
                effect="forbid",
                target=ToolTarget(names=("workspace/run_shell",)),
                input={"/root": OneOfPrimitive(("src", "tests", 3, True, None))},
            ),
        ),
    )

    assert policy.to_json() == {
        "version": 1,
        "rules": [
            {
                "rule_id": "allow-reads",
                "effect": "permit",
                "target": {"type": "capability", "names": ["read_data"]},
                "input": {},
            },
            {
                "rule_id": "deny-shell-outside-project",
                "effect": "forbid",
                "target": {"type": "tool", "names": ["workspace/run_shell"]},
                "input": {"/root": ["src", "tests", 3, True, None]},
            },
        ],
    }
