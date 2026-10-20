//! What the Go side draws from, as JSON.
//!
//! One poll returns only what changed: `state` (small — the chrome, the
//! sidebar, the composer's surroundings) whenever anything did, `messages` when
//! the transcript's structure changed, `deltas` for messages whose text only
//! streamed further, and `settings` after a preference changed.

use serde_json::{json, Value};

use van_goal_core::agent::openclaw;
use van_goal_core::local_server::ManagedServer;
use van_goal_core::logger::global_logger;
use van_goal_core::models::{AgentSession, BackendCaps, ChatMessage, ConnectionState, MessageRole};
use van_goal_core::settings::{AppearanceMode, BackendKind, FontSize, Settings};

use crate::hermes::permission_id;
use crate::state::{model_groups, truncate_on_char, ComposerAction, State};

/// Tool output is shown for what it did, not as a log: past this it is cut.
const TOOL_DETAIL_LIMIT: usize = 4_000;

pub fn poll(state: &mut State) -> Value {
    let mut update = json!({});
    if state.dirty.messages {
        update["messages"] = messages(state.messages());
        update["revision"] = json!(state.revision);
        state.streamed.clear();
    } else if !state.streamed.is_empty() {
        let ids = std::mem::take(&mut state.streamed);
        let deltas: Vec<Value> = ids
            .iter()
            .filter_map(|id| state.messages().iter().find(|message| &message.id == id))
            .map(|message| json!({ "id": message.id, "content": message.content }))
            .collect();
        update["deltas"] = json!(deltas);
    }
    if state.dirty.settings {
        update["settings"] = settings(&state.settings);
    }
    if state.dirty.state || state.dirty.messages || state.dirty.settings {
        update["state"] = view_state(state);
    }
    let composer = std::mem::take(&mut state.composer);
    if !composer.is_empty() {
        update["composer"] = json!(composer.iter().map(composer_action).collect::<Vec<_>>());
    }
    state.dirty = Default::default();
    update
}

fn composer_action(action: &ComposerAction) -> Value {
    match action {
        ComposerAction::Clear => json!({ "action": "clear" }),
        ComposerAction::Restore { text, attachments } => json!({
            "action": "restore",
            "text": text,
            "attachments": attachments.iter().map(|a| a.path.clone()).collect::<Vec<_>>(),
        }),
    }
}

fn role(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::System => "system",
        MessageRole::Tool => "tool",
    }
}

pub fn messages(messages: &[ChatMessage]) -> Value {
    json!(messages
        .iter()
        .filter(|message| !message.is_empty_shell())
        .map(message)
        .collect::<Vec<_>>())
}

fn message(message: &ChatMessage) -> Value {
    json!({
        "id": message.id,
        "role": role(message.role),
        "content": message.content,
        "streaming": message.is_streaming,
        "timestamp": message.timestamp,
        "completed_at": message.completed_at,
        "tools": message.tool_calls.iter().map(|tool| {
            let detail = truncate_on_char(&tool.detail, TOOL_DETAIL_LIMIT);
            json!({
                "id": tool.id,
                "name": tool.name,
                "status": tool.status,
                "detail": detail,
                "truncated": detail.len() < tool.detail.len(),
                "timestamp": tool.timestamp,
            })
        }).collect::<Vec<_>>(),
        "attachments": message.attachments.iter().map(|a| json!({
            "path": a.path,
            "name": a.name(),
        })).collect::<Vec<_>>(),
    })
}

fn connection(state: &ConnectionState) -> Value {
    let id = match state {
        ConnectionState::Disconnected => "disconnected",
        ConnectionState::Connecting => "connecting",
        ConnectionState::Connected => "connected",
        ConnectionState::Degraded(_) => "degraded",
        ConnectionState::Failed(_) => "failed",
    };
    json!({ "state": id, "label": state.label(), "pill": state.pill_label() })
}

fn session(session: &AgentSession) -> Value {
    json!({
        "id": session.id,
        "title": session.display_title(),
        "subtitle": session.subtitle(),
        "active": session.is_active == Some(true),
        "messages": session.message_count,
        "profile": session.profile,
        "last_active": session.last_active.or(session.started_at),
    })
}

fn caps(caps: BackendCaps) -> Value {
    json!({
        "history": caps.contains(BackendCaps::SESSION_HISTORY),
        "resume": caps.contains(BackendCaps::SESSION_RESUME),
        "interrupt": caps.contains(BackendCaps::INTERRUPT),
        "interaction": caps.contains(BackendCaps::USER_INTERACTION),
        "tools": caps.contains(BackendCaps::TOOL_EVENTS),
        "attachments": caps.contains(BackendCaps::ATTACHMENTS),
        "models": caps.contains(BackendCaps::MODEL_SELECTION),
        "permissions": caps.contains(BackendCaps::PERMISSION_MODES),
    })
}

fn view_state(state: &State) -> Value {
    let kind = state.kind();
    let usage = state.context_usage();
    let groups = model_groups(&state.models, &state.current_provider, &state.current_model);
    let selected_model = state
        .selected
        .as_ref()
        .and_then(|session| session.model.clone())
        .filter(|model| !model.trim().is_empty());
    json!({
        "backend": {
            "id": kind.id(),
            "name": kind.display_name(),
            "enabled": state.settings.is_backend_enabled(kind),
            "caps": caps(state.caps),
        },
        "connection": connection(&state.connection),
        "reconnecting": state.reconnecting,
        "sessions": state.sessions.iter().map(session).collect::<Vec<_>>(),
        "selected": state.selected.as_ref().map(session),
        "sending": state.is_sending(),
        "stopping": state.stopping,
        "refreshing": state.is_refreshing,
        "queue": state.queue.iter().map(|item| json!({
            "id": item.id,
            "text": item.text,
            "attachments": item.attachments.iter().map(|a| a.name()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "clarify": state.clarify.as_ref().map(|clarify| json!({
            "question": clarify.question,
            "choices": clarify.choices,
        })),
        "error": state.last_error,
        "context": {
            "used": usage.used_tokens,
            "max": usage.max_tokens,
            "measured": usage.measured,
            "ratio": usage.ratio(),
        },
        "model": {
            "name": if state.current_model.is_empty() { selected_model.clone() } else { Some(state.current_model.clone()) },
            "session_model": selected_model,
            "provider": state.current_provider,
            "switching": state.switching_model,
            "groups": groups.iter().map(|(provider, models)| json!({
                "provider": provider,
                "models": models.iter().map(|m| m.model.clone()).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        },
        "permission": {
            "mode": permission_id(state.permission_mode),
            "label": state.permission_mode.label(),
            "changing": state.changing_permission,
        },
        "cache_summary": crate::cache::summary(state),
        "revision": state.revision,
    })
}

pub fn settings(settings: &Settings) -> Value {
    let kind = settings.backend_kind;
    let device_token = if kind == BackendKind::OpenClaw {
        openclaw::stored_device_token_characters(&settings.active_backend_url())
    } else {
        0
    };
    json!({
        "backends": BackendKind::ALL.iter().map(|kind| json!({
            "id": kind.id(),
            "name": kind.display_name(),
            "description": kind.description(),
            "enabled": settings.is_backend_enabled(*kind),
            "network": kind.uses_network_server(),
            "credential_label": kind.credential_label(),
        })).collect::<Vec<_>>(),
        "backend": kind.id(),
        "appearance": settings.appearance.id(),
        "appearances": AppearanceMode::ALL.iter().map(|a| json!({ "id": a.id(), "name": a.display_name() })).collect::<Vec<_>>(),
        "font_size": settings.font_size.id(),
        "font_scale": settings.font_size.scale(),
        "ui_scale": settings.ui_scale,
        "font_sizes": FontSize::ALL.iter().map(|f| json!({ "id": f.id(), "name": f.display_name() })).collect::<Vec<_>>(),
        "host": settings.backend_host,
        "port": settings.resolved_port(),
        "use_tls": settings.backend_use_tls,
        "url": settings.active_backend_url(),
        "credential": settings.session_token,
        "credential_chars": settings.stored_credential_characters(kind),
        "device_token_chars": device_token,
        "workspace": settings.workspace_path,
        "profile": settings.selected_profile,
        "managed_label": settings.managed_server_label(),
        "managed_workspace": ManagedServer::for_kind(kind).is_some_and(|s| s.uses_workspace()),
        "debug_logging": settings.debug_logging_enabled,
        "log_path": global_logger().path().to_string_lossy(),
        "sidebar_open": settings.sidebar_open,
        "show_tool_calls": settings.show_tool_calls,
        "window": settings.window.filter(|w| w.is_plausible()).map(|w| json!({
            "x": w.x, "y": w.y, "width": w.width, "height": w.height, "maximized": w.maximized,
        })),
    })
}
