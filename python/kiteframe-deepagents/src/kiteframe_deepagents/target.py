"""Static metadata for the pinned Deep Agents runtime target."""

from __future__ import annotations

import argparse
import json
from collections.abc import Sequence
from pathlib import Path

from .compatibility import (
    DEEPAGENTS_VERSION,
    EXPECTED_CREATE_DEEP_AGENT_PARAMETERS,
)

TARGET = "deepagents"
DEEPAGENTS_UPSTREAM_COMMIT = "196a0870fcf8a7f29d1fb37886dd323b190f9c16"
SUPPORTED_FEATURES = frozenset(
    {
        "kiteframe.runtime.deepagents.public-create@1",
        "kiteframe.capability.point-of-use-auth@1",
        "kiteframe.capability.dynamic-visibility@1",
        "kiteframe.capability.deferred@1",
        "kiteframe.capability.suspendable@1",
        "kiteframe.delegation.narrowing@1",
    }
)


def target_metadata() -> dict[str, object]:
    """Return the generated, reviewable metadata for this adapter build."""

    return {
        "createDeepAgentParameters": list(EXPECTED_CREATE_DEEP_AGENT_PARAMETERS),
        "deepagentsVersion": DEEPAGENTS_VERSION,
        "supportedFeatures": sorted(SUPPORTED_FEATURES),
        "target": TARGET,
        "upstreamCommit": DEEPAGENTS_UPSTREAM_COMMIT,
    }


def render_target_metadata() -> bytes:
    """Render deterministic target metadata with a trailing file newline."""

    return (
        json.dumps(
            target_metadata(),
            ensure_ascii=False,
            indent=2,
            sort_keys=True,
        )
        + "\n"
    ).encode()


def check_target_metadata(path: Path) -> None:
    """Fail when checked-in target metadata differs from the generated value."""

    if path.read_bytes() != render_target_metadata():
        raise RuntimeError(f"runtime target metadata is stale: {path}")


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", type=Path, required=True)
    arguments = parser.parse_args(argv)
    check_target_metadata(arguments.check)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())


__all__ = [
    "DEEPAGENTS_UPSTREAM_COMMIT",
    "EXPECTED_CREATE_DEEP_AGENT_PARAMETERS",
    "SUPPORTED_FEATURES",
    "TARGET",
    "check_target_metadata",
    "render_target_metadata",
    "target_metadata",
]
