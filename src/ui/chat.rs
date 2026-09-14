use crate::markdown;
use crate::models::{
    compact_token_count, duration_string, ContextUsage, MessageRole, PermissionMode,
};
use crate::state::AppState;
use crate::ui::editor::{Editor, EditorEvent};
use crate::ui::markdown_view::render_blocks;
use crate::ui::theme::Theme;
use gpui::{
    div, list, prelude::*, px, AnyElement, Context, Entity, Focusable, FontWeight,
    InteractiveElement, IntoElement, ListAlignment, ListState, ParentElement, Render, Styled,
    Window,
};
use std::time::Duration;

pub struct ChatView {
    state: Entity<AppState>,
    editor: Entity<Editor>,
    list_state: ListState,
    last_list_count: usize,
    last_scroll_signature: u64,
    needs_initial_focus: bool,
    expanded_messages: std::collections::HashSet<String>,
    model_menu_open: bool,
    permission_menu_open: bool,
}

impl ChatView {
    pub fn new(state: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| {
            let mut editor = Editor::new(cx);
            editor.set_placeholder("Ask your agent for follow-up changes", cx);
            editor
        });
        cx.observe(&state, |this, state, cx| {
            let composer_text = state.read(cx).composer_text.clone();
            if this.editor.read(cx).text() != composer_text {
                this.editor.update(cx, |editor, cx| {
                    editor.set_text(composer_text, cx);
                });
            }
            cx.notify();
        })
        .detach();
        cx.subscribe(&editor, |this, _editor, event: &EditorEvent, cx| {
            let state = this.state.clone();
            match event {
                EditorEvent::Change => {
                    let text = this.editor.read(cx).text().to_string();
                    state.update(cx, |state, cx| {
                        state.composer_text = text;
                        cx.notify();
                    });
                }
                EditorEvent::Submit => {
                    state.update(cx, |state, cx| state.send_composer(cx));
                }
            }
        })
        .detach();

        // Ticker keeping the "thinking" duration fresh while a turn runs.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let _ = this.update(cx, |this, cx| {
                let busy = this.state.read(cx).is_sending;
                if busy {
                    cx.notify();
                }
            });
        })
        .detach();

        Self {
            state,
            editor,
            list_state: ListState::new(0, ListAlignment::Top, px(500.0)),
            last_list_count: 0,
            last_scroll_signature: 0,
            needs_initial_focus: true,
            expanded_messages: std::collections::HashSet::new(),
            model_menu_open: false,
            permission_menu_open: false,
        }
    }

    fn scroll_signature(&self, cx: &Context<Self>) -> u64 {
        let state = self.state.read(cx);
        let mut signature: u64 = state.messages.len() as u64;
        if let Some(last) = state.messages.last() {
            signature = signature
                .wrapping_mul(31)
                .wrapping_add(last.content.len() as u64)
                .wrapping_add(if last.is_streaming { 7 } else { 0 })
                .wrapping_add(last.tool_calls.len() as u64 * 13);
        }
        if state.pending_clarify.is_some() {
            signature = signature.wrapping_add(1_000_003);
        }
        signature
    }
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Focus the composer once on first paint so typing works immediately.
        if self.needs_initial_focus {
            self.needs_initial_focus = false;
            let handle = self.editor.focus_handle(cx);
            window.on_next_frame(move |window, _cx| window.focus(&handle));
        }

        let signature = self.scroll_signature(cx);
        let (message_count, is_streaming) = {
            let state = self.state.read(cx);
            (
                state.messages.len(),
                state
                    .messages
                    .last()
                    .map(|m| m.is_streaming)
                    .unwrap_or(false),
            )
        };

        // Keep the variable-height list measurements in sync with the message
        // vec, and follow the streaming bubble while a turn is running.
        if message_count != self.last_list_count {
            let old_count = self.last_list_count;
            self.last_list_count = message_count;
            self.list_state.splice(0..old_count, message_count);
            if message_count > old_count && message_count > 0 {
                self.list_state.scroll_to_reveal_item(message_count - 1);
            }
        } else if signature != self.last_scroll_signature && is_streaming && message_count > 0 {
            self.list_state.splice(message_count - 1..message_count, 1);
            self.list_state.scroll_to_reveal_item(message_count - 1);
        }
        self.last_scroll_signature = signature;

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(Theme::window_bg())
            .text_color(Theme::text())
            .child(self.render_message_list(cx))
            .children(self.render_clarify_card(cx))
            .child(self.render_composer(cx))
    }
}

impl ChatView {
    fn render_message_list(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let state = self.state.read(cx);
        let is_empty = state.messages.is_empty();
        let backend_name = state.backend_display_name().to_string();

        if is_empty {
            return div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .justify_center()
                .px(px(28.0))
                .child(
                    div()
                        .max_w(px(520.0))
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .text_size(px(20.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(Theme::text())
                                .child("Ready"),
                        )
                        .child(
                            div()
                                .text_size(px(13.0))
                                .text_color(Theme::text_secondary())
                                .child(format!(
                                    "Hermit is ready to use {backend_name}. Send a message or resume a session from the sidebar."
                                )),
                        ),
                )
                .into_any();
        }

        let list_state = self.list_state.clone();
        let chat = cx.entity();
        let state_entity = self.state.clone();
        let messages = state_entity.read(cx).messages.clone();

        list(list_state, move |index, _window, cx| {
            let Some(message) = messages.get(index) else {
                return div().into_any();
            };
            let expanded = chat.read(cx).expanded_messages.contains(&message.id);
            render_message_bubble(message, expanded, chat.clone(), state_entity.clone())
        })
        .flex_1()
        .min_h_0()
        .into_any()
    }

    fn render_clarify_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state_snapshot = self.state.read(cx);
        let clarify = state_snapshot.pending_clarify.clone()?;
        let backend_name = state_snapshot.backend_display_name().to_string();
        let state = self.state.clone();

        let mut card = div()
            .mx(px(18.0))
            .mb_1()
            .p(px(14.0))
            .max_w(px(1120.0))
            .rounded_lg()
            .bg(Theme::clarify_bg())
            .border_1()
            .border_color(Theme::border_strong())
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(11.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(Theme::text_secondary())
                            .child(format!("{backend_name} needs your confirmation")),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("clarify-dismiss")
                            .text_size(px(11.0))
                            .text_color(Theme::text_secondary())
                            .cursor_pointer()
                            .hover(|style| style.text_color(Theme::text()))
                            .on_click(move |_event, _window, cx| {
                                state.update(cx, |state, cx| state.dismiss_clarify(cx));
                            })
                            .child("Skip"),
                    ),
            )
            .child(
                div()
                    .text_size(px(13.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(Theme::text())
                    .child(clarify.question.clone()),
            );

        for (index, choice) in clarify.choices.iter().enumerate() {
            let state = self.state.clone();
            let choice_for_click = choice.clone();
            card = card.child(
                div()
                    .id(gpui::ElementId::NamedInteger(
                        "clarify-choice".into(),
                        index as u64,
                    ))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(Theme::surface())
                    .border_1()
                    .border_color(Theme::border())
                    .cursor_pointer()
                    .hover(|style| style.bg(Theme::surface_hover()))
                    .on_click(move |_event, _window, cx| {
                        state.update(cx, |state, cx| {
                            state.answer_clarify(choice_for_click.clone(), cx)
                        });
                    })
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(Theme::text_secondary())
                            .child(format!("{}", index + 1)),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(Theme::text())
                            .child(choice.clone()),
                    ),
            );
        }

        let hint = if clarify.choices.is_empty() {
            "Type your answer below and press Enter"
        } else {
            "Or type a custom answer below and press Enter"
        };
        card = card.child(
            div()
                .text_size(px(10.0))
                .text_color(Theme::text_tertiary())
                .child(hint),
        );

        Some(card.into_any())
    }

    fn render_composer(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let chat = cx.entity();
        let caps = self.state.read(cx).backend_caps();
        let attachments = self.state.read(cx).composer_attachments.clone();
        let queued = self.state.read(cx).pending_queue.clone();
        let (can_send, is_sending, has_clarify) = {
            let state = self.state.read(cx);
            (
                state.can_send(),
                state.is_sending,
                state.pending_clarify.is_some(),
            )
        };
        let usage = self.state.read(cx).context_usage();
        let model_label = {
            let state = self.state.read(cx);
            let configured = state.current_model_name.trim().to_string();
            if configured.is_empty() {
                state
                    .selected_session
                    .as_ref()
                    .and_then(|session| session.model.clone())
                    .unwrap_or_else(|| state.backend_display_name().to_string())
            } else {
                configured
            }
        };
        let permission_mode = self.state.read(cx).permission_mode;
        let is_changing_permission = self.state.read(cx).is_changing_permission_mode;
        let is_switching_model = self.state.read(cx).is_switching_model;
        let provider_groups = self.state.read(cx).model_provider_groups.clone();
        let current_provider = self.state.read(cx).current_model_provider.clone();
        let current_model = self.state.read(cx).current_model_name.clone();

        let mut composer = div()
            .flex()
            .flex_col()
            .gap_2()
            .px(px(18.0))
            .pt_2()
            .pb_3()
            .max_w(px(1120.0))
            .w_full();

        if !attachments.is_empty() {
            let mut strip = div()
                .id("attachment-strip")
                .flex()
                .flex_row()
                .gap_2()
                .overflow_x_scroll();
            for attachment in attachments {
                let state = self.state.clone();
                let id = attachment.id.clone();
                strip = strip.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(Theme::surface())
                        .border_1()
                        .border_color(Theme::border())
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(Theme::text())
                                .max_w(px(140.0))
                                .text_ellipsis()
                                .child(attachment.name()),
                        )
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(Theme::text_tertiary())
                                .child(attachment.kind_label()),
                        )
                        .child(
                            div()
                                .id(gpui::ElementId::NamedInteger(
                                    "attachment-remove".into(),
                                    crate::ui::hash_id(&id),
                                ))
                                .text_size(px(11.0))
                                .text_color(Theme::text_secondary())
                                .cursor_pointer()
                                .hover(|style| style.text_color(Theme::danger()))
                                .on_click(move |_event, _window, cx| {
                                    state.update(cx, |state, cx| state.remove_attachment(&id, cx));
                                })
                                .child("✕"),
                        ),
                );
            }
            composer = composer.child(strip);
        }

        if !queued.is_empty() {
            let mut queue_block = div()
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_md()
                .bg(Theme::surface())
                .border_1()
                .border_color(Theme::border())
                .child(
                    div()
                        .text_size(px(10.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(Theme::text_secondary())
                        .child(format!("Queued ({})", queued.len())),
                );
            for item in queued {
                let id_hash = crate::ui::hash_id(&item.id);
                let state_send = self.state.clone();
                let state_edit = self.state.clone();
                let state_cancel = self.state.clone();
                let send_id = item.id.clone();
                let edit_id = item.id.clone();
                let cancel_id = item.id.clone();
                let preview = if !item.attachments.is_empty() {
                    let names = item
                        .attachments
                        .iter()
                        .map(|a| format!("@{}", a.name()))
                        .collect::<Vec<_>>()
                        .join(" ");
                    let base = if item.text.is_empty() {
                        "Attached files".to_string()
                    } else {
                        item.text.clone()
                    };
                    format!("{base}\n[{names}]")
                } else {
                    item.text.clone()
                };
                queue_block = queue_block.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(Theme::input_bg())
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(Theme::text_secondary())
                                .flex_1()
                                .child(preview),
                        )
                        .child(
                            div()
                                .id(gpui::ElementId::NamedInteger("queue-send".into(), id_hash))
                                .text_size(px(11.0))
                                .text_color(Theme::accent())
                                .cursor_pointer()
                                .on_click(move |_event, _window, cx| {
                                    state_send.update(cx, |state, cx| {
                                        state.send_queued_now(&send_id, cx)
                                    });
                                })
                                .child("Send now"),
                        )
                        .child(
                            div()
                                .id(gpui::ElementId::NamedInteger("queue-edit".into(), id_hash))
                                .text_size(px(11.0))
                                .text_color(Theme::text_secondary())
                                .cursor_pointer()
                                .on_click(move |_event, _window, cx| {
                                    state_edit
                                        .update(cx, |state, cx| state.edit_queued(&edit_id, cx));
                                })
                                .child("Edit"),
                        )
                        .child(
                            div()
                                .id(gpui::ElementId::NamedInteger(
                                    "queue-cancel".into(),
                                    id_hash,
                                ))
                                .text_size(px(11.0))
                                .text_color(Theme::text_secondary())
                                .cursor_pointer()
                                .hover(|style| style.text_color(Theme::danger()))
                                .on_click(move |_event, _window, cx| {
                                    state_cancel.update(cx, |state, cx| {
                                        state.cancel_queued(&cancel_id, cx)
                                    });
                                })
                                .child("Cancel"),
                        ),
                );
            }
            composer = composer.child(queue_block);
        }

        let editor = self.editor.clone();
        let state_for_files = self.state.clone();
        let shows_stop = is_sending && !has_clarify;
        let send_enabled = if has_clarify {
            !self.editor.read(cx).text().trim().is_empty()
        } else if is_sending {
            true
        } else {
            can_send
        };

        composer = composer.child(
            div()
                .rounded_xl()
                .bg(Theme::input_bg())
                .border_1()
                .border_color(Theme::border())
                .flex()
                .flex_col()
                .child(div().px_4().pt_2().pb_1().child(editor.clone()))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_3()
                        .px_4()
                        .pb_2()
                        .child(
                            div()
                                .id("add-attachment")
                                .text_size(px(14.0))
                                .text_color(Theme::text_secondary())
                                .cursor_pointer()
                                .hover(|style| style.text_color(Theme::text()))
                                .on_click(move |_event, _window, cx| {
                                    if let Some(files) = rfd::FileDialog::new()
                                        .set_title("Add attachments")
                                        .pick_files()
                                    {
                                        let paths: Vec<String> = files
                                            .into_iter()
                                            .map(|path| path.to_string_lossy().to_string())
                                            .collect();
                                        state_for_files.update(cx, |state, cx| {
                                            state.add_attachments(paths, cx)
                                        });
                                    }
                                })
                                .child("+"),
                        )
                        .when(
                            caps.contains(crate::models::BackendCaps::PERMISSION_MODES),
                            |this| {
                                this.child(render_menu_button(
                                    "permission-menu-button",
                                    format!(
                                        "{}{}",
                                        permission_mode.label(),
                                        if is_changing_permission { "…" } else { "" }
                                    ),
                                    Theme::warn(),
                                    {
                                        let chat = chat.clone();
                                        move |_event, _window, cx| {
                                            chat.update(cx, |chat, cx| {
                                                chat.permission_menu_open =
                                                    !chat.permission_menu_open;
                                                chat.model_menu_open = false;
                                                cx.notify();
                                            });
                                        }
                                    },
                                ))
                            },
                        )
                        .child(div().flex_1())
                        .when(
                            caps.contains(crate::models::BackendCaps::MODEL_SELECTION),
                            |this| {
                                this.child(render_context_ring(usage))
                                    .child(render_menu_button(
                                        "model-menu-button",
                                        if is_switching_model {
                                            "Switching…".to_string()
                                        } else {
                                            format!("{model_label} ▾")
                                        },
                                        Theme::text(),
                                        {
                                            let chat = chat.clone();
                                            move |_event, _window, cx| {
                                                chat.update(cx, |chat, cx| {
                                                    chat.model_menu_open = !chat.model_menu_open;
                                                    chat.permission_menu_open = false;
                                                    cx.notify();
                                                });
                                            }
                                        },
                                    ))
                            },
                        )
                        .child(render_send_button(
                            shows_stop,
                            send_enabled,
                            has_clarify,
                            is_sending,
                            self.state.clone(),
                        )),
                ),
        );

        let mut container = div()
            .relative()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .child(composer);

        // Menu overlays (rendered above the composer; a scrim closes them).
        if self.permission_menu_open {
            container =
                container
                    .child(render_menu_scrim(chat.clone()))
                    .child(render_permission_menu(
                        permission_mode,
                        chat.clone(),
                        self.state.clone(),
                    ));
        } else if self.model_menu_open {
            container = container
                .child(render_menu_scrim(chat.clone()))
                .child(render_model_menu(
                    &provider_groups,
                    &current_provider,
                    &current_model,
                    chat.clone(),
                    self.state.clone(),
                ));
        }

        container.into_any()
    }
}

fn render_menu_scrim(chat: Entity<ChatView>) -> AnyElement {
    div()
        .id("menu-scrim")
        .absolute()
        .inset_0()
        .cursor_pointer()
        .on_click(move |_event, _window, cx| {
            chat.update(cx, |chat, cx| {
                chat.model_menu_open = false;
                chat.permission_menu_open = false;
                cx.notify();
            });
        })
        .into_any()
}

fn render_menu_button(
    id: &'static str,
    label: String,
    color: gpui::Hsla,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> AnyElement {
    div()
        .id(gpui::ElementId::Name(id.into()))
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_2()
        .py_1()
        .rounded_md()
        .text_size(px(11.0))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .cursor_pointer()
        .hover(|style| style.bg(Theme::tool_bg()))
        .on_click(on_click)
        .child(label)
        .into_any()
}

fn render_permission_menu(
    current: PermissionMode,
    chat: Entity<ChatView>,
    state: Entity<AppState>,
) -> AnyElement {
    div()
        .absolute()
        .bottom(px(64.0))
        .left(px(48.0))
        .rounded_md()
        .bg(Theme::surface())
        .border_1()
        .border_color(Theme::border_strong())
        .py_1()
        .min_w(px(150.0))
        .flex()
        .flex_col()
        .children(PermissionMode::ALL.iter().map(|option| {
            let state = state.clone();
            let chat = chat.clone();
            let option = *option;
            div()
                .id(gpui::ElementId::Name(format!("perm-{option:?}").into()))
                .flex()
                .flex_row()
                .items_center()
                .px_3()
                .py_1()
                .text_size(px(11.0))
                .text_color(Theme::text())
                .cursor_pointer()
                .hover(|style| style.bg(Theme::surface_hover()))
                .on_click(move |_event, _window, cx| {
                    chat.update(cx, |chat, cx| {
                        chat.permission_menu_open = false;
                        cx.notify();
                    });
                    state.update(cx, |state, cx| state.set_permission_mode(option, cx));
                })
                .child(div().flex_1().child(option.label()))
                .when(option == current, |this| {
                    this.child(div().text_color(Theme::accent()).child("✓"))
                })
        }))
        .into_any()
}

fn render_model_menu(
    groups: &[(String, Vec<crate::models::ModelOption>)],
    current_provider: &str,
    current_model: &str,
    chat: Entity<ChatView>,
    state: Entity<AppState>,
) -> AnyElement {
    let mut menu = div()
        .absolute()
        .bottom(px(64.0))
        .right(px(48.0))
        .id("model-menu")
        .max_h(px(360.0))
        .w(px(280.0))
        .overflow_y_scroll()
        .rounded_lg()
        .bg(Theme::surface())
        .border_1()
        .border_color(Theme::border_strong())
        .py_1()
        .flex()
        .flex_col()
        .child(
            div()
                .px_3()
                .py_1()
                .text_size(px(10.0))
                .text_color(Theme::text_tertiary())
                .child("Models"),
        );

    for (provider, models) in groups {
        menu = menu.child(
            div()
                .px_3()
                .pt_1()
                .text_size(px(10.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(Theme::text_secondary())
                .child(provider.clone()),
        );
        for option in models {
            let is_current = option.provider == current_provider && option.model == current_model;
            let state = state.clone();
            let chat = chat.clone();
            let option = option.clone();
            let option_label = option.model.clone();
            let option_hash =
                crate::ui::hash_id(&format!("{}\u{1f}{}", option.provider, option.model));
            menu = menu.child(
                div()
                    .id(gpui::ElementId::NamedInteger(
                        "model-option".into(),
                        option_hash,
                    ))
                    .flex()
                    .flex_row()
                    .items_center()
                    .px_3()
                    .py_1()
                    .text_size(px(11.0))
                    .text_color(Theme::text())
                    .cursor_pointer()
                    .hover(|style| style.bg(Theme::surface_hover()))
                    .on_click(move |_event, _window, cx| {
                        chat.update(cx, |chat, cx| {
                            chat.model_menu_open = false;
                            cx.notify();
                        });
                        state.update(cx, |state, cx| {
                            state.select_hermes_model(option.clone(), cx)
                        });
                    })
                    .child(div().flex_1().child(option_label))
                    .when(is_current, |this| {
                        this.child(div().text_color(Theme::accent()).child("✓"))
                    }),
            );
        }
    }

    menu.into_any()
}

fn render_message_bubble(
    message: &crate::models::ChatMessage,
    expanded: bool,
    chat: Entity<ChatView>,
    state: Entity<AppState>,
) -> AnyElement {
    let id_hash = crate::ui::hash_id(&message.id);
    match message.role {
        MessageRole::User => div()
            .w_full()
            .flex()
            .flex_row()
            .justify_end()
            .px(px(24.0))
            .py_1()
            .child(
                div()
                    .max_w(px(640.0))
                    .px(px(14.0))
                    .py(px(10.0))
                    .rounded_lg()
                    .bg(Theme::user_bubble())
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(message.attachments.iter().map(|attachment| {
                        div()
                            .text_size(px(10.0))
                            .text_color(Theme::text_secondary())
                            .child(format!("@{}", attachment.name()))
                    }))
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(Theme::text())
                            .child(render_blocks(&markdown::parse(&message.content))),
                    ),
            )
            .into_any(),
        MessageRole::Assistant => {
            let mut bubble = div().w_full().flex().flex_col().gap_1().px(px(24.0)).py_1();

            bubble = bubble.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(10.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(Theme::text_secondary())
                            .child("HERMES"),
                    )
                    .when(message.is_streaming, |this| {
                        this.child(
                            div()
                                .text_size(px(10.0))
                                .text_color(Theme::accent())
                                .child("streaming…"),
                        )
                    }),
            );

            if message.is_streaming || !message.tool_calls.is_empty() {
                bubble = bubble.child(render_activity(message, expanded, id_hash, chat.clone()));
            }

            if !message.content.trim().is_empty() {
                bubble = bubble.child(render_blocks(&markdown::parse(&message.content)));
            }

            if !message.is_streaming && !message.content.trim().is_empty() {
                let content = message.content.clone();
                bubble = bubble.child(
                    div().flex().flex_row().child(
                        div()
                            .id(gpui::ElementId::NamedInteger(
                                "copy-message".into(),
                                id_hash,
                            ))
                            .px_2()
                            .py(px(2.0))
                            .rounded_full()
                            .bg(Theme::tool_bg())
                            .text_size(px(10.0))
                            .text_color(Theme::text_tertiary())
                            .cursor_pointer()
                            .hover(|style| style.text_color(Theme::text()))
                            .on_click(move |_event, _window, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                    content.clone(),
                                ));
                            })
                            .child("Copy"),
                    ),
                );
            }

            let _ = state;
            bubble.into_any()
        }
        _ => div()
            .px(px(24.0))
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(Theme::text_secondary())
                    .child(message.content.clone()),
            )
            .into_any(),
    }
}

fn render_activity(
    message: &crate::models::ChatMessage,
    expanded: bool,
    id_hash: u64,
    chat: Entity<ChatView>,
) -> AnyElement {
    let seconds = (crate::models::now_unix() - message.timestamp).max(0.0) as i64;
    let thinking_label = if message.is_streaming {
        format!("Thinking · {}", duration_string(seconds))
    } else {
        format!("Thought for {}", duration_string(seconds))
    };

    let mut row = div().flex().flex_row().items_center().gap_2().child(
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(Theme::tool_bg())
            .text_size(px(11.0))
            .text_color(Theme::text_secondary())
            .child(thinking_label),
    );

    if !message.tool_calls.is_empty() {
        let message_id = message.id.clone();
        row = row.child(
            div()
                .id(gpui::ElementId::NamedInteger("tool-pill".into(), id_hash))
                .px_2()
                .py_1()
                .rounded_md()
                .bg(Theme::tool_bg())
                .text_size(px(11.0))
                .text_color(Theme::text_secondary())
                .cursor_pointer()
                .hover(|style| style.bg(Theme::surface_hover()))
                .on_click(move |_event, _window, cx| {
                    chat.update(cx, |chat, cx| {
                        if chat.expanded_messages.contains(&message_id) {
                            chat.expanded_messages.remove(&message_id);
                        } else {
                            chat.expanded_messages.insert(message_id.clone());
                        }
                        cx.notify();
                    });
                })
                .child(format!(
                    "Tool calls · {} · {}",
                    message.tool_calls.len(),
                    if expanded { "hide" } else { "show" }
                )),
        );
    }

    let mut container = div().flex().flex_col().gap_1().child(row);
    if expanded {
        for (tool_index, call) in message.tool_calls.iter().take(12).enumerate() {
            let detail = if call.detail.len() > 3000 {
                format!("{}\n...", &call.detail[..3000])
            } else if call.detail.is_empty() {
                "No detail".to_string()
            } else {
                call.detail.clone()
            };
            container = container.child(
                div()
                    .ml_2()
                    .p_2()
                    .rounded_md()
                    .bg(Theme::input_bg())
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(11.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(Theme::text())
                            .child(format!(
                                "{}. {} · {}",
                                tool_index + 1,
                                call.name,
                                call.status
                            )),
                    )
                    .child(
                        div()
                            .font_family("Menlo")
                            .text_size(px(10.0))
                            .text_color(Theme::text_secondary())
                            .child(detail),
                    ),
            );
        }
        if message.tool_calls.len() > 12 {
            container = container.child(
                div()
                    .ml_2()
                    .text_size(px(10.0))
                    .text_color(Theme::text_tertiary())
                    .child(format!(
                        "{} more tool event(s) hidden",
                        message.tool_calls.len() - 12
                    )),
            );
        }
    }
    container.into_any()
}

fn render_context_ring(usage: ContextUsage) -> AnyElement {
    let percent = usage.percent();
    let color = if usage.ratio() >= 0.9 {
        Theme::danger()
    } else if usage.ratio() >= 0.72 {
        Theme::warn()
    } else {
        Theme::text_secondary()
    };
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .child(
            div()
                .w(px(36.0))
                .h(px(4.0))
                .rounded_full()
                .bg(Theme::border())
                .child(
                    div()
                        .h_full()
                        .w(gpui::relative(usage.ratio().max(0.01)))
                        .rounded_full()
                        .bg(color),
                ),
        )
        .child(
            div()
                .text_size(px(10.0))
                .text_color(Theme::text_tertiary())
                .child(format!(
                    "{}% · {}/{}",
                    percent,
                    compact_token_count(usage.used_tokens),
                    compact_token_count(usage.max_tokens)
                )),
        )
        .into_any()
}

fn render_send_button(
    shows_stop: bool,
    enabled: bool,
    has_clarify: bool,
    is_sending: bool,
    state: Entity<AppState>,
) -> AnyElement {
    let color = if shows_stop {
        Theme::danger()
    } else if enabled {
        Theme::accent()
    } else {
        Theme::border_strong()
    };
    let label = if shows_stop { "■" } else { "↑" };
    div()
        .id("send-button")
        .size(px(28.0))
        .rounded_full()
        .bg(color)
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(12.0))
        .text_color(gpui::white())
        .cursor_pointer()
        .when(!enabled, |this| this.opacity(0.5))
        .hover(|style| style.opacity(0.85))
        .on_click(move |_event, _window, cx| {
            if has_clarify {
                state.update(cx, |state, cx| state.send_composer(cx));
            } else if is_sending {
                state.update(cx, |state, cx| state.interrupt_running(cx));
            } else {
                state.update(cx, |state, cx| state.send_composer(cx));
            }
        })
        .child(label)
        .into_any()
}
