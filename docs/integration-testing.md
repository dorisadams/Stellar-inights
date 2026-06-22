# Integration testing: Stellar RPC and contract submission flow

This document covers the backend/SDK integration test suite that exercises
the Stellar RPC contract flow across the backend, frontend, and mobile
layers (issue #99 / #60).

## Test coverage strategy

| Layer | File | What it exercises |
|---|---|---|
| Backend | [`backend/tests/stellar_rpc_integration_test.rs`](../backend/tests/stellar_rpc_integration_test.rs) | `StellarRpcClient` in mock mode against the deterministic Stellar node fixtures in `rpc::mock_stellar` (health check, latest ledger, paginated ledgers, payments, account payments, trades, order book), plus a real network call against an unreachable node to confirm connection failures are mapped to `RpcError::NetworkError` instead of panicking or hanging. |
| Frontend | [`frontend/src/__tests__/contractSubmission.test.ts`](../frontend/src/__tests__/contractSubmission.test.ts) | `ContractSubmissionService` (simulate → sign/submit → poll for confirmation), including transient simulation retries, a non-retryable backend rejection, and an on-chain transaction failure surfaced during confirmation polling. |
| Mobile | [`mobile/src/hooks/__tests__/useOfflineQueue.contractFlow.test.ts`](../mobile/src/hooks/__tests__/useOfflineQueue.contractFlow.test.ts) | `useOfflineQueue` enqueuing a contract submission while offline, auto-syncing once connectivity returns, and retaining/retrying a failed submission with correct local queue state updates. |

## Shared fixtures

All three suites read from a single fixture file,
[`fixtures/contract-flow.json`](../fixtures/contract-flow.json), so a single
ledger sequence and transaction hash flow through every layer's assertions
instead of each layer inventing its own disconnected mock data:

- `ledger` mirrors the exact values returned by the backend's
  `mock_stellar::mock_ledger_info()` fixture.
- `contractSubmission` / `contractSubmissionFailure` describe a successful
  and a failing contract call, reused by the frontend submission tests and
  cross-checked against `ledger.sequence` by the backend test.
- `offlineQueueItem` is the request payload the mobile offline queue tests
  enqueue and replay.

If the backend's mock ledger fixture ever changes, update
`fixtures/contract-flow.json` to match — the backend test
(`test_fetch_latest_ledger_matches_shared_fixture`) will fail otherwise.

## Running the suite

```bash
# Backend
cd backend && cargo test --test stellar_rpc_integration_test

# Frontend
cd frontend && npx vitest run src/__tests__/contractSubmission.test.ts

# Mobile
cd mobile && npx jest src/hooks/__tests__/useOfflineQueue.contractFlow.test.ts

# All three layers together
bash scripts/run-regression-harness.sh
```

See [docs/regression-harness.md](./regression-harness.md) for how this suite
fits into the broader multi-layer regression harness and CI.
