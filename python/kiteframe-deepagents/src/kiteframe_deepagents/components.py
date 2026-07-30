"""Resolution-only validation of deployment-owned runtime components."""

from __future__ import annotations

import json
import posixpath
from dataclasses import dataclass
from pathlib import PurePosixPath
from typing import Literal, Protocol, runtime_checkable

from deepagents.backends import BackendProtocol, StateBackend
from deepagents.backends.protocol import (
    EditResult,
    FileDownloadResponse,
    FileInfo,
    FileUploadResponse,
    GlobResult,
    GrepResult,
    LsResult,
    ReadResult,
    WriteResult,
)
from kiteframe import (
    CompilationReport,
    ComponentKind,
    FrozenComponentRegistry,
    KiteframeDiagnosticError,
    ResolvedRuntimeInputs,
    ResolvedTextAsset,
)
from kiteframe.provider import CapabilityInvoker
from kiteframe.registry import ComponentUnresolvedError
from langchain.agents.middleware import AgentMiddleware
from langchain_core.runnables import RunnableConfig
from langgraph.checkpoint.base import CheckpointTuple
from langgraph.store.base import BaseStore

from .compatibility import (
    AMBIENT_TOOL_NAMES,
    DEEPAGENTS_VERSION,
    KiteframeHarnessProfileToken,
)
from .suspension import EvidenceResumeCredentialVerifier

RUNTIME_COMPONENT_UNRESOLVED = "KF-RUNTIME-001"
RUNTIME_CONSTRUCTION_FAILED = "KF-RUNTIME-002"
PACKAGE_PREFIX = "/__kiteframe__"


@runtime_checkable
class CheckpointerProtocol(Protocol):
    """The public async operation required from a configured checkpointer."""

    async def aget_tuple(
        self,
        config: RunnableConfig,
    ) -> CheckpointTuple | None: ...


@runtime_checkable
class DurableCheckpointer(
    CheckpointerProtocol,
    EvidenceResumeCredentialVerifier,
    Protocol,
):
    """The additional restart-safe attestation required for suspension."""

    kiteframe_durable: Literal[True]


@runtime_checkable
class AuditSink(Protocol):
    """Deployment-owned append-only audit boundary."""

    async def append(self, record: object) -> object: ...


@dataclass(frozen=True, slots=True)
class ValidatedPackageBackend(BackendProtocol):
    """Read-only validated package assets over a deployment runtime backend."""

    runtime_backend: BackendProtocol
    _assets: tuple[tuple[str, str], ...]
    _skill_assets: tuple[tuple[str, str], ...]
    _skill_sources: tuple[str, ...]

    def __post_init__(self) -> None:
        if not isinstance(self.runtime_backend, BackendProtocol):
            raise TypeError("runtime_backend must implement BackendProtocol")
        paths = tuple(path for path, _text in self._assets)
        if len(paths) != len(set(paths)):
            raise ValueError("validated package asset paths must be unique")
        if any(
            not path.startswith(f"{PACKAGE_PREFIX}/")
            for path in paths
        ):
            raise ValueError("validated package assets must use virtual paths")

    def skill_sources(
        self,
        skills: tuple[ResolvedTextAsset, ...],
    ) -> list[str]:
        """Return virtual sources only for the exact validated skill snapshot."""

        if tuple((asset.path, asset.text) for asset in skills) != (
            self._skill_assets
        ):
            raise ValueError("skill assets do not match the validated snapshot")
        return list(self._skill_sources)

    @staticmethod
    def _is_package_path(path: str) -> bool:
        normalized = posixpath.normpath(f"/{path.lstrip('/')}")
        return normalized == PACKAGE_PREFIX or normalized.startswith(
            f"{PACKAGE_PREFIX}/"
        )

    def _content(self, path: str) -> str | None:
        return next(
            (content for candidate, content in self._assets if candidate == path),
            None,
        )

    def ls(self, path: str) -> LsResult:
        if not self._is_package_path(path):
            return self.runtime_backend.ls(path)
        normalized = path.rstrip("/")
        prefix = f"{normalized}/"
        entries: dict[str, FileInfo] = {}
        for asset_path, content in self._assets:
            if not asset_path.startswith(prefix):
                continue
            relative = asset_path[len(prefix) :]
            if "/" in relative:
                directory = relative.split("/", maxsplit=1)[0]
                entry_path = f"{prefix}{directory}/"
                entries[entry_path] = FileInfo(
                    path=entry_path,
                    is_dir=True,
                    size=0,
                    modified_at="",
                )
            elif relative:
                entries[asset_path] = FileInfo(
                    path=asset_path,
                    is_dir=False,
                    size=len(content.encode()),
                    modified_at="",
                )
        return LsResult(
            entries=[entries[key] for key in sorted(entries)]
        )

    def read(
        self,
        file_path: str,
        offset: int = 0,
        limit: int = 2000,
    ) -> ReadResult:
        if not self._is_package_path(file_path):
            return self.runtime_backend.read(file_path, offset, limit)
        content = self._content(file_path)
        if content is None:
            return ReadResult(error="file_not_found")
        lines = content.splitlines(keepends=True)
        sliced = "".join(lines[offset : offset + limit])
        return ReadResult(
            file_data={"content": sliced, "encoding": "utf-8"}
        )

    def grep(
        self,
        pattern: str,
        path: str | None = None,
        glob: str | None = None,
    ) -> GrepResult:
        if path is not None and self._is_package_path(path):
            del pattern, glob
            return GrepResult(error="permission_denied")
        return self.runtime_backend.grep(pattern, path, glob)

    def glob(
        self,
        pattern: str,
        path: str | None = None,
    ) -> GlobResult:
        if path is not None and self._is_package_path(path):
            del pattern
            return GlobResult(error="permission_denied")
        return self.runtime_backend.glob(pattern, path)

    def write(self, file_path: str, content: str) -> WriteResult:
        if self._is_package_path(file_path):
            del content
            return WriteResult(error="permission_denied")
        return self.runtime_backend.write(file_path, content)

    def edit(
        self,
        file_path: str,
        old_string: str,
        new_string: str,
        replace_all: bool = False,
    ) -> EditResult:
        if self._is_package_path(file_path):
            del old_string, new_string, replace_all
            return EditResult(error="permission_denied")
        return self.runtime_backend.edit(
            file_path,
            old_string,
            new_string,
            replace_all,
        )

    def upload_files(
        self,
        files: list[tuple[str, bytes]],
    ) -> list[FileUploadResponse]:
        responses: list[FileUploadResponse] = []
        for path, content in files:
            if self._is_package_path(path):
                responses.append(
                    FileUploadResponse(path=path, error="permission_denied")
                )
            else:
                responses.extend(
                    self.runtime_backend.upload_files([(path, content)])
                )
        return responses

    def download_files(
        self,
        paths: list[str],
    ) -> list[FileDownloadResponse]:
        responses: list[FileDownloadResponse] = []
        for path in paths:
            if not self._is_package_path(path):
                responses.extend(self.runtime_backend.download_files([path]))
                continue
            content = self._content(path)
            if content is None:
                responses.append(
                    FileDownloadResponse(
                        path=path,
                        content=None,
                        error="file_not_found",
                    )
                )
            else:
                responses.append(
                    FileDownloadResponse(
                        path=path,
                        content=content.encode(),
                        error=None,
                    )
                )
        return responses


@dataclass(frozen=True, slots=True)
class ValidatedComponents:
    """The exact registry objects resolved for one immutable input snapshot."""

    models: tuple[tuple[str, str], ...]
    middleware: tuple[AgentMiddleware, ...]
    package_backend: BackendProtocol | None
    checkpointer: CheckpointerProtocol | None
    store: BaseStore | None
    capability_provider: CapabilityInvoker
    audit_sink: AuditSink
    harness_profile: KiteframeHarnessProfileToken
    compilation_report: CompilationReport

    @property
    def primary_model(self) -> str:
        for role, model in self.models:
            if role == "primary":
                return model
        raise _runtime_error("primary model component is unresolved")


def _runtime_error(message: str) -> KiteframeDiagnosticError:
    error = KiteframeDiagnosticError(message)
    # PyO3 exception projections expose these attributes only on native raises.
    setattr(error, "code", RUNTIME_COMPONENT_UNRESOLVED)  # noqa: B010
    setattr(  # noqa: B010
        error,
        "diagnostics_json",
        json.dumps(
            [
                {
                    "category": "runtime",
                    "code": RUNTIME_COMPONENT_UNRESOLVED,
                    "details": {},
                    "help": None,
                    "message": message,
                    "package_path": None,
                    "retry": "never",
                    "severity": "error",
                    "source_range": None,
                    "stage": "compile",
                }
            ],
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=True,
        ).encode(),
    )
    return error


def _construction_error(
    component_symbol: str,
    exception_class: str,
) -> KiteframeDiagnosticError:
    message = (
        f"component {component_symbol} construction failed "
        f"({exception_class})"
    )
    error = KiteframeDiagnosticError(message)
    setattr(error, "code", RUNTIME_CONSTRUCTION_FAILED)  # noqa: B010
    setattr(  # noqa: B010
        error,
        "diagnostics_json",
        json.dumps(
            [
                {
                    "category": "runtime",
                    "code": RUNTIME_CONSTRUCTION_FAILED,
                    "details": {
                        "component": component_symbol,
                        "exceptionClass": exception_class,
                    },
                    "help": None,
                    "message": message,
                    "package_path": None,
                    "retry": "never",
                    "severity": "error",
                    "source_range": None,
                    "stage": "compile",
                }
            ],
            ensure_ascii=False,
            separators=(",", ":"),
            sort_keys=True,
        ).encode(),
    )
    return error


def build_package_backend(
    prompts: tuple[ResolvedTextAsset, ...],
    skills: tuple[ResolvedTextAsset, ...],
    runtime_backend: BackendProtocol | None,
) -> ValidatedPackageBackend:
    """Build a closed virtual view from native validated text assets."""

    if not isinstance(prompts, tuple) or not all(
        isinstance(asset, ResolvedTextAsset) for asset in prompts
    ):
        raise TypeError("prompts must be native ResolvedTextAsset values")
    if not isinstance(skills, tuple) or not all(
        isinstance(asset, ResolvedTextAsset) for asset in skills
    ):
        raise TypeError("skills must be native ResolvedTextAsset values")

    assets: list[tuple[str, str]] = [
        (f"{PACKAGE_PREFIX}/{asset.path}", asset.text) for asset in prompts
    ]
    skill_sources: list[str] = []
    skill_names: set[str] = set()
    for asset in skills:
        path = PurePosixPath(asset.path)
        skill_name = path.parent.name if path.name == "SKILL.md" else path.stem
        if not skill_name or skill_name in skill_names:
            raise ValueError("validated skill names must be unique")
        skill_names.add(skill_name)
        source = f"{PACKAGE_PREFIX}/skills/{skill_name}"
        skill_sources.append(source)
        assets.append(
            (f"{source}/{skill_name}/SKILL.md", asset.text)
        )

    return ValidatedPackageBackend(
        runtime_backend=(
            runtime_backend
            if runtime_backend is not None
            else StateBackend()
        ),
        _assets=tuple(assets),
        _skill_assets=tuple((asset.path, asset.text) for asset in skills),
        _skill_sources=tuple(skill_sources),
    )


def _target_descriptors(
    inputs: ResolvedRuntimeInputs,
) -> dict[str, object]:
    return {descriptor.symbol: descriptor for descriptor in inputs.target_components}


def _require_descriptor(
    descriptors: dict[str, object],
    symbol: str,
    kind: ComponentKind,
) -> object:
    descriptor = descriptors.get(symbol)
    if descriptor is None or getattr(descriptor, "kind", None) != kind.value:
        raise _runtime_error(f"{kind.value} component metadata is unresolved")
    return descriptor


def _resolve(
    registry: FrozenComponentRegistry,
    kind: ComponentKind,
    symbol: str,
) -> object:
    try:
        return registry.resolve(kind, symbol)
    except ComponentUnresolvedError as error:
        raise _runtime_error(f"{kind.value} component is unresolved") from error


def _provider_qualified_model_key(value: str) -> str:
    provider, separator, model_name = value.partition(":")
    if (
        separator != ":"
        or not provider
        or not model_name
        or ":" in model_name
    ):
        raise _runtime_error(
            "model component must use the exact provider:model form"
        )
    return value


def _requires_durable_checkpoint(inputs: ResolvedRuntimeInputs) -> bool:
    return any(
        "suspendable" in requirement.descriptor.execution_modes
        for requirement in inputs.resolved_agent.capability_requirements
    )


def validate_components(
    inputs: ResolvedRuntimeInputs,
    registry: FrozenComponentRegistry,
) -> ValidatedComponents:
    """Resolve and validate every trusted object before graph construction."""

    descriptors = _target_descriptors(inputs)
    binding = inputs.runtime_binding

    models: list[tuple[str, str]] = []
    for role, symbol in binding.model_symbols:
        _require_descriptor(descriptors, symbol, ComponentKind.MODEL)
        model = _resolve(registry, ComponentKind.MODEL, symbol)
        if not isinstance(model, str):
            raise _runtime_error(
                "model component must be a provider:model string"
            )
        model = _provider_qualified_model_key(model)
        models.append((role, model))

    middleware: list[AgentMiddleware] = []
    for symbol in binding.middleware_symbols:
        _require_descriptor(descriptors, symbol, ComponentKind.MIDDLEWARE)
        component = _resolve(registry, ComponentKind.MIDDLEWARE, symbol)
        if not isinstance(component, AgentMiddleware):
            raise _runtime_error("middleware component has an invalid public type")
        middleware.append(component)

    package_backend: BackendProtocol | None = None
    if binding.backend is not None:
        _require_descriptor(descriptors, binding.backend, ComponentKind.BACKEND)
        candidate = _resolve(registry, ComponentKind.BACKEND, binding.backend)
        if not isinstance(candidate, BackendProtocol):
            raise _runtime_error("backend component has an invalid public type")
        package_backend = candidate

    checkpointer: CheckpointerProtocol | None = None
    checkpointer_descriptor: object | None = None
    if binding.checkpointer is not None:
        checkpointer_descriptor = _require_descriptor(
            descriptors,
            binding.checkpointer,
            ComponentKind.CHECKPOINTER,
        )
        candidate = _resolve(
            registry,
            ComponentKind.CHECKPOINTER,
            binding.checkpointer,
        )
        if not isinstance(candidate, CheckpointerProtocol):
            raise _runtime_error(
                "checkpointer component has an invalid public type"
            )
        checkpointer = candidate

    requires_durable = _requires_durable_checkpoint(inputs)
    descriptor_is_durable = (
        checkpointer_descriptor is not None
        and getattr(checkpointer_descriptor, "durable", False) is True
    )
    if requires_durable and (
        checkpointer is None
        or not isinstance(checkpointer, DurableCheckpointer)
        or checkpointer.kiteframe_durable is not True
        or not descriptor_is_durable
    ):
        raise _runtime_error(
            "suspendable capability requires a durable checkpointer"
        )

    store: BaseStore | None = None
    capture = binding.content_capture
    if capture is not None and capture.enabled:
        symbol = capture.encrypted_content_store
        _require_descriptor(
            descriptors,
            symbol,
            ComponentKind.ENCRYPTED_CONTENT_STORE,
        )
        candidate = _resolve(
            registry,
            ComponentKind.ENCRYPTED_CONTENT_STORE,
            symbol,
        )
        if not isinstance(candidate, BaseStore):
            raise _runtime_error("store component has an invalid public type")
        store = candidate

    _require_descriptor(
        descriptors,
        binding.capability_provider,
        ComponentKind.CAPABILITY_PROVIDER,
    )
    capability_provider = _resolve(
        registry,
        ComponentKind.CAPABILITY_PROVIDER,
        binding.capability_provider,
    )
    if not isinstance(capability_provider, CapabilityInvoker):
        raise _runtime_error(
            "capability provider component has an invalid public type"
        )
    _require_descriptor(
        descriptors,
        binding.audit_sink,
        ComponentKind.AUDIT_SINK,
    )
    audit_sink = _resolve(
        registry,
        ComponentKind.AUDIT_SINK,
        binding.audit_sink,
    )
    if not isinstance(audit_sink, AuditSink):
        raise _runtime_error("audit sink component has an invalid public type")

    profile_symbol = binding.harness_profile
    if profile_symbol is None:
        raise _runtime_error("harness profile component metadata is unresolved")
    _require_descriptor(
        descriptors,
        profile_symbol,
        ComponentKind.HARNESS_PROFILE,
    )
    harness_profile = _resolve(
        registry,
        ComponentKind.HARNESS_PROFILE,
        profile_symbol,
    )
    primary = next(
        (model for role, model in models if role == "primary"),
        None,
    )
    if primary is None:
        raise _runtime_error("primary model component is unresolved")
    expected_profile = KiteframeHarnessProfileToken(
        model_key=primary,
        deepagents_version=DEEPAGENTS_VERSION,
        excluded_tools=AMBIENT_TOOL_NAMES,
        general_purpose_subagent_disabled=True,
    )
    if (
        not isinstance(harness_profile, KiteframeHarnessProfileToken)
        or harness_profile != expected_profile
    ):
        raise _runtime_error("harness profile token does not match the model")

    return ValidatedComponents(
        models=tuple(models),
        middleware=tuple(middleware),
        package_backend=package_backend,
        checkpointer=checkpointer,
        store=store,
        capability_provider=capability_provider,
        audit_sink=audit_sink,
        harness_profile=harness_profile,
        compilation_report=inputs.compilation_report,
    )


__all__ = [
    "AuditSink",
    "CheckpointerProtocol",
    "DurableCheckpointer",
    "ValidatedPackageBackend",
    "ValidatedComponents",
    "build_package_backend",
    "validate_components",
]
