"""Adapter-facing provider protocols and strict HTTP transport."""

from .auth import (
    ProviderAuthenticator,
    ProviderAuthRequest,
    ProviderOperation,
)
from .http import (
    PROVIDER_RESPONSE_LIMIT_BYTES,
    ProviderHttpClient,
    ProviderTransportError,
    require_https,
)
from .protocols import (
    AdmissionProvider,
    CapabilityInvoker,
    CatalogProvider,
)
from .trace import trace_headers

__all__ = [
    "PROVIDER_RESPONSE_LIMIT_BYTES",
    "AdmissionProvider",
    "CapabilityInvoker",
    "CatalogProvider",
    "ProviderHttpClient",
    "ProviderAuthRequest",
    "ProviderAuthenticator",
    "ProviderOperation",
    "ProviderTransportError",
    "require_https",
    "trace_headers",
]
