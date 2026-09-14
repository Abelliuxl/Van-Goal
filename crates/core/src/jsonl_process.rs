use anyhow::{anyhow, Result};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::mpsc::UnboundedSender;

pub enum ProcessOutput {
    Line(String),
    Exit(i32),
}

#[cfg(unix)]
const SIGINT: i32 = 2;
#[cfg(unix)]
const SIGTERM: i32 = 15;

struct TransportInner {
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    running: Arc<AtomicBool>,
    pid: Arc<AtomicU32>,
}

/// JSONL subprocess transport for the Codex / Claude Code / Pi CLIs.
/// Line-delimited JSON in, line-delimited JSON out; stderr is logged.
pub struct JsonlProcessTransport {
    inner: Arc<std::sync::Mutex<Option<TransportInner>>>,
    generation: AtomicU64,
}

impl Default for JsonlProcessTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl JsonlProcessTransport {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(None)),
            generation: AtomicU64::new(0),
        }
    }

    pub fn is_running(&self) -> bool {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(inner) => inner.running.load(Ordering::SeqCst),
            None => false,
        }
    }

    pub async fn start(
        &self,
        executable_name: &str,
        arguments: &[String],
        working_directory: Option<&str>,
        output: UnboundedSender<ProcessOutput>,
    ) -> Result<()> {
        self.stop().await;
        let executable = find_executable(executable_name)
            .ok_or_else(|| anyhow!("Could not find the {executable_name} executable."))?;

        let mut command = Command::new(&executable);
        command
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("PATH", augmented_path());
        if let Some(dir) = working_directory {
            if std::path::Path::new(dir).exists() {
                command.current_dir(dir);
            }
        }
        let mut child = command
            .spawn()
            .map_err(|error| anyhow!("Could not start the agent process: {error}"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("agent process stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("agent process stdout unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("agent process stderr unavailable"))?;

        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let running = Arc::new(AtomicBool::new(true));
        let pid = child.id().unwrap_or(0);
        let pid = Arc::new(AtomicU32::new(pid));

        // stdout lines
        tokio::spawn({
            let output = output.clone();
            async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let trimmed = line.trim().to_string();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if output.send(ProcessOutput::Line(trimmed)).is_err() {
                        break;
                    }
                }
            }
        });

        // stderr (logged, not forwarded)
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                crate::log_debug!("agent-process", "{line}");
            }
        });

        // exit: owns the child
        tokio::spawn({
            let output = output.clone();
            let running = running.clone();
            async move {
                let mut child = child;
                let code = match child.wait().await {
                    Ok(status) => status.code().unwrap_or(-1),
                    Err(_) => -1,
                };
                running.store(false, Ordering::SeqCst);
                let _ = output.send(ProcessOutput::Exit(code));
            }
        });

        *self.inner.lock().unwrap_or_else(|e| e.into_inner()) = Some(TransportInner {
            stdin: Arc::new(tokio::sync::Mutex::new(stdin)),
            running,
            pid,
        });
        crate::log_debug!(
            "agent-process",
            "started executable={} args={:?}",
            executable.display(),
            arguments
        );
        let _ = generation;
        Ok(())
    }

    pub async fn send(&self, value: &serde_json::Value) -> Result<()> {
        let mut line = serde_json::to_vec(value)?;
        line.push(b'\n');
        let stdin = {
            let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard
                .as_ref()
                .map(|inner| inner.stdin.clone())
                .ok_or_else(|| anyhow!("The agent process is not running."))?
        };
        let mut stdin = stdin.lock().await;
        stdin.write_all(&line).await?;
        stdin.flush().await?;
        Ok(())
    }

    /// SIGINT the child (Claude Code interrupt semantics).
    pub fn interrupt(&self) {
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(inner) = guard.as_ref() {
            let pid = inner.pid.load(Ordering::SeqCst);
            if pid != 0 {
                signal_pid(pid, SIGINT);
            }
        }
    }

    pub async fn stop(&self) {
        let inner = {
            let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            guard.take()
        };
        if let Some(inner) = inner {
            let pid = inner.pid.load(Ordering::SeqCst);
            if pid != 0 {
                signal_pid(pid, SIGTERM);
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(600);
            while inner.running.load(Ordering::SeqCst) {
                if std::time::Instant::now() >= deadline {
                    if pid != 0 {
                        signal_pid(pid, 9);
                    }
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        }
    }
}

#[cfg(unix)]
fn signal_pid(pid: u32, sig: i32) {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe {
        kill(pid as i32, sig);
    }
}

#[cfg(not(unix))]
fn signal_pid(_pid: u32, _sig: i32) {}

/// Mirror the Swift PATH augmentation so CLIs installed via npm / homebrew /
/// ~/.local are found even when launched from Finder.
pub fn augmented_path() -> String {
    let home = crate::logger::dirs::home();
    let required = [
        home.join(".npm-global/bin"),
        home.join(".local/bin"),
        home.join(".pi/bin"),
        home.join(".mimo/bin"),
        home.join(".opencode/bin"),
        home.join(".hermes/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    let existing = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".into());
    let mut parts: Vec<String> = required
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    parts.extend(existing.split(':').map(str::to_string));
    parts.join(":")
}

pub fn find_executable(name: &str) -> Option<PathBuf> {
    let home = crate::logger::dirs::home();
    let candidates = [
        home.join(format!(".npm-global/bin/{name}")),
        home.join(format!(".local/bin/{name}")),
        home.join(format!(".pi/bin/{name}")),
        home.join(format!(".mimo/bin/{name}")),
        home.join(format!(".opencode/bin/{name}")),
        home.join(format!(".hermes/bin/{name}")),
        PathBuf::from(format!("/opt/homebrew/bin/{name}")),
        PathBuf::from(format!("/usr/local/bin/{name}")),
        PathBuf::from(format!("/usr/bin/{name}")),
    ];
    candidates.into_iter().find(|p| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(p)
                .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        }
        #[cfg(not(unix))]
        {
            p.is_file()
        }
    })
}
