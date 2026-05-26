//! Symmetric encryption vault for tenant LLM API keys.
//!
//! Keys are encrypted with AES-256-GCM using a master key loaded from the
//! `TENANT_LLM_SECRET_KEY` environment variable (32 raw bytes, base64url-no-pad).
//!
//! Storage format in the database:  `enc:{nonce_b64}.{ciphertext_b64}`
//! Dev-mode (no master key):         `raw:{plaintext}` (logged as a warning)
//!
//! Generate a key:  openssl rand -base64 32

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Key, Nonce,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::rngs::OsRng;
use rand::RngCore;

use crate::error::{GatewayError, GatewayResult};

const RAW_PREFIX: &str = "raw:";
const ENC_PREFIX: &str = "enc:";

#[derive(Clone)]
pub struct KeyVault {
    cipher: Aes256Gcm,
}

impl KeyVault {
    /// Build from `TENANT_LLM_SECRET_KEY` env var. Returns `None` if unset or invalid.
    pub fn from_env() -> Option<Self> {
        let key_b64 = std::env::var("TENANT_LLM_SECRET_KEY").ok()?;
        let key_bytes = URL_SAFE_NO_PAD.decode(key_b64.trim()).ok()?;
        if key_bytes.len() != 32 {
            tracing::error!(
                len = key_bytes.len(),
                "TENANT_LLM_SECRET_KEY must decode to exactly 32 bytes"
            );
            return None;
        }
        let key = Key::<Aes256Gcm>::from_slice(&key_bytes);
        Some(Self {
            cipher: Aes256Gcm::new(key),
        })
    }

    /// Encrypt `plaintext` → database-stored string.
    pub fn encrypt(&self, plaintext: &str) -> GatewayResult<String> {
        let mut nonce_bytes = [0u8; 12];
        // OsRng is a CSPRNG — required for AES-256-GCM; thread_rng() is not cryptographically secure.
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| GatewayError::Internal(format!("vault encrypt: {e}")))?;
        Ok(format!(
            "{ENC_PREFIX}{}.{}",
            URL_SAFE_NO_PAD.encode(nonce_bytes),
            URL_SAFE_NO_PAD.encode(ciphertext)
        ))
    }

    /// Decrypt a stored value produced by `encrypt`.
    pub fn decrypt(&self, stored: &str) -> GatewayResult<String> {
        let inner = stored
            .strip_prefix(ENC_PREFIX)
            .ok_or_else(|| GatewayError::Internal("vault: missing enc: prefix".to_string()))?;
        let (nonce_b64, ct_b64) = inner
            .split_once('.')
            .ok_or_else(|| GatewayError::Internal("vault: malformed stored key".to_string()))?;
        let nonce_bytes = URL_SAFE_NO_PAD
            .decode(nonce_b64)
            .map_err(|e| GatewayError::Internal(format!("vault decode nonce: {e}")))?;
        let ct_bytes = URL_SAFE_NO_PAD
            .decode(ct_b64)
            .map_err(|e| GatewayError::Internal(format!("vault decode ct: {e}")))?;
        let nonce = Nonce::from_slice(&nonce_bytes);
        let plaintext = self
            .cipher
            .decrypt(nonce, ct_bytes.as_slice())
            .map_err(|e| GatewayError::Internal(format!("vault decrypt: {e}")))?;
        String::from_utf8(plaintext)
            .map_err(|e| GatewayError::Internal(format!("vault utf8: {e}")))
    }
}

/// Encode a plaintext key for storage.
/// - If a vault is provided: encrypts it.
/// - Otherwise: wraps with `raw:` prefix (dev mode).
pub fn encode_for_storage(vault: Option<&KeyVault>, plaintext: &str) -> GatewayResult<String> {
    match vault {
        Some(v) => v.encrypt(plaintext),
        None => {
            tracing::warn!(
                "TENANT_LLM_SECRET_KEY is not set — storing LLM API key unencrypted (dev mode only)"
            );
            Ok(format!("{RAW_PREFIX}{plaintext}"))
        }
    }
}

/// Decode a stored key.
/// - `enc:…` → decrypt with vault (vault must be present).
/// - `raw:…` → strip prefix and return plaintext.
pub fn decode_from_storage(vault: Option<&KeyVault>, stored: &str) -> GatewayResult<String> {
    if let Some(inner) = stored.strip_prefix(RAW_PREFIX) {
        return Ok(inner.to_string());
    }
    if stored.starts_with(ENC_PREFIX) {
        let v = vault.ok_or_else(|| {
            GatewayError::Internal(
                "TENANT_LLM_SECRET_KEY required to decrypt stored LLM API key".to_string(),
            )
        })?;
        return v.decrypt(stored);
    }
    Err(GatewayError::Internal(
        "vault: unknown storage prefix".to_string(),
    ))
}
