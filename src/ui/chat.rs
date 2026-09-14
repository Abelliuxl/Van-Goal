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
    InteractiveElement, IntoElement, ListAlignment, ListOffset, ListState, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement, Pixels, Point, Render, Styled, Window,
};
use std::time::Duration;

/// Smallest overlay-scrollbar thumb, as a fraction of the track.
const MIN_THUMB_FRACTION: f32 = 0.06;
/// How close the newest message has to be to the bottom edge to count as
/// "pinned to the latest message".
const BOTTOM_SLACK: f32 = 24.0;
const SCROLLBAR_TRACK_WIDTH: f32 = 11.0;
const SCROLLBAR_THUMB_WIDTH: f32 = 6.0;
/// Horizontal gutter between a message and the edge of the transcript.
const MESSAGE_GUTTER: f32 = 24.0;
/// Upper bound on how many messages the visibility probe walks per frame.
const MAX_VISIBLE_PROBE: usize = 64;

/// What the transcript currently shows, expressed in *messages* rather than
/// pixels: the list only measures the items it renders, so pixel totals are
/// unreliable until the user has scrolled through everything.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TranscriptWindow {
    /// Index of the first message inside the viewport.
    first_visible: usize,
    /// How many messages fit in the viewport right now.
    visible_count: usize,
    total: usize,
    /// The newest message is on screen, so new content can be followed.
    pinned_to_latest: bool,
}

impl TranscriptWindow {
    /// Thumb geometry as fractions of the scrollbar track: (top, height).
    fn thumb(&self) -> (f32, f32) {
        let total = self.total.max(1) as f32;
        let visible = self.visible_count.max(1) as f32;
        let height = (visible / total).clamp(MIN_THUMB_FRACTION.min(1.0), 1.0);
        let travel = 1.0 - height;
        let scrollable_messages = self.total.saturating_sub(self.visible_count.max(1));
        let progress = if scrollable_messages == 0 {
            0.0
        } else {
            self.first_visible.min(scrollable_messages) as f32 / scrollable_messages as f32
        };
        (progress * travel, height)
    }

    /// Inverse of [`Self::thumb`]: the message that should end up at the top of
    /// the viewport when the thumb is dragged to `thumb_top`.
    fn message_for_thumb_top(&self, thumb_top: f32) -> usize {
        let (_, height) = self.thumb();
        let travel = 1.0 - height;
        if travel <= 0.0 {
            return 0;
        }
        let progress = (thumb_top / travel).clamp(0.0, 1.0);
        let scrollable_messages = self.total.saturating_sub(self.visible_count.max(1));
        (progress * scrollable_messages as f32).round() as usize
    }

    /// There is more transcript than the viewport can show.
    fn is_scrollable(&self) -> bool {
        self.total > self.visible_count.max(1)
    }
}

pub struct ChatView {
    state: Entity<AppState>,
    editor: Entity<Editor>,
    list_state: ListState,
    last_list_count: usize,
    last_scroll_signature: u64,
    /// Session id of the list currently in `list_state`; when it changes the
    /// list has to be rebuilt from scratch rather than appended to.
    last_list_identity: Option<String>,
    needs_initial_focus: bool,
    expanded_messages: std::collections::HashSet<String>,
    model_menu_open: bool,
    permission_menu_open: bool,
    /// Overlay scrollbar fade (0 = hidden, 1 = fully visible).
    scrollbar_alpha: f32,
    scrollbar_target: f32,
    fade_generation: u64,
    list_hovered: bool,
    /// How far inside the thumb the user grabbed it, in pixels.
    scrollbar_grab: Option<f32>,
    /// Bumped on every programmatic scroll so stale next-frame callbacks bail.
    scroll_epoch: u64,
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
            last_list_identity: None,
            needs_initial_focus: true,
            expanded_messages: std::collections::HashSet::new(),
            model_menu_open: false,
            permission_menu_open: false,
            scrollbar_alpha: 0.0,
            scrollbar_target: 0.0,
            fade_generation: 0,
            list_hovered: false,
            scrollbar_grab: None,
            scroll_epoch: 0,
        }
    }

    /// Identity of the message list currently rendered: switching sessions has
    /// to rebuild the list instead of appending to it.
    fn list_identity(&self, cx: &Context<Self>) -> Option<String> {
        self.state
            .read(cx)
            .selected_session
            .as_ref()
            .map(|session| session.id.clone())
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

    /// What the transcript currently shows. Derived from the rendered items,
    /// which is the only trustworthy source while the list measures lazily.
    fn transcript_window(&self) -> TranscriptWindow {
        let total = self.list_state.item_count();
        let first_visible = self.list_state.logical_scroll_top().item_ix.min(total);
        let viewport = self.list_state.viewport_bounds();
        let viewport_bottom = f32::from(viewport.origin.y) + f32::from(viewport.size.height);
        let mut visible_count = 0usize;
        for index in first_visible..total {
            let Some(bounds) = self.list_state.bounds_for_item(index) else {
                break;
            };
            if f32::from(bounds.origin.y) >= viewport_bottom {
                break;
            }
            visible_count += 1;
            if visible_count >= MAX_VISIBLE_PROBE {
                break;
            }
        }
        TranscriptWindow {
            first_visible,
            visible_count: visible_count.max(1),
            total,
            pinned_to_latest: self.pinned_to_latest(total),
        }
    }

    /// True when the newest message sits inside the viewport: new content may be
    /// followed. A transcript the user scrolled away from reports false.
    fn pinned_to_latest(&self, total: usize) -> bool {
        if total == 0 {
            return true;
        }
        let Some(bounds) = self.list_state.bounds_for_item(total - 1) else {
            return false;
        };
        let viewport = self.list_state.viewport_bounds();
        let viewport_bottom = f32::from(viewport.origin.y) + f32::from(viewport.size.height);
        f32::from(bounds.origin.y) + f32::from(bounds.size.height) <= viewport_bottom + BOTTOM_SLACK
    }

    fn scroll_to_message(&self, index: usize) {
        let total = self.list_state.item_count();
        if total == 0 {
            return;
        }
        self.list_state.scroll_to(ListOffset {
            item_ix: index.min(total - 1),
            offset_in_item: px(0.0),
        });
    }

    /// Height of the scrollbar track: the transcript viewport.
    fn scrollbar_track_height(&self) -> f32 {
        f32::from(self.list_state.viewport_bounds().size.height).max(1.0)
    }

    /// Pointer position over the track, as a 0..1 fraction.
    fn pointer_fraction(&self, position: Point<Pixels>) -> f32 {
        let track = self.list_state.viewport_bounds();
        let height = f32::from(track.size.height).max(1.0);
        ((f32::from(position.y) - f32::from(track.origin.y)) / height).clamp(0.0, 1.0)
    }

    /// Move the view to the newest message. This is index based on purpose:
    /// item heights are only known once an item has been rendered, so a
    /// height-based reveal can stop short of messages that were never measured.
    /// The list renders forward from the index and fills the viewport upwards,
    /// which lands on the newest screenful every time.
    fn scroll_to_latest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.state.read(cx).messages.len();
        if count == 0 {
            return;
        }
        self.scroll_epoch = self.scroll_epoch.wrapping_add(1);
        let epoch = self.scroll_epoch;
        let visible = self.transcript_window().visible_count.max(1);
        self.scroll_to_message(count.saturating_sub(visible));
        // Polish on the next frame, once the newest items have been measured:
        // revealing the last one puts it flush with the bottom of the viewport.
        let list_state = self.list_state.clone();
        let chat = cx.entity();
        window.on_next_frame(move |_window, cx| {
            chat.update(cx, |chat, cx| {
                if chat.scroll_epoch != epoch {
                    return;
                }
                let count = chat.state.read(cx).messages.len();
                if count == 0 {
                    return;
                }
                list_state.scroll_to_reveal_item(count - 1);
                cx.notify();
            });
        });
    }

    fn set_list_hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if self.list_hovered == hovered {
            return;
        }
        self.list_hovered = hovered;
        self.refresh_scrollbar_target(cx);
    }

    /// The scrollbar is shown on hover (and while dragging); everything else
    /// fades it back out.
    fn refresh_scrollbar_target(&mut self, cx: &mut Context<Self>) {
        let visible = (self.list_hovered || self.scrollbar_grab.is_some())
            && self.transcript_window().is_scrollable();
        let target = if visible { 1.0 } else { 0.0 };
        if (self.scrollbar_target - target).abs() < f32::EPSILON {
            return;
        }
        self.scrollbar_target = target;
        self.start_scrollbar_fade(cx);
    }

    fn start_scrollbar_fade(&mut self, cx: &mut Context<Self>) {
        self.fade_generation = self.fade_generation.wrapping_add(1);
        let generation = self.fade_generation;
        let target = self.scrollbar_target;
        if (self.scrollbar_alpha - target).abs() < 0.02 {
            self.scrollbar_alpha = target;
            cx.notify();
            return;
        }
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(16))
                .await;
            let finished = this
                .update(cx, |this, cx| {
                    if this.fade_generation != generation {
                        return true;
                    }
                    let delta = target - this.scrollbar_alpha;
                    if delta.abs() < 0.02 {
                        this.scrollbar_alpha = target;
                        cx.notify();
                        return true;
                    }
                    // Exponential ease-out: quick to appear, gentle to leave.
                    this.scrollbar_alpha += delta * 0.22;
                    cx.notify();
                    false
                })
                .unwrap_or(true);
            if finished {
                break;
            }
        })
        .detach();
    }

    /// Mouse down on the track: grabs the thumb, or jumps the view to the
    /// message under the cursor first.
    fn begin_scrollbar_drag(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        let window = self.transcript_window();
        if !window.is_scrollable() {
            return;
        }
        let pointer = self.pointer_fraction(event.position);
        let (thumb_top, thumb_height) = window.thumb();
        if pointer < thumb_top || pointer > thumb_top + thumb_height {
            self.scroll_to_message(window.message_for_thumb_top(pointer - thumb_height / 2.0));
            let (thumb_top, _) = self.transcript_window().thumb();
            self.scrollbar_grab = Some((pointer - thumb_top).clamp(0.0, thumb_height));
        } else {
            self.scrollbar_grab = Some((pointer - thumb_top).clamp(0.0, thumb_height));
        }
        self.list_state.scrollbar_drag_started();
        self.refresh_scrollbar_target(cx);
        cx.notify();
    }

    fn drag_scrollbar(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(grab) = self.scrollbar_grab else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let window = self.transcript_window();
        if !window.is_scrollable() {
            return;
        }
        let pointer = self.pointer_fraction(event.position);
        self.scroll_to_message(window.message_for_thumb_top(pointer - grab));
        cx.notify();
    }

    fn end_scrollbar_drag(&mut self, cx: &mut Context<Self>) {
        if self.scrollbar_grab.is_none() {
            return;
        }
        self.scrollbar_grab = None;
        self.list_state.scrollbar_drag_ended();
        self.refresh_scrollbar_target(cx);
        cx.notify();
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
        let identity = self.list_identity(cx);
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
        let mut scroll_to_latest = false;
        if identity != self.last_list_identity {
            // Different session: rebuild the list and land on its newest message.
            self.last_list_identity = identity;
            self.last_list_count = message_count;
            self.list_state.reset(message_count);
            self.last_scroll_signature = signature;
            scroll_to_latest = message_count > 0;
        } else if message_count != self.last_list_count {
            let old_count = self.last_list_count;
            self.last_list_count = message_count;
            if message_count > old_count {
                // Append the new messages instead of re-splicing the whole
                // range: re-splicing threw every measured height away, which is
                // why a send while scrolled up left the view where it was.
                self.list_state
                    .splice(old_count..old_count, message_count - old_count);
                // Sending a prompt always snaps the view to the new message.
                scroll_to_latest = self
                    .state
                    .read(cx)
                    .messages
                    .get(old_count..)
                    .is_some_and(|added| added.iter().any(|m| m.role == MessageRole::User));
            } else {
                self.list_state.splice(0..old_count, message_count);
            }
        } else if signature != self.last_scroll_signature && is_streaming && message_count > 0 {
            self.list_state.splice(message_count - 1..message_count, 1);
            // Follow the stream only while the newest message is on screen.
            scroll_to_latest = self.pinned_to_latest(message_count);
        }
        self.last_scroll_signature = signature;
        if scroll_to_latest {
            self.scroll_to_latest(window, cx);
        }

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
                                    "Van-Goal is ready to use {backend_name}. Send a message or resume a session from the sidebar."
                                )),
                        ),
                )
                .into_any();
        }

        let list_state = self.list_state.clone();
        let chat = cx.entity();
        let state_entity = self.state.clone();
        let messages = state_entity.read(cx).messages.clone();

        let chat_for_list = chat.clone();
        let list = list(list_state, move |index, _window, cx| {
            let Some(message) = messages.get(index) else {
                return div().into_any();
            };
            let expanded = chat_for_list
                .read(cx)
                .expanded_messages
                .contains(&message.id);
            render_message_bubble(
                message,
                expanded,
                index,
                chat_for_list.clone(),
                state_entity.clone(),
            )
        })
        .flex_1()
        .min_h_0();

        let transcript = self.transcript_window();
        let chat_track = chat.clone();
        let chat_move = chat.clone();
        let chat_up = chat.clone();
        let chat_jump = chat.clone();
        let dragging = self.scrollbar_grab.is_some();

        let mut container = div()
            .id("chat-message-scroll-area")
            .debug_selector(|| "chat-message-scroll-area".into())
            .relative()
            .w_full()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .on_hover(move |hovered, _window, cx| {
                chat.update(cx, |chat, cx| chat.set_list_hovered(*hovered, cx));
            })
            .child(list);

        // Overlay scrollbar: hidden until the pointer is over the transcript,
        // then faded in; it fades back out once the pointer leaves.
        if transcript.is_scrollable() {
            let track_height = self.scrollbar_track_height();
            let (thumb_top, thumb_height) = transcript.thumb();
            let thumb_color = if dragging {
                Theme::scrollbar_thumb_active()
            } else {
                Theme::scrollbar_thumb()
            };
            container = container.child(
                div()
                    .id("chat-scrollbar-track")
                    .debug_selector(|| "chat-scrollbar-track".into())
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(px(0.0))
                    .w(px(SCROLLBAR_TRACK_WIDTH))
                    .on_mouse_down(MouseButton::Left, move |event, _window, cx| {
                        chat_track.update(cx, |chat, cx| chat.begin_scrollbar_drag(event, cx));
                    })
                    .on_mouse_move(move |event, _window, cx| {
                        chat_move.update(cx, |chat, cx| chat.drag_scrollbar(event, cx));
                    })
                    .on_mouse_up(MouseButton::Left, move |_event, _window, cx| {
                        chat_up.update(cx, |chat, cx| chat.end_scrollbar_drag(cx));
                    })
                    .child(
                        div()
                            .id("chat-scrollbar-thumb")
                            .debug_selector(|| "chat-scrollbar-thumb".into())
                            .absolute()
                            .top(px(thumb_top * track_height))
                            .right(px((SCROLLBAR_TRACK_WIDTH - SCROLLBAR_THUMB_WIDTH) / 2.0))
                            .w(px(SCROLLBAR_THUMB_WIDTH))
                            .h(px((thumb_height * track_height).max(SCROLLBAR_TRACK_WIDTH)))
                            .rounded_full()
                            .bg(thumb_color)
                            .opacity(self.scrollbar_alpha),
                    ),
            );
        }

        // "Jump to the newest message" pill, shown while the view is scrolled up.
        if transcript.is_scrollable() && !transcript.pinned_to_latest {
            container = container.child(
                div()
                    .id("chat-jump-to-latest")
                    .debug_selector(|| "chat-jump-to-latest".into())
                    .absolute()
                    .bottom(px(18.0))
                    .right(px(26.0))
                    .w(px(34.0))
                    .h(px(34.0))
                    .rounded_full()
                    .bg(Theme::surface())
                    .border_1()
                    .border_color(Theme::border_strong())
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|style| style.bg(Theme::surface_hover()))
                    .on_click(move |_event, window, cx| {
                        chat_jump.update(cx, |chat, cx| chat.scroll_to_latest(window, cx));
                    })
                    .child(
                        div()
                            .text_size(px(14.0))
                            .text_color(Theme::text_secondary())
                            .child("↓"),
                    ),
            );
        }

        // Keeps the fade in sync when the list only became scrollable (or was
        // hovered) after the hover event itself.
        self.refresh_scrollbar_target(cx);
        container.into_any()
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
    index: usize,
    chat: Entity<ChatView>,
    state: Entity<AppState>,
) -> AnyElement {
    let id_hash = crate::ui::hash_id(&message.id);
    let bubble = match message.role {
        MessageRole::User => div()
            .w_full()
            .flex()
            .flex_row()
            .justify_end()
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
            let mut bubble = div()
                .w_full()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap_1()
                .py_1();

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
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(Theme::text_secondary())
                    .child(message.content.clone()),
            )
            .into_any(),
    };

    // Every message sits in the same centred, width-capped column, so long
    // paragraphs and tables have a bounded width to wrap inside.
    div()
        .id(gpui::ElementId::NamedInteger(
            "message-item".into(),
            id_hash,
        ))
        .debug_selector(move || format!("message-item-{index}"))
        .w_full()
        .flex()
        .flex_col()
        .items_center()
        .child(
            div()
                .id(gpui::ElementId::NamedInteger(
                    "message-column".into(),
                    id_hash.wrapping_add(1),
                ))
                .debug_selector(move || format!("message-column-{index}"))
                .w_full()
                .min_w(px(0.0))
                .px(px(MESSAGE_GUTTER))
                .child(bubble),
        )
        .into_any()
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

#[cfg(test)]
mod chat_view_render_tests {
    use super::*;
    use crate::models::ChatMessage;
    use gpui::{AppContext, TestAppContext, VisualTestContext};

    /// A box to lay the transcript out in: the test platform's window has no
    /// intrinsic size.
    struct SizedChat(Entity<ChatView>);

    impl Render for SizedChat {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(800.0))
                .h(px(600.0))
                .flex()
                .flex_col()
                .child(self.0.clone())
        }
    }

    fn transcript_of(count: usize) -> Vec<ChatMessage> {
        (0..count)
            .map(|index| {
                let role = if index % 2 == 0 {
                    MessageRole::User
                } else {
                    MessageRole::Assistant
                };
                ChatMessage::new(role, format!("message {index}"))
            })
            .collect()
    }

    /// A transcript that fills one screen must still be laid out as a real
    /// scroll area with an overlay scrollbar thumb inside it — the overlay is
    /// painted with a fade, so it exists in the frame at every opacity.
    #[gpui::test]
    fn long_transcript_lays_out_scrollbar_and_jump_button(cx: &mut TestAppContext) {
        let state = cx.new(|cx| AppState::new(cx));
        state.update(cx, |state, _cx| {
            // Deterministic: no cached session means the list starts at the top.
            state.selected_session = None;
            state.messages = transcript_of(40);
        });
        let (host, cx) =
            cx.add_window_view(|_window, cx| SizedChat(cx.new(|cx| ChatView::new(state, cx))));
        let view = host.read_with(cx, |host, _cx| host.0.clone());
        cx.run_until_parked();

        let area = cx
            .debug_bounds("chat-message-scroll-area")
            .expect("scroll area was not laid out");
        assert!(
            f32::from(area.size.height) > 0.0,
            "transcript collapsed to zero height"
        );
        let thumb = cx
            .debug_bounds("chat-scrollbar-thumb")
            .expect("scrollbar thumb was not laid out");
        assert!(f32::from(thumb.size.height) > 0.0);
        assert!(
            f32::from(thumb.size.height) <= f32::from(area.size.height),
            "thumb is taller than the transcript"
        );
        assert!(
            f32::from(thumb.origin.y) >= f32::from(area.origin.y) - 1.0,
            "thumb is outside the transcript"
        );

        // Park at the top of the transcript, as if the user scrolled back to
        // read: the "jump to the newest message" button has to be on screen, and
        // the view must know it is not at the newest message (that predicate is
        // also what decides whether the stream keeps being followed).
        cx.update(|_window, cx| {
            view.update(cx, |chat, cx| {
                chat.scroll_to_message(0);
                cx.notify();
            });
        });
        cx.run_until_parked();
        let window = cx.update(|_window, cx| view.update(cx, |chat, _cx| chat.transcript_window()));
        assert!(window.is_scrollable(), "long transcript is not scrollable");
        assert!(
            !window.pinned_to_latest,
            "top of the transcript claims to be at the newest message"
        );
        // `debug_bounds` keeps whatever was inserted by any frame, so this only
        // proves the button is rendered while reading old messages.
        assert!(
            cx.debug_bounds("chat-jump-to-latest").is_some(),
            "no jump button while reading old messages"
        );

        // Scrolling back to the newest message flips that predicate.
        let total =
            cx.update(|_window, cx| view.update(cx, |chat, _cx| chat.list_state.item_count()));
        cx.update(|_window, cx| {
            view.update(cx, |chat, cx| {
                chat.scroll_to_message(total.saturating_sub(1));
                cx.notify();
            });
        });
        cx.run_until_parked();
        let settled = settle_until_pinned(cx, &view);
        assert!(settled, "never reported being at the newest message");
    }

    /// Sending a prompt appends the user message plus the assistant
    /// placeholder; the view has to move to what was just sent instead of
    /// staying where the user had scrolled to.
    #[gpui::test]
    fn sending_moves_the_view_to_the_new_message(cx: &mut TestAppContext) {
        let state = cx.new(|cx| AppState::new(cx));
        state.update(cx, |state, _cx| {
            state.selected_session = None;
            state.messages = transcript_of(40);
        });
        let (host, cx) = cx.add_window_view(|_window, cx| {
            SizedChat(cx.new(|cx| ChatView::new(state.clone(), cx)))
        });
        let view = host.read_with(cx, |host, _cx| host.0.clone());
        cx.run_until_parked();

        // Park the view at the top, as if the user scrolled up to read.
        cx.update(|_window, cx| {
            view.update(cx, |chat, cx| {
                chat.scroll_to_message(0);
                cx.notify();
            });
        });
        cx.run_until_parked();
        let parked = cx.update(|_window, cx| view.update(cx, |chat, _cx| chat.transcript_window()));
        assert_eq!(parked.first_visible, 0, "did not park at the top");

        // A send appends the prompt and the streaming placeholder.
        state.update(cx, |state, cx| {
            state.messages.push(ChatMessage::new(
                MessageRole::User,
                "new prompt".to_string(),
            ));
            state
                .messages
                .push(ChatMessage::streaming(MessageRole::Assistant));
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, _cx| window.refresh());
        cx.run_until_parked();

        for round in 0..4 {
            cx.update(|window, _cx| window.refresh());
            cx.run_until_parked();
            let probe = cx.update(|_window, cx| {
                view.update(cx, |chat, _cx| {
                    (
                        chat.transcript_window(),
                        chat.scroll_epoch,
                        chat.list_state.logical_scroll_top().item_ix,
                    )
                })
            });
            println!("[round {round}] {probe:?}");
        }
        let after_send =
            cx.update(|_window, cx| view.update(cx, |chat, _cx| chat.transcript_window()));
        assert_eq!(after_send.total, 42, "the sent messages were not appended");
        assert!(
            after_send.first_visible > 20,
            "view stayed at the top after sending: {after_send:?}"
        );
    }

    /// The rendered state lags one frame behind the scroll position, and the
    /// test platform only draws while the executor has work, so poll a few
    /// frames before deciding.
    fn settle_until_pinned(cx: &mut VisualTestContext, view: &Entity<ChatView>) -> bool {
        for _ in 0..4 {
            cx.update(|window, _cx| window.refresh());
            cx.run_until_parked();
            let pinned = cx.update(|_window, cx| {
                view.update(cx, |chat, _cx| {
                    let total = chat.list_state.item_count();
                    chat.pinned_to_latest(total)
                })
            });
            if pinned {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod transcript_window_tests {
    use super::*;

    fn window(first_visible: usize, visible_count: usize, total: usize) -> TranscriptWindow {
        TranscriptWindow {
            first_visible,
            visible_count,
            total,
            pinned_to_latest: false,
        }
    }

    #[test]
    fn nothing_to_scroll_when_the_transcript_fits() {
        let fits = window(0, 5, 5);
        assert!(!fits.is_scrollable());
        let (top, height) = fits.thumb();
        assert_eq!(top, 0.0);
        assert_eq!(height, 1.0);
        assert_eq!(fits.message_for_thumb_top(0.5), 0);
    }

    #[test]
    fn thumb_sits_at_the_top_and_the_bottom_of_the_track() {
        let at_top = window(0, 5, 35);
        let (top, height) = at_top.thumb();
        assert_eq!(top, 0.0);
        assert!((height - 5.0 / 35.0).abs() < 0.001, "height {height}");

        let at_bottom = window(30, 5, 35);
        let (top, _) = at_bottom.thumb();
        assert!((top - (1.0 - 5.0 / 35.0)).abs() < 0.001, "top {top}");
    }

    #[test]
    fn dragging_the_thumb_round_trips_to_the_same_message() {
        let w = window(0, 6, 60);
        let (_, height) = w.thumb();
        let travel = 1.0 - height;
        for fraction in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let index = w.message_for_thumb_top(travel * fraction);
            let moved = TranscriptWindow {
                first_visible: index,
                ..w
            };
            let (top, _) = moved.thumb();
            assert!(
                (top - travel * fraction).abs() < 0.02,
                "fraction {fraction}: thumb landed at {top}"
            );
        }
    }

    #[test]
    fn thumb_keeps_a_minimum_size_on_very_long_transcripts() {
        let w = window(0, 1, 4000);
        let (_, height) = w.thumb();
        assert!(
            (height - MIN_THUMB_FRACTION).abs() < f32::EPSILON,
            "height {height}"
        );
    }

    #[test]
    fn a_single_message_never_divides_by_zero() {
        let w = window(0, 1, 1);
        assert!(!w.is_scrollable());
        let (top, height) = w.thumb();
        assert_eq!(top, 0.0);
        assert_eq!(height, 1.0);
        assert_eq!(w.message_for_thumb_top(0.9), 0);
    }
}

#[cfg(test)]
mod message_width_tests {
    use super::*;
    use crate::models::ChatMessage;
    use gpui::{AppContext, TestAppContext, VisualTestContext};

    struct SizedChat(Entity<ChatView>);

    impl Render for SizedChat {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(900.0))
                .h(px(600.0))
                .flex()
                .flex_col()
                .child(self.0.clone())
        }
    }

    const WIDE_TABLE: &str = "远程机上的实时记录：\n\n\
        | 项目目录 | 最近更新 | 状态说明 |\n|---|---|---|\n\
        | PHLDB1-Gene-Study | 2026-08-24 (Records.md) | 进行中 — 飞书表状态「BSMC 检测小干扰效果」；2026-08-24 时大鼠 BMSC 24 孔板×2 已铺板用于后续检测 |\n\
        | COL10-Gene-Study | 2026-09-13 | 分析 Zeta 电位数据，判断 2026-08-24 (Records.md) 记录是否合理，重要‼️ |\n";

    const LONG_PARAGRAPH: &str = "· 测试 ROS 检测手段 | 重要 ‼️ | 开始 2026 时间序列 D0/D3/D7/D14 + qRT-PCR 检测 PHLDB1/COLEC10/RUNX5-12-2 的引物设计进度，需要跟昆工团队确认审核材料清单。\n";

    fn transcript(messages: Vec<&str>) -> Vec<ChatMessage> {
        messages
            .into_iter()
            .map(|content| ChatMessage::new(MessageRole::Assistant, content.to_string()))
            .collect()
    }

    fn render<'a>(
        cx: &'a mut TestAppContext,
        messages: Vec<&str>,
    ) -> (Entity<ChatView>, &'a mut VisualTestContext) {
        let state = cx.new(|cx| AppState::new(cx));
        state.update(cx, |state, _cx| {
            state.selected_session = None;
            state.messages = transcript(messages);
        });
        let (host, cx) =
            cx.add_window_view(|_window, cx| SizedChat(cx.new(|cx| ChatView::new(state, cx))));
        let view = host.read_with(cx, |host, _cx| host.0.clone());
        cx.run_until_parked();
        (view, cx)
    }

    #[gpui::test]
    fn messages_never_overflow_the_transcript(cx: &mut TestAppContext) {
        let (_view, cx) = render(cx, vec![WIDE_TABLE, LONG_PARAGRAPH]);
        let area = cx
            .debug_bounds("chat-message-scroll-area")
            .expect("scroll area was not laid out");
        let area_width = f32::from(area.size.width);
        for index in 0..2 {
            let item = cx
                .debug_bounds(Box::leak(format!("message-item-{index}").into_boxed_str()))
                .unwrap_or_else(|| panic!("message {index} was not laid out"));
            let column = cx
                .debug_bounds(Box::leak(
                    format!("message-column-{index}").into_boxed_str(),
                ))
                .unwrap_or_else(|| panic!("message column {index} was not laid out"));
            println!(
                "message {index}: area={area_width} item={} column={}",
                f32::from(item.size.width),
                f32::from(column.size.width)
            );
            assert!(
                f32::from(item.size.width) <= area_width + 0.5,
                "message {index} item is wider than the transcript: {} > {area_width}",
                f32::from(item.size.width)
            );
        }
    }
}
