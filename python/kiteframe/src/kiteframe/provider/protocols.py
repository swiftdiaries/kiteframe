"""Structural provider contracts at the Python adapter boundary."""

from typing import Protocol, TypeVar, runtime_checkable

from kiteframe._native import (
    AdmissionRequest,
    CapabilityCatalog,
    CapabilityGrantSet,
    CatalogRequest,
    InvocationOutcome,
    InvocationRequest,
    InvocationStatus,
)

AuditRecordT_contra = TypeVar("AuditRecordT_contra", contravariant=True)
DurableAuditReceiptT_co = TypeVar("DurableAuditReceiptT_co", covariant=True)


@runtime_checkable
class CatalogProvider(Protocol):
    async def catalog(self, request: CatalogRequest) -> CapabilityCatalog: ...


@runtime_checkable
class AdmissionProvider(Protocol):
    async def admit(
        self,
        request: AdmissionRequest,
    ) -> CapabilityGrantSet: ...


@runtime_checkable
class CapabilityInvoker(Protocol):
    async def invoke(
        self,
        request: InvocationRequest,
    ) -> InvocationOutcome: ...

    async def status(self, invocation_id: str) -> InvocationStatus: ...


@runtime_checkable
class AuditSink(
    Protocol[AuditRecordT_contra, DurableAuditReceiptT_co],
):
    """Typed append boundary for immutable audit values supplied by a backend."""

    async def append(
        self,
        record: AuditRecordT_contra,
    ) -> DurableAuditReceiptT_co: ...


__all__ = [
    "AdmissionProvider",
    "AuditSink",
    "CapabilityInvoker",
    "CatalogProvider",
]
