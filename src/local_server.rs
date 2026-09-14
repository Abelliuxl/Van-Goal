use crate::hermes_config::hermes_executable_path;
use crate::log_debug;
use crate::logger::global_logger;
use anyhow::{anyhow, Result};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::process::Command;

/// Manages the local `hermes serve` subprocess: probe, start, wait for ready.
#[derive(Default)]
pub struct LocalHermesServer {
    child: Arc<Mutex<Option<tokio::process::Child>>>,
    last_message: Mutex<String>,
    is_launching: Mutex<bool>,
}

impl LocalHermesServer {
    fn set_message(&self, message: impl Into<String>) {
        let message = message.into();
        global_logger().log("server", message.clone());
        *self.last_message.lock().unwrap_or_else(|e| e.into_inner()) = message;
    }

    pub fn take_message(&self) -> String {
        self.last_message
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub async fn start(&self, port: u16) {
        if self
            .child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            self.set_message("Hermes server process is already managed by Hermit.");
            return;
        }
        let executable = hermes_executable_path().unwrap_or_else(|| PathBuf::from("hermes"));
        let mut command = Command::new(&executable);
        command
            .args(["serve", "--port", &port.to_string(), "--host", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match command.spawn() {
            Ok(child) => {
                *self.is_launching.lock().unwrap_or_else(|e| e.into_inner()) = true;
                *self.child.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
                self.set_message(format!("Started local Hermes server on 127.0.0.1:{port}."));
            }
            Err(error) => {
                self.set_message(format!("Failed to start Hermes: {error}"));
            }
        }
    }

    pub async fn ensure_running(&self, port: u16) -> Result<String> {
        let url = format!("http://127.0.0.1:{port}");
        if is_reachable(&url).await {
            self.set_message(format!("Using local Hermes on 127.0.0.1:{port}."));
            return Ok(url);
        }
        self.start(port).await;
        let deadline = std::time::Instant::now() + Duration::from_secs(45);
        while std::time::Instant::now() < deadline {
            if is_reachable(&url).await {
                *self.is_launching.lock().unwrap_or_else(|e| e.into_inner()) = false;
                self.set_message("Local Hermes is ready.");
                return Ok(url);
            }
            tokio::time::sleep(Duration::from_millis(350)).await;
        }
        *self.is_launching.lock().unwrap_or_else(|e| e.into_inner()) = false;
        log_debug!("server", "local hermes did not become ready before timeout");
        Err(anyhow!(
            "Local Hermes started but did not become ready in time."
        ))
    }

    pub async fn stop(&self) {
        if let Some(mut child) = self.child.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = child.start_kill();
        }
        *self.is_launching.lock().unwrap_or_else(|e| e.into_inner()) = false;
        self.set_message("Stopped local Hermes server.");
    }
}

async fn is_reachable(url: &str) -> bool {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap_or_default();
    matches!(
        client
            .get(format!("{url}/api/status"))
            .header("Accept", "application/json")
            .send()
            .await,
        Ok(response) if response.status().is_success()
    )
}
