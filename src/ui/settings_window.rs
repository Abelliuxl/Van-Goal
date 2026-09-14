use crate::state::AppState;
use crate::ui::editor::{Editor, EditorEvent};
use crate::ui::theme::Theme;
use gpui::{
    actions, div, prelude::*, px, AnyElement, Context, Entity, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, Styled, Window,
};

actions!(van_goal, [OpenSettings]);

pub struct SettingsView {
    state: Entity<AppState>,
    host_editor: Entity<Editor>,
    port_editor: Entity<Editor>,
    workspace_editor: Entity<Editor>,
    profile_editor: Entity<Editor>,
    token_editor: Entity<Editor>,
    /// Which backend the text fields currently hold, so they can be re-filled
    /// when a switch changes the active backend.
    shown_backend: crate::settings::BackendKind,
}

impl SettingsView {
    pub fn open(
        state: Entity<AppState>,
        cx: &mut gpui::App,
    ) -> gpui::Result<gpui::WindowHandle<Self>> {
        let bounds = gpui::Bounds::centered(None, gpui::size(px(560.0), px(680.0)), cx);
        cx.open_window(
            gpui::WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
                titlebar: Some(gpui::TitlebarOptions {
                    title: Some("Van-Goal Settings".into()),
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                window_min_size: Some(gpui::size(px(480.0), px(520.0))),
                ..Default::default()
            },
            |window, cx| {
                let state_entity = state.clone();
                cx.new(|cx| Self::new(state_entity, window, cx))
            },
        )
    }

    fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = state.read(cx).settings.clone();
        let make_field = |value: &str, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut editor = Editor::single_line(cx);
                editor.set_text(value, cx);
                editor
            })
        };
        let host_editor = make_field(&settings.backend_host, cx);
        let port_editor = make_field(&settings.backend_port.to_string(), cx);
        let workspace_editor = make_field(&settings.workspace_path, cx);
        let profile_editor = make_field(&settings.selected_profile, cx);
        let token_editor = cx.new(|cx| {
            let mut editor = Editor::wrapped_single_line(cx);
            editor.set_text(&settings.session_token, cx);
            editor
        });

        // Persist edits straight into the settings store.
        {
            let state = state.clone();
            cx.subscribe(
                &host_editor,
                move |this, _editor, event: &EditorEvent, cx| {
                    if *event == EditorEvent::Change {
                        let text = this.host_editor.read(cx).text().to_string();
                        state.update(cx, |state, _cx| {
                            state.settings.backend_host = text;
                            state.settings.save();
                        });
                    }
                },
            )
            .detach()
        }
        let port_state = state.clone();
        cx.subscribe(
            &port_editor,
            move |this, _editor, event: &EditorEvent, cx| {
                if *event == EditorEvent::Change {
                    let text = this.port_editor.read(cx).text().to_string();
                    port_state.update(cx, |state, _cx| {
                        if let Ok(port) = text.trim().parse::<u16>() {
                            if port > 0 {
                                state.settings.backend_port = port;
                                state.settings.save();
                            }
                        }
                    });
                }
            },
        )
        .detach();
        let workspace_state = state.clone();
        cx.subscribe(
            &workspace_editor,
            move |this, _editor, event: &EditorEvent, cx| {
                if *event == EditorEvent::Change {
                    let text = this.workspace_editor.read(cx).text().to_string();
                    workspace_state.update(cx, |state, _cx| {
                        state.settings.workspace_path = text;
                        state.settings.save();
                    });
                }
            },
        )
        .detach();
        let profile_state = state.clone();
        cx.subscribe(
            &profile_editor,
            move |this, _editor, event: &EditorEvent, cx| {
                if *event == EditorEvent::Change {
                    let text = this.profile_editor.read(cx).text().to_string();
                    profile_state.update(cx, |state, _cx| {
                        state.settings.selected_profile = text;
                        state.settings.save();
                    });
                }
            },
        )
        .detach();
        let token_state = state.clone();
        cx.subscribe(
            &token_editor,
            move |this, _editor, event: &EditorEvent, cx| {
                if *event == EditorEvent::Change {
                    let text = this.token_editor.read(cx).text().to_string();
                    token_state.update(cx, |state, _cx| {
                        state.settings.session_token = text;
                        state.settings.save();
                    });
                }
            },
        )
        .detach();

        cx.observe(&state, |this, state, cx| {
            // Switching a backend on swaps the whole connection block, so the
            // fields have to be re-filled from the newly active backend instead
            // of showing the previous one's address.
            let settings = state.read(cx).settings.clone();
            if settings.backend_kind != this.shown_backend {
                this.shown_backend = settings.backend_kind;
                this.host_editor.update(cx, |editor, cx| {
                    editor.set_text(settings.backend_host.clone(), cx)
                });
                this.port_editor.update(cx, |editor, cx| {
                    editor.set_text(settings.backend_port.to_string(), cx)
                });
                this.token_editor.update(cx, |editor, cx| {
                    editor.set_text(settings.session_token.clone(), cx)
                });
            }
            cx.notify();
        })
        .detach();
        cx.observe_window_appearance(window, |_, window, cx| {
            window.refresh();
            cx.notify();
        })
        .detach();

        Self {
            state,
            host_editor,
            port_editor,
            workspace_editor,
            profile_editor,
            token_editor,
            shown_backend: settings.backend_kind,
        }
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = self.state.read(cx).settings.clone();
        Theme::sync(settings.appearance, window.appearance());
        let (status_label, status_error, cache_summary, log_path, last_server_message) = {
            let state = self.state.read(cx);
            (
                state.connection_state.label(),
                state.last_error.clone(),
                state.cache_summary.clone(),
                crate::logger::global_logger().path().display().to_string(),
                state.local_server.take_message(),
            )
        };
        let state = self.state.clone();
        let uses_network = settings.backend_kind.uses_network_server();

        let mut root = div()
            .id("settings-root")
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .bg(Theme::window_bg())
            .text_color(Theme::text())
            .p_4()
            .overflow_y_scroll();

        // Backend section. Every backend owns a switch: turning one on makes it
        // the active backend and connects it, turning it off disconnects it and
        // keeps it off on the next launch.
        let mut backend_section = section("Agent backend");
        for kind in crate::settings::BackendKind::ALL {
            let is_enabled = settings.is_backend_enabled(kind);
            let is_active = settings.backend_kind == kind;
            let status = match (is_enabled, is_active) {
                (true, true) => status_label.clone(),
                (true, false) => "On".to_string(),
                (false, _) => "Off".to_string(),
            };
            backend_section = backend_section.child(backend_row(
                kind,
                is_enabled,
                is_active,
                status,
                state.clone(),
            ));
        }
        backend_section = backend_section.child(hint(
            "One backend runs at a time. Switching one on switches the others off, and a backend you switch off stays off after a restart.",
        ));
        root = root.child(backend_section);

        let mut appearance_options = div().flex().flex_row().gap_2();
        for mode in crate::settings::AppearanceMode::ALL {
            let selected = settings.appearance == mode;
            let state = state.clone();
            appearance_options = appearance_options.child(render_menu_button(
                format!("appearance-{}", mode.id()),
                if selected {
                    format!("✓ {}", mode.display_name())
                } else {
                    mode.display_name().to_string()
                },
                if selected {
                    Theme::accent()
                } else {
                    Theme::text_secondary()
                },
                move |_event, window, cx| {
                    state.update(cx, |state, cx| {
                        state.settings.appearance = mode;
                        state.settings.save();
                        cx.notify();
                    });
                    window.refresh();
                },
            ));
        }
        root = root.child(section("Appearance").child(appearance_options).child(hint(
            "System follows the current macOS light or dark appearance.",
        )));

        // Connection section
        let mut connection = section("Connection");
        if uses_network {
            connection = connection
                .child(field_row("Host or URL", self.host_editor.clone()))
                .child(field_row("Port", self.port_editor.clone()))
                .child(toggle_row(
                    "Use TLS (HTTPS/WSS)",
                    settings.backend_use_tls,
                    state.clone(),
                    |state, _cx| {
                        state.settings.backend_use_tls = !state.settings.backend_use_tls;
                        state.settings.save();
                    },
                ))
                .child(hint(if settings.is_managed_local_backend() {
                    "Local address: Van-Goal starts and manages hermes serve automatically."
                } else if settings.backend_kind == crate::settings::BackendKind::OpenClaw {
                    "For a reverse-proxy path, paste the complete ws:// or wss:// URL; it overrides Port and TLS."
                } else {
                    "Van-Goal connects to an already-running server at this address."
                }));
        } else {
            connection = connection
                .child(field_row("Workspace", self.workspace_editor.clone()))
                .child(hint(
                    "Van-Goal launches the installed CLI in this directory.",
                ));
        }
        if settings.backend_kind == crate::settings::BackendKind::Hermes {
            connection = connection
                .child(field_row("Default profile", self.profile_editor.clone()))
                .child(hint("Leave empty to use the Hermes default profile."));
        }
        let active_backend = settings.backend_kind;
        let active_enabled = settings.is_backend_enabled(active_backend);
        connection = connection.child(
            div()
                .text_size(px(11.0))
                .text_color(Theme::text_secondary())
                .child(active_backend.description()),
        );
        connection = connection.child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(if active_enabled {
                    small_button(
                        "disconnect-now",
                        format!("Switch off {}", active_backend.display_name()),
                        Theme::danger(),
                        {
                            let state = state.clone();
                            move |_event, _window, cx| {
                                state.update(cx, |state, cx| {
                                    state.disable_backend(active_backend, cx)
                                });
                            }
                        },
                    )
                } else {
                    small_button(
                        "connect-now",
                        format!("Switch on {}", active_backend.display_name()),
                        Theme::accent(),
                        {
                            let state = state.clone();
                            move |_event, _window, cx| {
                                state.update(cx, |state, cx| {
                                    state.enable_backend(active_backend, cx)
                                });
                            }
                        },
                    )
                })
                .when(
                    active_backend == crate::settings::BackendKind::Hermes,
                    |this| {
                        this.child(small_button(
                            "stop-managed",
                            "Stop Managed Local",
                            Theme::danger(),
                            {
                                let state = state.clone();
                                move |_event, _window, cx| {
                                    state.update(cx, |state, cx| state.stop_managed_local(cx));
                                }
                            },
                        ))
                    },
                ),
        );
        if !last_server_message.is_empty() {
            connection = connection.child(hint(&last_server_message));
        }
        root = root.child(connection);

        // Credentials section. Read the credential that is actually used rather
        // than the raw field: an empty field does not mean nothing is stored.
        let stored_credential = self.state.read(cx).stored_credential();
        let mut credentials = section("Credentials");
        if uses_network {
            credentials = credentials
                .child(credential_field(
                    settings.backend_kind.credential_label(),
                    self.token_editor.clone(),
                    stored_credential,
                ))
                .child(hint(credential_help(settings.backend_kind)));
        } else {
            credentials = credentials.child(hint(
                "Authentication is managed by the installed CLI. Sign in with its normal login command before connecting.",
            ));
        }
        credentials = credentials.child(small_button(
            "clear-credential",
            "Clear Credential",
            Theme::danger(),
            {
                let state = state.clone();
                let token_editor = self.token_editor.clone();
                move |_event, _window, cx| {
                    token_editor.update(cx, |editor, cx| editor.clear(cx));
                    state.update(cx, |state, cx| state.clear_credential(cx));
                }
            },
        ));
        root = root.child(credentials);

        // Debug section
        root = root.child(
            section("Debug")
                .child(toggle_row(
                    "Debug logging",
                    settings.debug_logging_enabled,
                    state.clone(),
                    |state, _cx| {
                        state.settings.debug_logging_enabled =
                            !state.settings.debug_logging_enabled;
                        crate::logger::global_logger()
                            .set_enabled(state.settings.debug_logging_enabled);
                        state.settings.save();
                    },
                ))
                .child(hint(&log_path))
                .child(small_button(
                    "clear-log",
                    "Clear Log",
                    Theme::surface_hover(),
                    {
                        move |_event, _window, _cx| {
                            crate::logger::global_logger().clear();
                        }
                    },
                )),
        );

        // Cache + status
        root = root
            .child(
                section("Cache")
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(Theme::text_secondary())
                            .child(cache_summary),
                    )
                    .child(small_button(
                        "clear-cache",
                        "Clear Local Cache",
                        Theme::danger(),
                        {
                            let state = state.clone();
                            move |_event, _window, cx| {
                                state.update(cx, |state, cx| state.clear_cache(cx));
                            }
                        },
                    )),
            )
            .child(
                section("Status")
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(Theme::text_secondary())
                            .child(format!("Connection: {status_label}")),
                    )
                    .children(status_error.map(|error| {
                        div()
                            .text_size(px(11.0))
                            .text_color(Theme::warn())
                            .child(error)
                    })),
            );

        root
    }
}

// -- small building blocks ---------------------------------------------------

fn section(title: &'static str) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .p_3()
        .rounded_lg()
        .bg(Theme::input_bg())
        .border_1()
        .border_color(Theme::border())
        .child(
            div()
                .text_size(px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(Theme::text_secondary())
                .child(title),
        )
}

fn hint(text: &str) -> AnyElement {
    div()
        .text_size(px(10.0))
        .text_color(Theme::text_tertiary())
        .child(text.to_string())
        .into_any()
}

fn field_row(label: &'static str, editor: Entity<Editor>) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .child(
            div()
                .w(px(140.0))
                .text_size(px(12.0))
                .text_color(Theme::text())
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(Theme::surface())
                .border_1()
                .border_color(Theme::border())
                .text_size(px(12.0))
                .child(editor),
        )
        .into_any()
}

/// One backend and its own on/off switch. The switch is the only control:
/// switching a backend on makes it the active backend and connects it,
/// switching it off disconnects it and leaves it off next launch.
fn backend_row(
    kind: crate::settings::BackendKind,
    is_enabled: bool,
    is_active: bool,
    status: String,
    state: Entity<AppState>,
) -> AnyElement {
    let on_toggle = state.clone();
    div()
        .id(gpui::ElementId::Name(
            format!("backend-row-{}", kind.id()).into(),
        ))
        .debug_selector(move || format!("backend-row-{}", kind.id()))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .rounded_md()
        .when(is_active, |this| this.bg(Theme::accent_soft()))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(Theme::text())
                        .child(kind.display_name()),
                )
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(if is_enabled {
                            Theme::ok()
                        } else {
                            Theme::text_tertiary()
                        })
                        .child(status),
                ),
        )
        .child(switch_toggle(
            format!("backend-switch-{}", kind.id()),
            is_enabled,
            move |_event, _window, cx| {
                on_toggle.update(cx, |state, cx| {
                    if is_enabled {
                        state.disable_backend(kind, cx);
                    } else {
                        state.enable_backend(kind, cx);
                    }
                });
            },
        ))
        .into_any()
}

/// A labelled track-and-knob switch. The caller supplies the id, which doubles
/// as the debug selector so a test can find each backend's switch.
fn switch_toggle<F>(id: String, is_on: bool, on_click: F) -> AnyElement
where
    F: Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
{
    let selector = id.clone();
    div()
        .id(gpui::ElementId::Name(id.into()))
        .debug_selector(move || selector.clone())
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .on_click(on_click)
        .child(
            div()
                .text_size(px(10.0))
                .text_color(if is_on {
                    Theme::ok()
                } else {
                    Theme::text_tertiary()
                })
                .child(if is_on { "ON" } else { "OFF" }),
        )
        .child(
            div()
                .w(px(34.0))
                .h(px(18.0))
                .rounded_full()
                .bg(if is_on {
                    Theme::accent()
                } else {
                    Theme::border()
                })
                .p(px(2.0))
                .flex()
                .when(is_on, |this| this.justify_end())
                .when(!is_on, |this| this.justify_start())
                .child(div().size(px(14.0)).rounded_full().bg(Theme::text())),
        )
        .into_any()
}

fn credential_field(
    label: &'static str,
    editor: Entity<Editor>,
    stored: crate::state::StoredCredential,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(Theme::text())
                        .child(label),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(if stored.is_stored() {
                            Theme::ok()
                        } else {
                            Theme::text_tertiary()
                        })
                        .child(if stored.saved_characters > 0 {
                            format!("{} characters stored", stored.saved_characters)
                        } else if stored.is_stored() {
                            "Stored as a paired device token".to_string()
                        } else {
                            "Nothing stored".to_string()
                        }),
                ),
        )
        .child(
            div()
                .w_full()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(Theme::surface())
                .border_1()
                .border_color(Theme::border())
                .text_size(px(12.0))
                .child(editor),
        )
        .when(stored.device_token_characters > 0, |this| {
            this.child(
                div()
                    .text_size(px(10.0))
                    .text_color(Theme::text_tertiary())
                    .child(format!(
                        "Van-Goal also holds a paired-device token for this gateway ({} characters), and that is what it connects with — this field can stay empty.",
                        stored.device_token_characters
                    )),
            )
        })
        .into_any()
}

fn toggle_row<F>(label: &'static str, is_on: bool, state: Entity<AppState>, apply: F) -> AnyElement
where
    F: Fn(&mut AppState, &mut gpui::App) + 'static,
{
    div()
        .id(gpui::ElementId::Name(format!("toggle-{label}").into()))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .on_click(move |_event, _window, cx| {
            state.update(cx, |state, cx| {
                apply(state, cx);
                cx.notify();
            });
        })
        .child(
            div()
                .text_size(px(12.0))
                .text_color(Theme::text())
                .child(label),
        )
        .child(div().flex_1())
        .child(
            div()
                .w(px(34.0))
                .h(px(18.0))
                .rounded_full()
                .bg(if is_on {
                    Theme::accent()
                } else {
                    Theme::border()
                })
                .p(px(2.0))
                .flex()
                .when(is_on, |this| this.justify_end())
                .when(!is_on, |this| this.justify_start())
                .child(div().size(px(14.0)).rounded_full().bg(Theme::text())),
        )
        .into_any()
}

fn small_button<F>(
    id: &'static str,
    label: impl Into<String>,
    color: gpui::Hsla,
    on_click: F,
) -> AnyElement
where
    F: Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
{
    div()
        .id(gpui::ElementId::Name(id.into()))
        .px_2()
        .py_1()
        .rounded_md()
        .text_size(px(11.0))
        .text_color(Theme::label_on(color))
        .bg(color)
        .cursor_pointer()
        .hover(|style| style.opacity(0.85))
        .on_click(on_click)
        .child(label.into())
        .into_any()
}

fn render_menu_button<F>(
    id: impl Into<SharedString>,
    label: String,
    color: gpui::Hsla,
    on_click: F,
) -> AnyElement
where
    F: Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
{
    div()
        .id(gpui::ElementId::Name(id.into()))
        .px_2()
        .py_1()
        .rounded_md()
        .text_size(px(12.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .bg(Theme::surface())
        .border_1()
        .border_color(Theme::border())
        .cursor_pointer()
        .hover(|style| style.bg(Theme::surface_hover()))
        .on_click(on_click)
        .child(label)
        .into_any()
}

fn credential_help(kind: crate::settings::BackendKind) -> &'static str {
    match kind {
        crate::settings::BackendKind::Hermes => "Local Hermes tokens are discovered automatically.",
        crate::settings::BackendKind::OpenClaw => {
            "Use gateway.auth.token. Van-Goal stores it here and keeps the paired-device token the gateway issues in its local app-data file; clearing removes both."
        }
        crate::settings::BackendKind::OpenCode => {
            "Matches OPENCODE_SERVER_PASSWORD when server authentication is enabled."
        }
        crate::settings::BackendKind::MiMoCode => {
            "Matches the password configured for mimo serve, when enabled."
        }
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::BackendKind;
    use gpui::{AppContext, TestAppContext};

    /// A box to lay the backend list out in: the test platform's window has no
    /// intrinsic size.
    struct SizedList(Vec<AnyElement>);

    impl Render for SizedList {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(560.0))
                .h(px(680.0))
                .flex()
                .flex_col()
                .children(std::mem::take(&mut self.0))
        }
    }

    /// Every backend needs its own switch, and each one has to be laid out with
    /// a real size inside its row — a list of seven is exactly where a layout
    /// bug would hide.
    #[gpui::test]
    fn every_backend_gets_its_own_switch(cx: &mut TestAppContext) {
        let state = cx.new(AppState::new);
        let settings = state.update(cx, |state, _cx| state.settings.clone());
        let rows: Vec<AnyElement> = BackendKind::ALL
            .iter()
            .map(|kind| {
                backend_row(
                    *kind,
                    settings.is_backend_enabled(*kind),
                    settings.backend_kind == *kind,
                    "Off".to_string(),
                    state.clone(),
                )
            })
            .collect();
        let (_host, cx) = cx.add_window_view(|_window, _cx| SizedList(rows));
        cx.run_until_parked();

        // `debug_bounds` takes a `&'static str`, so the selectors are spelled
        // out here in the same order as `BackendKind::ALL`.
        const SELECTORS: [(&str, &str); 7] = [
            ("backend-row-hermes", "backend-switch-hermes"),
            ("backend-row-opencode", "backend-switch-opencode"),
            ("backend-row-mimocode", "backend-switch-mimocode"),
            ("backend-row-codex", "backend-switch-codex"),
            ("backend-row-claudecode", "backend-switch-claudecode"),
            ("backend-row-pi", "backend-switch-pi"),
            ("backend-row-openclaw", "backend-switch-openclaw"),
        ];

        for (kind, (row_selector, switch_selector)) in BackendKind::ALL.iter().zip(SELECTORS) {
            let row = cx
                .debug_bounds(row_selector)
                .unwrap_or_else(|| panic!("{} has no row", kind.id()));
            assert!(
                f32::from(row.size.height) > 0.0,
                "{} row collapsed to zero height",
                kind.id()
            );

            let switch = cx
                .debug_bounds(switch_selector)
                .unwrap_or_else(|| panic!("{} has no switch", kind.id()));
            assert!(
                f32::from(switch.size.width) > 0.0 && f32::from(switch.size.height) > 0.0,
                "{} switch has no size",
                kind.id()
            );
            assert!(
                f32::from(switch.origin.x) >= f32::from(row.origin.x)
                    && f32::from(switch.origin.x) + f32::from(switch.size.width)
                        <= f32::from(row.origin.x) + f32::from(row.size.width) + 1.0,
                "{} switch escaped its row",
                kind.id()
            );
        }
    }
}
