from dataclasses import dataclass
from typing import Any


class HarnessError(RuntimeError):
    pass


@dataclass(frozen=True)
class HarnessResult:
    kind: str
    value: dict[str, Any]
