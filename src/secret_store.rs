use crate::logger::dirs;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

#[derive(Default, Deserialize, Serialize)]
struct StoredSecrets {
    values: BTreeMap<String, String>,
}

/// App-local storage for OpenClaw device identity and paired-device tokens.
/// The file is readable only by the current macOS user and never touches Keychain.
pub struct LocalSecretStore {
    path: PathBuf,
}

impl LocalSecretStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn save(&self, account: &str, secret: &str) -> Result<()> {
        let _guard = store_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut stored = self.load()?;
        stored
            .values
            .insert(account.to_string(), secret.to_string());
        self.write(&stored)
    }

    pub fn read(&self, account: &str) -> Result<Option<String>> {
        let _guard = store_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Ok(self.load()?.values.get(account).cloned())
    }

    fn load(&self) -> Result<StoredSecrets> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("Could not read {}", self.path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(StoredSecrets::default())
            }
            Err(error) => {
                Err(error).with_context(|| format!("Could not read {}", self.path.display()))
            }
        }
    }

    fn write(&self, stored: &StoredSecrets) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = self.path.with_extension("tmp");
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(stored)?)?;
        file.sync_all()?;
        std::fs::rename(&temporary, &self.path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

fn store_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub fn openclaw_device_store() -> LocalSecretStore {
    LocalSecretStore::new(dirs::app_support().join("HermitGPUI/openclaw-device-identity.json"))
}
