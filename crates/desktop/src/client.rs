//! The one client the desktop has: the runtime, the state behind a lock, and
//! the signal that wakes the Go side when something changed.
//!
//! The state lock is a plain `std::sync::Mutex` and is never held across an
//! `.await`: every async operation takes what it needs, releases the lock,
//! talks to the backend, and locks again to apply the result — after checking
//! the result still belongs to the view that asked for it.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::Value;
use tokio::sync::Mutex as AsyncMutex;

use van_goal_core::agent::Backend;
use van_goal_core::cache::SessionCacheStore;
use van_goal_core::chat::Conversation;
use van_goal_core::local_server::LocalServerManager;
use van_goal_core::models::{BackendConfig, ConnectionState, PermissionMode};
use van_goal_core::settings::Settings;
use van_goal_core::{hermes_config, logger};

use crate::state::{cache_key, visible_messages, visible_sessions, Dirty, State};

static CLIENT: OnceLock<Client> = OnceLock::new();

pub fn client() -> &'static Client {
    CLIENT.get_or_init(Client::new)
}

pub struct Client {
    runtime: tokio::runtime::Runtime,
    pub(crate) shared: Arc<Shared>,
}

/// What every background task holds a handle to.
pub struct Shared {
    state: Mutex<State>,
    signal: Signal,
    pub cache: SessionCacheStore,
    pub local_server: Arc<LocalServerManager>,
    pub handle: tokio::runtime::Handle,
}

/// "Something changed": a flag and a condvar the Go side's poll goroutine
/// waits on. Raised whenever a lock on the state is released after a change.
#[derive(Default)]
struct Signal {
    pending: Mutex<bool>,
    wake: Condvar,
}

impl Signal {
    fn raise(&self) {
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.wake.notify_all();
    }

    fn wait(&self, timeout: Duration) -> bool {
        let pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let (mut pending, _) = self
            .wake
            .wait_timeout_while(pending, timeout, |pending| !*pending)
            .unwrap_or_else(|e| e.into_inner());
        std::mem::take(&mut *pending)
    }
}

/// A lock on the state that raises the signal on release when anything was
/// marked dirty, so no code path can change the state and forget to say so.
pub struct Locked<'a> {
    guard: MutexGuard<'a, State>,
    signal: &'a Signal,
}

impl std::ops::Deref for Locked<'_> {
    type Target = State;
    fn deref(&self) -> &State {
        &self.guard
    }
}

impl std::ops::DerefMut for Locked<'_> {
    fn deref_mut(&mut self) -> &mut State {
        &mut self.guard
    }
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        let dirty = &self.guard.dirty;
        if dirty.state || dirty.messages || dirty.settings {
            self.signal.raise();
        }
    }
}

impl Shared {
    pub fn lock(&self) -> Locked<'_> {
        Locked {
            guard: self.state.lock().unwrap_or_else(|e| e.into_inner()),
            signal: &self.signal,
        }
    }

    pub fn spawn<F>(self: &Arc<Self>, future: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.handle.spawn(future);
    }
}

pub fn backend_config(settings: &Settings) -> BackendConfig {
    BackendConfig {
        base_url: settings.active_backend_url(),
        credential: settings.session_token.trim().to_string(),
        profile: settings.normalized_profile(),
        workspace: settings.workspace_trimmed(),
    }
}

impl Client {
    fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .thread_name("van-goal")
            .build()
            .expect("failed to build the tokio runtime");

        let settings = Settings::load();
        logger::global_logger().set_enabled(settings.debug_logging_enabled);
        let cache = SessionCacheStore::new();
        let cached = cache.load();
        let kind = settings.backend_kind;
        let backend = Backend::make(kind);
        let caps = backend.capabilities();

        let sessions = visible_sessions(&cached.sessions, kind.id(), &cached);
        let selected = cached.selected_session_id.as_deref().and_then(|key| {
            sessions
                .iter()
                .find(|session| cache_key(kind.id(), &session.id) == key)
                .cloned()
        });
        let mut conversation = Conversation::new();
        if let Some(session) = &selected {
            if let Some(messages) = cached
                .messages_by_session_id
                .get(&cache_key(kind.id(), &session.id))
            {
                conversation.set_transcript(visible_messages(
                    messages,
                    van_goal_core::models::now_unix(),
                ));
            }
        }
        // The session that was open is the one a launch comes back to: shown
        // straight away from the cache, resumed once connected.
        let stored_id = selected.as_ref().map(|session| session.id.clone());

        let mut state = State {
            settings,
            connection: ConnectionState::Disconnected,
            sessions,
            selected,
            conversation,
            is_refreshing: false,
            refresh_inflight: false,
            transport_ready: false,
            gateway_connecting: false,
            queue: Vec::new(),
            clarify: None,
            last_error: None,
            stopping: false,
            backend: Arc::new(AsyncMutex::new(backend)),
            caps,
            cached,
            view_generation: 0,
            live_id: None,
            stored_id,
            models: Vec::new(),
            current_provider: String::new(),
            current_model: String::new(),
            switching_model: false,
            context_window: hermes_config::DEFAULT_CONTEXT_WINDOW,
            permission_mode: PermissionMode::FullAccess,
            changing_permission: false,
            composer: Vec::new(),
            streamed: Vec::new(),
            cache_generation: 0,
            revision: 0,
            connection_epoch: 0,
            connect_generation: 0,
            retry_attempt: 0,
            reconnecting: false,
            turn_seq: 0,
            dirty: Dirty {
                state: true,
                messages: true,
                settings: true,
            },
        };
        crate::hermes::read_config(&mut state);

        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            signal: Signal::default(),
            cache,
            local_server: Arc::new(LocalServerManager::default()),
            handle: runtime.handle().clone(),
        });

        Self { runtime, shared }
    }

    pub fn wait(&self, timeout_ms: u32) -> bool {
        self.shared
            .signal
            .wait(Duration::from_millis(u64::from(timeout_ms)))
    }

    pub fn poll(&self) -> Value {
        let mut state = self.shared.lock();
        crate::payload::poll(&mut state)
    }

    pub fn run(&self, command: &str) -> Result<Option<Value>> {
        let _enter = self.runtime.enter();
        let value: Value = serde_json::from_str(command)
            .map_err(|error| anyhow!("command was not JSON: {error}"))?;
        let name = value
            .get("cmd")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("command has no cmd"))?
            .to_string();
        crate::commands::dispatch(&self.shared, &name, &value)
    }
}
