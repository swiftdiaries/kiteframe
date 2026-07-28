"""Adapter-facing provider protocols and strict HTTP transport."""

from .http import (
    PROVIDER_RESPONSE_LIMIT_BYTES,
    ProviderHttpClient,
    ProviderTransportError,
    require_https,
)
from .protocols import (
    AdmissionProvider,
    AuditSink,
    CapabilityInvoker,
    CatalogProvider,
)
from .trace import trace_headers

__all__ = [
    "PROVIDER_RESPONSE_LIMIT_BYTES",
    "AdmissionProvider",
    "AuditSink",
    "CapabilityInvoker",
    "CatalogProvider",
    "ProviderHttpClient",
    "ProviderTransportError",
    "require_https",
    "trace_headers",
]
