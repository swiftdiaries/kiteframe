# Wave 3R Final Review Fix Report

## Scope

Closed the final whole-branch review finding without adding workflow execution,
effect execution, policy evaluation, or provider-specific portable fields.

## Contract fix

- `InvocationOutcome::validate_against` and
  `InvocationStatus::validate_against` now derive the exact
  `EffectProposal` from the original `InvocationRequest` and locked
  `CapabilityDescriptor`.
- A suspended outcome or status must exact-match the derived proposal digest.
  A mismatch returns the stable `KF-CAP-002` result-invalid diagnostic.
- Durable status validation now requires the original persisted
  `InvocationRequest` in addition to `StatusRequest` and the locked resolved
  requirement. Rust correlates both invocation IDs, revalidates the invocation
  against the descriptor, derives the proposal natively, and compares the
  suspension digest.
- The PyO3 correlated loader, generated stub, Python provider protocol, and
  `ProviderHttpClient.status` all carry that original invocation. Python does
  not reconstruct or compare proposal digests in production.

## Regression coverage

- Rust contract tests reject suspended outcome and status payloads whose only
  semantic mismatch is `proposalDigest`.
- The Rust/PyO3 correlated-loader test proves the exact payload is accepted and
  changing only `proposalDigest` is rejected before projection.
- Python public-loader and provider tests prove the same exact-match boundary
  and `KF-CAP-002` failure before adapter delivery.
- Child projection coverage now exercises the frozen/nonconstructible
  `RuntimeBinding`, `ComponentDescriptor`, `ResolvedTextAsset`,
  `ResolvedModelRequirement`, `ResolvedCapabilityRequirement`, and
  `CapabilityDescriptor` projections, plus the new tuple-valued collections.
- Schema regression coverage asserts
  `ResolvedCapabilityRequirement.resources.minItems == 1` and the shared
  `Sha256Digest` reference for all five locked digest properties.

## TDD evidence

- The initial suspended-outcome test failed because
  `validate_against` returned `Ok(())` for a mismatched proposal digest.
- The initial suspended-status test failed for the same reason.
- The desired durable-status API initially failed to compile because the
  validator did not accept the original invocation.
- The provider test initially failed because `ProviderHttpClient.status`
  accepted only status request plus requirement.
- Each boundary was implemented only after its focused failing test, then
  rerun green.

## Verification

- Focused Rust contract/schema: 58 passed.
- Focused Rust/PyO3 correlated loader: 15 passed.
- Focused Python provider/authentication/immutability/Wave 5: 120 passed.
- Full Python tests: 176 passed.
- Ruff: all checks passed.
- Pyright: 0 errors, 0 warnings, 0 informations.
- `uv run --project python/kiteframe python scripts/verify_wave3r.py`: passed,
  including format, warning-denied Clippy, all-feature workspace tests, schema
  drift, and generated-stub drift.
- `git diff --check`: passed.

The first complete verifier attempt stopped at `cargo fmt --check`; formatting
was applied and the verifier was rerun successfully from the beginning.
