use crate::state::AppState;
use crate::ui::editor::{Editor, EditorEvent};
use crate::ui::markdown_view::{render_blocks, SelectionBlock};
use crate::ui::selectable_text::SelectionHost;
use crate::ui::theme::Theme;
use gpui::{
    actions, div, list, point, prelude::*, px, AnyElement, Context, Entity, FocusHandle, Focusable,
    FontWeight, InteractiveElement, IntoElement, ListAlignment, ListState, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement, Pixels, Point, Render, Styled, Window,
};
use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::time::Duration;
use van_goal_core::markdown;
use van_goal_core::markdown::MarkdownBlock;
use van_goal_core::models::{
    compact_token_count, duration_string, ContextUsage, MessageRole, PermissionMode,
};

actions!(chat, [CopySelection, QuoteSelection]);

/// Smallest overlay-scrollbar thumb, as a fraction of the track.
const MIN_THUMB_FRACTION: f32 = 0.06;
/// How close the newest message has to be to the bottom edge to count as
/// "pinned to the latest message".
const BOTTOM_SLACK: f32 = 24.0;
const SCROLLBAR_TRACK_WIDTH: f32 = 11.0;
const SCROLLBAR_THUMB_WIDTH: f32 = 6.0;
/// Horizontal gutter between a message and the edge of the transcript.
const MESSAGE_GUTTER: f32 = 24.0;
/// Widest a user bubble may grow before its text wraps.
const USER_BUBBLE_MAX_WIDTH: f32 = 640.0;
const USER_BUBBLE_PADDING_X: f32 = 14.0;
/// The width left for text inside a full-width bubble. Capping the text keeps
/// the bubble's intrinsic height equal to its final wrapped height.
const USER_BUBBLE_TEXT_MAX_WIDTH: f32 = USER_BUBBLE_MAX_WIDTH - USER_BUBBLE_PADDING_X * 2.0;
/// Far past the end of any transcript. Scrolling there makes the list clamp to
/// its own maximum, which is also what puts a bottom-aligned list back into
/// "follow the newest message" mode — see [`ChatView::scroll_to_latest`].
const SCROLL_TO_END: f32 = 1_000_000.0;

/// Where the transcript sits, measured in pixels rather than in messages.
///
/// This used to be derived from how many *messages* were on screen against how
/// many existed, probed out of the list's rendered items. Both halves of that
/// were unusable. A one-line prompt and a fifty-line code block each count as
/// one message, so the ratio said nothing about how much of the transcript was
/// visible; and the list measures items lazily, so the number of items it could
/// report changed from frame to frame as measurement caught up. That is why the
/// thumb grew and shrank while scrolling.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ScrollGeometry {
    /// Thumb top and height as fractions of the scrollbar track.
    thumb_top: f32,
    thumb_height: f32,
    /// How far the content can scroll, in pixels.
    travel: f32,
    /// There is more content than the viewport can show.
    is_scrollable: bool,
    /// The newest message is on screen, so new content may be followed.
    pinned_to_latest: bool,
}

impl ScrollGeometry {
    /// Geometry for a `viewport`-tall list showing `travel` pixels of scrollable
    /// content and scrolled `offset` pixels down. The content is
    /// `viewport + travel` tall, which is what makes the thumb proportional to
    /// the transcript rather than to how many messages happen to be measured.
    fn new(viewport: f32, travel: f32, offset: f32) -> Self {
        // The finiteness test comes first on purpose: a NaN compares false
        // against everything, so it would slip past both range checks below and
        // poison the thumb geometry.
        if !viewport.is_finite() || !travel.is_finite() || viewport <= 0.0 || travel <= 0.0 {
            return Self {
                thumb_top: 0.0,
                thumb_height: 1.0,
                travel: 0.0,
                is_scrollable: false,
                pinned_to_latest: true,
            };
        }
        let thumb_height = (viewport / (viewport + travel)).clamp(MIN_THUMB_FRACTION, 1.0);
        let offset = offset.clamp(0.0, travel);
        Self {
            thumb_top: (offset / travel) * (1.0 - thumb_height),
            thumb_height,
            travel,
            is_scrollable: true,
            pinned_to_latest: travel - offset <= BOTTOM_SLACK,
        }
    }

    /// The offset that puts the thumb's top at `thumb_top` (a fraction of the
    /// track). Inverse of the mapping in [`Self::new`], and what dragging uses.
    /// The clamp is measured against the *clamped* thumb height that was
    /// actually drawn, so dragging round-trips instead of drifting.
    fn offset_for_thumb_top(&self, thumb_top: f32) -> f32 {
        let room = 1.0 - self.thumb_height;
        if room <= 0.0 {
            return 0.0;
        }
        (thumb_top / room).clamp(0.0, 1.0) * self.travel
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
    /// How far inside the thumb the user grabbed it, as a fraction of the
    /// track. Recorded on mouse-down so the thumb does not jump under the
    /// pointer for the rest of the drag.
    scrollbar_grab: Option<f32>,
    /// Message the pointer is over, which is what reveals its copy button. It
    /// stays set briefly after the pointer leaves so the button can be reached.
    hovered_message: Option<String>,
    /// Pending hide of `hovered_message`. Dropping the task cancels it, which
    /// is how re-entering the message keeps the button up.
    copy_hide_task: Option<gpui::Task<()>>,
    /// Message copied most recently, so its button can confirm the copy.
    copied_message: Option<String>,
    /// Pending reset of `copied_message`.
    copied_reset_task: Option<gpui::Task<()>>,
    /// Parsed markdown per message id. Parsing is instant but rebuilding the
    /// div tree is not, and render reads every visible message every frame;
    /// a reply that has not changed since the last frame reuses its blocks.
    /// Cleared when another session is opened.
    markdown_memo: HashMap<String, (String, Rc<Vec<MarkdownBlock>>)>,
    /// Whether the list bookkeeping has run for the state ChatView was created
    /// with: the observer only fires on later changes, so the first render
    /// calls the sync once itself.
    list_synced: bool,
    /// Focus for the transcript area: clicking a message moves focus here so
    /// ⌘C copies the selection instead of the composer's content.
    transcript_focus_handle: FocusHandle,
    /// The one active text selection across every block the transcript draws.
    text_selection: Option<TextSelection>,
    /// The right-click menu for the current selection, positioned relative to
    /// the view.
    context_menu: Option<ContextMenu>,
    /// Origin of this view in window coordinates, recorded at paint so a
    /// window-coordinate event can be turned into a view-relative one.
    view_origin: Option<gpui::Point<Pixels>>,
}

/// The small popup a right-click on a selection opens.
#[derive(Clone)]
struct ContextMenu {
    position: gpui::Point<Pixels>,
    text: String,
}

/// What a selected range in one markdown block holds while it is live.
#[derive(Clone)]
struct TextSelection {
    /// Which block the selection belongs to: a SelectableText only paints a
    /// range whose key matches its own.
    key: u64,
    range: Range<usize>,
    text: String,
}

/// Bind the transcript's own keys. Called beside `bind_editor_keys`, since a
/// key binding lives in the context that is focused when it should fire.
pub fn bind_chat_keys(cx: &mut gpui::App) {
    cx.bind_keys([gpui::KeyBinding::new(
        "cmd-c",
        CopySelection,
        Some("Transcript"),
    )]);
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
            // The list bookkeeping lives here rather than in render: keeping
            // `ListState` in step with the message vec is a reaction to the
            // state having changed, not something a frame should decide.
            this.sync_list_state(cx);
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
                let busy = this.state.read(cx).is_sending();
                if busy {
                    cx.notify();
                }
            });
        })
        .detach();

        Self {
            state,
            editor,
            // A chat log reads bottom-up: `ListAlignment::Bottom` keeps the
            // newest message in view as the reply streams in, and leaves the
            // view where it is once the user scrolls back to read. `measure_all`
            // costs one full pass over the session when it is opened, and buys a
            // scrollbar whose thumb is sized from the real content height rather
            // than from the handful of items the list happens to have measured.
            list_state: ListState::new(0, ListAlignment::Bottom, px(500.0)).measure_all(),
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
            hovered_message: None,
            copy_hide_task: None,
            copied_message: None,
            copied_reset_task: None,
            markdown_memo: HashMap::new(),
            list_synced: false,
            transcript_focus_handle: cx.focus_handle(),
            text_selection: None,
            context_menu: None,
            view_origin: None,
        }
    }

    /// How long the copy button stays after the pointer leaves a message, so it
    /// can actually be reached.
    const COPY_HIDE_DELAY: Duration = Duration::from_millis(700);
    /// How long the checkmark stays before the button returns to the copy icon.
    const COPY_CONFIRMATION: Duration = Duration::from_millis(1400);

    /// The markdown of one message, parsed at most once per change.
    ///
    /// Parsing is instant but rebuilding the div tree is not, and render reads
    /// every visible message every frame; a reply that has not changed since
    /// the last frame reuses its blocks.
    fn markdown_blocks(
        &mut self,
        message: &van_goal_core::models::ChatMessage,
    ) -> Rc<Vec<MarkdownBlock>> {
        if let Some((cached_content, blocks)) = self.markdown_memo.get(&message.id) {
            if *cached_content == message.content {
                return blocks.clone();
            }
        }
        let blocks = Rc::new(markdown::parse(&message.content));
        if self.markdown_memo.len() >= 512 {
            self.markdown_memo.clear();
        }
        self.markdown_memo.insert(
            message.id.clone(),
            (message.content.clone(), blocks.clone()),
        );
        blocks
    }

    /// Keep the variable-height list measurements in sync with the message
    /// vec. Runs when the app state changes, not inside render: render reads
    /// and the observer reacts.
    ///
    /// Following the streaming bubble is not done here: a bottom-aligned list
    /// keeps the newest message in view by itself, and only stops doing so once
    /// the user scrolls away.
    fn sync_list_state(&mut self, cx: &mut Context<Self>) {
        let signature = self.scroll_signature(cx);
        let identity = self.list_identity(cx);
        let (message_count, is_streaming) = {
            let state = self.state.read(cx);
            (
                state.messages().len(),
                state
                    .messages()
                    .last()
                    .map(|m| m.is_streaming)
                    .unwrap_or(false),
            )
        };

        let mut scroll_to_latest = false;
        if identity != self.last_list_identity {
            // Different session: rebuild the list. Bottom alignment already
            // opens on its newest message, so this needs no follow-up scroll.
            self.last_list_identity = identity;
            self.last_list_count = message_count;
            self.list_state.reset(message_count);
            self.last_scroll_signature = signature;
            // Every bubble in the old session is out of scope.
            self.markdown_memo.clear();
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
                    .messages()
                    .get(old_count..)
                    .is_some_and(|added| added.iter().any(|m| m.role == MessageRole::User));
            } else {
                self.list_state.splice(0..old_count, message_count);
            }
        } else if signature != self.last_scroll_signature && is_streaming && message_count > 0 {
            // The bubble is still growing, so the height the list recorded for
            // it is stale. Re-splicing the last item is what re-measures it.
            self.list_state.splice(message_count - 1..message_count, 1);
        }
        self.last_scroll_signature = signature;
        if scroll_to_latest {
            self.scroll_to_latest();
        }
        self.list_synced = true;
    }

    /// Reveal or hide the copy button for a message. Leaving starts a timer
    /// rather than hiding at once: the button sits away from the text, so an
    /// immediate hide would make it impossible to click.
    fn set_message_hovered(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        match id {
            Some(id) => {
                // Cancels a pending hide: the pointer came back in time.
                self.copy_hide_task = None;
                if self.hovered_message.as_deref() != Some(id.as_str()) {
                    self.hovered_message = Some(id);
                    cx.notify();
                }
            }
            None => {
                if self.hovered_message.is_none() {
                    return;
                }
                self.copy_hide_task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(Self::COPY_HIDE_DELAY).await;
                    let _ = this.update(cx, |this, cx| {
                        this.hovered_message = None;
                        this.copy_hide_task = None;
                        cx.notify();
                    });
                }));
            }
        }
    }

    /// Put a message on the clipboard and show the checkmark on its button.
    fn copy_message(&mut self, id: &str, content: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(content));
        self.copied_message = Some(id.to_string());
        self.copied_reset_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Self::COPY_CONFIRMATION)
                .await;
            let _ = this.update(cx, |this, cx| {
                this.copied_message = None;
                this.copied_reset_task = None;
                cx.notify();
            });
        }));
        cx.notify();
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
        let mut signature: u64 = state.messages().len() as u64;
        if let Some(last) = state.messages().last() {
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

    /// Where the transcript sits, from the list's own pixel bookkeeping.
    fn scroll_geometry(&self) -> ScrollGeometry {
        ScrollGeometry::new(
            f32::from(self.list_state.viewport_bounds().size.height),
            f32::from(self.list_state.max_offset_for_scrollbar().height),
            self.scroll_offset(),
        )
    }

    /// How far the transcript has been scrolled, in pixels.
    fn scroll_offset(&self) -> f32 {
        -f32::from(self.list_state.scroll_px_offset_for_scrollbar().y)
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

    /// Put the thumb's top at `thumb_top`, a fraction of the track. The list
    /// owns the mapping from pixels to items, so this only has to hand back the
    /// scroll offset the drag is asking for.
    fn set_thumb_top(&mut self, thumb_top: f32) {
        let offset = self.scroll_geometry().offset_for_thumb_top(thumb_top);
        self.list_state
            .set_offset_from_scrollbar(point(px(0.0), px(-offset)));
    }

    /// Put the transcript back on the newest message.
    ///
    /// A bottom-aligned list follows new content only while it is showing the
    /// bottom, and it leaves that mode the moment the user scrolls. Scrolling
    /// past the end is what re-enters it: the list clamps the offset it is given
    /// to its own maximum, and recognising that maximum is what clears the
    /// pinned scroll position. A merely *near*-bottom offset would not, and the
    /// next streamed paragraph would push itself out of view.
    ///
    /// Nothing here has to know how tall the transcript is, which is the point:
    /// the previous version guessed a target from the number of messages it
    /// could see and jumped there, so it landed in a different place every time
    /// the guess changed.
    fn scroll_to_latest(&mut self) {
        self.list_state
            .set_offset_from_scrollbar(point(px(0.0), px(-SCROLL_TO_END)));
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
            && self.scroll_geometry().is_scrollable;
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

    /// Mouse down on the track: grabs the thumb, or centres the thumb under the
    /// cursor and starts dragging from there.
    fn begin_scrollbar_drag(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        // Snapshot the content height first: until the drag ends, the list
        // reports the height it had when the drag started, so measurements
        // arriving mid-drag cannot move the thumb out from under the pointer.
        self.list_state.scrollbar_drag_started();
        let geometry = self.scroll_geometry();
        if !geometry.is_scrollable {
            self.list_state.scrollbar_drag_ended();
            return;
        }
        let pointer = self.pointer_fraction(event.position);
        let on_thumb =
            pointer >= geometry.thumb_top && pointer <= geometry.thumb_top + geometry.thumb_height;
        if !on_thumb {
            self.set_thumb_top(pointer - geometry.thumb_height / 2.0);
        }
        let geometry = self.scroll_geometry();
        self.scrollbar_grab =
            Some((pointer - geometry.thumb_top).clamp(0.0, geometry.thumb_height));
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
        let pointer = self.pointer_fraction(event.position);
        self.set_thumb_top(pointer - grab);
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

impl ChatView {
    fn copy_text_selection(
        &mut self,
        _: &CopySelection,
        _: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let Some(selection) = self.text_selection.as_ref() else {
            return;
        };
        if !selection.text.is_empty() {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(selection.text.clone()));
        }
        cx.notify();
    }

    /// Quote the selected text into the composer: `----` above it, so the next
    /// prompt opens with what the agent is being asked to look at.
    fn quote_text_selection(
        &mut self,
        _: &QuoteSelection,
        _: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) {
        let Some(selection) = self.text_selection.take() else {
            return;
        };
        if selection.text.is_empty() {
            return;
        }
        self.context_menu = None;
        let quoted = format!("----\n{}\n", selection.text);
        self.editor.update(cx, |editor, cx| {
            let draft = editor.text().to_string();
            let combined = if draft.trim().is_empty() {
                quoted
            } else {
                format!("{quoted}{draft}")
            };
            editor.set_text(combined, cx);
        });
        cx.notify();
    }
}

/// The transcript is the selection's home: every selectable markdown block
/// stores and reads its range through here.
impl SelectionHost for Entity<ChatView> {
    fn selection(&self, cx: &gpui::App) -> Option<(u64, std::ops::Range<usize>, String)> {
        self.read(cx).text_selection.as_ref().map(|selection| {
            (
                selection.key,
                selection.range.clone(),
                selection.text.clone(),
            )
        })
    }

    fn set_selection(&self, key: u64, range: Range<usize>, text: String, cx: &mut gpui::App) {
        self.update(cx, |chat, cx| {
            chat.text_selection = Some(TextSelection { key, range, text });
            chat.context_menu = None;
            cx.notify();
        });
    }

    fn clear_selection(&self, cx: &mut gpui::App) {
        self.update(cx, |chat, cx| {
            chat.text_selection.take();
            if chat.context_menu.take().is_some() {
                cx.notify();
            }
        });
    }

    fn focus_transcript(&self, window: &mut gpui::Window, cx: &mut gpui::App) {
        self.update(cx, |chat, _cx| {
            let handle = chat.transcript_focus_handle.clone();
            window.focus(&handle);
        });
    }

    fn open_context_menu(&self, position: gpui::Point<Pixels>, text: String, cx: &mut gpui::App) {
        self.update(cx, |chat, cx| {
            if text.is_empty() {
                return;
            }
            // The event reports window coordinates; the menu is drawn inside
            // the transcript, so it is stored relative to the view's origin.
            let view_origin = chat.view_origin.unwrap_or_default();
            chat.context_menu = Some(ContextMenu {
                position: gpui::point(position.x - view_origin.x, position.y - view_origin.y),
                text,
            });
            cx.notify();
        });
    }
}

impl ChatView {
    /// The copy / quote popup for the active selection, drawn inside the
    /// transcript at the right-click position.
    fn render_context_menu(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let menu = self.context_menu.clone()?;
        let chat = cx.entity();
        let menu_for_close = chat.clone();
        Some(
            div()
                .id("selection-context-menu")
                .debug_selector(|| "selection-context-menu".into())
                .absolute()
                .left(menu.position.x)
                .top((menu.position.y + px(8.0)).min(px(600.0)))
                .w(px(120.0))
                .rounded_md()
                .border_1()
                .border_color(Theme::border_strong())
                .bg(Theme::surface())
                .shadow_sm()
                .flex()
                .flex_col()
                .overflow_hidden()
                .child(
                    div()
                        .id("menu-copy-selection")
                        .px_2()
                        .py_1()
                        .text_size(Theme::text_px(12.0))
                        .cursor_pointer()
                        .hover(|style| style.bg(Theme::surface_hover()))
                        .on_click(move |_event, _window, cx| {
                            // The menu carries the text it opened for: the
                            // selection under the pointer may have moved on.
                            menu_for_close.update(cx, |chat, cx| {
                                if !menu.text.is_empty() {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                        menu.text.clone(),
                                    ));
                                }
                                chat.context_menu = None;
                                cx.notify();
                            });
                        })
                        .child("Copy"),
                )
                .child(
                    div()
                        .id("menu-quote-selection")
                        .px_2()
                        .py_1()
                        .text_size(Theme::text_px(12.0))
                        .cursor_pointer()
                        .hover(|style| style.bg(Theme::surface_hover()))
                        .on_click(move |_event, window, cx| {
                            chat.update(cx, |chat, cx| {
                                chat.quote_text_selection(&QuoteSelection, window, cx);
                            });
                        })
                        .child("Quote"),
                )
                .into_any(),
        )
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

        // The observer keeps `list_state` in step with the message vec; the
        // only time render has to do it itself is before that observer has
        // ever fired (ChatView created against a state that already holds a
        // restored session).
        if !self.list_synced {
            self.sync_list_state(cx);
        }

        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .bg(Theme::window_bg())
            .text_color(Theme::text())
            // The origin probe sits at the top of the view so a mouse event
            // reported in window coordinates can be drawn in view coordinates.
            .child(OriginProbe { chat: cx.entity() })
            .child(self.render_message_list(cx))
            .children(self.render_clarify_card(cx))
            .child(self.render_composer(cx))
            .children(self.render_context_menu(cx))
    }
}

/// A zero-height marker at the top of the view that records the view's origin
/// in window coordinates at paint, for turning window-coordinate events into
/// view-relative positions.
struct OriginProbe {
    chat: Entity<ChatView>,
}

impl IntoElement for OriginProbe {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl gpui::Element for OriginProbe {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _global_id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) -> (gpui::LayoutId, Self::RequestLayoutState) {
        let mut style = gpui::Style::default();
        style.size.width = gpui::relative(1.).into();
        style.size.height = px(0.0).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _global_id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: gpui::Bounds<Pixels>,
        _state: &mut Self::RequestLayoutState,
        _window: &mut gpui::Window,
        _cx: &mut gpui::App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _global_id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: gpui::Bounds<Pixels>,
        _state: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        _window: &mut gpui::Window,
        cx: &mut gpui::App,
    ) {
        self.chat.update(cx, |chat, _cx| {
            chat.view_origin = Some(bounds.origin);
        });
    }
}

impl ChatView {
    fn render_message_list(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let state = self.state.read(cx);
        let is_empty = state.messages().is_empty();
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
                                .text_size(Theme::text_px(20.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(Theme::text())
                                .child("Ready"),
                        )
                        .child(
                            div()
                                .text_size(Theme::text_px(13.0))
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
        // The transcript belongs to the active backend, so that is what labels
        // each reply. It used to be the literal "HERMES" whatever was running.
        let author = state_entity.read(cx).backend_display_name().to_uppercase();

        let chat_for_list = chat.clone();
        // The list reads the state per item instead of holding a copy of the
        // whole transcript: a copy would be a full clone of every message —
        // every content string and every tool-call detail — on every frame,
        // including one frame per streaming delta. One visible message is
        // cloned per rendered item; the markdown blocks it needs are reused
        // from the memo.
        let list = list(list_state, move |index, _window, cx| {
            let Some(message) = state_entity.read(cx).messages().get(index).cloned() else {
                return div().into_any();
            };
            let (expanded, revealed, copied) = {
                let chat = chat_for_list.read(cx);
                (
                    chat.expanded_messages.contains(&message.id),
                    chat.hovered_message.as_deref() == Some(message.id.as_str()),
                    chat.copied_message.as_deref() == Some(message.id.as_str()),
                )
            };
            let blocks = chat_for_list.update(cx, |chat, _cx| chat.markdown_blocks(&message));
            render_message_bubble(
                &message,
                blocks,
                expanded,
                CopyButton { revealed, copied },
                &author,
                index,
                chat_for_list.clone(),
            )
        })
        .flex_1()
        .min_h_0();

        let transcript = self.scroll_geometry();
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
            // The transcript holds the text selection: while it is focused a
            // ⌘C copies the selection through the host, not the composer.
            .key_context("Transcript")
            .track_focus(&self.transcript_focus_handle)
            .on_action(cx.listener(ChatView::copy_text_selection))
            .on_hover(move |hovered, _window, cx| {
                chat.update(cx, |chat, cx| chat.set_list_hovered(*hovered, cx));
            })
            .child(list);

        // Overlay scrollbar: hidden until the pointer is over the transcript,
        // then faded in; it fades back out once the pointer leaves.
        if transcript.is_scrollable {
            let track_height = self.scrollbar_track_height();
            let (thumb_top, thumb_height) = (transcript.thumb_top, transcript.thumb_height);
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
        if transcript.is_scrollable && !transcript.pinned_to_latest {
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
                    .on_click(move |_event, _window, cx| {
                        chat_jump.update(cx, |chat, cx| {
                            chat.scroll_to_latest();
                            cx.notify();
                        });
                    })
                    .child(
                        div()
                            .text_size(Theme::text_px(14.0))
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
                            .text_size(Theme::text_px(11.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(Theme::text_secondary())
                            .child(format!("{backend_name} needs your confirmation")),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("clarify-dismiss")
                            .text_size(Theme::text_px(11.0))
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
                    .text_size(Theme::text_px(13.0))
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
                            .text_size(Theme::text_px(11.0))
                            .text_color(Theme::text_secondary())
                            .child(format!("{}", index + 1)),
                    )
                    .child(
                        div()
                            .text_size(Theme::text_px(13.0))
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
                .text_size(Theme::text_px(10.0))
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
                state.is_sending(),
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
                                .text_size(Theme::text_px(11.0))
                                .text_color(Theme::text())
                                .max_w(px(140.0))
                                .text_ellipsis()
                                .child(attachment.name()),
                        )
                        .child(
                            div()
                                .text_size(Theme::text_px(10.0))
                                .text_color(Theme::text_tertiary())
                                .child(attachment.kind_label()),
                        )
                        .child(
                            div()
                                .id(gpui::ElementId::NamedInteger(
                                    "attachment-remove".into(),
                                    crate::ui::hash_id(&id),
                                ))
                                .text_size(Theme::text_px(11.0))
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
                        .text_size(Theme::text_px(10.0))
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
                                .text_size(Theme::text_px(11.0))
                                .text_color(Theme::text_secondary())
                                .flex_1()
                                .child(preview),
                        )
                        .child(
                            div()
                                .id(gpui::ElementId::NamedInteger("queue-send".into(), id_hash))
                                .text_size(Theme::text_px(11.0))
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
                                .text_size(Theme::text_px(11.0))
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
                                .text_size(Theme::text_px(11.0))
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
                                .text_size(Theme::text_px(14.0))
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
                            caps.contains(van_goal_core::models::BackendCaps::PERMISSION_MODES),
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
                            caps.contains(van_goal_core::models::BackendCaps::MODEL_SELECTION),
                            |this| {
                                this.child(context_meter(usage, 36.0))
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
        .text_size(Theme::text_px(11.0))
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
                .text_size(Theme::text_px(11.0))
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
    groups: &[(String, Vec<van_goal_core::models::ModelOption>)],
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
                .text_size(Theme::text_px(10.0))
                .text_color(Theme::text_tertiary())
                .child("Models"),
        );

    for (provider, models) in groups {
        menu = menu.child(
            div()
                .px_3()
                .pt_1()
                .text_size(Theme::text_px(10.0))
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
                    .text_size(Theme::text_px(11.0))
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

/// Whether a message's copy button is showing, and whether it is currently
/// confirming a copy.
#[derive(Clone, Copy, Default)]
struct CopyButton {
    revealed: bool,
    copied: bool,
}

/// Two offset sheets. Drawn rather than typed: the glyphs that mean "copy" are
/// missing from most fonts and would fall back to something unrecognisable.
fn copy_icon() -> AnyElement {
    let ink = Theme::text_secondary();
    div()
        .relative()
        .w(px(13.0))
        .h(px(13.0))
        .child(
            div()
                .absolute()
                .top_0()
                .left_0()
                .w(px(9.0))
                .h(px(9.0))
                .rounded_sm()
                .border_1()
                .border_color(ink),
        )
        .child(
            // The front sheet is filled with the transcript background so it
            // covers the back sheet's corner instead of crossing its outline.
            div()
                .absolute()
                .bottom_0()
                .right_0()
                .w(px(9.0))
                .h(px(9.0))
                .rounded_sm()
                .border_1()
                .border_color(ink)
                .bg(Theme::window_bg()),
        )
        .into_any()
}

fn copied_icon() -> AnyElement {
    div()
        .text_size(Theme::text_px(12.0))
        .text_color(Theme::ok())
        .child("✓")
        .into_any()
}

/// The copy affordance. It is hidden, not absent, while the pointer is away:
/// removing it from the tree would reflow the header every time the pointer
/// crossed the transcript.
fn copy_button<F>(state: CopyButton, id_hash: u64, on_click: F) -> AnyElement
where
    F: Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
{
    div()
        .id(gpui::ElementId::NamedInteger(
            "copy-message".into(),
            id_hash,
        ))
        .debug_selector(move || {
            format!(
                "copy-button-{}",
                match (state.copied, state.revealed) {
                    (true, _) => "copied",
                    (false, true) => "revealed",
                    (false, false) => "hidden",
                }
            )
        })
        .flex()
        .flex_row()
        .items_center()
        .justify_center()
        .w(px(22.0))
        .h(px(20.0))
        .rounded_sm()
        .cursor_pointer()
        .opacity(if state.revealed || state.copied {
            1.0
        } else {
            0.0
        })
        .on_click(on_click)
        .child(if state.copied {
            copied_icon()
        } else {
            copy_icon()
        })
        .into_any()
}

fn render_message_bubble(
    message: &van_goal_core::models::ChatMessage,
    blocks: Rc<Vec<MarkdownBlock>>,
    expanded: bool,
    copy: CopyButton,
    author: &str,
    index: usize,
    chat: Entity<ChatView>,
) -> AnyElement {
    let id_hash = crate::ui::hash_id(&message.id);
    let selection = SelectionBlock {
        message_id: message.id.clone(),
        host: Rc::new(chat.clone()),
    };
    let bubble = match message.role {
        MessageRole::User => div()
            .w_full()
            .flex()
            .flex_row()
            .justify_end()
            .py_2()
            .child(
                div()
                    .id(gpui::ElementId::NamedInteger("user-bubble".into(), id_hash))
                    .debug_selector(move || format!("user-bubble-{index}"))
                    .max_w(px(USER_BUBBLE_MAX_WIDTH))
                    .px(px(USER_BUBBLE_PADDING_X))
                    .py(px(10.0))
                    .rounded_lg()
                    .bg(Theme::user_bubble())
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(message.attachments.iter().map(|attachment| {
                        div()
                            .text_size(Theme::text_px(10.0))
                            .text_color(Theme::text_secondary())
                            .child(format!("@{}", attachment.name()))
                    }))
                    .child(
                        div()
                            .id(gpui::ElementId::NamedInteger(
                                "user-bubble-text".into(),
                                id_hash,
                            ))
                            .debug_selector(move || format!("user-bubble-text-{index}"))
                            // The bubble is sized by its content, so a plain
                            // max-width on the bubble alone leaves this text
                            // measured at its unwrapped width: the bubble then
                            // reports the height of the *unwrapped* text and the
                            // wrapped lines paint outside it, over the next
                            // message. Capping the text as well means the
                            // intrinsic pass already sees the final line count.
                            .max_w(px(USER_BUBBLE_TEXT_MAX_WIDTH))
                            .text_size(Theme::text_px(13.0))
                            .text_color(Theme::text())
                            .child(render_blocks(&blocks, Some(&selection))),
                    ),
            )
            .into_any(),
        MessageRole::Assistant => {
            let has_content = !message.is_streaming && !message.content.trim().is_empty();
            let mut bubble = div()
                .id(gpui::ElementId::NamedInteger(
                    "assistant-message".into(),
                    id_hash,
                ))
                .debug_selector(move || format!("assistant-message-{index}"))
                .w_full()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap_1()
                .py_2()
                // The pointer has to be over the message itself, not over the
                // button, for the button to appear — and leaving it starts a
                // grace period rather than hiding straight away.
                .on_hover({
                    let chat = chat.clone();
                    let message_id = message.id.clone();
                    move |hovered: &bool, _window, cx| {
                        let id = hovered.then(|| message_id.clone());
                        chat.update(cx, |chat, cx| chat.set_message_hovered(id, cx));
                    }
                });

            bubble = bubble.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child({
                        // The slug lets a test assert which backend the label
                        // came from; the label itself used to be hardcoded.
                        let slug = author.to_lowercase().replace(' ', "-");
                        div()
                            .debug_selector(move || format!("message-author-{slug}"))
                            .text_size(Theme::text_px(10.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(Theme::text_secondary())
                            .child(author.to_string())
                    })
                    .when(message.is_streaming, |this| {
                        this.child(
                            div()
                                .text_size(Theme::text_px(10.0))
                                .text_color(Theme::accent())
                                .child("streaming…"),
                        )
                    }),
            );

            if message.is_streaming || !message.tool_calls.is_empty() {
                bubble = bubble.child(render_activity(message, expanded, id_hash, chat.clone()));
            }

            if !message.content.trim().is_empty() {
                bubble = bubble.child(render_blocks(&blocks, Some(&selection)));
            }

            // Under the reply and against its left edge, so it sits next to the
            // text it copies instead of drifting to the far side of the window.
            // The row is always present, and doubles as the gap between
            // messages; only its contents fade.
            if has_content {
                let chat = chat.clone();
                let message_id = message.id.clone();
                let content = message.content.clone();
                bubble = bubble.child(div().flex().flex_row().pt_1().child(copy_button(
                    copy,
                    id_hash,
                    move |_event, _window, cx| {
                        let message_id = message_id.clone();
                        let content = content.clone();
                        chat.update(cx, |chat, cx| {
                            chat.copy_message(&message_id, content.clone(), cx)
                        });
                    },
                )));
            }

            bubble.into_any()
        }
        _ => div()
            .child(
                div()
                    .text_size(Theme::text_px(11.0))
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
    message: &van_goal_core::models::ChatMessage,
    expanded: bool,
    id_hash: u64,
    chat: Entity<ChatView>,
) -> AnyElement {
    let seconds = (van_goal_core::models::now_unix() - message.timestamp).max(0.0) as i64;
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
            .text_size(Theme::text_px(11.0))
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
                .text_size(Theme::text_px(11.0))
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
                            .text_size(Theme::text_px(11.0))
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
                            .text_size(Theme::text_px(10.0))
                            .text_color(Theme::text_secondary())
                            .child(detail),
                    ),
            );
        }
        if message.tool_calls.len() > 12 {
            container = container.child(
                div()
                    .ml_2()
                    .text_size(Theme::text_px(10.0))
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

/// How full the context is: a bar, and the numbers behind it.
///
/// One element for both places it is drawn — beside the model picker, and in the
/// status bar — so the two cannot disagree about it. The bar is sized by the
/// caller because the status bar has room the composer row does not.
///
/// [`ContextUsage::measured`] decides how the numbers are written. A backend
/// that reported them gets them plainly; one that reported nothing gets this
/// client's own estimate behind an "≈", because a reading and a guess are not
/// the same claim and should not look alike.
pub(crate) fn context_meter(usage: ContextUsage, bar_width: f32) -> AnyElement {
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
                .w(px(bar_width))
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
                .text_size(Theme::text_px(10.0))
                .text_color(Theme::text_tertiary())
                .child(format!(
                    "{}{} / {}",
                    if usage.measured { "" } else { "≈" },
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
        .text_size(Theme::text_px(12.0))
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
    use gpui::{AppContext, ListOffset, TestAppContext, VisualTestContext};
    use van_goal_core::models::ChatMessage;

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
            state.selected_session = None;
            state.conversation.set_transcript(transcript_of(40));
        });
        let (host, cx) =
            cx.add_window_view(|_window, cx| SizedChat(cx.new(|cx| ChatView::new(state, cx))));
        let view = host.read_with(cx, |host, _cx| host.0.clone());
        // The list measures its items during the first layout pass, so it only
        // knows how tall the transcript is — and only draws a thumb — from the
        // frame after that.
        let opened = settled_geometry(cx, &view);
        assert!(opened.is_scrollable, "long transcript is not scrollable");

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
                chat.list_state.scroll_to(ListOffset {
                    item_ix: 0,
                    offset_in_item: px(0.0),
                });
                cx.notify();
            });
        });
        let parked = settled_geometry(cx, &view);
        assert!(parked.is_scrollable, "long transcript is not scrollable");
        assert!(
            !parked.pinned_to_latest,
            "top of the transcript claims to be at the newest message"
        );
        // `debug_bounds` keeps whatever was inserted by any frame, so this only
        // proves the button is rendered while reading old messages.
        assert!(
            cx.debug_bounds("chat-jump-to-latest").is_some(),
            "no jump button while reading old messages"
        );

        // Returning to the newest message flips that predicate.
        cx.update(|_window, cx| {
            view.update(cx, |chat, cx| {
                chat.scroll_to_latest();
                cx.notify();
            });
        });
        let settled = settled_geometry(cx, &view);
        assert!(
            settled.pinned_to_latest,
            "never reported being at the newest message: {settled:?}"
        );
    }

    /// Sending a prompt appends the user message plus the assistant
    /// placeholder; the view has to move to what was just sent instead of
    /// staying where the user had scrolled to.
    #[gpui::test]
    fn sending_moves_the_view_to_the_new_message(cx: &mut TestAppContext) {
        let state = cx.new(|cx| AppState::new(cx));
        state.update(cx, |state, _cx| {
            state.selected_session = None;
            state.conversation.set_transcript(transcript_of(40));
        });
        let (host, cx) = cx.add_window_view(|_window, cx| {
            SizedChat(cx.new(|cx| ChatView::new(state.clone(), cx)))
        });
        let view = host.read_with(cx, |host, _cx| host.0.clone());
        cx.run_until_parked();

        // Park the view at the top, as if the user scrolled up to read.
        cx.update(|_window, cx| {
            view.update(cx, |chat, cx| {
                chat.list_state.scroll_to(ListOffset {
                    item_ix: 0,
                    offset_in_item: px(0.0),
                });
                cx.notify();
            });
        });
        let parked = settled_geometry(cx, &view);
        assert!(
            !parked.pinned_to_latest,
            "did not park away from the newest message: {parked:?}"
        );

        // A send appends the prompt and the streaming placeholder.
        state.update(cx, |state, cx| {
            state.conversation.begin_turn_with_placeholder("new prompt");
            cx.notify();
        });

        let after_send = settled_geometry(cx, &view);
        assert_eq!(
            cx.update(|_window, cx| view.update(cx, |chat, _cx| chat.list_state.item_count())),
            42,
            "the sent messages were not appended"
        );
        assert!(
            after_send.pinned_to_latest,
            "view stayed where it was after sending: {after_send:?}"
        );
    }

    /// Let the list lay itself out again, then report what the view says about
    /// its own scroll position.
    fn settled_geometry(cx: &mut VisualTestContext, view: &Entity<ChatView>) -> ScrollGeometry {
        for _ in 0..4 {
            cx.update(|window, _cx| window.refresh());
            cx.run_until_parked();
        }
        cx.update(|_window, cx| view.update(cx, |chat, _cx| chat.scroll_geometry()))
    }

    /// The thumb is the share of the transcript the viewport shows, so moving
    /// through the transcript must not resize it. It used to be sized from how
    /// many *messages* the list had managed to measure, which is a different
    /// number on every frame of a scroll — the thumb grew and shrank as it was
    /// dragged.
    #[gpui::test]
    fn the_thumb_does_not_change_length_while_scrolling(cx: &mut TestAppContext) {
        let state = cx.new(AppState::new);
        state.update(cx, |state, _cx| {
            state.selected_session = None;
            state.conversation.set_transcript(transcript_of(40));
        });
        let (host, cx) =
            cx.add_window_view(|_window, cx| SizedChat(cx.new(|cx| ChatView::new(state, cx))));
        let view = host.read_with(cx, |host, _cx| host.0.clone());

        let opened = settled_geometry(cx, &view);
        assert!(opened.is_scrollable, "transcript is not scrollable");

        // Walk the thumb down the track, as a drag would.
        for step in 0..=4 {
            let fraction = step as f32 / 4.0;
            cx.update(|_window, cx| {
                view.update(cx, |chat, cx| {
                    let geometry = chat.scroll_geometry();
                    chat.set_thumb_top((1.0 - geometry.thumb_height) * fraction);
                    cx.notify();
                });
            });
            let moved = settled_geometry(cx, &view);
            assert!(
                (moved.thumb_height - opened.thumb_height).abs() < 0.001,
                "step {step}: thumb changed length from {} to {}",
                opened.thumb_height,
                moved.thumb_height
            );
        }
    }

    /// A reply streaming in must not drag the view back down while the reader is
    /// further up the transcript.
    #[gpui::test]
    fn a_streaming_reply_does_not_drag_the_reader_back(cx: &mut TestAppContext) {
        let state = cx.new(AppState::new);
        state.update(cx, |state, _cx| {
            state.selected_session = None;
            state.conversation.set_transcript(transcript_of(40));
        });
        let (host, cx) = cx.add_window_view(|_window, cx| {
            SizedChat(cx.new(|cx| ChatView::new(state.clone(), cx)))
        });
        let view = host.read_with(cx, |host, _cx| host.0.clone());

        // Read from the top of the transcript.
        cx.update(|_window, cx| {
            view.update(cx, |chat, cx| {
                chat.list_state.scroll_to(ListOffset {
                    item_ix: 0,
                    offset_in_item: px(0.0),
                });
                cx.notify();
            });
        });
        let parked = settled_geometry(cx, &view);
        assert!(
            !parked.pinned_to_latest,
            "did not park away from the newest message"
        );
        let offset = cx.update(|_window, cx| view.update(cx, |chat, _cx| chat.scroll_offset()));

        // A reply arrives and streams in over several frames.
        state.update(cx, |state, cx| {
            state.conversation.begin_turn();
            state
                .conversation
                .messages_mut()
                .push(ChatMessage::streaming(MessageRole::Assistant));
            cx.notify();
        });
        for chunk in 0..5 {
            state.update(cx, |state, cx| {
                if let Some(last) = state.conversation.messages_mut().last_mut() {
                    last.content.push_str(&format!("streamed line {chunk}\n\n"));
                }
                cx.notify();
            });
            settled_geometry(cx, &view);
            let now = cx.update(|_window, cx| view.update(cx, |chat, _cx| chat.scroll_offset()));
            assert!(
                (now - offset).abs() < 1.0,
                "chunk {chunk}: the reader was moved from {offset} to {now}"
            );
        }
    }
}

#[cfg(test)]
mod scroll_geometry_tests {
    use super::*;

    /// A viewport showing `travel` pixels of scrollable content.
    fn geometry(viewport: f32, travel: f32, offset: f32) -> ScrollGeometry {
        ScrollGeometry::new(viewport, travel, offset)
    }

    #[test]
    fn nothing_to_scroll_when_the_transcript_fits() {
        let fits = geometry(600.0, 0.0, 0.0);
        assert!(!fits.is_scrollable);
        assert_eq!(fits.thumb_top, 0.0);
        assert_eq!(fits.thumb_height, 1.0);
        assert!(fits.pinned_to_latest);
        assert_eq!(fits.offset_for_thumb_top(0.5), 0.0);
    }

    /// The thumb is the share of the content the viewport shows, so it is sized
    /// from pixels. Sizing it from a count of *messages* is what made it change
    /// length while scrolling: one line and fifty lines both count as one
    /// message, and the list only reports the ones it has measured so far.
    #[test]
    fn thumb_is_proportional_to_the_content() {
        let at_top = geometry(600.0, 3000.0, 0.0);
        assert_eq!(at_top.thumb_top, 0.0);
        assert!(
            (at_top.thumb_height - 600.0 / 3600.0).abs() < 0.001,
            "height {}",
            at_top.thumb_height
        );

        let at_bottom = geometry(600.0, 3000.0, 3000.0);
        assert!(
            (at_bottom.thumb_top - (1.0 - at_bottom.thumb_height)).abs() < 0.001,
            "top {}",
            at_bottom.thumb_top
        );
        assert!(at_bottom.pinned_to_latest);
    }

    #[test]
    fn dragging_the_thumb_round_trips_to_the_same_offset() {
        let start = geometry(600.0, 3000.0, 0.0);
        let room = 1.0 - start.thumb_height;
        for fraction in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let offset = start.offset_for_thumb_top(room * fraction);
            let moved = geometry(600.0, 3000.0, offset);
            assert!(
                (moved.thumb_top - room * fraction).abs() < 0.001,
                "fraction {fraction}: thumb landed at {}",
                moved.thumb_top
            );
        }
    }

    #[test]
    fn thumb_keeps_a_minimum_size_on_very_long_transcripts() {
        let long = geometry(600.0, 600_000.0, 0.0);
        assert!(
            (long.thumb_height - MIN_THUMB_FRACTION).abs() < f32::EPSILON,
            "height {}",
            long.thumb_height
        );
    }

    #[test]
    fn a_single_message_never_divides_by_zero() {
        let one = geometry(600.0, 0.0, 0.0);
        assert!(!one.is_scrollable);
        assert_eq!(one.thumb_top, 0.0);
        assert_eq!(one.thumb_height, 1.0);
        assert_eq!(one.offset_for_thumb_top(0.9), 0.0);

        // Not laid out yet, so there is no viewport to divide by.
        let unlaid = geometry(0.0, 0.0, 0.0);
        assert!(!unlaid.is_scrollable);
        assert_eq!(unlaid.thumb_height, 1.0);
    }

    /// An offset past either end must not push the thumb off the track. The list
    /// clamps its own offset, but the geometry is also fed offsets it computes
    /// before the list has re-laid out.
    #[test]
    fn an_out_of_range_offset_stays_on_the_track() {
        let past_the_end = geometry(600.0, 3000.0, 9_999.0);
        assert!(past_the_end.pinned_to_latest);
        assert!(past_the_end.thumb_top <= 1.0 - past_the_end.thumb_height + f32::EPSILON);

        let before_the_start = geometry(600.0, 3000.0, -500.0);
        assert_eq!(before_the_start.thumb_top, 0.0);
        assert!(!before_the_start.pinned_to_latest);
    }
}

#[cfg(test)]
mod message_width_tests {
    use super::*;
    use gpui::{AppContext, TestAppContext, VisualTestContext};
    use van_goal_core::models::ChatMessage;

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
            state.conversation.set_transcript(transcript(messages));
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

    /// What a scheduled cron turn actually delivers: a long bracketed id, a file
    /// path, CJK text and explicit newlines. The transcript used to lay the
    /// following message on top of this bubble's last line, because the bubble
    /// was measured narrower than it was finally laid out.
    const CRON_PROMPT: &str = "[cron:057a3b4d-44ae-4d5e-9ec6-b81cb715369e 情话-晚间档 20:05] 运行每日情话任务（晚间档）：执行 python3 /home/liuxl/.openclaw/workspace/skills/flirt/scripts/send_love.py。脚本会生成情话并通过 macbridge 发 iMessage 给雪宝，自带日志和失败 Bark 告警。不要重复发送，只执行一次并确认日志写入\n。\nCurrent time: Monday, September 14th, 2026 - 8:05 PM (Asia/Shanghai)\nReference UTC: 2026-09-14 12:05 UTC";

    fn render_roles<'a>(
        cx: &'a mut TestAppContext,
        messages: Vec<(MessageRole, &str)>,
    ) -> &'a mut VisualTestContext {
        let state = cx.new(AppState::new);
        state.update(cx, |state, _cx| {
            state.selected_session = None;
            state.conversation.set_transcript(
                messages
                    .into_iter()
                    .map(|(role, content)| ChatMessage::new(role, content.to_string()))
                    .collect(),
            );
        });
        let (_host, cx) =
            cx.add_window_view(|_window, cx| SizedChat(cx.new(|cx| ChatView::new(state, cx))));
        cx.run_until_parked();
        cx
    }

    fn message_bounds(cx: &mut VisualTestContext, index: usize) -> gpui::Bounds<gpui::Pixels> {
        let selector: &'static str = Box::leak(format!("message-item-{index}").into_boxed_str());
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("message {index} was not laid out"))
    }

    #[gpui::test]
    fn a_cron_prompt_does_not_overlap_the_next_message(cx: &mut TestAppContext) {
        let cx = render_roles(
            cx,
            vec![
                (MessageRole::User, CRON_PROMPT),
                (
                    MessageRole::Assistant,
                    "[assistant turn failed before producing a message]",
                ),
            ],
        );

        let bubble_selector: &'static str = "user-bubble-0";
        let text_selector: &'static str = "user-bubble-text-0";
        let bubble = cx.debug_bounds(bubble_selector).expect("bubble");
        let text = cx.debug_bounds(text_selector).expect("bubble text");
        println!(
            "bubble x={} w={} h={} bottom={} | text x={} w={} h={} bottom={}",
            f32::from(bubble.origin.x),
            f32::from(bubble.size.width),
            f32::from(bubble.size.height),
            f32::from(bubble.origin.y) + f32::from(bubble.size.height),
            f32::from(text.origin.x),
            f32::from(text.size.width),
            f32::from(text.size.height),
            f32::from(text.origin.y) + f32::from(text.size.height),
        );
        assert!(
            f32::from(text.origin.y) + f32::from(text.size.height)
                <= f32::from(bubble.origin.y) + f32::from(bubble.size.height) + 0.5,
            "the prompt text paints below its own bubble"
        );

        let first = message_bounds(cx, 0);
        let second = message_bounds(cx, 1);
        let first_bottom = f32::from(first.origin.y) + f32::from(first.size.height);
        assert!(
            f32::from(second.origin.y) >= first_bottom - 0.5,
            "the reply starts at {} but the prompt bubble runs to {first_bottom}",
            f32::from(second.origin.y)
        );
    }

    #[gpui::test]
    fn a_short_prompt_still_gets_a_bubble_that_hugs_its_text(cx: &mut TestAppContext) {
        let cx = render_roles(
            cx,
            vec![(MessageRole::User, "hi"), (MessageRole::Assistant, "hello")],
        );

        let bubble_selector: &'static str = "user-bubble-0";
        let bubble = cx.debug_bounds(bubble_selector).expect("bubble");
        assert!(
            f32::from(bubble.size.width) < USER_BUBBLE_MAX_WIDTH,
            "capping the text must not stretch a short bubble to the full width: {}",
            f32::from(bubble.size.width)
        );
    }

    /// A prompt that is one long unbreakable run: the bubble still has to
    /// reserve the height its wrapped text needs.
    #[gpui::test]
    fn a_long_unbreakable_prompt_does_not_overlap_the_next_message(cx: &mut TestAppContext) {
        let hash = "0ec691c7175f4e50f6e7f758fef99ebd7222482fa424c2a8c1d2";
        let prompt =
            format!("commit {hash} touched PHLDB1/COLEC10/RUNX5-12-22 and more text after it");
        let cx = render_roles(
            cx,
            vec![(MessageRole::User, &prompt), (MessageRole::Assistant, "ok")],
        );

        let first = message_bounds(cx, 0);
        let second = message_bounds(cx, 1);
        let first_bottom = f32::from(first.origin.y) + f32::from(first.size.height);
        assert!(
            f32::from(second.origin.y) >= first_bottom - 0.5,
            "the reply starts at {} but the prompt bubble runs to {first_bottom}",
            f32::from(second.origin.y)
        );
    }
}

#[cfg(test)]
mod copy_button_tests {
    use super::*;
    use gpui::{AppContext, TestAppContext, VisualTestContext};
    use van_goal_core::models::ChatMessage;

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

    /// One reply, plus the id it was given so the test can address it.
    fn render_reply(cx: &mut TestAppContext) -> (Entity<ChatView>, String, &mut VisualTestContext) {
        let message = ChatMessage::new(MessageRole::Assistant, "hello there".to_string());
        let id = message.id.clone();
        let state = cx.new(AppState::new);
        state.update(cx, |state, _cx| {
            state.selected_session = None;
            state.conversation.set_transcript(vec![message]);
        });
        let (host, cx) =
            cx.add_window_view(|_window, cx| SizedChat(cx.new(|cx| ChatView::new(state, cx))));
        let view = host.read_with(cx, |host, _cx| host.0.clone());
        cx.run_until_parked();
        (view, id, cx)
    }

    fn hover(view: &Entity<ChatView>, id: Option<String>, cx: &mut VisualTestContext) {
        cx.update(|_window, cx| {
            view.update(cx, |chat, cx| chat.set_message_hovered(id, cx));
        });
        cx.run_until_parked();
    }

    #[gpui::test]
    fn the_copy_button_stays_hidden_until_the_message_is_hovered(cx: &mut TestAppContext) {
        let (view, id, cx) = render_reply(cx);
        assert!(
            cx.debug_bounds("copy-button-hidden").is_some(),
            "the copy button is showing before anything is hovered"
        );

        hover(&view, Some(id), cx);
        assert!(
            cx.debug_bounds("copy-button-revealed").is_some(),
            "hovering the message did not reveal its copy button"
        );
        assert!(cx.debug_bounds("copy-button-hidden").is_none());
    }

    /// The button sits in the message header, away from the text, so hiding it
    /// the instant the pointer leaves would make it unclickable.
    #[gpui::test]
    fn the_copy_button_lingers_after_the_pointer_leaves(cx: &mut TestAppContext) {
        let (view, id, cx) = render_reply(cx);
        hover(&view, Some(id), cx);
        assert!(cx.debug_bounds("copy-button-revealed").is_some());

        hover(&view, None, cx);
        assert!(
            cx.debug_bounds("copy-button-revealed").is_some(),
            "the button vanished the moment the pointer left"
        );

        cx.executor()
            .advance_clock(ChatView::COPY_HIDE_DELAY + Duration::from_millis(50));
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("copy-button-hidden").is_some(),
            "the button never went away after the grace period"
        );
    }

    #[gpui::test]
    fn a_copy_is_confirmed_then_the_button_returns(cx: &mut TestAppContext) {
        let (view, id, cx) = render_reply(cx);
        hover(&view, Some(id.clone()), cx);

        cx.update(|_window, cx| {
            view.update(cx, |chat, cx| {
                chat.copy_message(&id, "hello there".to_string(), cx)
            });
        });
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("copy-button-copied").is_some(),
            "copying gave no visible confirmation"
        );

        cx.executor()
            .advance_clock(ChatView::COPY_CONFIRMATION + Duration::from_millis(50));
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("copy-button-copied").is_none(),
            "the confirmation never cleared"
        );
        assert!(
            cx.debug_bounds("copy-button-revealed").is_some(),
            "the button did not come back after confirming"
        );
    }

    /// Leaving and coming back must cancel the pending hide rather than let it
    /// fire while the pointer is back on the message.
    #[gpui::test]
    fn returning_to_the_message_cancels_the_pending_hide(cx: &mut TestAppContext) {
        let (view, id, cx) = render_reply(cx);
        hover(&view, Some(id.clone()), cx);
        hover(&view, None, cx);
        hover(&view, Some(id), cx);

        cx.executor()
            .advance_clock(ChatView::COPY_HIDE_DELAY + Duration::from_millis(50));
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("copy-button-revealed").is_some(),
            "the button hid even though the pointer was back on the message"
        );
    }

    /// The reply header used to read "HERMES" no matter which backend was
    /// running, so an OpenClaw transcript claimed to be Hermes.
    #[gpui::test]
    fn the_reply_header_names_the_active_backend(cx: &mut TestAppContext) {
        let (view, _id, cx) = render_reply(cx);
        assert!(
            cx.debug_bounds("message-author-hermes").is_some(),
            "the default backend should label its replies"
        );

        cx.update(|_window, cx| {
            view.update(cx, |_chat, cx| {
                let state = _chat.state.clone();
                state.update(cx, |state, _cx| {
                    state.settings.backend_kind = van_goal_core::settings::BackendKind::OpenClaw;
                    state.backend_display_name = "OpenClaw";
                });
                cx.notify();
            });
        });
        cx.run_until_parked();

        assert!(
            cx.debug_bounds("message-author-openclaw").is_some(),
            "the label did not follow the active backend"
        );
        assert!(
            cx.debug_bounds("message-author-hermes").is_none(),
            "the label is still hardcoded to Hermes"
        );
    }

    /// The button used to sit in the header row, pushed to the far right of a
    /// full-width row, miles away from the left-aligned reply it copies.
    #[gpui::test]
    fn the_copy_button_sits_under_the_reply_against_its_left_edge(cx: &mut TestAppContext) {
        let (view, id, cx) = render_reply(cx);
        hover(&view, Some(id), cx);

        let column = cx
            .debug_bounds("message-column-0")
            .expect("the message column was not laid out");
        let author = cx
            .debug_bounds("message-author-hermes")
            .expect("the reply header was not laid out");
        let button = cx
            .debug_bounds("copy-button-revealed")
            .expect("the copy button was not revealed");

        assert!(
            f32::from(button.origin.y) >= f32::from(author.origin.y),
            "the copy button is not under the reply"
        );
        let offset = f32::from(button.origin.x) - f32::from(column.origin.x);
        assert!(
            offset < 40.0,
            "the copy button drifted {offset}px away from the reply's left edge"
        );
    }
}
