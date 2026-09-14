use crate::state::AppState;
use crate::ui::editor::{Editor, EditorEvent};
use crate::ui::theme::Theme;
use gpui::{
    actions, div, prelude::*, px, AnyElement, Context, Entity, FontWeight, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, Styled, Window,
};
use std::time::Duration;

actions!(hermit, [OpenSettings]);

pub struct SettingsView {
    state: Entity<AppState>,
    host_editor: Entity<Editor>,
    port_editor: Entity<Editor>,
    workspace_editor: Entity<Editor>,
    profile_editor: Entity<Editor>,
    token_editor: Entity<Editor>,
    backend_menu_open: bool,
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
                    title: Some("Hermit Settings".into()),
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
        let persist_host = {
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
        };
        let _ = persist_host;
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

        cx.observe(&state, |_, _, cx| cx.notify()).detach();
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
            backend_menu_open: false,
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

        // Backend section
        let mut backend_section = section("Agent backend");
        let chat = cx.entity();
        backend_section = backend_section
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(render_menu_button(
                        "settings-backend-picker",
                        format!("{} ▾", settings.backend_kind.display_name()),
                        Theme::accent(),
                        {
                            let chat = chat.clone();
                            move |_event, _window, cx| {
                                chat.update(cx, |chat, cx| {
                                    chat.backend_menu_open = !chat.backend_menu_open;
                                    cx.notify();
                                });
                            }
                        },
                    ))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(Theme::text_secondary())
                            .child(settings.backend_kind.description()),
                    ),
            )
            .child(toggle_row(
                "Connect on launch",
                settings.auto_connect,
                state.clone(),
                |state, _cx| {
                    state.settings.auto_connect = !state.settings.auto_connect;
                    state.settings.save();
                },
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
                    "Local address: Hermit starts and manages hermes serve automatically."
                } else if settings.backend_kind == crate::settings::BackendKind::OpenClaw {
                    "For a reverse-proxy path, paste the complete ws:// or wss:// URL; it overrides Port and TLS."
                } else {
                    "Hermit connects to an already-running server at this address."
                }));
        } else {
            connection = connection
                .child(field_row("Workspace", self.workspace_editor.clone()))
                .child(hint("Hermit launches the installed CLI in this directory."));
        }
        if settings.backend_kind == crate::settings::BackendKind::Hermes {
            connection = connection
                .child(field_row("Default profile", self.profile_editor.clone()))
                .child(hint("Leave empty to use the Hermes default profile."));
        }
        connection = connection.child(
            div()
                .flex()
                .flex_row()
                .gap_2()
                .child(small_button(
                    "connect-now",
                    format!("Connect to {}", settings.backend_kind.display_name()),
                    Theme::accent(),
                    {
                        let state = state.clone();
                        move |_event, _window, cx| {
                            state.update(cx, |state, cx| state.connect(cx));
                        }
                    },
                ))
                .when(
                    settings.backend_kind == crate::settings::BackendKind::Hermes,
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

        // Credentials section
        let mut credentials = section("Credentials");
        if uses_network {
            credentials = credentials
                .child(credential_field(
                    settings.backend_kind.credential_label(),
                    self.token_editor.clone(),
                    settings.session_token.trim().chars().count(),
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
                    state.update(cx, |state, _cx| {
                        state.settings.session_token = String::new();
                        state.settings.save();
                    });
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

        // Backend picker popup
        if self.backend_menu_open {
            root = root.child(render_backend_menu(cx.entity(), self.state.clone()));
        }

        root
    }
}

impl SettingsView {
    fn toggle_backend(&mut self, kind: crate::settings::BackendKind, cx: &mut Context<Self>) {
        self.backend_menu_open = false;
        let state = self.state.clone();
        state.update(cx, |state, cx| state.switch_backend(kind, cx));
        // Refresh editor fields from the (possibly changed) settings.
        let settings = state.read(cx).settings.clone();
        self.host_editor.update(cx, |editor, cx| {
            editor.set_text(settings.backend_host.clone(), cx)
        });
        self.port_editor.update(cx, |editor, cx| {
            editor.set_text(settings.backend_port.to_string(), cx)
        });
        self.token_editor.update(cx, |editor, cx| {
            editor.set_text(settings.session_token.clone(), cx)
        });
        cx.notify();
    }
}

fn render_backend_menu(chat: Entity<SettingsView>, state: Entity<AppState>) -> AnyElement {
    div()
        .absolute()
        .top(px(48.0))
        .left(px(28.0))
        .w(px(240.0))
        .rounded_lg()
        .bg(Theme::surface())
        .border_1()
        .border_color(Theme::border_strong())
        .py_1()
        .flex()
        .flex_col()
        .children(crate::settings::BackendKind::ALL.iter().map(|kind| {
            let chat = chat.clone();
            let state = state.clone();
            let kind = *kind;
            div()
                .id(gpui::ElementId::Name(
                    format!("backend-{}", kind.id()).into(),
                ))
                .px_3()
                .py_1()
                .text_size(px(12.0))
                .text_color(Theme::text())
                .cursor_pointer()
                .hover(|style| style.bg(Theme::surface_hover()))
                .on_click(move |_event, _window, cx| {
                    chat.update(cx, |chat, cx| chat.toggle_backend(kind, cx));
                    let _ = &state;
                })
                .child(kind.display_name())
        }))
        .into_any()
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

fn credential_field(
    label: &'static str,
    editor: Entity<Editor>,
    character_count: usize,
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
                        .text_color(Theme::text_tertiary())
                        .child(format!("{character_count} characters stored")),
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
        .text_color(if color == Theme::surface_hover() {
            Theme::text()
        } else {
            gpui::black()
        })
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
            "Use gateway.auth.token. Hermit stores it in its local settings and keeps the paired-device token in its local app-data file."
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

// Keep SharedString import used on all platforms.
#[allow(unused)]
fn _use_shared(s: SharedString) -> String {
    s.to_string()
}

// Silence unused import warning for Duration (kept for future debounces).
#[allow(unused)]
fn _use_duration() -> Duration {
    Duration::from_secs(1)
}
