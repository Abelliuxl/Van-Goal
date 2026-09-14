use crate::secret_store::openclaw_device_store;
use anyhow::{anyhow, Result};
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::json;

pub const OPENCLAW_CLIENT_ID: &str = "gateway-client";
pub const OPENCLAW_CLIENT_MODE: &str = "backend";
pub const OPENCLAW_ROLE: &str = "operator";
pub const OPENCLAW_SCOPES: &[&str] = &["operator.read", "operator.write", "operator.approvals"];
pub const OPENCLAW_PLATFORM: &str = "darwin";
pub const OPENCLAW_DEVICE_FAMILY: &str = "desktop";

/// Stable Ed25519 device identity persisted in Hermit's local app data, used to answer
/// the OpenClaw Gateway connect challenge (port of OpenClawDeviceIdentity).
pub struct OpenClawDeviceIdentity {
    pub device_id: String,
    pub public_key: String,
    signing_key: SigningKey,
}

impl OpenClawDeviceIdentity {
    pub fn load_or_create() -> Result<Self> {
        let store = openclaw_device_store();
        let account = "ed25519-private-key";
        let signing_key = match store.read(account)? {
            Some(encoded) => {
                let raw = base64::engine::general_purpose::STANDARD.decode(encoded)?;
                let bytes: [u8; 32] = raw
                    .as_slice()
                    .try_into()
                    .map_err(|_| anyhow!("Invalid stored OpenClaw device key."))?;
                SigningKey::from_bytes(&bytes)
            }
            None => {
                let mut seed = [0u8; 32];
                use rand::RngCore;
                rand::rngs::OsRng.fill_bytes(&mut seed);
                let key = SigningKey::from_bytes(&seed);
                seed.fill(0);
                store.save(
                    account,
                    &base64::engine::general_purpose::STANDARD.encode(key.to_bytes()),
                )?;
                key
            }
        };

        let public = signing_key.verifying_key().to_bytes();
        let fingerprint = {
            use sha2::{Digest, Sha256};
            let digest = Sha256::digest(public);
            digest
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };

        Ok(Self {
            device_id: fingerprint,
            public_key: base64_url(&public),
            signing_key,
        })
    }

    pub fn signed_connect_device(
        &self,
        nonce: &str,
        signed_at: i64,
        token: &str,
    ) -> serde_json::Value {
        let payload = device_auth_payload(&self.device_id, nonce, signed_at, token);
        let signature = self.signing_key.sign(payload.as_bytes());
        json!({
            "id": self.device_id,
            "publicKey": self.public_key,
            "signature": base64_url(&signature.to_bytes()),
            "signedAt": signed_at,
            "nonce": nonce
        })
    }

    pub fn load_device_token(gateway: &str) -> Result<Option<String>> {
        openclaw_device_store().read(&device_token_account(gateway))
    }

    pub fn save_device_token(gateway: &str, token: &str) -> Result<()> {
        openclaw_device_store().save(&device_token_account(gateway), token)
    }
}

fn device_auth_payload(device_id: &str, nonce: &str, signed_at: i64, token: &str) -> String {
    [
        "v3",
        device_id,
        OPENCLAW_CLIENT_ID,
        OPENCLAW_CLIENT_MODE,
        OPENCLAW_ROLE,
        &OPENCLAW_SCOPES.join(","),
        &signed_at.to_string(),
        token,
        nonce,
        OPENCLAW_PLATFORM,
        OPENCLAW_DEVICE_FAMILY,
    ]
    .join("|")
}

fn device_token_account(gateway: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(gateway.trim().as_bytes());
    let fingerprint = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("operator-device-token-{fingerprint}")
}

fn base64_url(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD
        .encode(data)
        .replace('+', "-")
        .replace('/', "_")
        .replace('=', "")
}

#[cfg(test)]
mod tests {
    use super::{device_auth_payload, device_token_account};

    #[test]
    fn device_auth_matches_openclaw_v3_wire_format() {
        assert_eq!(
            device_auth_payload("device", "nonce", 1234, "token"),
            "v3|device|gateway-client|backend|operator|operator.read,operator.write,operator.approvals|1234|token|nonce|darwin|desktop"
        );
    }

    #[test]
    fn device_tokens_are_scoped_to_the_gateway() {
        assert_eq!(
            device_token_account("ws://127.0.0.1:18789"),
            device_token_account("ws://127.0.0.1:18789")
        );
        assert_ne!(
            device_token_account("ws://127.0.0.1:18789"),
            device_token_account("wss://claw.example.com:18789")
        );
    }
}
