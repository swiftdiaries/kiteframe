"""Structural provider contracts at the Python adapter boundary."""

from typing import Protocol, runtime_checkable

from kiteframe._native import (
    AdmissionRequest,
    CatalogFetchResult,
    CapabilityGrantSet,
    CatalogRequest,
    InvocationOutcome,
    InvocationRequest,
    InvocationStatus,
    ResolvedCapabilityRequirement,
    StatusRequest,
)


@runtime_checkable
class CatalogProvider(Protocol):
    async def catalog(self, request: CatalogRequest) -> CatalogFetchResult: ...


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

    async def status(
        self,
        request: StatusRequest,
        requirement: ResolvedCapabilityRequirement,
    ) -> InvocationStatus: ...


__all__ = [
    "AdmissionProvider",
    "CapabilityInvoker",
    "CatalogProvider",
]
