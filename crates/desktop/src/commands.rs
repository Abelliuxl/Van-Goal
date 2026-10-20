//! The commands the Go side sends, and the settings it writes.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use van_goal_core::agent::openclaw;
use van_goal_core::logger::global_logger;
use van_goal_core::models::ModelOption;
use van_goal_core::settings::{AppearanceMode, BackendKind, FontSize, SavedWindow};

use crate::client::Shared;
use crate::state::attachments_from;

pub fn dispatch(shared: &Arc<Shared>, name: &str, value: &Value) -> Result<Option<Value>> {
    match name {
        // Everything the window needs to draw its first frame, without waiting
        // for the first poll — and the connect a launch makes when the backend
        // the user left switched on is still on.
        "start" => {
            let enabled = {
                let mut state = shared.lock();
                state.dirty.state = true;
                state.dirty.messages = true;
                state.dirty.settings = true;
                let kind = state.kind();
                state.settings.is_backend_enabled(kind)
            };
            if enabled {
                crate::connect::connect(shared);
            }
            let mut state = shared.lock();
            Ok(Some(crate::payload::poll(&mut state)))
        }
        "connect" => {
            let kind = shared.lock().kind();
            crate::connect::enable_backend(shared, kind);
            Ok(None)
        }
        "disconnect" => {
            let kind = shared.lock().kind();
            crate::connect::disable_backend(shared, kind);
            Ok(None)
        }
        "enable_backend" => {
            crate::connect::enable_backend(shared, kind(value)?);
            Ok(None)
        }
        "disable_backend" => {
            crate::connect::disable_backend(shared, kind(value)?);
            Ok(None)
        }
        "refresh" => {
            crate::sessions::refresh(shared, true);
            Ok(None)
        }
        "open_session" => {
            let id = string(value, "id")?;
            let session = crate::sessions::find(&shared.lock(), &id)
                .ok_or_else(|| anyhow!("That session is no longer in the list."))?;
            crate::sessions::open(shared, session);
            Ok(None)
        }
        "new_chat" => {
            crate::sessions::new_chat(shared);
            Ok(None)
        }
        "archive" | "delete" => {
            let ids = match value.get("ids").and_then(Value::as_array) {
                Some(ids) => ids
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect(),
                None => vec![string(value, "id")?],
            };
            crate::sessions::remove(shared, ids, name == "delete");
            Ok(None)
        }
        "clear_cache" => {
            crate::sessions::clear_cache(shared);
            Ok(None)
        }
        "send" => {
            let text = value
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            crate::send::submit(
                shared,
                text,
                attachments_from(&strings(value, "attachments")),
            );
            Ok(None)
        }
        "stop" => {
            crate::send::stop(shared);
            Ok(None)
        }
        "answer" => {
            crate::send::answer(shared, string(value, "text")?);
            Ok(None)
        }
        "skip_question" => {
            crate::send::dismiss_clarify(shared);
            Ok(None)
        }
        "queue_send_now" => {
            crate::send::send_queued_now(shared, &string(value, "id")?);
            Ok(None)
        }
        "queue_edit" => {
            crate::send::edit_queued(shared, &string(value, "id")?);
            Ok(None)
        }
        "queue_cancel" => {
            crate::send::cancel_queued(shared, &string(value, "id")?);
            Ok(None)
        }
        "select_model" => {
            crate::hermes::select_model(
                shared,
                ModelOption {
                    provider: string(value, "provider")?,
                    model: string(value, "model")?,
                },
            );
            Ok(None)
        }
        "set_permission" => {
            let id = string(value, "mode")?;
            let mode = crate::hermes::permission_from_id(&id)
                .ok_or_else(|| anyhow!("unknown permission mode {id:?}"))?;
            crate::hermes::set_permission_mode(shared, mode);
            Ok(None)
        }
        "reload_hermes" => {
            crate::hermes::read_config(&mut shared.lock());
            Ok(None)
        }
        "settings" => {
            set(shared, value)?;
            Ok(Some(
                json!({ "settings": crate::payload::settings(&shared.lock().settings) }),
            ))
        }
        "apply_workspace" => {
            crate::connect::apply_workspace(shared);
            Ok(None)
        }
        "clear_credential" => {
            let mut state = shared.lock();
            let kind = state.kind();
            state.settings.session_token.clear();
            if kind == BackendKind::OpenClaw {
                openclaw::forget_device_token(&state.settings.active_backend_url());
            }
            state.settings.save();
            state.settings_changed();
            Ok(None)
        }
        "stop_managed_server" => {
            shared.local_server.stop();
            shared.lock().changed();
            Ok(None)
        }
        "server_message" => Ok(Some(
            json!({ "message": shared.local_server.take_message() }),
        )),
        "clear_log" => {
            global_logger().clear();
            Ok(None)
        }
        "markdown" => {
            let text = value.get("text").and_then(Value::as_str).unwrap_or("");
            Ok(Some(json!({ "blocks": crate::markdown::blocks(text) })))
        }
        // The app is quitting. A server scoped to a workspace (mimo serve) is
        // this app's own and is stopped with it; one that serves every client
        // (hermes serve) is left running for them.
        "shutdown" => {
            {
                let mut state = shared.lock();
                crate::cache::save_now(shared, &mut state);
            }
            if shared
                .local_server
                .managed_server()
                .is_some_and(|server| server.uses_workspace())
            {
                shared.local_server.stop();
            }
            Ok(None)
        }
        other => Err(anyhow!("unknown command {other:?}")),
    }
}

/// Write the preferences a command names. Only the fields present change.
fn set(shared: &Arc<Shared>, value: &Value) -> Result<()> {
    let mut state = shared.lock();
    let settings = &mut state.settings;
    if let Some(id) = value.get("appearance").and_then(Value::as_str) {
        settings.appearance = AppearanceMode::ALL
            .into_iter()
            .find(|mode| mode.id() == id)
            .ok_or_else(|| anyhow!("unknown appearance {id:?}"))?;
    }
    if let Some(id) = value.get("font_size").and_then(Value::as_str) {
        settings.font_size =
            FontSize::from_id(id).ok_or_else(|| anyhow!("unknown font size {id:?}"))?;
    }
    if let Some(host) = value.get("host").and_then(Value::as_str) {
        settings.backend_host = host.trim().to_string();
    }
    if let Some(port) = value.get("port") {
        let port = port
            .as_u64()
            .or_else(|| port.as_str().and_then(|text| text.trim().parse().ok()))
            .filter(|port| (1..=u64::from(u16::MAX)).contains(port))
            .ok_or_else(|| anyhow!("The port must be a number from 1 to 65535."))?;
        settings.backend_port = port as u16;
    }
    if let Some(tls) = value.get("use_tls").and_then(Value::as_bool) {
        settings.backend_use_tls = tls;
    }
    if let Some(credential) = value.get("credential").and_then(Value::as_str) {
        // A pasted token keeps the newline it was copied with.
        settings.session_token = credential.replace(['\r', '\n'], "").trim().to_string();
    }
    if let Some(workspace) = value.get("workspace").and_then(Value::as_str) {
        settings.workspace_path = workspace.to_string();
    }
    if let Some(profile) = value.get("profile").and_then(Value::as_str) {
        settings.selected_profile = profile.to_string();
    }
    if let Some(debug) = value.get("debug_logging").and_then(Value::as_bool) {
        settings.debug_logging_enabled = debug;
        global_logger().set_enabled(debug);
    }
    if let Some(open) = value.get("sidebar_open").and_then(Value::as_bool) {
        settings.sidebar_open = open;
    }
    if let Some(show) = value.get("show_tool_calls").and_then(Value::as_bool) {
        settings.show_tool_calls = show;
    }
    if let Some(window) = value.get("window") {
        let number = |key: &str| window.get(key).and_then(Value::as_f64).map(|n| n as f32);
        if let (Some(x), Some(y), Some(width), Some(height)) =
            (number("x"), number("y"), number("width"), number("height"))
        {
            let saved = SavedWindow {
                x,
                y,
                width,
                height,
                maximized: window
                    .get("maximized")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            };
            if saved.is_plausible() && settings.window != Some(saved) {
                settings.window = Some(saved);
            }
        }
    }
    settings.save();
    state.settings_changed();
    Ok(())
}

fn kind(value: &Value) -> Result<BackendKind> {
    let id = string(value, "backend")?;
    BackendKind::ALL
        .into_iter()
        .find(|kind| kind.id() == id)
        .ok_or_else(|| anyhow!("unknown backend {id:?}"))
}

fn string(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("command needs a {key}"))
}

fn strings(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}
