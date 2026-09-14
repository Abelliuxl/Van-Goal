use crate::state::AppState;
use crate::ui::chat::ChatView;
use crate::ui::sidebar::SidebarView;
use crate::ui::theme::Theme;
use gpui::{
    div, prelude::*, px, Context, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, Window,
};

/// Root shell: toolbar + sidebar + chat split.
pub struct RootView {
    state: Entity<AppState>,
    sidebar: Entity<SidebarView>,
    chat: Entity<ChatView>,
    /// The session list can be collapsed to give the transcript the full width.
    sidebar_open: bool,
}

impl RootView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sidebar = cx.new(|cx| SidebarView::new(state.clone(), cx));
        let chat = cx.new(|cx| ChatView::new(state.clone(), cx));
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        cx.observe_window_appearance(window, |_, window, cx| {
            window.refresh();
            cx.notify();
        })
        .detach();
        Self {
            state,
            sidebar,
            chat,
            sidebar_open: true,
        }
    }

    /// Show or hide the session sidebar (toolbar button / ⌘B).
    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        cx.notify();
    }
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Theme::sync(self.state.read(cx).settings.appearance, window.appearance());
        let state = self.state.clone();
        let state_new = self.state.clone();
        let state_settings = self.state.clone();
        let sidebar_open = self.sidebar_open;
        let root = cx.entity();
        let (connection_label, pill_color, last_error) = {
            let state = state.read(cx);
            (
                state.connection_state.label(),
                pill_color(&state.connection_state),
                state.last_error.clone(),
            )
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(Theme::window_bg())
            .text_color(Theme::text())
            // Toolbar
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .pl(px(84.0))
                    .pr_4()
                    .py_2()
                    .border_b_1()
                    .border_color(Theme::border())
                    .bg(Theme::sidebar_bg())
                    .child(sidebar_toggle(sidebar_open, move |_event, _window, cx| {
                        root.update(cx, |root, cx| root.toggle_sidebar(cx));
                    }))
                    .child(
                        div()
                            .text_size(px(15.0))
                            .font_weight(FontWeight::BOLD)
                            .text_color(Theme::accent())
                            .child("Van-Goal"),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(Theme::text_tertiary())
                            .child("GPUI"),
                    )
                    .child(div().flex_1())
                    .child(toolbar_button("refresh", "⟳ Refresh", {
                        let state = state_settings;
                        move |_event, _window, cx| {
                            state.update(cx, |state, cx| state.refresh_sessions(true, cx));
                        }
                    }))
                    .child(toolbar_button("new-session", "＋ New Session", {
                        let state = state_new;
                        move |_event, _window, cx| {
                            state.update(cx, |state, cx| state.start_fresh_chat(cx));
                        }
                    }))
                    .child(toolbar_button("settings", "Settings", {
                        move |_event, window, cx| {
                            window.dispatch_action(
                                Box::new(crate::ui::settings_window::OpenSettings),
                                cx,
                            );
                        }
                    })),
            )
            // Body
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .overflow_hidden()
                    .when(sidebar_open, |this| this.child(self.sidebar.clone()))
                    .child(
                        div()
                            .id("content-column")
                            .debug_selector(|| "content-column".into())
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .child(self.chat.clone()),
                    ),
            )
            // Status bar
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_1()
                    .border_t_1()
                    .border_color(Theme::border())
                    .bg(Theme::sidebar_bg())
                    .child(div().size(px(7.0)).rounded_full().bg(pill_color))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(Theme::text_secondary())
                            .child(connection_label),
                    )
                    .child(div().flex_1())
                    .children(last_error.map(|error| {
                        div()
                            .text_size(px(11.0))
                            .text_color(Theme::warn())
                            .max_w(px(720.0))
                            .text_ellipsis()
                            .child(error)
                    })),
            )
    }
}

fn toolbar_button(
    id: &'static str,
    label: &'static str,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(gpui::ElementId::Name(id.into()))
        .px_2()
        .py_1()
        .rounded_md()
        .text_size(px(11.0))
        .text_color(Theme::text_secondary())
        .cursor_pointer()
        .hover(|style| style.bg(Theme::surface_hover()).text_color(Theme::text()))
        .on_click(on_click)
        .child(label)
}

/// Collapse / expand control for the sidebar. Lives in the toolbar so it stays
/// reachable while the sidebar is hidden.
fn sidebar_toggle(
    open: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id("sidebar-toggle")
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .w(px(26.0))
        .h(px(24.0))
        .rounded_md()
        .bg(Theme::surface())
        .border_1()
        .border_color(Theme::border())
        .text_size(px(12.0))
        .text_color(Theme::text_secondary())
        .cursor_pointer()
        .hover(|style| style.bg(Theme::surface_hover()).text_color(Theme::text()))
        .on_click(on_click)
        .child(if open { "◀" } else { "▶" })
}

fn pill_color(state: &crate::models::ConnectionState) -> gpui::Hsla {
    match state {
        crate::models::ConnectionState::Connected => Theme::ok(),
        crate::models::ConnectionState::Connecting => Theme::warn(),
        crate::models::ConnectionState::Disconnected => Theme::text_tertiary(),
        crate::models::ConnectionState::Degraded(_) => Theme::warn(),
        crate::models::ConnectionState::Failed(_) => Theme::danger(),
    }
}
