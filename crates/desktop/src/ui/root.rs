use crate::state::AppState;
use crate::ui::chat::ChatView;
use crate::ui::sidebar::SidebarView;
use crate::ui::theme::Theme;
use gpui::{
    div, prelude::*, px, Context, Entity, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, Window,
};

/// How long the window geometry has to hold still before it is written out.
const WINDOW_SAVE_DELAY_MS: u64 = 400;

/// Root shell: toolbar + sidebar + chat split.
pub struct RootView {
    state: Entity<AppState>,
    sidebar: Entity<SidebarView>,
    chat: Entity<ChatView>,
    /// The session list can be collapsed to give the transcript the full width.
    sidebar_open: bool,
    /// Pending write of the window geometry. Resizing fires an event per frame,
    /// so the file is only touched once the geometry stops changing.
    window_save_task: Option<gpui::Task<()>>,
}

impl RootView {
    pub fn new(state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sidebar = cx.new(|cx| SidebarView::new(state.clone(), cx));
        let chat = cx.new(|cx| ChatView::new(state.clone(), cx));
        // Picks up where the last run left off rather than always opening the
        // list.
        let sidebar_open = state.read(cx).settings.sidebar_open;
        cx.observe(&state, |_, _, cx| cx.notify()).detach();
        cx.observe_window_appearance(window, |_, window, cx| {
            window.refresh();
            cx.notify();
        })
        .detach();
        cx.observe_window_bounds(window, |this, window, cx| {
            this.remember_window(window, cx);
        })
        .detach();
        let mut view = Self {
            state,
            sidebar,
            chat,
            sidebar_open,
            window_save_task: None,
        };
        // The bounds observer only fires when the geometry *changes*, so record
        // where the window opened too. Otherwise a first run that is never
        // resized or moved would never write anything down.
        view.remember_window(window, cx);
        view
    }

    /// Remember where and how big the window is, so the next launch opens the
    /// same way. Fires for every frame of a resize or move, so the write is
    /// deferred until the geometry settles.
    fn remember_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (bounds, maximized) = match window.window_bounds() {
            gpui::WindowBounds::Windowed(bounds) => (bounds, false),
            gpui::WindowBounds::Maximized(bounds) => (bounds, true),
            // Opening straight into a full-screen space is disorienting, so only
            // the size to restore to is kept.
            gpui::WindowBounds::Fullscreen(bounds) => (bounds, false),
        };
        let saved = van_goal_core::settings::SavedWindow {
            x: f32::from(bounds.origin.x),
            y: f32::from(bounds.origin.y),
            width: f32::from(bounds.size.width),
            height: f32::from(bounds.size.height),
            maximized,
        };
        self.window_save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(WINDOW_SAVE_DELAY_MS))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.window_save_task = None;
                let state = this.state.clone();
                state.update(cx, |state, _cx| {
                    if state.settings.window != Some(saved) {
                        state.settings.window = Some(saved);
                        state.settings.save();
                    }
                });
            });
        }));
    }

    /// Show or hide the session sidebar (toolbar button / ⌘B).
    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        let open = self.sidebar_open;
        let state = self.state.clone();
        state.update(cx, |state, _cx| {
            state.settings.sidebar_open = open;
            state.settings.save();
        });
        cx.notify();
    }
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        Theme::sync(self.state.read(cx).settings.appearance, window.appearance());
        Theme::sync_font_size(self.state.read(cx).settings.font_size);
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
                            .text_size(Theme::text_px(15.0))
                            .font_weight(FontWeight::BOLD)
                            .text_color(Theme::accent())
                            .child("Van-Goal"),
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
                            .text_size(Theme::text_px(11.0))
                            .text_color(Theme::text_secondary())
                            .child(connection_label),
                    )
                    .child(div().flex_1())
                    .children(last_error.map(|error| {
                        div()
                            .text_size(Theme::text_px(11.0))
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
        .text_size(Theme::text_px(11.0))
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
        .debug_selector(|| "sidebar-toggle".into())
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .w(px(28.0))
        .h(px(24.0))
        .rounded_md()
        .cursor_pointer()
        .hover(|style| style.bg(Theme::surface_hover()))
        .on_click(on_click)
        .child(sidebar_icon(open))
}

/// A window with its side panel: the panel is filled while the list is showing
/// and empty once it is hidden, so the icon reads as a state rather than a
/// direction. Drawn from primitives because a glyph would not sit on the text
/// baseline predictably.
fn sidebar_icon(open: bool) -> gpui::Div {
    let ink = Theme::text_secondary();
    div()
        .debug_selector(move || format!("sidebar-icon-{}", if open { "open" } else { "hidden" }))
        .w(px(16.0))
        .h(px(14.0))
        .rounded_sm()
        .border_1()
        .border_color(ink)
        .overflow_hidden()
        .flex()
        .flex_row()
        .child(
            div()
                .debug_selector(move || {
                    format!(
                        "sidebar-icon-panel-{}",
                        if open { "open" } else { "hidden" }
                    )
                })
                .w(px(4.0))
                .h_full()
                .when(open, |this| this.bg(ink)),
        )
        .child(div().w(px(1.0)).h_full().bg(ink))
}

fn pill_color(state: &van_goal_core::models::ConnectionState) -> gpui::Hsla {
    match state {
        van_goal_core::models::ConnectionState::Connected => Theme::ok(),
        van_goal_core::models::ConnectionState::Connecting => Theme::warn(),
        van_goal_core::models::ConnectionState::Disconnected => Theme::text_tertiary(),
        van_goal_core::models::ConnectionState::Degraded(_) => Theme::warn(),
        van_goal_core::models::ConnectionState::Failed(_) => Theme::danger(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};

    /// The test platform's window has no intrinsic size, and the shell is
    /// `size_full`.
    struct SizedRoot(Entity<RootView>);

    impl Render for SizedRoot {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(1000.0))
                .h(px(700.0))
                .flex()
                .flex_col()
                .child(self.0.clone())
        }
    }

    fn root(cx: &mut TestAppContext, open: bool) -> &mut gpui::VisualTestContext {
        let state = cx.new(AppState::new);
        let (_host, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| RootView::new(state.clone(), window, cx));
            view.update(cx, |view, _cx| view.sidebar_open = open);
            SizedRoot(view)
        });
        cx.run_until_parked();
        cx
    }

    /// The toggle was a solid triangle glyph. It is now a drawn window with its
    /// side panel, so it needs a real box to draw into.
    #[gpui::test]
    fn the_toolbar_draws_a_sidebar_icon_inside_the_toggle(cx: &mut TestAppContext) {
        let cx = root(cx, true);

        let toggle = cx
            .debug_bounds("sidebar-toggle")
            .expect("the sidebar toggle was not laid out");
        assert!(
            f32::from(toggle.size.width) >= 20.0 && f32::from(toggle.size.height) >= 20.0,
            "the toggle is too small to hold an icon: {}x{}",
            f32::from(toggle.size.width),
            f32::from(toggle.size.height)
        );

        let icon = cx
            .debug_bounds("sidebar-icon-open")
            .expect("the sidebar icon was not laid out");
        assert!(
            f32::from(icon.size.width) > 0.0 && f32::from(icon.size.height) > 0.0,
            "the sidebar icon collapsed"
        );
        assert!(
            f32::from(icon.origin.x) >= f32::from(toggle.origin.x) - 0.5
                && f32::from(icon.origin.x) + f32::from(icon.size.width)
                    <= f32::from(toggle.origin.x) + f32::from(toggle.size.width) + 0.5
                && f32::from(icon.origin.y) + f32::from(icon.size.height)
                    <= f32::from(toggle.origin.y) + f32::from(toggle.size.height) + 0.5,
            "the sidebar icon escapes its button"
        );
    }

    /// The panel is what distinguishes shown from hidden, so it has to exist in
    /// both states and fill the icon's height.
    #[gpui::test]
    fn the_icon_panel_is_drawn_in_both_states(cx: &mut TestAppContext) {
        for (open, selector) in [
            (true, "sidebar-icon-panel-open"),
            (false, "sidebar-icon-panel-hidden"),
        ] {
            let cx = root(cx, open);
            let panel = cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("no panel for open={open}"));
            let icon = cx
                .debug_bounds(if open {
                    "sidebar-icon-open"
                } else {
                    "sidebar-icon-hidden"
                })
                .expect("icon");
            assert!(
                f32::from(panel.size.width) > 0.0 && f32::from(panel.size.height) > 0.0,
                "the panel has no size for open={open}"
            );
            // The panel fills the icon's interior, which sits inside a 1px
            // border, so it spans the icon without exceeding it.
            assert!(
                f32::from(panel.size.height) <= f32::from(icon.size.height)
                    && f32::from(panel.size.height) >= f32::from(icon.size.height) - 3.0,
                "the panel does not span the icon for open={open}: panel={} icon={}",
                f32::from(panel.size.height),
                f32::from(icon.size.height)
            );
        }
    }

    fn sized_root(
        cx: &mut TestAppContext,
        state: Entity<AppState>,
    ) -> (Entity<RootView>, &mut gpui::VisualTestContext) {
        let (host, cx) = cx.add_window_view(|window, cx| {
            let view = cx.new(|cx| RootView::new(state, window, cx));
            SizedRoot(view)
        });
        let view = host.read_with(cx, |host, _cx| host.0.clone());
        cx.run_until_parked();
        (view, cx)
    }

    /// The geometry observer fires for every frame of a resize, so the write is
    /// deferred until the window settles.
    #[gpui::test]
    fn the_window_geometry_is_remembered_once_it_settles(cx: &mut TestAppContext) {
        let state = cx.new(AppState::new);
        let (_view, cx) = sized_root(cx, state.clone());

        assert_eq!(
            state.update(cx, |state, _cx| state.settings.window),
            None,
            "the geometry was written before the window settled"
        );

        cx.executor()
            .advance_clock(std::time::Duration::from_millis(WINDOW_SAVE_DELAY_MS + 50));
        cx.run_until_parked();

        let saved = state
            .update(cx, |state, _cx| state.settings.window)
            .expect("the window geometry was never remembered");
        assert!(
            saved.width > 0.0 && saved.height > 0.0 && saved.x.is_finite() && saved.y.is_finite(),
            "nonsense geometry was saved: {saved:?}"
        );
    }

    #[gpui::test]
    fn collapsing_the_sidebar_is_remembered(cx: &mut TestAppContext) {
        let state = cx.new(AppState::new);
        let (view, cx) = sized_root(cx, state.clone());

        assert!(
            view.read_with(cx, |view, _cx| view.sidebar_open),
            "the sidebar should start open"
        );

        cx.update(|_window, cx| {
            view.update(cx, |view, cx| view.toggle_sidebar(cx));
        });

        assert!(
            !view.read_with(cx, |view, _cx| view.sidebar_open),
            "the sidebar did not collapse"
        );
        assert!(
            !state.update(cx, |state, _cx| state.settings.sidebar_open),
            "the collapsed sidebar was not written down"
        );
    }

    /// A reopened window has to come back the way it was left.
    #[gpui::test]
    fn a_remembered_collapsed_sidebar_reopens_collapsed(cx: &mut TestAppContext) {
        let state = cx.new(AppState::new);
        state.update(cx, |state, _cx| {
            state.settings.sidebar_open = false;
        });

        let (view, _cx) = sized_root(cx, state);
        assert!(
            !view.read_with(_cx, |view, _cx| view.sidebar_open),
            "the window ignored the remembered sidebar state"
        );
    }
}
