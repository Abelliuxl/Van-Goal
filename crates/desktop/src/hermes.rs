//! Hermes' model and permission settings, which live in Hermes' own config
//! files rather than in this app's settings.

use std::sync::Arc;

use anyhow::anyhow;
use van_goal_core::hermes_config;
use van_goal_core::models::{ModelOption, PermissionMode};

use crate::client::Shared;
use crate::state::State;

pub fn read_config(state: &mut State) {
    let (provider, model) = hermes_config::read_model_config();
    state.models = hermes_config::read_model_options(&provider, &model);
    state.context_window = hermes_config::read_context_window_tokens(&provider, &model);
    state.current_provider = provider;
    state.current_model = model;
    state.permission_mode = hermes_config::read_permission_mode();
    state.changed();
}

pub fn select_model(shared: &Arc<Shared>, option: ModelOption) {
    {
        let mut state = shared.lock();
        if state.switching_model {
            return;
        }
        state.switching_model = true;
        state.changed();
    }
    let task = shared.clone();
    shared.spawn(async move {
        let chosen = option.clone();
        let result = tokio::task::spawn_blocking(move || hermes_config::select_model(&chosen))
            .await
            .map_err(|error| anyhow!("{error}"))
            .and_then(|result| result);
        let mut state = task.lock();
        state.switching_model = false;
        match result {
            Ok(()) => read_config(&mut state),
            Err(error) => state.error(format!("Could not switch the Hermes model: {error}")),
        }
    });
}

pub fn set_permission_mode(shared: &Arc<Shared>, mode: PermissionMode) {
    {
        let mut state = shared.lock();
        if state.changing_permission {
            return;
        }
        state.changing_permission = true;
        state.changed();
    }
    let task = shared.clone();
    shared.spawn(async move {
        let result = tokio::task::spawn_blocking(move || hermes_config::set_permission_mode(mode))
            .await
            .map_err(|error| anyhow!("{error}"))
            .and_then(|result| result);
        let mut state = task.lock();
        state.changing_permission = false;
        match result {
            Ok(()) => state.permission_mode = mode,
            Err(error) => {
                state.error(format!("Could not change Hermes permissions: {error}"));
                state.permission_mode = hermes_config::read_permission_mode();
            }
        }
        state.changed();
    });
}

pub fn permission_id(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::FullAccess => "full",
        PermissionMode::AskBeforeRisky => "ask",
        PermissionMode::RestrictedTools => "restricted",
    }
}

pub fn permission_from_id(id: &str) -> Option<PermissionMode> {
    PermissionMode::ALL
        .into_iter()
        .find(|mode| permission_id(*mode) == id)
}
