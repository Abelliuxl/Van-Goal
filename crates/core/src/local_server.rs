use crate::agent::opencode;
use crate::hermes_config::{hermes_executable_path, is_executable};
use crate::log_debug;
use crate::logger::global_logger;
use crate::settings::BackendKind;
use anyhow::{anyhow, Result};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::process::Command;

/// A local agent server Van-Goal knows how to launch and keep an eye on.
///
/// Each variant is one recipe: where its CLI usually lives, the arguments that
/// put it on a loopback port, and the path that answers once it is listening.
/// A backend with no recipe here is never started by the app — it keeps
/// connecting to a server the user is already running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedServer {
    Hermes,
    MiMoCode,
}

impl ManagedServer {
    /// The server that serves this backend, or `None` when the app does not
    /// start one for it. A backend missing from here keeps its existing
    /// behaviour: Van-Goal connects to an already-running server.
    pub fn for_kind(kind: BackendKind) -> Option<Self> {
        match kind {
            BackendKind::Hermes => Some(ManagedServer::Hermes),
            BackendKind::MiMoCode => Some(ManagedServer::MiMoCode),
            _ => None,
        }
    }

    /// How the settings window names the server it manages.
    pub fn label(self) -> &'static str {
        match self {
            ManagedServer::Hermes => "hermes serve",
            ManagedServer::MiMoCode => "mimo serve",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            ManagedServer::Hermes => "Hermes",
            ManagedServer::MiMoCode => "MiMoCode",
        }
    }

    /// The username its HTTP server pairs with the configured password. `None`
    /// for a server whose health endpoint takes no credentials, which is the
    /// case for a local Hermes.
    pub fn auth_username(self) -> Option<&'static str> {
        match self {
            ManagedServer::Hermes => None,
            ManagedServer::MiMoCode => Some(opencode::MIMOCODE_AUTH_USER),
        }
    }

    /// The CLI, preferring a well-known install location over `PATH`.
    fn executable(self) -> PathBuf {
        match self {
            ManagedServer::Hermes => {
                hermes_executable_path().unwrap_or_else(|| PathBuf::from("hermes"))
            }
            ManagedServer::MiMoCode => self
                .binary_search_directories()
                .iter()
                .map(|dir| dir.join("mimo"))
                .find(|path| is_executable(path))
                .unwrap_or_else(|| PathBuf::from("mimo")),
        }
    }

    /// Directories a CLI of this kind is normally installed in, searched first
    /// as a list of candidate paths and then prepended to the child's `PATH`.
    ///
    /// The second half matters: `mimo` is a `#!/usr/bin/env node` script, so the
    /// process it is started as needs to find `node` on its own `PATH`. An app
    /// launched from Finder inherits only the system default
    /// (`/usr/bin:/bin:/usr/sbin:/sbin`), where neither a user's own bin
    /// directory nor Homebrew appears — the CLI would be found and then fail to
    /// run at all.
    fn binary_search_directories(self) -> Vec<PathBuf> {
        let home = crate::logger::dirs::home();
        match self {
            ManagedServer::Hermes => vec![
                home.join(".local/bin"),
                PathBuf::from("/opt/homebrew/bin"),
                PathBuf::from("/usr/local/bin"),
            ],
            ManagedServer::MiMoCode => vec![
                home.join(".npm-global/bin"),
                home.join(".local/bin"),
                PathBuf::from("/opt/homebrew/bin"),
                PathBuf::from("/usr/local/bin"),
            ],
        }
    }

    /// The `PATH` its process is started with: the directories above, then
    /// whatever the app itself was launched with.
    fn child_path(self) -> std::ffi::OsString {
        let mut parts: Vec<std::ffi::OsString> = Vec::new();
        let mut push = |value: std::ffi::OsString| {
            if !value.is_empty() && !parts.contains(&value) {
                parts.push(value);
            }
        };
        for dir in self.binary_search_directories() {
            push(dir.into_os_string());
        }
        if let Some(existing) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&existing) {
                push(dir.into_os_string());
            }
        }
        std::env::join_paths(parts).unwrap_or_default()
    }

    /// Arguments that put the server on `port`. The two CLIs spell the bind
    /// address differently, and a flag one of them does not know is ignored
    /// rather than refused: the server would quietly keep its own port.
    fn arguments(self, port: u16) -> Vec<String> {
        let port = port.to_string();
        match self {
            ManagedServer::Hermes => vec![
                "serve".into(),
                "--port".into(),
                port,
                "--host".into(),
                "127.0.0.1".into(),
            ],
            ManagedServer::MiMoCode => vec![
                "serve".into(),
                "--port".into(),
                port,
                "--hostname".into(),
                "127.0.0.1".into(),
            ],
        }
    }

    /// The path that answers once the server is ready to take requests.
    fn health_path(self) -> &'static str {
        match self {
            ManagedServer::Hermes => "/api/status",
            ManagedServer::MiMoCode => "/global/health",
        }
    }

    /// Whether the process is started in the configured workspace.
    ///
    /// `mimo serve` scopes every session to the project it was started in — the
    /// session list and the agent's own working directory both come from there —
    /// so a server started anywhere else answers with a different conversation
    /// list and edits a different tree. Hermes keeps its workspace in its own
    /// config file, so passing one would be a second, contradictory place to
    /// set it.
    fn uses_workspace(self) -> bool {
        matches!(self, ManagedServer::MiMoCode)
    }
}

/// Manages one local agent server subprocess: probe, start, wait for ready.
///
/// One process at a time, whichever backend asked for it: a second `start`
/// while a child is alive is refused rather than obeyed, so switching backends
/// cannot leave two servers fighting over the same port.
#[derive(Default)]
pub struct LocalServerManager {
    child: Arc<Mutex<Option<tokio::process::Child>>>,
    server: Mutex<Option<ManagedServer>>,
    /// The directory the running child was started in. `None` means it was
    /// started without one, in the app's own directory.
    started_in: Mutex<Option<String>>,
    last_message: Mutex<String>,
    is_launching: Mutex<bool>,
}

impl LocalServerManager {
    /// Record a message for the settings window. It is the manager's own voice:
    /// what it did, or why it did nothing.
    pub fn set_message(&self, message: impl Into<String>) {
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

    /// Whether a server this app started is still running. A server the user
    /// started is not this app's to stop, move or claim.
    pub fn is_managing(&self) -> bool {
        self.child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// The directory the server this app started is running in. `None` when the
    /// app started no server, or started one without a directory.
    pub fn started_in(&self) -> Option<String> {
        self.started_in
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub async fn start(&self, server: ManagedServer, port: u16, workspace: Option<&str>) {
        if self.is_managing() {
            self.set_message(format!(
                "{} process is already managed by Van-Goal.",
                server.display_name()
            ));
            return;
        }
        let mut command = Command::new(server.executable());
        command
            .args(server.arguments(port))
            .env("PATH", server.child_path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        // Its own process group, so the server can be stopped as a whole. The
        // CLIs are wrappers that run the real server as a child — `mimo` is a
        // node script that `spawnSync`s the bundled binary and forwards no
        // signals — so signalling only the process this app spawned leaves the
        // server itself running, still holding the port. Measured: the port
        // answered after the wrapper was gone.
        #[cfg(unix)]
        command.process_group(0);
        let mut started_in = None;
        if server.uses_workspace() {
            match workspace.map(str::trim).filter(|path| !path.is_empty()) {
                Some(path) if std::path::Path::new(path).is_dir() => {
                    command.current_dir(path);
                    started_in = Some(path.to_string());
                }
                Some(path) => self.set_message(format!(
                    "Workspace {path} does not exist; {} is started in the app's own directory.",
                    server.label()
                )),
                None => {}
            }
        }
        match command.spawn() {
            Ok(mut child) => {
                forward_output(child.stdout.take(), server);
                forward_output(child.stderr.take(), server);
                *self.is_launching.lock().unwrap_or_else(|e| e.into_inner()) = true;
                *self.child.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
                *self.server.lock().unwrap_or_else(|e| e.into_inner()) = Some(server);
                *self.started_in.lock().unwrap_or_else(|e| e.into_inner()) = started_in.clone();
                self.set_message(match &started_in {
                    Some(directory) => format!(
                        "Started local {} on 127.0.0.1:{port} in {directory}.",
                        server.display_name()
                    ),
                    None => format!(
                        "Started local {} on 127.0.0.1:{port}.",
                        server.display_name()
                    ),
                });
            }
            Err(error) => {
                self.set_message(format!("Failed to start {}: {error}", server.display_name()));
            }
        }
    }

    /// Use the server already listening on `port`, or start one and wait for it
    /// to answer. Returns the base URL the backend should talk to.
    pub async fn ensure_running(
        &self,
        server: ManagedServer,
        port: u16,
        workspace: Option<&str>,
        credential: &str,
    ) -> Result<String> {
        let url = format!("http://127.0.0.1:{port}");
        if is_reachable(&url, server, credential).await {
            self.set_message(format!(
                "Using local {} on 127.0.0.1:{port}.",
                server.display_name()
            ));
            return Ok(url);
        }
        self.start(server, port, workspace).await;
        let deadline = std::time::Instant::now() + Duration::from_secs(45);
        while std::time::Instant::now() < deadline {
            if is_reachable(&url, server, credential).await {
                *self.is_launching.lock().unwrap_or_else(|e| e.into_inner()) = false;
                self.set_message(match self.started_in() {
                    Some(directory) => format!(
                        "Local {} is ready in {directory}.",
                        server.display_name()
                    ),
                    None => format!("Local {} is ready.", server.display_name()),
                });
                return Ok(url);
            }
            // A server that is gone cannot become ready, however the port looks.
            // Its own output is in the app log; report the exit instead of
            // waiting out the whole deadline for a timeout nobody can act on.
            if let Some(status) = self.take_exited_child() {
                log_debug!(
                    "server",
                    "local {} exited before it was ready: {status}",
                    server.label()
                );
                return Err(anyhow!(
                    "Local {} exited before it was ready ({status}). Its output is in the app log.",
                    server.display_name()
                ));
            }
            tokio::time::sleep(Duration::from_millis(350)).await;
        }
        *self.is_launching.lock().unwrap_or_else(|e| e.into_inner()) = false;
        log_debug!("server", "local server did not become ready before timeout");
        Err(anyhow!(
            "Local {} started but did not become ready in time.",
            server.display_name()
        ))
    }

    /// Start the server again in `workspace`, because the project it serves is
    /// fixed by the directory it was started in.
    ///
    /// A session is not: one resumed on a server started elsewhere runs its next
    /// turn in *that* server's directory — measured — so moving the directory is
    /// the whole of what "change the project" means. Only a server this app
    /// started is moved; one the user started keeps its directory and its
    /// process.
    pub async fn restart_in(
        &self,
        server: ManagedServer,
        port: u16,
        workspace: Option<&str>,
        credential: &str,
    ) -> Result<String> {
        let ours = self.is_managing();
        self.stop().await;
        if ours {
            // Wait for the port to go quiet. `ensure_running` would otherwise
            // find the server we just stopped still answering and keep using it,
            // which is how a "restart" ends up changing nothing.
            let url = format!("http://127.0.0.1:{port}");
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                if !is_reachable(&url, server, credential).await {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
        let url = self
            .ensure_running(server, port, workspace, credential)
            .await?;
        if !ours {
            self.set_message(format!(
                "Using local {} on 127.0.0.1:{port}. It was not started by Van-Goal, so its project is the directory it was started in.",
                server.display_name()
            ));
        }
        Ok(url)
    }

    /// The child's exit status, if it is gone. Reaping it here also lets the
    /// next `start` try again.
    fn take_exited_child(&self) -> Option<String> {
        let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let status = match guard.as_mut() {
            Some(child) => child.try_wait().ok().flatten(),
            None => None,
        };
        if status.is_some() {
            *guard = None;
            self.server
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            *self.started_in.lock().unwrap_or_else(|e| e.into_inner()) = None;
            *self.is_launching.lock().unwrap_or_else(|e| e.into_inner()) = false;
        }
        status.map(|status| status.to_string())
    }

    pub async fn stop(&self) {
        let managed = self.server.lock().unwrap_or_else(|e| e.into_inner()).take();
        let was_managing = self.is_managing();
        if let Some(child) = self
            .child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            stop_process_group(child);
        }
        self.child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        *self.started_in.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.is_launching.lock().unwrap_or_else(|e| e.into_inner()) = false;
        match managed {
            Some(server) if was_managing => {
                self.set_message(format!("Stopped local {} server.", server.display_name()))
            }
            // The server in use was already running when the app arrived, so
            // there is nothing of the app's to stop — killing a process the user
            // started is not the app's business.
            _ => self.set_message("No server started by Van-Goal is running."),
        }
    }
}

/// Stop a spawned server, and whatever it spawned in turn.
///
/// The child was started in its own process group, so the group is the unit to
/// signal: the wrapper and the server it runs are one thing to the app even
/// though they are two processes to the operating system.
fn stop_process_group(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        // `process_group(0)` makes the child the leader of its own group, so its
        // pid is the group to signal.
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
    }
    // The child itself, for a platform with no process groups and as a backstop
    // when the group leader is already gone.
    let _ = child.start_kill();
}

/// Keep a server's own output in the app log. A server that refuses to start —
/// a port already taken, a missing login — says so on its stderr, and that
/// sentence is the only actionable part of the failure.
fn forward_output(
    stream: Option<impl tokio::io::AsyncRead + Unpin + Send + 'static>,
    server: ManagedServer,
) {
    let Some(stream) = stream else {
        return;
    };
    let label = server.label();
    tokio::spawn(async move {
        use tokio::io::AsyncBufReadExt;
        let mut lines = tokio::io::BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            global_logger().log(label, line);
        }
    });
}

async fn is_reachable(url: &str, server: ManagedServer, credential: &str) -> bool {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap_or_default();
    let mut request = client
        .get(format!("{url}{}", server.health_path()))
        .header("Accept", "application/json");
    // A server started with a password answers 401 to everything else, so the
    // readiness check presents the same credentials the backend will.
    if let Some(username) = server.auth_username() {
        if !credential.is_empty() {
            use base64::Engine;
            let encoded = base64::engine::general_purpose::STANDARD
                .encode(format!("{username}:{credential}"));
            request = request.header("Authorization", format!("Basic {encoded}"));
        }
    }
    matches!(
        request.send().await,
        Ok(response) if response.status().is_success()
    )
}

#[cfg(test)]
mod tests {
    use super::{LocalServerManager, ManagedServer};
    use crate::agent::opencode;
    use crate::settings::BackendKind;
    use std::time::Duration;

    /// Only a backend whose server this app can actually launch reports one. A
    /// backend missing here keeps connecting to an already-running server.
    #[test]
    fn only_the_backends_with_a_launchable_server_are_managed() {
        assert_eq!(
            ManagedServer::for_kind(BackendKind::Hermes),
            Some(ManagedServer::Hermes)
        );
        assert_eq!(
            ManagedServer::for_kind(BackendKind::MiMoCode),
            Some(ManagedServer::MiMoCode)
        );
        for kind in [
            BackendKind::OpenCode,
            BackendKind::Codex,
            BackendKind::ClaudeCode,
            BackendKind::Pi,
            BackendKind::OpenClaw,
        ] {
            assert_eq!(
                ManagedServer::for_kind(kind),
                None,
                "{} must keep connecting to an already-running server",
                kind.id()
            );
        }
    }

    /// The two CLIs disagree on the bind-address flag, and a flag one of them
    /// does not know is ignored rather than refused: the server would quietly
    /// keep its own port while the app waited for an address nothing listens on.
    #[test]
    fn each_server_is_told_where_to_listen_in_its_own_words() {
        assert_eq!(
            ManagedServer::Hermes.arguments(9119),
            ["serve", "--port", "9119", "--host", "127.0.0.1"]
        );
        assert_eq!(
            ManagedServer::MiMoCode.arguments(4096),
            ["serve", "--port", "4096", "--hostname", "127.0.0.1"]
        );
        assert_eq!(ManagedServer::Hermes.health_path(), "/api/status");
        assert_eq!(ManagedServer::MiMoCode.health_path(), "/global/health");
    }

    /// The readiness check authenticates as the same user the adapter does. A
    /// server started with a password answers 401 to everything else, so a
    /// mismatch would make a healthy server look dead — measured against
    /// `mimo serve`: `mimocode:<password>` is 200, `opencode:<password>` is 401.
    #[test]
    fn the_readiness_check_authenticates_as_the_adapter_does() {
        let backend = opencode::OpenCodeBackend::new("MiMoCode", opencode::MIMOCODE_AUTH_USER);
        assert_eq!(
            ManagedServer::MiMoCode.auth_username(),
            Some(backend.auth_username())
        );
        assert_eq!(ManagedServer::MiMoCode.auth_username(), Some("mimocode"));
        assert_eq!(
            ManagedServer::Hermes.auth_username(),
            None,
            "a local hermes takes no credentials on its health path"
        );
    }

    /// The spawned CLI has to be able to find its own runtime. `mimo` is a
    /// `#!/usr/bin/env node` script and `node` is not on the `PATH` an app
    /// launched from Finder inherits, so the directories a user installs into
    /// are prepended to the child's environment.
    #[test]
    fn the_child_can_find_a_user_installed_runtime() {
        let path = ManagedServer::MiMoCode.child_path();
        let entries: Vec<String> = std::env::split_paths(&path)
            .map(|dir| dir.to_string_lossy().into_owned())
            .collect();
        let home = crate::logger::dirs::home().to_string_lossy().into_owned();
        for directory in [".local/bin", ".npm-global/bin"] {
            let expected = format!("{home}/{directory}");
            assert!(
                entries.contains(&expected),
                "{expected} must be on the child's PATH, so its own runtime is found: {entries:?}"
            );
        }
    }

    /// A server the user started is not the app's to stop or move: their
    /// process keeps running, keeps its directory, and the app says so instead
    /// of pretending the workspace changed.
    #[tokio::test]
    async fn a_server_started_outside_the_app_is_never_stopped() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port");
        let port = listener.local_addr().expect("an address").port();
        // A stand-in for a `mimo serve` the user started: it answers the health
        // path and stays up.
        let foreign = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut request = vec![0_u8; 1024];
                    let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut request).await;
                    let _ = tokio::io::AsyncWriteExt::write_all(
                        &mut socket,
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}",
                    )
                    .await;
                    // Hold the connection open: a health check is not a shutdown.
                    tokio::time::sleep(Duration::from_secs(5)).await;
                });
            }
        });

        let manager = LocalServerManager::default();
        assert!(!manager.is_managing(), "nothing has been started yet");
        assert_eq!(manager.started_in(), None);

        let url = manager
            .restart_in(ManagedServer::MiMoCode, port, Some("/tmp/elsewhere"), "")
            .await
            .expect("the running server is reused");

        assert_eq!(url, format!("http://127.0.0.1:{port}"));
        assert!(!manager.is_managing());
        assert_eq!(manager.started_in(), None);
        let message = manager.take_message();
        assert!(
            message.contains("not started by Van-Goal"),
            "the app must say why the directory did not change: {message}"
        );
        assert!(
            !message.contains("Stopped"),
            "nothing of the app's was stopped: {message}"
        );
        foreign.abort();
    }

    /// `stop` says what it did, so the button that calls it is never silent.
    #[tokio::test]
    async fn stopping_nothing_says_so() {
        let manager = LocalServerManager::default();
        manager.stop().await;
        assert_eq!(
            manager.take_message(),
            "No server started by Van-Goal is running."
        );
    }

    /// Only `mimo serve` is scoped by its working directory; passing one to
    /// hermes would set a workspace the app cannot keep in step with its own
    /// config file.
    #[test]
    fn only_mimo_is_scoped_by_the_workspace() {
        assert!(ManagedServer::MiMoCode.uses_workspace());
        assert!(!ManagedServer::Hermes.uses_workspace());
    }
}
