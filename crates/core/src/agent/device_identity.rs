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

/// What this frontend calls itself on the Gateway.
///
/// The Gateway titles a session after the client that created it, and it reads
/// that name from the handshake rather than from the session key — measured: a
/// session created under the key `agent:main:van-goal:isolation-probe` by a
/// client whose `displayName` was "Van-Goal probe" came back titled
/// **"Van-Goal probe"**. So two frontends sharing one name fill the session
/// list with identical entries (ten of them read "Van-Goal" on the Gateway this
/// was measured against), and picking the wrong one is indistinguishable from a
/// client that crossed two conversations.
///
/// `displayName` is not part of the signed device payload, so naming the
/// frontend here does not disturb an existing device pairing.
#[cfg(any(target_os = "android", target_os = "ios"))]
pub const OPENCLAW_DISPLAY_NAME: &str = "Van-Goal Mobile";
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub const OPENCLAW_DISPLAY_NAME: &str = "Van-Goal Desktop";

/// The namespace a session key this frontend proposes is filed under.
///
/// The rest of a key the client proposes is its own to choose — another client
/// pins the fixed key `agent:main:main` — and this third segment is what the
/// Gateway's own records and logs name the origin by. Keeping it per-frontend
/// means a session whose title has not been set still shows a readable id
/// instead of one that could belong to either client.
#[cfg(any(target_os = "android", target_os = "ios"))]
pub const OPENCLAW_SESSION_NAMESPACE: &str = "van-goal-mobile";
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub const OPENCLAW_SESSION_NAMESPACE: &str = "van-goal-desktop";

/// Stable Ed25519 device identity persisted in Van-Goal's local app data, used to answer
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

    /// Drop the paired-device token for a gateway. The device identity itself is
    /// kept: the gateway still knows the device, so re-pairing does not have to
    /// be approved again from scratch.
    pub fn forget_device_token(gateway: &str) -> Result<bool> {
        openclaw_device_store().remove(&device_token_account(gateway))
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
