use crate::models::{AgentSession, ConnectionState};
use crate::state::AppState;
use gpui::{
    div, prelude::*, px, AnyElement, Context, Entity, FontWeight, Hsla, InteractiveElement,
    IntoElement, ParentElement, Render, Stateful, StatefulInteractiveElement, Styled, Window,
};

/// Session list width. Wide enough for a model name under a truncated title,
/// narrow enough to leave the transcript the bulk of the window.
const SIDEBAR_WIDTH: f32 = 244.0;

/// Sidebar: status pill, batch actions, and the session list.
pub struct SidebarView {
    state: Entity<AppState>,
    confirm_action: Option<ConfirmAction>,
}

#[derive(Clone, PartialEq)]
enum ConfirmAction {
    ArchiveAll,
    DeleteAll,
    /// Per-session actions, so one conversation can be archived or deleted
    /// without touching the rest of the list. Confirmed the same way as the
    /// bulk actions.
    ArchiveOne(Box<AgentSession>),
    DeleteOne(Box<AgentSession>),
}

impl SidebarView {
    pub fn new(state: Entity<AppState>, _cx: &mut Context<Self>) -> Self {
        Self {
            state,
            confirm_action: None,
        }
    }
}

impl Render for SidebarView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.clone();
        let (connection_label, pill_color, session_count) = {
            let state = state.read(cx);
            (
                state.connection_state.pill_label().to_string(),
                pill_color(&state.connection_state),
                state.sessions.len(),
            )
        };

        let confirm = self.confirm_action.clone();

        div()
            .flex()
            .flex_col()
            .h_full()
            .w(gpui::px(SIDEBAR_WIDTH))
            .debug_selector(|| "session-sidebar".into())
            .bg(crate::ui::theme::Theme::sidebar_bg())
            .border_r_1()
            .border_color(crate::ui::theme::Theme::border())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_3()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .px_2()
                            .py_1()
                            .rounded_full()
                            .bg(crate::ui::theme::Theme::surface())
                            .child(div().size(px(7.0)).rounded_full().bg(pill_color))
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(crate::ui::theme::Theme::text_secondary())
                                    .child(connection_label),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        header_button("Archive all", session_count > 0).on_click(cx.listener(
                            move |this, _event, _window, cx| {
                                this.confirm_action = Some(ConfirmAction::ArchiveAll);
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        header_button("Delete all", session_count > 0).on_click(cx.listener(
                            move |this, _event, _window, cx| {
                                this.confirm_action = Some(ConfirmAction::DeleteAll);
                                cx.notify();
                            },
                        )),
                    ),
            )
            .child(
                div()
                    .h(px(1.0))
                    .w_full()
                    .bg(crate::ui::theme::Theme::border()),
            )
            .children(confirm.map(|action| self.render_confirm(action, cx)))
            .child(
                div()
                    .id("sidebar-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(self.render_session_list(cx)),
            )
    }
}

impl SidebarView {
    fn render_confirm(&self, action: ConfirmAction, cx: &mut Context<Self>) -> AnyElement {
        let count = self.state.read(cx).sessions.len();
        let (title, body, confirm_label, is_destructive) = match &action {
            ConfirmAction::ArchiveAll => (
                format!("Archive all {count} sessions?"),
                "Sessions will be removed from the list; transcripts are kept.".to_string(),
                "Archive all",
                false,
            ),
            ConfirmAction::DeleteAll => (
                format!("Delete all {count} sessions?"),
                "Sessions and their transcripts will be permanently deleted.".to_string(),
                "Delete all",
                true,
            ),
            ConfirmAction::ArchiveOne(session) => (
                format!("Archive “{}”?", session.display_title()),
                "The session leaves the list; its transcript is kept.".to_string(),
                "Archive",
                false,
            ),
            ConfirmAction::DeleteOne(session) => (
                format!("Delete “{}”?", session.display_title()),
                "The session and its transcript will be permanently deleted.".to_string(),
                "Delete",
                true,
            ),
        };
        div()
            .mx_3()
            .my_2()
            .p_3()
            .rounded_lg()
            .bg(crate::ui::theme::Theme::surface())
            .border_1()
            .border_color(crate::ui::theme::Theme::border())
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(crate::ui::theme::Theme::text())
                    .child(title),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(crate::ui::theme::Theme::text_secondary())
                    .child(body),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .justify_end()
                    .child(
                        small_button("Cancel", crate::ui::theme::Theme::surface_hover()).on_click(
                            cx.listener(move |this, _event, _window, cx| {
                                this.confirm_action = None;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        small_button(
                            confirm_label,
                            if is_destructive {
                                crate::ui::theme::Theme::danger()
                            } else {
                                crate::ui::theme::Theme::accent()
                            },
                        )
                        .on_click(cx.listener(
                            move |this, _event, _window, cx| {
                                this.confirm_action = None;
                                let state = this.state.clone();
                                match action.clone() {
                                    ConfirmAction::ArchiveAll => {
                                        state.update(cx, |state, cx| state.archive_all(cx))
                                    }
                                    ConfirmAction::DeleteAll => {
                                        state.update(cx, |state, cx| state.delete_all(cx))
                                    }
                                    ConfirmAction::ArchiveOne(session) => state
                                        .update(cx, |state, cx| {
                                            state.archive_session(&session, cx)
                                        }),
                                    ConfirmAction::DeleteOne(session) => state
                                        .update(cx, |state, cx| state.delete_session(&session, cx)),
                                }
                            },
                        )),
                    ),
            )
            .into_any()
    }

    fn render_session_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let state = self.state.clone();
        let (selected_id, sessions) = {
            let state = state.read(cx);
            (
                state
                    .selected_session
                    .as_ref()
                    .map(|session| session.id.clone()),
                state.sessions.clone(),
            )
        };

        if sessions.is_empty() {
            return div()
                .p_4()
                .text_size(px(11.0))
                .text_color(crate::ui::theme::Theme::text_tertiary())
                .child("No sessions yet. Send a message to start one.")
                .into_any();
        }

        let sidebar = cx.entity();
        let rows = sessions
            .into_iter()
            .map(|session| {
                let is_selected = selected_id.as_deref() == Some(session.id.as_str());
                let row_hash = hash_id(&session.id);
                let archive_target = session.clone();
                let delete_target = session.clone();
                let group = format!("session-row-{row_hash}");
                let has_metadata = session.message_count.is_some()
                    || !session.profile.clone().unwrap_or_default().is_empty();
                div()
                    .id(gpui::ElementId::NamedInteger(
                        "session-row".into(),
                        hash_id(&session.id),
                    ))
                    .group(group.clone())
                    .debug_selector(move || format!("session-row-{row_hash}"))
                    .relative()
                    .px_3()
                    .py_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .when(is_selected, |this| {
                        this.bg(crate::ui::theme::Theme::accent_soft())
                    })
                    .hover(|style| style.bg(crate::ui::theme::Theme::surface_hover()))
                    .cursor_pointer()
                    .on_click({
                        let state = state.clone();
                        let session_for_click = session.clone();
                        move |_event, _window, cx| {
                            state.update(cx, |state, cx| {
                                state.resume_session(session_for_click.clone(), cx)
                            });
                        }
                    })
                    .child(render_row_title(&session))
                    .when(!session.subtitle().is_empty(), |this| {
                        this.child(
                            div()
                                .text_size(px(11.0))
                                .text_color(crate::ui::theme::Theme::text_tertiary())
                                .max_w_full()
                                .text_ellipsis()
                                .child(session.subtitle()),
                        )
                    })
                    // Only spend a line on metadata when there is some. The row
                    // actions are an overlay instead of another line, so a plain
                    // session stays two lines tall and the title keeps its width.
                    .when(has_metadata, |this| {
                        this.child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .text_size(px(10.0))
                                .text_color(crate::ui::theme::Theme::text_tertiary())
                                .children(
                                    session
                                        .message_count
                                        .map(|count| div().child(format!("{count} messages"))),
                                )
                                .when(
                                    !session.profile.clone().unwrap_or_default().is_empty(),
                                    |this| {
                                        this.child(
                                            div()
                                                .child(session.profile.clone().unwrap_or_default()),
                                        )
                                    },
                                ),
                        )
                    })
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .bottom_0()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .pl_1()
                            .pr_2()
                            .bg(crate::ui::theme::Theme::surface_hover())
                            .opacity(0.0)
                            .group_hover(group.clone(), |style| style.opacity(1.0))
                            .child(session_row_action(
                                format!("session-archive-{row_hash}"),
                                "Archive",
                                {
                                    let sidebar = sidebar.clone();
                                    move |_event, _window, cx| {
                                        // The row itself resumes the session, so the
                                        // action must not also trigger that.
                                        cx.stop_propagation();
                                        sidebar.update(cx, |this, cx| {
                                            this.confirm_action = Some(ConfirmAction::ArchiveOne(
                                                Box::new(archive_target.clone()),
                                            ));
                                            cx.notify();
                                        });
                                    }
                                },
                            ))
                            .child(session_row_action(
                                format!("session-delete-{row_hash}"),
                                "Delete",
                                {
                                    let sidebar = sidebar.clone();
                                    move |_event, _window, cx| {
                                        cx.stop_propagation();
                                        sidebar.update(cx, |this, cx| {
                                            this.confirm_action = Some(ConfirmAction::DeleteOne(
                                                Box::new(delete_target.clone()),
                                            ));
                                            cx.notify();
                                        });
                                    }
                                },
                            )),
                    )
            })
            .collect::<Vec<_>>();

        div().flex().flex_col().children(rows).into_any()
    }
}

/// A small text action on a session row. The row's own click resumes the
/// session, so these stop propagation and hand the choice to the confirmation
/// panel above the list.
fn session_row_action<F>(id: String, label: &'static str, on_click: F) -> Stateful<gpui::Div>
where
    F: Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
{
    div()
        .id(gpui::ElementId::Name(id.into()))
        .px_1()
        .rounded_sm()
        .cursor_pointer()
        .hover(|style| style.bg(crate::ui::theme::Theme::accent_soft()))
        .on_click(on_click)
        .child(label)
}

fn render_row_title(session: &AgentSession) -> gpui::AnyElement {
    let active = session.is_active == Some(true);
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(crate::ui::theme::Theme::text())
                .max_w_full()
                .text_ellipsis()
                .child(session.display_title()),
        )
        .when(active, |this| {
            this.child(
                div()
                    .size(px(6.0))
                    .rounded_full()
                    .bg(crate::ui::theme::Theme::ok()),
            )
        })
        .into_any()
}

fn header_button(label: &'static str, enabled: bool) -> Stateful<gpui::Div> {
    let base = div()
        .id(element_id_name(label))
        .px_2()
        .py_1()
        .rounded_md()
        .text_size(px(11.0))
        .cursor_pointer();
    if enabled {
        base.text_color(crate::ui::theme::Theme::text_secondary())
            .hover(|style| {
                style
                    .bg(crate::ui::theme::Theme::surface_hover())
                    .text_color(crate::ui::theme::Theme::text())
            })
    } else {
        base.text_color(crate::ui::theme::Theme::text_tertiary())
    }
}

fn small_button(label: &'static str, color: Hsla) -> Stateful<gpui::Div> {
    div()
        .id(element_id_name(label))
        .px_2()
        .py_1()
        .rounded_md()
        .text_size(px(11.0))
        .text_color(gpui::black())
        .bg(color)
        .cursor_pointer()
        .hover(|style| style.opacity(0.85))
}

// ElementId::Name takes a SharedString; tiny shim keeps call sites tidy.
fn element_id_name(label: &'static str) -> gpui::ElementId {
    gpui::ElementId::Name(label.into())
}

pub fn hash_id(value: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in value.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn pill_color(state: &ConnectionState) -> Hsla {
    match state {
        ConnectionState::Connected => crate::ui::theme::Theme::ok(),
        ConnectionState::Connecting => crate::ui::theme::Theme::warn(),
        ConnectionState::Disconnected => crate::ui::theme::Theme::text_tertiary(),
        ConnectionState::Degraded(_) => crate::ui::theme::Theme::warn(),
        ConnectionState::Failed(_) => crate::ui::theme::Theme::danger(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext, VisualTestContext};

    /// A box to lay the sidebar out in: the test platform's window has no
    /// intrinsic size.
    struct SizedSidebar(Entity<SidebarView>);

    impl Render for SizedSidebar {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(900.0))
                .h(px(600.0))
                .flex()
                .flex_row()
                .child(self.0.clone())
        }
    }

    fn sidebar_with(
        cx: &mut TestAppContext,
        sessions: Vec<AgentSession>,
    ) -> &mut VisualTestContext {
        let state = cx.new(AppState::new);
        state.update(cx, |state, _cx| {
            state.selected_session = None;
            state.messages = Vec::new();
            state.sessions = sessions;
        });
        let (_host, cx) = cx
            .add_window_view(|_window, cx| SizedSidebar(cx.new(|cx| SidebarView::new(state, cx))));
        cx.run_until_parked();
        cx
    }

    fn session(id: &str, title: &str, model: &str, messages: Option<i64>) -> AgentSession {
        AgentSession {
            id: id.to_string(),
            title: Some(title.to_string()),
            model: Some(model.to_string()),
            message_count: messages,
            ..AgentSession::default()
        }
    }

    fn row_bounds(cx: &mut VisualTestContext, id: &str) -> gpui::Bounds<gpui::Pixels> {
        let selector: &'static str =
            Box::leak(format!("session-row-{}", hash_id(id)).into_boxed_str());
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("row for {id} was not laid out"))
    }

    #[gpui::test]
    fn the_sidebar_keeps_its_configured_width(cx: &mut TestAppContext) {
        let cx = sidebar_with(
            cx,
            vec![session(
                "a-very-long-session-identifier-that-would-love-to-widen-the-list",
                "Cron: 情话-晚间档 20:05 — a title long enough to want more room than it gets",
                "deepseek/deepseek-v4.1-flash",
                None,
            )],
        );
        let sidebar = cx
            .debug_bounds("session-sidebar")
            .expect("sidebar was not laid out");
        assert_eq!(
            f32::from(sidebar.size.width),
            SIDEBAR_WIDTH,
            "a long title must not widen the sidebar"
        );
    }

    /// A session with no message count and no profile is two lines: title and
    /// model. The row actions are an overlay, so they must not add a line.
    #[gpui::test]
    fn a_plain_session_row_stays_two_lines_tall(cx: &mut TestAppContext) {
        let cx = sidebar_with(
            cx,
            vec![session(
                "plain",
                "Cron: 情话-晚间档 20:05",
                "deepseek/deepseek-v4.1-flash",
                None,
            )],
        );
        let row = row_bounds(cx, "plain");
        let height = f32::from(row.size.height);
        // Title + model + row padding measures 60px. A third line of metadata
        // pushes it past 74px, so this bound is what keeps the actions from
        // quietly becoming a line of their own again.
        assert!(
            height <= 66.0,
            "a plain row should be title + model, but it is {height}px tall"
        );
    }

    /// The actions must sit inside the row, not push it wider.
    #[gpui::test]
    fn every_row_fits_inside_the_sidebar(cx: &mut TestAppContext) {
        let cx = sidebar_with(
            cx,
            vec![
                session(
                    "one",
                    "First session",
                    "deepseek/deepseek-v4.1-flash",
                    Some(12),
                ),
                session(
                    "two",
                    "Second session",
                    "deepseek/deepseek-v4.1-flash",
                    None,
                ),
            ],
        );
        let sidebar = cx.debug_bounds("session-sidebar").expect("sidebar");
        let sidebar_right = f32::from(sidebar.origin.x) + f32::from(sidebar.size.width);
        for id in ["one", "two"] {
            let row = row_bounds(cx, id);
            let right = f32::from(row.origin.x) + f32::from(row.size.width);
            assert!(
                right <= sidebar_right + 0.5,
                "row {id} runs past the sidebar: {right} > {sidebar_right}"
            );
        }
    }
}
