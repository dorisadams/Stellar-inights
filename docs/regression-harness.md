# Multi-layer regression harness

This harness runs a single representative scenario — submitting and
confirming a Soroban contract transaction — across the backend, frontend,
and mobile layers in one pass, so a regression in any layer's RPC/contract
flow is caught together rather than discovered separately. It implements
issue #73 / #99.

## What it runs

`scripts/run-regression-harness.sh` runs three test suites in sequence,
all anchored on the same scenario via the fixtures in
[`fixtures/contract-flow.json`](../fixtures/contract-flow.json):

1. **Backend** — `cargo test --test stellar_rpc_integration_test`: the
   Stellar RPC client against mock nodes, plus a real unreachable-node
   failure case.
2. **Frontend** — `vitest run src/__tests__/contractSubmission.test.ts`:
   contract simulation, submission, retries, and confirmation polling.
3. **Mobile** — `jest src/hooks/__tests__/useOfflineQueue.contractFlow.test.ts`:
   offline queueing, auto-sync on reconnect, and manual retry of a failed
   submission.

Each layer is independent (its tests do not call into the other layers'
processes), but all three assert against the same ledger sequence and
transaction hash from the shared fixture, so the harness as a whole is
checking one coherent end-to-end scenario rather than three unrelated ones.
See [docs/integration-testing.md](./integration-testing.md) for the
per-layer test coverage in detail.

## Running it locally

```bash
bash scripts/run-regression-harness.sh
```

The script prints a `[PASS]`/`[FAIL]` line per layer and a summary count,
and exits non-zero if any layer regressed — suitable for both local use
and CI.

## CI integration

The `regression-harness` job in
[`.github/workflows/ci-quality.yml`](../.github/workflows/ci-quality.yml)
runs this script on every push and pull request to `main`/`develop`, and is
included in the `ci-quality-gate` job's required checks, alongside the
existing per-layer coverage, lint, and smoke-test jobs.

## Extending the harness

To add another scenario to the regression harness:

1. Add the new fixture data to `fixtures/contract-flow.json` (or a new
   fixture file under `fixtures/`, documented in
   `docs/integration-testing.md`).
2. Add the corresponding test(s) in each affected layer.
3. Add a `run_check` line to `scripts/run-regression-harness.sh` for any
   new test command.
