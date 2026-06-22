//! Realistic backend/SDK integration tests for the Stellar RPC client.
//!
//! These tests exercise `StellarRpcClient` the way the rest of the backend
//! actually uses it: in mock mode against deterministic Stellar node
//! fixtures (`rpc::mock_stellar`) for the happy paths, and with a real
//! `reqwest` client pointed at an unreachable endpoint to prove that
//! genuine network failures are mapped to `RpcError` rather than panicking
//! or hanging. Several assertions cross-check against
//! `fixtures/contract-flow.json`, the fixture file shared with the
//! frontend and mobile contract-flow test suites (see
//! docs/integration-testing.md).

use std::path::Path;

use serde_json::Value;

use stellar_insights_backend::rpc::error::RpcError;
use stellar_insights_backend::rpc::mock_stellar::{MOCK_LATEST_LEDGER, MOCK_OLDEST_LEDGER};
use stellar_insights_backend::rpc::{Asset, StellarRpcClient};

fn load_contract_flow_fixture() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/contract-flow.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read shared fixture at {path:?}: {e}"));
    serde_json::from_str(&raw).expect("fixtures/contract-flow.json must be valid JSON")
}

// ── mock-node happy paths ────────────────────────────────────────────────────

#[tokio::test]
async fn test_check_health_against_mock_node() {
    let client = StellarRpcClient::new_with_defaults(true);

    let health = client.check_health().await.expect("mock health check");

    assert_eq!(health.status, "healthy");
    assert_eq!(health.latest_ledger, MOCK_LATEST_LEDGER);
    assert_eq!(health.oldest_ledger, MOCK_OLDEST_LEDGER);
}

#[tokio::test]
async fn test_fetch_latest_ledger_matches_shared_fixture() {
    let client = StellarRpcClient::new_with_defaults(true);
    let fixture = load_contract_flow_fixture();
    let expected = &fixture["ledger"];

    let ledger = client.fetch_latest_ledger().await.expect("mock ledger");

    assert_eq!(ledger.sequence, expected["sequence"].as_u64().unwrap());
    assert_eq!(ledger.hash, expected["hash"].as_str().unwrap());
    assert_eq!(
        ledger.previous_hash,
        expected["previousHash"].as_str().unwrap()
    );
    assert_eq!(
        ledger.transaction_count,
        expected["transactionCount"].as_u64().unwrap() as u32
    );
    assert_eq!(
        ledger.operation_count,
        expected["operationCount"].as_u64().unwrap() as u32
    );
    assert_eq!(ledger.closed_at, expected["closedAt"].as_str().unwrap());
    assert_eq!(ledger.base_fee, expected["baseFee"].as_u64().unwrap() as u32);
}

#[tokio::test]
async fn test_fetch_ledgers_pagination_cursor_advances_without_overlap() {
    let client = StellarRpcClient::new_with_defaults(true);

    let first_page = client
        .fetch_ledgers(Some(MOCK_OLDEST_LEDGER), 10, None)
        .await
        .expect("first page");
    assert_eq!(first_page.ledgers.len(), 10);
    let cursor = first_page.cursor.clone().expect("cursor present");

    let second_page = client
        .fetch_ledgers(None, 10, Some(&cursor))
        .await
        .expect("second page");

    let first_sequences: Vec<u64> = first_page.ledgers.iter().map(|l| l.sequence).collect();
    let second_sequences: Vec<u64> = second_page.ledgers.iter().map(|l| l.sequence).collect();
    assert!(
        first_sequences
            .iter()
            .all(|seq| !second_sequences.contains(seq)),
        "paginated pages must not overlap: {first_sequences:?} vs {second_sequences:?}"
    );

    // Paging all the way past the latest mock ledger yields an empty, well-formed page.
    let exhausted = client
        .fetch_ledgers(Some(MOCK_LATEST_LEDGER + 1), 10, None)
        .await
        .expect("exhausted page");
    assert!(exhausted.ledgers.is_empty());
    assert_eq!(exhausted.latest_ledger, MOCK_LATEST_LEDGER);
}

#[tokio::test]
async fn test_fetch_payments_and_account_payments_against_mock_node() {
    let client = StellarRpcClient::new_with_defaults(true);

    let payments = client
        .fetch_payments(5, None)
        .await
        .expect("mock payments");
    assert_eq!(payments.len(), 5);

    let account_payments = client
        .fetch_account_payments("GDEMOACCOUNT0000000000000000000000000000000000000", 3)
        .await
        .expect("mock account payments");
    assert_eq!(account_payments.len(), 3);
}

#[tokio::test]
async fn test_fetch_trades_and_order_book_against_mock_node() {
    let client = StellarRpcClient::new_with_defaults(true);

    let trades = client.fetch_trades(4, None).await.expect("mock trades");
    assert_eq!(trades.len(), 4);

    let selling = Asset {
        asset_type: "native".to_string(),
        asset_code: None,
        asset_issuer: None,
    };
    let buying = Asset {
        asset_type: "credit_alphanum4".to_string(),
        asset_code: Some("USDC".to_string()),
        asset_issuer: Some("GA5ZSEJYB37JRC5AVCIA5MOP4RHTM335X2KGX3IHOJAPP5RE34K4KZVN".to_string()),
    };

    let order_book = client
        .fetch_order_book(&selling, &buying, 3)
        .await
        .expect("mock order book");
    assert_eq!(order_book.bids.len(), 3);
    assert_eq!(order_book.asks.len(), 3);
    assert_eq!(order_book.base.asset_type, "native");
    assert_eq!(order_book.counter.asset_code, Some("USDC".to_string()));
}

// ── real network failure path ────────────────────────────────────────────────

#[tokio::test]
async fn test_unreachable_node_surfaces_network_error() {
    // Make the retry path fast so the test doesn't hang on backoff: a single
    // attempt against a closed local port fails immediately with "connection
    // refused", with no DNS lookup involved.
    std::env::set_var("RPC_MAX_RETRIES", "0");
    std::env::set_var("RPC_INITIAL_BACKOFF_MS", "10");
    std::env::set_var("RPC_REQUEST_TIMEOUT_SECONDS", "1");

    let client = StellarRpcClient::new(
        "http://127.0.0.1:9/rpc".to_string(),
        "http://127.0.0.1:9".to_string(),
        false,
    );

    let result = client.fetch_latest_ledger().await;

    assert!(
        matches!(result, Err(RpcError::NetworkError(_))),
        "expected NetworkError for an unreachable node, got: {result:?}"
    );
}

// ── cross-layer fixture sanity ───────────────────────────────────────────────

#[test]
fn test_contract_flow_fixture_ledger_is_internally_consistent() {
    let fixture = load_contract_flow_fixture();

    let ledger_sequence = fixture["ledger"]["sequence"].as_u64().unwrap();
    let confirmed_ledger = fixture["contractSubmission"]["confirmedLedger"]
        .as_u64()
        .unwrap();

    assert_eq!(
        ledger_sequence, confirmed_ledger,
        "the contract submission fixture must report confirmation in the same \
         ledger the mock RPC node reports as latest, so backend, frontend and \
         mobile tests agree on a single regression scenario"
    );
}
