/*
Soroban transaction signing and submission service.
*/
use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use tracing::{debug, error, info, warn};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use ed25519_dalek::{SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};
use stellar_xdr::{
    curr::{DecoratedSignature, Signature, TransactionEnvelope},
    ReadXdr,
};
use std::convert::TryInto;

const MAX_RETRIES: u32 = 3;
const INITIAL_BACKOFF_MS: u64 = 1000;
const BACKOFF_MULTIPLIER: u64 = 2;
const REQUEST_TIMEOUT_SECS: u64 = 30;

/// Stellar secret key version byte
const STELLAR_SECRET_KEY_VERSION: u8 = 48; // '0' in base32

/// Helper to decode a Stellar secret key (starts with 'S')
/// Stellar secret keys are base32-encoded with version byte at the start
fn decode_stellar_secret_key(secret: &str) -> Result<[u8; 32]> {
    if !secret.starts_with('S') {
        return Err(anyhow::anyhow!("Invalid Stellar secret key format: must start with 'S'"));
    }

    // Decode the base32 secret key (without the 'S' prefix)
    let decoded = base32_decode(&secret[1..])?;
    
    if decoded.len() != 33 {
        return Err(anyhow::anyhow!(
            "Invalid Stellar secret key length: expected 33 bytes (32 key + 1 version), got {}",
            decoded.len()
        ));
    }

    // First byte should be version byte
    if decoded[0] != STELLAR_SECRET_KEY_VERSION {
        return Err(anyhow::anyhow!(
            "Invalid Stellar secret key version byte: expected {}, got {}",
            STELLAR_SECRET_KEY_VERSION,
            decoded[0]
        ));
    }

    // Extract the 32-byte key
    Ok(decoded[1..].try_into()?)
}

/// Simple base32 decoder for Stellar keys
/// Stellar uses a custom base32 alphabet
fn base32_decode(input: &str) -> Result<Vec<u8>> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    
    let mut result = Vec::new();
    let mut buffer: u32 = 0;
    let mut bits: u32 = 0;

    for ch in input.chars() {
        if ch == '=' {
            break;
        }
        
        let idx = ALPHABET.iter().position(|&b| b == ch as u8)
            .ok_or_else(|| anyhow::anyhow!("Invalid base32 character: {}", ch))?;
        
        buffer = (buffer << 5) | (idx as u32);
        bits += 5;

        if bits >= 8 {
            bits -= 8;
            result.push(((buffer >> bits) & 0xFF) as u8);
        }
    }

    Ok(result)
}

/// Helper to compute the transaction hash for signing
/// This follows the Stellar transaction envelope hashing convention
fn compute_transaction_hash(network_passphrase: &str, tx_envelope_xdr: &[u8]) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    
    // Hash the network passphrase (Stellar convention)
    let network_hash = {
        let mut nh = Sha256::new();
        nh.update("StellarNetwork\0");
        let mut name = nh.finalize().to_vec();
        name.extend_from_slice(network_passphrase.as_bytes());
        let mut n2 = Sha256::new();
        n2.update(&name);
        n2.finalize()
    };

    hasher.update(&network_hash);
    
    // Hash the transaction envelope XDR
    hasher.update(b"\x00\x00\x00\x02"); // ENVELOPE_TYPE_TX = 2 in XDR format
    hasher.update(tx_envelope_xdr);

    let hash = hasher.finalize();
    Ok(hash.as_slice().try_into()?)
}

/// Configuration for the contract service
#[derive(Clone, Debug)]
pub struct ContractConfig {
    /// Soroban RPC endpoint URL
    pub rpc_url: String,
    /// Contract address (ID) on Stellar
    pub contract_id: String,
    /// Network passphrase (e.g., "Test SDF Network ; September 2015" for testnet)
    pub network_passphrase: String,
    /// Source account secret key for signing transactions
    pub source_secret_key: String,
}

/// Service for interacting with the Soroban snapshot contract
#[derive(Clone)]
pub struct ContractService {
    client: Client,
    config: ContractConfig,
}

/// RPC request structure for Soroban
#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: u64,
    method: String,
    params: serde_json::Value,
}

/// RPC response structure
/// Note: All fields required for JSON deserialization from Stellar RPC
#[derive(Debug, Deserialize)]
struct JsonRpcResponse<T> {
    #[allow(dead_code)] // Required for JSON deserialization
    jsonrpc: String,
    #[allow(dead_code)] // Required for JSON deserialization
    id: u64,
    #[serde(default)]
    result: Option<T>,
    #[serde(default)]
    error: Option<RpcError>,
}

/// RPC error details
/// Note: All fields required for JSON deserialization from Stellar RPC
#[derive(Debug, Deserialize, Clone)]
struct RpcError {
    #[allow(dead_code)] // Required for JSON deserialization
    code: i32,
    message: String,
    #[serde(default)]
    #[allow(dead_code)] // Required for JSON deserialization
    data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmissionResult {
    pub hash: String,
    pub transaction_hash: String,
    pub ledger: u64,
    pub timestamp: u64,
}

impl ContractService {
    #[must_use]
    pub fn new(config: ContractConfig) -> Self {
        // SAFETY: reqwest::ClientBuilder only fails when TLS is misconfigured.
        // With only a timeout set this is infallible; panicking at startup is
        // the correct behaviour for an unconfigurable HTTP stack.
        #[allow(clippy::expect_used)]
        let client = Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .expect("Failed to build HTTP client");
        Self { client, config }
    }

    /// Create from environment variables
    pub fn from_env() -> Result<Self> {
        let config = ContractConfig {
            rpc_url: std::env::var("SOROBAN_RPC_URL")
                .unwrap_or_else(|_| "https://soroban-testnet.stellar.org".to_string()),
            contract_id: std::env::var("SNAPSHOT_CONTRACT_ID")
                .context("SNAPSHOT_CONTRACT_ID environment variable not set")?,
            network_passphrase: std::env::var("STELLAR_NETWORK_PASSPHRASE")
                .unwrap_or_else(|_| "Test SDF Network ; September 2015".to_string()),
            source_secret_key: std::env::var("STELLAR_SOURCE_SECRET_KEY")
                .context("STELLAR_SOURCE_SECRET_KEY environment variable not set")?,
        };

        Ok(Self::new(config))
    }

    pub async fn submit_snapshot(&self, hash: [u8; 32], epoch: u64) -> Result<SubmissionResult> {
        self.submit_snapshot_hash(hash, epoch).await
    }

    /// Submit a snapshot hash to the on-chain contract
    ///
    /// This function will:
    /// 1. Build and simulate the transaction
    /// 2. Sign the transaction
    /// 3. Submit to the network
    /// 4. Wait for confirmation
    /// 5. Retry on transient failures
    ///
    /// # Arguments
    /// * `hash` - 32-byte snapshot hash
    /// * `epoch` - Epoch identifier
    ///
    /// # Returns
    /// Result containing submission details or error
    pub async fn submit_snapshot_hash(
        &self,
        hash: [u8; 32],
        epoch: u64,
    ) -> Result<SubmissionResult> {
        info!(
            "Submitting snapshot hash for epoch {}: {}",
            epoch,
            hex::encode(hash)
        );

        let mut attempt = 0;
        let mut backoff_ms = INITIAL_BACKOFF_MS;

        loop {
            attempt += 1;

            match self.try_submit_snapshot(hash, epoch).await {
                Ok(result) => {
                    info!(
                        "✓ Successfully submitted snapshot for epoch {} (tx: {}, ledger: {})",
                        epoch, result.transaction_hash, result.ledger
                    );
                    return Ok(result);
                }
                Err(e) => {
                    if attempt >= MAX_RETRIES {
                        error!(
                            "✗ Failed to submit snapshot for epoch {} after {} attempts: {}",
                            epoch, MAX_RETRIES, e
                        );
                        return Err(e).context(format!(
                            "Failed to submit snapshot after {MAX_RETRIES} retries"
                        ));
                    }

                    warn!(
                        "Attempt {}/{} failed for epoch {}: {}. Retrying in {}ms...",
                        attempt, MAX_RETRIES, epoch, e, backoff_ms
                    );

                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                    backoff_ms *= BACKOFF_MULTIPLIER;
                }
            }
        }
    }

    /// Single attempt to submit snapshot (without retry logic)
    async fn try_submit_snapshot(&self, hash: [u8; 32], epoch: u64) -> Result<SubmissionResult> {
        // Step 1: Build the contract invocation
        debug!("Building contract invocation for epoch {}", epoch);
        let invoke_args = self.build_invoke_args(hash, epoch)?;

        // Step 2: Simulate the transaction
        debug!("Simulating transaction");
        let simulated = self.simulate_transaction(&invoke_args).await?;

        // Step 3: Prepare and sign the transaction
        debug!("Preparing and signing transaction");
        let signed_xdr = self.prepare_and_sign_transaction(&simulated)?;

        // Step 4: Send the transaction
        debug!("Sending transaction to network");
        let tx_hash = self.send_transaction(&signed_xdr).await?;

        // Step 5: Wait for transaction confirmation
        debug!("Waiting for transaction confirmation: {}", tx_hash);
        let result = self.wait_for_transaction(&tx_hash, epoch).await?;

        Ok(result)
    }

    /// Build contract invocation arguments
    fn build_invoke_args(&self, hash: [u8; 32], epoch: u64) -> Result<serde_json::Value> {
        // Convert hash to hex for the contract call
        let hash_hex = hex::encode(hash);

        // Build Soroban contract invocation parameters
        // Format: invoke contract_id submit_snapshot [hash_bytes, epoch_u64]
        Ok(json!({
            "contractId": self.config.contract_id,
            "function": "submit_snapshot",
            "args": [
                {
                    "type": "bytes",
                    "value": hash_hex
                },
                {
                    "type": "u64",
                    "value": epoch.to_string()
                }
            ]
        }))
    }

    /// Simulate the transaction to get resource estimates
    async fn simulate_transaction(
        &self,
        invoke_args: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 1,
            method: "simulateTransaction".to_string(),
            params: json!({
                "transaction": invoke_args
            }),
        };

        let response = self
            .client
            .post(&self.config.rpc_url)
            .json(&request)
            .send()
            .await
            .context("Failed to send simulation request")?;

        let status = response.status();
        let body: JsonRpcResponse<serde_json::Value> = response
            .json()
            .await
            .context("Failed to parse simulation response")?;

        if let Some(error) = body.error {
            return Err(anyhow::anyhow!(
                "Transaction simulation failed: {} (code: {})",
                error.message,
                error.code
            ));
        }

        body.result
            .ok_or_else(|| anyhow::anyhow!("No simulation result returned (status: {status})"))
    }

    /// Prepare and sign the transaction using the Soroban RPC simulation result.
    ///
    /// The simulation response contains a `transactionData` field with the
    /// assembled XDR that already includes resource estimates. The RPC layer
    /// handles authorization via the source account configured in the node;
    /// full client-side keypair signing can be layered on top once a
    /// Soroban-compatible Rust SDK is stabilised.
    fn prepare_and_sign_transaction(&self, simulated: &serde_json::Value) -> Result<String> {
        let transaction_xdr = simulated
            .get("transactionData")
            .and_then(|t| t.as_str())
            .ok_or_else(|| anyhow::anyhow!("Simulation did not return transaction data"))?;

        // Validate the XDR is non-empty base64
        if transaction_xdr.is_empty() {
            return Err(anyhow::anyhow!("Simulation returned empty transactionData"));
        }

        debug!("Decoding XDR from simulation response ({} chars)", transaction_xdr.len());

        // Step 1: Decode the base64 XDR
        let xdr_bytes = BASE64
            .decode(transaction_xdr)
            .context("Failed to decode base64 XDR from simulation")?;

        debug!("XDR decoded to {} bytes", xdr_bytes.len());

        // Step 2: Parse the transaction envelope
        let mut envelope = TransactionEnvelope::from_xdr(&xdr_bytes)
            .map_err(|e| anyhow::anyhow!("Failed to parse transaction XDR: {:?}", e))
            .context("Invalid transaction envelope XDR")?;

        debug!("Transaction envelope parsed successfully");

        // Step 3: Decode the secret key and create signing key
        let secret_key_bytes = decode_stellar_secret_key(&self.config.source_secret_key)
            .context("Failed to decode source secret key")?;
        
        let signing_key = SigningKey::from_bytes(&secret_key_bytes);
        let verifying_key: VerifyingKey = (&signing_key).into();

        debug!(
            "Keypair loaded (public key: {})",
            hex::encode(verifying_key.to_bytes())
        );

        // Step 4: Compute transaction hash for signing
        let mut tx_hasher = Sha256::new();
        
        // Hash network passphrase first (Stellar convention)
        let mut network_id_hasher = Sha256::new();
        network_id_hasher.update(b"StellarNetwork\0");
        let mut network_id_data = network_id_hasher.finalize().to_vec();
        network_id_data.extend_from_slice(self.config.network_passphrase.as_bytes());
        
        let mut network_id = Sha256::new();
        network_id.update(&network_id_data);
        let network_hash = network_id.finalize();

        tx_hasher.update(&network_hash[..]);
        tx_hasher.update(&[0, 0, 0, 2]); // ENVELOPE_TYPE_TX = 2

        // Hash the transaction envelope XDR
        let envelope_xdr = envelope.to_xdr()
            .map_err(|e| anyhow::anyhow!("Failed to re-encode transaction envelope: {:?}", e))?;
        tx_hasher.update(&envelope_xdr);

        let tx_hash: [u8; 32] = tx_hasher.finalize().as_slice().try_into()?;

        debug!("Transaction hash computed: {}", hex::encode(&tx_hash));

        // Step 5: Sign the transaction hash
        let signature = signing_key.sign(&tx_hash);
        let sig_bytes: [u8; 64] = signature.to_bytes();

        debug!("Transaction signed with Ed25519 signature");

        // Step 6: Add signature to envelope
        match &mut envelope {
            TransactionEnvelope::V1(e) => {
                // Compute signature hint from public key (last 4 bytes)
                let public_bytes = verifying_key.to_bytes();
                let hint_slice: [u8; 4] = public_bytes[28..32].try_into()?;
                
                let decorated_sig = DecoratedSignature {
                    hint: hint_slice,
                    signature: Signature(sig_bytes),
                };

                e.signatures.push(decorated_sig);
                debug!("Signature added to transaction envelope (total signatures: {})", e.signatures.len());
            }
            _ => {
                return Err(anyhow::anyhow!("Unsupported transaction envelope version"));
            }
        }

        // Step 7: Re-encode to base64
        let final_xdr = envelope.to_xdr()
            .map_err(|e| anyhow::anyhow!("Failed to encode signed transaction: {:?}", e))?;
        let signed_xdr = BASE64.encode(&final_xdr);

        debug!("Signed transaction XDR re-encoded to base64 ({} chars)", signed_xdr.len());
        info!("Transaction successfully signed and prepared for submission");

        Ok(signed_xdr)
    }

    async fn send_transaction(&self, signed_xdr: &str) -> Result<String> {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 1,
            method: "sendTransaction".to_string(),
            params: json!({ "transaction": signed_xdr }),
        };

        let response = self
            .client
            .post(&self.config.rpc_url)
            .json(&request)
            .send()
            .await
            .context("Failed to send sendTransaction RPC request")?;

        let body: JsonRpcResponse<serde_json::Value> = response
            .json()
            .await
            .context("Failed to parse sendTransaction RPC response")?;

        if let Some(error) = body.error {
            return Err(anyhow::anyhow!(
                "sendTransaction failed: {} (code: {})",
                error.message,
                error.code
            ));
        }

        let result = body
            .result
            .ok_or_else(|| anyhow::anyhow!("sendTransaction returned empty result"))?;

        result
            .get("hash")
            .or_else(|| result.get("transactionHash"))
            .and_then(|h| h.as_str())
            .map(std::string::ToString::to_string)
            .context("sendTransaction result missing transaction hash")
    }

    async fn wait_for_transaction(&self, tx_hash: &str, epoch: u64) -> Result<SubmissionResult> {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 1,
            method: "getTransaction".to_string(),
            params: json!({ "hash": tx_hash }),
        };

        for _ in 0..60 {
            let response = self
                .client
                .post(&self.config.rpc_url)
                .json(&request)
                .send()
                .await
                .context("Failed to send getTransaction RPC request")?;

            let body: JsonRpcResponse<serde_json::Value> = response
                .json()
                .await
                .context("Failed to parse getTransaction RPC response")?;

            if let Some(error) = &body.error {
                let transient = error.message.to_ascii_lowercase().contains("not found");
                if !transient {
                    return Err(anyhow::anyhow!(
                        "getTransaction failed: {} (code: {})",
                        error.message,
                        error.code
                    ));
                }
            } else if let Some(result) = body.result {
                let status = result.get("status").and_then(|s| s.as_str()).unwrap_or("");
                if status.eq_ignore_ascii_case("success") || status.eq_ignore_ascii_case("failed") {
                    let ledger = result
                        .get("ledger")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    let timestamp = result
                        .get("createdAt")
                        .and_then(|s| s.as_str())
                        .and_then(|s| {
                            chrono::DateTime::parse_from_rfc3339(s)
                                .ok()
                                .map(|d| d.timestamp() as u64)
                        })
                        .unwrap_or(0);

                    return Ok(SubmissionResult {
                        hash: tx_hash.to_string(),
                        transaction_hash: tx_hash.to_string(),
                        ledger,
                        timestamp,
                    });
                }
            }

            tokio::time::sleep(Duration::from_millis(250)).await;
        }

        Err(anyhow::anyhow!(
            "Timed out waiting for transaction {tx_hash} (epoch {epoch})"
        ))
    }

    pub async fn health_check(&self) -> Result<bool> {
        Ok(false)
    }

    pub async fn verify_snapshot_exists(&self, _hash: &str, _ledger: u64) -> Result<bool> {
        Err(anyhow::anyhow!("Contract service is temporarily disabled"))
    }

    pub async fn get_snapshot_by_epoch(&self, _epoch: u64) -> Result<Option<String>> {
        Err(anyhow::anyhow!("Contract service is temporarily disabled"))
    }
}
