use crate::ui::theme::Theme;
use gpui::prelude::FluentBuilder;
use gpui::{
    actions, div, fill, hsla, point, px, relative, size, App, Bounds, Context, CursorStyle,
    Element, ElementId, ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, Font, GlobalElementId, Hsla, InspectorElementId, InteractiveElement, IntoElement,
    KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad,
    ParentElement, Pixels, Point, Render, ScrollWheelEvent, SharedString, Size,
    StatefulInteractiveElement, Style, Styled, TextRun, UTF16Selection, Window, WrappedLine,
};
use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use unicode_segmentation::UnicodeSegmentation;

static NEXT_EDITOR_ID: AtomicU64 = AtomicU64::new(1);

actions!(
    editor,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectAll,
        Home,
        End,
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
        Newline,
        Submit,
    ]
);

pub fn bind_editor_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("Editor")),
        KeyBinding::new("delete", Delete, Some("Editor")),
        KeyBinding::new("left", Left, Some("Editor")),
        KeyBinding::new("right", Right, Some("Editor")),
        KeyBinding::new("up", Up, Some("Editor")),
        KeyBinding::new("down", Down, Some("Editor")),
        KeyBinding::new("shift-left", SelectLeft, Some("Editor")),
        KeyBinding::new("shift-right", SelectRight, Some("Editor")),
        KeyBinding::new("shift-up", SelectUp, Some("Editor")),
        KeyBinding::new("shift-down", SelectDown, Some("Editor")),
        KeyBinding::new("cmd-a", SelectAll, Some("Editor")),
        KeyBinding::new("home", Home, Some("Editor")),
        KeyBinding::new("end", End, Some("Editor")),
        KeyBinding::new("cmd-v", Paste, Some("Editor")),
        KeyBinding::new("cmd-c", Copy, Some("Editor")),
        KeyBinding::new("cmd-x", Cut, Some("Editor")),
        KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, Some("Editor")),
        KeyBinding::new("shift-enter", Newline, Some("Editor")),
        KeyBinding::new("enter", Submit, Some("Editor")),
    ]);
}

#[derive(Clone, Debug, PartialEq)]
pub enum EditorEvent {
    /// Enter pressed (composer send / field commit).
    Submit,
    /// Content changed.
    Change,
}

#[derive(Clone)]
struct LineEntry {
    /// Byte offset of this logical line inside the content.
    byte_start: usize,
    line: WrappedLine,
}

#[derive(Clone, PartialEq)]
struct EditorShapeKey {
    text: SharedString,
    wrap_width: Option<Pixels>,
    font: Font,
    font_size: Pixels,
    color: Hsla,
}

#[derive(Clone)]
struct RequestedShape {
    key: EditorShapeKey,
    wrapped: Vec<WrappedLine>,
}

fn shape_editor_text(window: &mut Window, key: &EditorShapeKey) -> Vec<WrappedLine> {
    let run = TextRun {
        len: key.text.len(),
        font: key.font.clone(),
        color: key.color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_text(
            key.text.clone(),
            key.font_size,
            &[run],
            key.wrap_width,
            None,
        )
        .unwrap_or_default()
        .into_vec()
}

fn line_entries(content: &str, wrapped: Vec<WrappedLine>) -> Vec<LineEntry> {
    let mut lines = Vec::with_capacity(wrapped.len());
    let mut byte_start = 0usize;
    for line in wrapped {
        lines.push(LineEntry { byte_start, line });
        let remainder = &content[byte_start.min(content.len())..];
        byte_start += remainder.find('\n').unwrap_or(remainder.len()) + 1;
    }
    lines
}

pub struct Editor {
    hitbox_id: SharedString,
    element_id: SharedString,
    focus_handle: FocusHandle,
    content: String,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    lines: Vec<LineEntry>,
    /// Shape used by `lines`. Long drafts are expensive to shape, so an
    /// unrelated chat repaint reuses it until text, width or typography moves.
    shape_key: Option<EditorShapeKey>,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
    /// Single-line fields hide newlines and use input cursor style.
    single_line: bool,
    /// Long single values such as credentials may wrap visually while still
    /// rejecting embedded line breaks and submitting on Enter.
    wrap_long_lines: bool,
    /// First visible wrapped row: content taller than the editor's box is
    /// clipped, and this window follows the caret so what is being typed or
    /// selected stays on screen.
    scroll_row: usize,
    /// Set by editing/caret movement and consumed after the next reshape.
    /// Manual wheel/thumb scrolling leaves it false so the viewport does not
    /// immediately snap back to the caret on the following frame.
    follow_cursor: bool,
    /// Pointer offset inside the vertical scrollbar thumb while it is dragged.
    scrollbar_grab: Option<f32>,
}

impl Editor {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self::new_with(cx, false, true)
    }

    pub fn single_line(cx: &mut Context<Self>) -> Self {
        Self::new_with(cx, true, false)
    }

    pub fn wrapped_single_line(cx: &mut Context<Self>) -> Self {
        Self::new_with(cx, true, true)
    }

    fn new_with(cx: &mut Context<Self>, single_line: bool, wrap_long_lines: bool) -> Self {
        let id = NEXT_EDITOR_ID.fetch_add(1, Ordering::Relaxed);
        Self {
            hitbox_id: format!("editor-hitbox-{id}").into(),
            element_id: format!("editor-element-{id}").into(),
            focus_handle: cx.focus_handle(),
            content: String::new(),
            placeholder: "Type here…".into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            lines: Vec::new(),
            shape_key: None,
            last_bounds: None,
            is_selecting: false,
            scroll_row: 0,
            follow_cursor: true,
            scrollbar_grab: None,
            single_line,
            wrap_long_lines,
        }
    }

    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.placeholder = placeholder.into();
        self.lines.clear();
        self.shape_key = None;
        cx.notify();
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    pub fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        let text = text.into();
        self.content = if self.single_line {
            normalize_single_line(&text)
        } else {
            text
        };
        self.selected_range = self.content.len()..self.content.len();
        self.selection_reversed = false;
        self.marked_range = None;
        // The text changed wholesale: rows are reshaped at the next paint, so
        // the scroll window restarts at the top and follows the caret again.
        self.lines.clear();
        self.shape_key = None;
        self.scroll_row = 0;
        self.follow_cursor = true;
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.set_text("", cx);
    }

    // -- movement ---------------------------------------------------------

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = self.clamp(offset);
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.follow_cursor = true;
        self.ensure_cursor_visible(cx);
        cx.notify();
    }

    fn cursor_offset(&self) -> usize {
        let offset = if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        };
        self.clamp(offset)
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = self.clamp(offset);
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        self.selected_range = self.clamp_range(self.selected_range.clone());
        self.follow_cursor = true;
        self.ensure_cursor_visible(cx);
        cx.notify();
    }

    /// Clamp `offset` into the content *and* onto a character boundary. Every
    /// range the editor stores has to stay valid for slicing `content`, no
    /// matter what offsets AppKit hands us.
    fn clamp(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.content.len());
        while offset > 0 && !self.content.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    /// Same as [`Self::clamp`], for both ends of a range. Inverted ranges are
    /// normalized instead of panicking on `start > end`.
    fn clamp_range(&self, range: Range<usize>) -> Range<usize> {
        let (start, end) = if range.start <= range.end {
            (range.start, range.end)
        } else {
            (range.end, range.start)
        };
        let start = self.clamp(start);
        start..self.clamp(end).max(start)
    }

    /// Start-of-line (or start-of-text for Home) index.
    fn line_start(&self, offset: usize) -> usize {
        self.content[..self.clamp(offset)]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0)
    }

    /// End-of-line index (exclusive of the newline itself).
    fn line_end(&self, offset: usize) -> usize {
        let offset = self.clamp(offset);
        self.content[offset..]
            .find('\n')
            .map(|index| offset + index)
            .unwrap_or(self.content.len())
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(index, _)| (index < offset).then_some(index))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(index, _)| (index > offset).then_some(index))
            .unwrap_or(self.content.len())
    }

    // -- utf16 mapping (IME) ----------------------------------------------

    /// Byte offsets of AppKit's utf16 ranges. Always lands on a character
    /// boundary, so the result is safe to slice `content` with.
    fn offset_from_utf16(&self, offset: usize) -> usize {
        byte_offset_from_utf16(&self.content, offset)
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        utf16_offset_from_byte(&self.content, self.clamp(offset))
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        let range = self.clamp_range(range.clone());
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.clamp_range(self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end))
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let Some(bounds) = self.last_bounds.as_ref() else {
            return 0;
        };
        let line_height = px(row_height());
        for (row_index, entry) in self.lines.iter().enumerate() {
            let rows = entry.line.wrap_boundaries().len() + 1;
            // Rows scrolled off the top sit above the box; a click can only
            // land on what is visible.
            let visible_row = self.rows_above(row_index) as isize - self.scroll_row as isize;
            let top = bounds.top() + px(visible_row as f32 * row_height());
            if top < bounds.top() {
                continue;
            }
            let span = px(row_height() * rows as f32);
            if position.y >= top && position.y <= top + span {
                let local = point(position.x - bounds.left(), position.y - top);
                let offset = entry.byte_start
                    + entry
                        .line
                        .closest_index_for_position(local, line_height)
                        .unwrap_or_else(|closest| closest);
                return self.clamp(offset);
            }
        }
        // Below/above all lines: clamp to start or end.
        if position.y < bounds.top() {
            0
        } else {
            self.content.len()
        }
    }

    fn rows_above(&self, up_to: usize) -> usize {
        self.lines[..up_to.min(self.lines.len())]
            .iter()
            .map(|entry| entry.line.wrap_boundaries().len() + 1)
            .sum()
    }

    /// Rows the content would paint in total.
    fn total_rows(&self) -> usize {
        self.rows_above(self.lines.len())
    }

    /// The wrapped row the caret sits on.
    fn caret_row(&self) -> usize {
        let cursor = self.cursor_offset();
        for (index, entry) in self.lines.iter().enumerate() {
            let line_end = entry.byte_start + entry.line.len();
            if cursor <= line_end {
                let within = entry
                    .line
                    .position_for_index(
                        (cursor - entry.byte_start).min(entry.line.len()),
                        px(row_height()),
                    )
                    .map(|point| (point.y / px(row_height())) as usize)
                    .unwrap_or(0);
                return self.rows_above(index) + within;
            }
        }
        self.total_rows()
    }

    /// Keep the caret row on screen once the content outgrows the box.
    fn ensure_cursor_visible(&mut self, cx: &mut Context<Self>) {
        if self.single_line && !self.wrap_long_lines {
            return;
        }
        let max = self.max_visible_rows();
        let total = self.total_rows();
        if total <= max {
            if self.scroll_row != 0 {
                self.scroll_row = 0;
                cx.notify();
            }
            return;
        }
        let caret = self.caret_row();
        let mut next = self.scroll_row;
        if caret < next {
            next = caret;
        }
        if caret >= next + max {
            next = caret + 1 - max;
        }
        if next != self.scroll_row {
            self.scroll_row = next;
            cx.notify();
        }
    }

    /// How many rows fit the editor's box.
    fn max_visible_rows(&self) -> usize {
        8
    }

    /// Scroll the visible window by whole rows, clamped to the content. This
    /// is the wheel's path; typing keeps following the caret.
    fn scroll_by(&mut self, rows: i64, cx: &mut Context<Self>) {
        self.follow_cursor = false;
        let max_scroll = self.total_rows().saturating_sub(self.max_visible_rows());
        let next = (self.scroll_row as i64 + rows).clamp(0, max_scroll as i64) as usize;
        if next != self.scroll_row {
            self.scroll_row = next;
            self.follow_cursor = false;
            cx.notify();
        }
    }

    /// `(thumb top, thumb height)` in window coordinates.
    fn scrollbar_geometry(&self, bounds: Bounds<Pixels>) -> Option<(Pixels, Pixels)> {
        let visible = self.max_visible_rows();
        let total = self.total_rows();
        if total <= visible {
            return None;
        }
        let track = f32::from(bounds.size.height);
        let thumb_height = (track * visible as f32 / total as f32).max(24.0).min(track);
        let travel = (track - thumb_height).max(0.0);
        let max_scroll = total.saturating_sub(visible).max(1);
        let thumb_top = f32::from(bounds.top())
            + travel * self.scroll_row.min(max_scroll) as f32 / max_scroll as f32;
        Some((px(thumb_top), px(thumb_height)))
    }

    fn scroll_thumb_to(
        &mut self,
        pointer_y: Pixels,
        grab: f32,
        bounds: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some((_, thumb_height)) = self.scrollbar_geometry(bounds) else {
            return;
        };
        let track = f32::from(bounds.size.height);
        let thumb_height = f32::from(thumb_height);
        let travel = (track - thumb_height).max(1.0);
        let top = (f32::from(pointer_y - bounds.top()) - grab).clamp(0.0, travel);
        let max_scroll = self.total_rows().saturating_sub(self.max_visible_rows());
        let next = (top / travel * max_scroll as f32).round() as usize;
        self.follow_cursor = false;
        if next != self.scroll_row {
            self.scroll_row = next;
            cx.notify();
        }
    }

    /// The scroll wheel: wheeling down scrolls the window down.
    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let total = self.total_rows();
        if total <= self.max_visible_rows() {
            return;
        }
        let pixels = event.delta.pixel_delta(px(row_height())).y;
        self.scroll_by((-pixels / px(row_height())).round() as i64, cx);
        window.prevent_default();
    }

    // -- editing -----------------------------------------------------------

    fn replace_text_in_range_internal(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        cx: &mut Context<Self>,
    ) {
        let new_text = if self.single_line {
            normalize_single_line(new_text)
        } else {
            new_text.to_string()
        };
        // AppKit's replacement range wins, then the live marked text, then the
        // selection. Whatever we get gets clamped: a stale range (AppKit
        // replays ranges from before the app cleared or rewrote the editor)
        // must never slice out of bounds, because that panic unwinds across
        // the Objective-C boundary and aborts the whole app.
        let range = self.clamp_range(
            range_utf16
                .as_ref()
                .map(|range| self.range_from_utf16(range))
                .or_else(|| self.marked_range.clone())
                .unwrap_or_else(|| self.selected_range.clone()),
        );
        let cursor = range.start + new_text.len();
        self.content = format!(
            "{}{new_text}{}",
            &self.content[..range.start],
            &self.content[range.end..]
        );
        self.selected_range = cursor..cursor;
        self.marked_range = None;
        // Content just moved under the caret: reshaped rows land later.
        self.lines.clear();
        self.shape_key = None;
        self.follow_cursor = true;
        self.ensure_cursor_visible(cx);
        cx.emit(EditorEvent::Change);
        cx.notify();
    }

    fn insert_newline(&mut self, cx: &mut Context<Self>) {
        if self.single_line {
            cx.emit(EditorEvent::Submit);
            return;
        }
        self.replace_text_in_range_internal(None, "\n", cx);
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let offset = self.cursor_offset();
            self.select_to(self.previous_boundary(offset), cx);
        }
        self.replace_text_in_range_internal(None, "", cx);
    }

    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let offset = self.cursor_offset();
            self.select_to(self.next_boundary(offset), cx);
        }
        self.replace_text_in_range_internal(None, "", cx);
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn up(&mut self, _: &Up, window: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.cursor_offset();
        let line_start = self.line_start(cursor);
        if line_start == 0 {
            self.move_to(0, cx);
            return;
        }
        // Column in bytes within the current line; find same column on the
        // previous line via the pixel position of the cursor.
        let column = cursor - line_start;
        let prev_line_start = self.line_start(line_start - 1);
        let prev_line_len = line_start - 1 - prev_line_start;
        let target = prev_line_start + column.min(prev_line_len);
        self.move_to(target, cx);
        let _ = window;
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.cursor_offset();
        let line_end = self.line_end(cursor);
        if line_end >= self.content.len() {
            self.move_to(self.content.len(), cx);
            return;
        }
        let line_start = self.line_start(cursor);
        let column = cursor - line_start;
        let next_line_start = line_end + 1;
        let next_line_end = self.line_end(next_line_start);
        let target = next_line_start + column.min(next_line_end - next_line_start);
        self.move_to(target, cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_start(self.cursor_offset()), cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_end(self.cursor_offset()), cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.cursor_offset();
        let line_start = self.line_start(cursor);
        if line_start == 0 {
            self.select_to(0, cx);
            return;
        }
        let column = cursor - line_start;
        let prev_line_start = self.line_start(line_start - 1);
        let prev_len = line_start - 1 - prev_line_start;
        self.select_to(prev_line_start + column.min(prev_len), cx);
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.cursor_offset();
        let line_end = self.line_end(cursor);
        if line_end >= self.content.len() {
            self.select_to(self.content.len(), cx);
            return;
        }
        let line_start = self.line_start(cursor);
        let column = cursor - line_start;
        let next_start = line_end + 1;
        let next_end = self.line_end(next_start);
        self.select_to(next_start + column.min(next_end - next_start), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        cx.notify();
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range_internal(None, &text, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        let range = self.clamp_range(self.selected_range.clone());
        if !range.is_empty() {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                self.content[range].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        let range = self.clamp_range(self.selected_range.clone());
        if !range.is_empty() {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                self.content[range].to_string(),
            ));
            self.replace_text_in_range_internal(None, "", cx);
        }
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(EditorEvent::Submit);
    }

    // -- mouse ---------------------------------------------------------------

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Clicking anywhere in the editor takes focus so typing works.
        window.focus(&self.focus_handle);
        if let Some(bounds) = self.last_bounds {
            if let Some((thumb_top, thumb_height)) = self.scrollbar_geometry(bounds) {
                if event.position.x >= bounds.right() - px(12.0) {
                    let y = f32::from(event.position.y);
                    let top = f32::from(thumb_top);
                    let height = f32::from(thumb_height);
                    let grab = if y >= top && y <= top + height {
                        y - top
                    } else {
                        height / 2.0
                    };
                    self.scrollbar_grab = Some(grab);
                    self.scroll_thumb_to(event.position.y, grab, bounds, cx);
                    self.is_selecting = false;
                    window.prevent_default();
                    return;
                }
            }
        }
        self.is_selecting = true;
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
        self.scrollbar_grab = None;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let (Some(grab), Some(bounds)) = (self.scrollbar_grab, self.last_bounds) {
            self.scroll_thumb_to(event.position.y, grab, bounds, cx);
            return;
        }
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }
}

fn normalize_single_line(text: &str) -> String {
    text.chars()
        .filter(|character| !matches!(character, '\r' | '\n'))
        .collect()
}

/// Byte offset of a utf16 offset inside `text`, clamped to `text.len()`.
///
/// AppKit speaks utf16 (`NSRange`), the editor stores byte offsets. Iterating
/// whole characters keeps the result on a character boundary.
fn byte_offset_from_utf16(text: &str, offset: usize) -> usize {
    let mut utf8_offset = 0;
    let mut utf16_count = 0;
    for character in text.chars() {
        if utf16_count >= offset {
            break;
        }
        utf16_count += character.len_utf16();
        utf8_offset += character.len_utf8();
    }
    utf8_offset
}

/// The inverse of [`byte_offset_from_utf16`].
fn utf16_offset_from_byte(text: &str, offset: usize) -> usize {
    let mut utf16_offset = 0;
    let mut utf8_count = 0;
    for character in text.chars() {
        if utf8_count >= offset {
            break;
        }
        utf8_count += character.len_utf8();
        utf16_offset += character.len_utf16();
    }
    utf16_offset
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, Empty, TestAppContext, VisualTestContext};

    #[test]
    fn pasted_single_line_values_drop_terminal_line_breaks() {
        assert_eq!(normalize_single_line("gateway-token\r\n"), "gateway-token");
    }

    // -- macOS input method (IMKit/AppKit) drivers -------------------------
    //
    // AppKit talks to the editor through `EntityInputHandler` with NSRange
    // utf16 offsets: `setMarkedText:selectedRange:replacementRange:` arrives
    // as `replace_and_mark_text_in_range`, `insertText:replacementRange:` as
    // `replace_text_in_range`, and `unmarkText` as `unmark_text`. The
    // `selectedRange` is relative to the *new marked text*, `replacementRange`
    // is in document coordinates (or none for NSNotFound).

    fn appkit_set_marked_text(
        editor: &Entity<Editor>,
        cx: &mut VisualTestContext,
        text: &str,
        selected_range: Option<(usize, usize)>,
        replacement_range: Option<(usize, usize)>,
    ) {
        let selected_range = selected_range.map(|(location, length)| location..location + length);
        let replacement_range =
            replacement_range.map(|(location, length)| location..location + length);
        editor.update_in(cx, |editor, window, cx| {
            editor.replace_and_mark_text_in_range(
                replacement_range,
                text,
                selected_range,
                window,
                cx,
            )
        });
    }

    fn appkit_insert_text(
        editor: &Entity<Editor>,
        cx: &mut VisualTestContext,
        text: &str,
        replacement_range: Option<(usize, usize)>,
    ) {
        let replacement_range =
            replacement_range.map(|(location, length)| location..location + length);
        editor.update_in(cx, |editor, window, cx| {
            editor.replace_text_in_range(replacement_range, text, window, cx)
        });
    }

    fn appkit_unmark_text(editor: &Entity<Editor>, cx: &mut VisualTestContext) {
        editor.update_in(cx, |editor, window, cx| editor.unmark_text(window, cx));
    }

    /// `markedRange` as AppKit reads it back from the editor.
    fn appkit_marked_range(
        editor: &Entity<Editor>,
        cx: &mut VisualTestContext,
    ) -> Option<(usize, usize)> {
        editor
            .update_in(cx, |editor, window, cx| {
                editor.marked_text_range(window, cx)
            })
            .map(|range| (range.start, range.end - range.start))
    }

    fn editor_state(
        editor: &Entity<Editor>,
        cx: &mut VisualTestContext,
    ) -> (String, Range<usize>, Option<Range<usize>>) {
        editor.update_in(cx, |editor, _, _| {
            (
                editor.content.clone(),
                editor.selected_range.clone(),
                editor.marked_range.clone(),
            )
        })
    }

    /// Every range the editor keeps must be usable for slicing `content`.
    fn assert_ranges_in_bounds(editor: &Entity<Editor>, cx: &mut VisualTestContext, step: &str) {
        let (content, selected, marked) = editor_state(editor, cx);
        for (name, range) in [("selected_range", Some(selected)), ("marked_range", marked)] {
            let Some(range) = range else { continue };
            assert!(
                range.start <= range.end && range.end <= content.len(),
                "{step}: {name} {range:?} escapes content of {} bytes ({content:?})",
                content.len()
            );
            assert!(
                content.is_char_boundary(range.start) && content.is_char_boundary(range.end),
                "{step}: {name} {range:?} is not on char boundaries ({content:?})"
            );
        }
    }

    fn pinyin_commit(
        editor: &Entity<Editor>,
        cx: &mut VisualTestContext,
        keys: &str,
        commit: &str,
    ) {
        for (index, _) in keys.char_indices().skip(1) {
            let marked = appkit_marked_range(editor, cx);
            let composition = &keys[..index];
            appkit_set_marked_text(
                editor,
                cx,
                composition,
                Some((composition.chars().count(), 0)),
                marked,
            );
            assert_ranges_in_bounds(editor, cx, &format!("composing {composition}"));
        }
        let marked = appkit_marked_range(editor, cx);
        appkit_set_marked_text(editor, cx, keys, Some((keys.chars().count(), 0)), marked);
        assert_ranges_in_bounds(editor, cx, &format!("composing {keys}"));
        appkit_insert_text(editor, cx, commit, None);
        assert_ranges_in_bounds(editor, cx, &format!("committing {commit}"));
    }

    #[gpui::test]
    fn ime_unmark_then_ascii_insert_does_not_slice_out_of_bounds(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::wrapped_single_line(cx));
        let (_root, cx) = cx.add_window_view(|_window, _cx| Empty);

        // Compose "ni" (AppKit passes the live `markedRange` as replacementRange).
        appkit_set_marked_text(&editor, cx, "n", Some((1, 0)), None);
        let marked = appkit_marked_range(&editor, cx);
        appkit_set_marked_text(&editor, cx, "ni", Some((2, 0)), marked);

        // AppKit unmarks, then delivers the next key with replacementRange = NSNotFound.
        appkit_unmark_text(&editor, cx);
        appkit_insert_text(&editor, cx, "x", None);

        let (content, selected, marked) = editor_state(&editor, cx);
        assert_eq!(content, "nix");
        assert_eq!(selected, content.len()..content.len());
        assert_eq!(marked, None);
    }

    #[gpui::test]
    fn pinyin_session_over_committed_chinese_stays_in_bounds(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| Editor::wrapped_single_line(cx));
        let (_root, cx) = cx.add_window_view(|_window, _cx| Empty);

        // Pinyin/Chinese typing, as the user does in the composer.
        pinyin_commit(&editor, cx, "nihao", "你好");
        appkit_insert_text(&editor, cx, " ", None);
        pinyin_commit(&editor, cx, "shi", "是");
        appkit_insert_text(&editor, cx, " ", None);
        pinyin_commit(&editor, cx, "zhongguo", "中国");

        // AppKit unmarks the composition and then delivers the next keystroke
        // with `replacementRange = NSNotFound` (plain ASCII insert).
        appkit_unmark_text(&editor, cx);
        assert_ranges_in_bounds(&editor, cx, "after unmarkText");
        appkit_insert_text(&editor, cx, "!", None);
        assert_ranges_in_bounds(&editor, cx, "after ascii insert");

        let (content, selected, marked) = editor_state(&editor, cx);
        assert_eq!(content, "你好 是 中国!");
        assert_eq!(selected, content.len()..content.len());
        assert_eq!(marked, None);
    }

    /// A drag across part of the content selects exactly those characters, and
    /// ⌘C puts them on the clipboard. Reported broken on the real app; this is
    /// the interaction pinned down so the fix is a matter of record.
    #[gpui::test]
    fn mouse_drag_selects_text_and_cmd_c_copies_it(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| {
            let mut editor = Editor::new(cx);
            editor.set_text("hello brave world", cx);
            editor
        });
        struct EditorRoot(Entity<Editor>);
        impl Render for EditorRoot {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                div().w(px(320.0)).h(px(60.0)).child(self.0.clone())
            }
        }
        let (root, cx) = cx.add_window_view(|_window, _cx| EditorRoot(editor.clone()));
        // The tests bypass the app entry, so the editor's key bindings have to
        // be registered for ⌘C to reach the Copy action.
        cx.update(|_window, cx| bind_editor_keys(cx));
        // A couple of frames settle the layout before positions are probed.
        for _ in 0..3 {
            cx.update(|window, _cx| window.refresh());
            cx.run_until_parked();
        }

        let hitbox: &'static str = Box::leak(
            editor
                .read_with(cx, |editor, _cx| editor.hitbox_id.clone())
                .to_string()
                .into_boxed_str(),
        );
        let bounds = cx
            .debug_bounds(&hitbox)
            .unwrap_or_else(|| panic!("{hitbox} was not laid out"));
        let modifiers = gpui::Modifiers::default();

        // Pixel positions of two byte offsets, from the shaped lines.
        let (start, end) = editor.read_with(cx, |editor, _cx| {
            let entry = &editor.lines[0];
            (
                entry.line.position_for_index(6, px(row_height())).unwrap(),
                entry.line.position_for_index(11, px(row_height())).unwrap(),
            )
        });
        let at = |local: gpui::Point<Pixels>| {
            gpui::point(
                bounds.origin.x + local.x,
                bounds.origin.y + local.y + px(row_height() / 2.0),
            )
        };

        // Drag over "brave" (bytes 6..11).
        cx.simulate_mouse_down(at(start), MouseButton::Left, modifiers);
        cx.simulate_mouse_move(at(end), Some(MouseButton::Left), modifiers);
        cx.simulate_mouse_up(at(end), MouseButton::Left, modifiers);
        cx.run_until_parked();

        let (content, selected, _) = editor_state(&editor, cx);
        assert_eq!(
            &content[selected.clone()],
            "brave",
            "drag did not select 'brave'"
        );

        cx.simulate_keystrokes("cmd-c");
        cx.run_until_parked();
        let copied = cx.update(|_window, cx| {
            cx.read_from_clipboard()
                .and_then(|item| item.text().map(|text| text.to_string()))
        });
        assert_eq!(copied.as_deref(), Some("brave"));

        let _ = root;
    }

    /// A drag that starts on one wrapped row and ends on the next must select
    /// only the span between the two points. Reported broken: crossing rows
    /// selected all the way to the end of the content.
    #[gpui::test]
    fn mouse_drag_across_wrapped_rows_selects_only_the_span(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| {
            let mut editor = Editor::new(cx);
            // Long enough to wrap at 320px: "brave" is on the first wrapped
            // row, and a later byte lands on the second.
            editor.set_text("hello brave world and hello brave world again", cx);
            editor
        });
        struct EditorRoot(Entity<Editor>);
        impl Render for EditorRoot {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                div().w(px(320.0)).h(px(80.0)).child(self.0.clone())
            }
        }
        let (root, cx) = cx.add_window_view(|_window, _cx| EditorRoot(editor.clone()));
        for _ in 0..3 {
            cx.update(|window, _cx| window.refresh());
            cx.run_until_parked();
        }

        let hitbox: &'static str = Box::leak(
            editor
                .read_with(cx, |editor, _cx| editor.hitbox_id.clone())
                .to_string()
                .into_boxed_str(),
        );
        let bounds = cx
            .debug_bounds(&hitbox)
            .unwrap_or_else(|| panic!("{hitbox} was not laid out"));
        let modifiers = gpui::Modifiers::default();

        // One probe point on the first wrapped row, and the first byte of the
        // row after it, taken from the shaped layout.
        let (start, end) = editor.read_with(cx, |editor, _cx| {
            let line_height = px(row_height());
            let entry = &editor.lines[0];
            let mut first = None;
            let mut second = None;
            for byte in 0..entry.line.len() {
                if let Some(point) = entry.line.position_for_index(byte, line_height) {
                    if point.y < line_height {
                        if first.is_none() {
                            first = Some((byte, point));
                        }
                    } else if second.is_none() {
                        second = Some((byte, point));
                        break;
                    }
                }
            }
            (
                first.expect("first row had no probe point"),
                second.expect("the content did not wrap onto a second row"),
            )
        });
        let at = |local: gpui::Point<Pixels>| {
            gpui::point(
                bounds.origin.x + local.x,
                bounds.origin.y + local.y + px(row_height() / 2.0),
            )
        };
        println!("bounds={bounds:?} start={start:?} end={end:?}");
        println!(
            "wrap_boundaries={:?}",
            editor.read_with(cx, |e, _cx| { e.lines[0].line.wrap_boundaries().len() })
        );

        cx.simulate_mouse_down(at(start.1), MouseButton::Left, modifiers);
        cx.simulate_mouse_move(at(end.1), Some(MouseButton::Left), modifiers);
        cx.simulate_mouse_up(at(end.1), MouseButton::Left, modifiers);
        cx.run_until_parked();

        let (content, selected, _) = editor_state(&editor, cx);
        assert_eq!(selected.start, start.0, "anchor moved");
        assert_eq!(selected.end, end.0, "the drag ran past the second row");
        assert!(
            selected.end < content.len(),
            "crossing rows selected to the end of the content"
        );

        let _ = root;
    }

    /// Pasting more rows than the box holds used to paint them straight past
    /// the editor, over whatever sat below. Now the visible window follows the
    /// caret: with the caret at the end of a twelve-row paste, the first rows
    /// are scrolled off and the caret's row is on screen.
    #[gpui::test]
    fn content_taller_than_the_box_follows_the_caret(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| {
            let mut editor = Editor::new(cx);
            let content: String = (0..12).map(|row| format!("line {row}\n")).collect();
            editor.set_text(content, cx);
            editor
        });
        struct EditorRoot(Entity<Editor>);
        impl Render for EditorRoot {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                div().w(px(320.0)).h(px(180.0)).child(self.0.clone())
            }
        }
        let (_root, cx) = cx.add_window_view(|_window, _cx| EditorRoot(editor.clone()));
        for _ in 0..3 {
            cx.update(|window, _cx| window.refresh());
            cx.run_until_parked();
        }

        let scroll = editor.read_with(cx, |editor, _cx| editor.scroll_row);
        assert!(
            scroll > 0,
            "the caret at the end of the paste was not scrolled into view"
        );
        assert!(scroll < 12, "the scroll window ran past the content");
    }

    /// Once the editor is scrolled, its first visible row owns the top of the
    /// hitbox. Previously `saturating_sub` stacked every hidden row there, so
    /// clicking visible line 5 put the caret in line 1.
    #[gpui::test]
    fn a_click_after_scrolling_hits_the_visible_row(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| {
            let mut editor = Editor::new(cx);
            editor.set_text(
                (0..14)
                    .map(|row| format!("line {row}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                cx,
            );
            editor
        });
        struct EditorRoot(Entity<Editor>);
        impl Render for EditorRoot {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                div().w(px(320.0)).h(px(300.0)).child(self.0.clone())
            }
        }
        let (_root, cx) = cx.add_window_view(|_window, _cx| EditorRoot(editor.clone()));
        for _ in 0..3 {
            cx.update(|window, _cx| window.refresh());
            cx.run_until_parked();
        }
        let hitbox: &'static str = Box::leak(
            editor
                .read_with(cx, |editor, _cx| editor.hitbox_id.clone())
                .to_string()
                .into_boxed_str(),
        );
        let bounds = cx.debug_bounds(hitbox).expect("editor hitbox");
        let (first_visible_byte, scroll_row) = editor.read_with(cx, |editor, _cx| {
            let row = editor.scroll_row;
            (editor.lines[row].byte_start, row)
        });
        assert!(scroll_row > 0, "fixture did not scroll");

        let position = point(
            bounds.left() + px(2.0),
            bounds.top() + px(row_height() / 2.0),
        );
        cx.simulate_mouse_down(position, MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_up(position, MouseButton::Left, gpui::Modifiers::default());
        cx.run_until_parked();

        let selected = editor.read_with(cx, |editor, _cx| editor.selected_range.clone());
        assert!(
            selected.start >= first_visible_byte,
            "top visible row starts at {first_visible_byte}, click landed at {}",
            selected.start
        );
    }

    /// Wheel/thumb scrolling is a deliberate viewport move and must survive
    /// the next paint instead of snapping straight back to the caret.
    #[gpui::test]
    fn dragging_the_vertical_thumb_keeps_the_new_scroll_position(cx: &mut TestAppContext) {
        let editor = cx.new(|cx| {
            let mut editor = Editor::new(cx);
            editor.set_text(
                (0..20)
                    .map(|row| format!("line {row}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                cx,
            );
            editor
        });
        struct EditorRoot(Entity<Editor>);
        impl Render for EditorRoot {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                div().w(px(320.0)).h(px(300.0)).child(self.0.clone())
            }
        }
        let (_root, cx) = cx.add_window_view(|_window, _cx| EditorRoot(editor.clone()));
        for _ in 0..3 {
            cx.update(|window, _cx| window.refresh());
            cx.run_until_parked();
        }
        let hitbox: &'static str = Box::leak(
            editor
                .read_with(cx, |editor, _cx| editor.hitbox_id.clone())
                .to_string()
                .into_boxed_str(),
        );
        let bounds = cx.debug_bounds(hitbox).expect("editor hitbox");
        editor.update(cx, |editor, cx| {
            editor.selected_range = 0..0;
            editor.scroll_row = 0;
            editor.follow_cursor = false;
            cx.notify();
        });
        cx.run_until_parked();

        let start = point(bounds.right() - px(2.0), bounds.top() + px(4.0));
        let end = point(bounds.right() - px(2.0), bounds.bottom() - px(4.0));
        cx.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_move(end, Some(MouseButton::Left), gpui::Modifiers::default());
        cx.simulate_mouse_up(end, MouseButton::Left, gpui::Modifiers::default());
        cx.run_until_parked();

        editor.read_with(cx, |editor, _cx| {
            assert!(editor.scroll_row > 0, "thumb drag did not scroll");
            assert!(!editor.follow_cursor, "paint re-enabled caret following");
            assert!(editor.scrollbar_grab.is_none(), "thumb stayed grabbed");
        });
    }
}

impl Focusable for Editor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<EditorEvent> for Editor {}

/// Row height the editor was laid out at, before the text-size preference is
/// applied. The font is drawn at 13px against a 20px row, and the caret and
/// selection quads are positioned from this same number, so the two have to
/// scale together or the caret drifts off the glyphs.
pub const BASE_ROW_HEIGHT: f32 = 20.0;

fn row_height() -> f32 {
    BASE_ROW_HEIGHT * Theme::font_scale()
}

impl EntityInputHandler for Editor {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.clamp_range(self.range_from_utf16(&range_utf16));
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }
    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replace_text_in_range_internal(range_utf16, new_text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.clamp_range(
            range_utf16
                .as_ref()
                .map(|range| self.range_from_utf16(range))
                .or_else(|| self.marked_range.clone())
                .unwrap_or_else(|| self.selected_range.clone()),
        );
        self.content = format!(
            "{}{new_text}{}",
            &self.content[..range.start],
            &self.content[range.end..]
        );
        let marked_end = range.start + new_text.len();
        if new_text.is_empty() {
            self.marked_range = None;
        } else {
            self.marked_range = Some(range.start..marked_end);
        }
        // AppKit's `selectedRange` is relative to the marked text it just
        // handed us, *not* to the document: measure it against `new_text` and
        // anchor it at the insertion point. Measuring it against the whole
        // content (as this used to) walks the cursor further past the end with
        // every utf8/utf16 length mismatch, which used to panic - and abort -
        // the app on later keystrokes.
        let selected_range = new_selected_range_utf16
            .map(|range_utf16| {
                let start = byte_offset_from_utf16(new_text, range_utf16.start);
                let end = byte_offset_from_utf16(new_text, range_utf16.end).max(start);
                range.start + start..range.start + end
            })
            .unwrap_or(marked_end..marked_end);
        self.selected_range = self.clamp_range(selected_range);
        cx.emit(EditorEvent::Change);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.clamp_range(self.range_from_utf16(&range_utf16));
        let row = self.row_for_byte(range.start)?;
        let entry = &self.lines[row];
        // `lines` may lag one frame behind `content`, so stay inside the line.
        let offset_in_line = range
            .start
            .saturating_sub(entry.byte_start)
            .min(entry.line.len());
        let local = entry
            .line
            .position_for_index(offset_in_line, px(row_height()))?;
        let top = bounds.top() + px(self.rows_above(row) as f32 * row_height());
        Some(Bounds::new(
            point(bounds.left() + local.x, top),
            size(px(2.0), px(row_height())),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.clamp(self.index_for_mouse_position(point)))
    }
}

impl Editor {
    fn row_for_byte(&self, byte: usize) -> Option<usize> {
        self.lines
            .iter()
            .rposition(|entry| entry.byte_start <= byte)
    }
}

/// The painted element: shapes + paints the wrapped text, cursor and selection.
struct EditorElement {
    entity: Entity<Editor>,
}

struct PrepaintState {
    lines: Vec<LineEntry>,
    content_size: Size<Pixels>,
    cursor: Option<PaintQuad>,
    selection: Vec<PaintQuad>,
    /// First visible wrapped row: rows before it paint above the box, where
    /// the content mask clips them.
    scroll_row: usize,
    /// The thumb of the editor's own scrollbar, if the content scrolls.
    thumb: Option<PaintQuad>,
}

impl IntoElement for EditorElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for EditorElement {
    type RequestLayoutState = Rc<RefCell<Option<RequestedShape>>>;
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        // The height comes from shaping at the real width: a rough guess from
        // character counts is how a wrapped second row ended up painted below
        // a one-row-tall box, outside its own hitbox — clicks and drags on
        // that row then went nowhere, and a drag that crossed a row seemed to
        // select the wrong span.
        let (content, placeholder, single_line, wrap_long_lines, cached_key, cached_rows) = {
            let editor = self.entity.read(cx);
            (
                editor.content.clone(),
                editor.placeholder.clone(),
                editor.single_line,
                editor.wrap_long_lines,
                editor.shape_key.clone(),
                editor.total_rows(),
            )
        };
        let text_style = window.text_style();
        let font = text_style.font();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let color = if content.is_empty() {
            hsla(0., 0., 0.5, 0.45)
        } else {
            text_style.color
        };
        let display_text: SharedString = if content.is_empty() {
            placeholder
        } else {
            content.clone().into()
        };
        let line_height = px(row_height());
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let requested_shape: Rc<RefCell<Option<RequestedShape>>> = Rc::new(RefCell::new(None));

        let layout_id = window.request_measured_layout(style, {
            let requested_shape = requested_shape.clone();
            move |known, available, window, _cx| {
                let width = known.width.or(match available.width {
                    gpui::AvailableSpace::Definite(width) => Some(width),
                    _ => None,
                });
                let wrap_width = if single_line && !wrap_long_lines {
                    None
                } else {
                    width
                };
                let key = EditorShapeKey {
                    text: display_text.clone(),
                    wrap_width,
                    font: font.clone(),
                    font_size,
                    color,
                };
                let rows = if cached_key.as_ref() == Some(&key) && cached_rows > 0 {
                    cached_rows
                } else {
                    let wrapped = shape_editor_text(window, &key);
                    let rows = wrapped
                        .iter()
                        .map(|line| line.wrap_boundaries().len() + 1)
                        .sum::<usize>();
                    *requested_shape.borrow_mut() = Some(RequestedShape { key, wrapped });
                    rows
                };
                gpui::Size::new(
                    known.width.unwrap_or(px(0.0)),
                    line_height * rows.clamp(1, 8) as f32,
                )
            }
        });
        (layout_id, requested_shape)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let (
            content,
            placeholder,
            selected_range,
            cursor,
            scroll_row,
            single_line,
            wrap_long_lines,
            cached_key,
            cached_lines,
        ) = {
            let editor = self.entity.read(cx);
            (
                editor.content.clone(),
                editor.placeholder.clone(),
                editor.selected_range.clone(),
                editor.cursor_offset(),
                editor.scroll_row,
                editor.single_line,
                editor.wrap_long_lines,
                editor.shape_key.clone(),
                editor.lines.clone(),
            )
        };
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let display_text: SharedString = if content.is_empty() {
            placeholder
        } else {
            content.clone().into()
        };
        let key = EditorShapeKey {
            text: display_text,
            wrap_width: (!(single_line && !wrap_long_lines)).then_some(bounds.size.width),
            font: style.font(),
            font_size,
            color: if content.is_empty() {
                hsla(0., 0., 0.5, 0.45)
            } else {
                style.color
            },
        };
        let lines = if cached_key.as_ref() == Some(&key) && !cached_lines.is_empty() {
            cached_lines
        } else if let Some(requested) = request_layout
            .borrow()
            .as_ref()
            .filter(|requested| requested.key == key)
        {
            line_entries(&content, requested.wrapped.clone())
        } else {
            line_entries(&content, shape_editor_text(window, &key))
        };

        // Cursor + selection.
        let mut cursor_quad = None;
        let mut selection_quads = Vec::new();
        let is_focused = self.entity.read(cx).focus_handle.is_focused(window);
        let row_for = |byte: usize, lines: &[LineEntry]| -> Option<usize> {
            lines.iter().rposition(|entry| entry.byte_start <= byte)
        };
        if selected_range.is_empty() {
            if let (true, Some(row)) = (is_focused, row_for(cursor, &lines)) {
                let entry = &lines[row];
                let rows_above: usize = lines[..row]
                    .iter()
                    .map(|entry| entry.line.wrap_boundaries().len() + 1)
                    .sum();
                if let Some(local) = entry
                    .line
                    .position_for_index(cursor - entry.byte_start, px(row_height()))
                {
                    let y = bounds.top()
                        + px((rows_above as f32 - scroll_row as f32) * row_height())
                        + local.y;
                    cursor_quad = Some(fill(
                        Bounds::new(
                            point(bounds.left() + local.x, y),
                            size(px(2.0), px(row_height())),
                        ),
                        gpui::blue(),
                    ));
                }
            }
        } else {
            // One quad per wrapped row the selection covers: a selection
            // that crosses a row boundary used to paint the whole logical
            // line as one block, which read as "everything got selected".
            //
            // The highlight keeps painting after the editor lost focus, in a
            // dimmed color — a retained selection must not read as gone just
            // because a message was clicked in the meantime.
            let selection_color = if is_focused {
                Theme::selection()
            } else {
                Theme::selection_unfocused()
            };
            let start = selected_range.start.min(selected_range.end);
            let end = selected_range.start.max(selected_range.end);
            let mut rows_above = 0usize;
            for entry in &lines {
                let row_ends: Vec<usize> = entry
                    .line
                    .wrap_boundaries()
                    .iter()
                    .map(|boundary| {
                        entry.line.unwrapped_layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix]
                            .index
                    })
                    .chain([entry.line.len()])
                    .collect();
                let mut row_start = 0usize;
                for (visual_row, row_end) in row_ends.iter().enumerate() {
                    let intersect_start = start.max(entry.byte_start + row_start);
                    let intersect_end = end.min(entry.byte_start + row_end);
                    if intersect_start < intersect_end {
                        // `position_for_index` assigns a wrap boundary to the
                        // preceding row. Computing x from the unwrapped line
                        // and subtracting this visual row's origin avoids a
                        // second-row selection starting at the previous row's
                        // right edge.
                        let local_start = intersect_start - entry.byte_start;
                        let local_end = intersect_end - entry.byte_start;
                        let row_origin = entry.line.unwrapped_layout.x_for_index(row_start);
                        let from_x =
                            entry.line.unwrapped_layout.x_for_index(local_start) - row_origin;
                        let to_x = entry.line.unwrapped_layout.x_for_index(local_end) - row_origin;
                        let visible_row = rows_above + visual_row;
                        let y = bounds.top()
                            + px((visible_row as f32 - scroll_row as f32) * row_height());
                        selection_quads.push(fill(
                            Bounds::new(
                                point(bounds.left() + from_x, y),
                                size((to_x - from_x).max(px(2.0)), px(row_height())),
                            ),
                            selection_color,
                        ));
                    }
                    row_start = *row_end;
                }
                rows_above += row_ends.len();
            }
        }

        let content_rows: usize = lines
            .iter()
            .map(|entry| entry.line.wrap_boundaries().len() + 1)
            .sum();
        let content_size = size(
            bounds.size.width,
            px(row_height() * content_rows.max(1) as f32),
        );

        // A thin thumb on the right edge whenever the content outgrows the
        // box, positioned from the same rows the text is painted from.
        let thumb_quad = if content_rows > 8 {
            let track = bounds.size.height;
            let thumb_height = (track * 8.0 / content_rows.max(1) as f32)
                .max(px(24.0))
                .min(track);
            let travel = (track - thumb_height).max(px(0.0));
            let thumb_top =
                bounds.top() + travel * (scroll_row as f32 / (content_rows - 8).max(1) as f32);
            Some(fill(
                Bounds::new(
                    point(bounds.right() - px(7.0), thumb_top),
                    size(px(4.0), thumb_height),
                ),
                Theme::scrollbar_thumb(),
            ))
        } else {
            None
        };

        // Store the freshly shaped rows now (paint used to do it later), so
        // the scroll window is computed against rows that exist, and the
        // caret stays in view after every reshape.
        self.entity.update(cx, |editor, cx| {
            editor.lines = lines.clone();
            editor.shape_key = Some(key);
            if editor.follow_cursor {
                editor.ensure_cursor_visible(cx);
                editor.follow_cursor = false;
            }
        });

        PrepaintState {
            lines,
            content_size,
            cursor: cursor_quad,
            selection: selection_quads,
            scroll_row,
            thumb: thumb_quad,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.entity.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.entity.clone()),
            cx,
        );

        // Everything is painted at its content position minus the scrolled
        // rows, then clipped to the box: content taller than the editor no
        // longer paints past the box and over whatever sits below it.
        let content_mask = gpui::ContentMask { bounds };
        window.with_content_mask(Some(content_mask), |window| {
            for quad in prepaint.selection.drain(..) {
                window.paint_quad(quad);
            }

            let line_height = px(row_height());
            let mut rows_above = 0usize;
            for entry in &prepaint.lines {
                let origin = point(
                    bounds.left(),
                    bounds.top()
                        + px((rows_above as f32 - prepaint.scroll_row as f32) * row_height()),
                );
                let _ =
                    entry
                        .line
                        .paint(origin, line_height, gpui::TextAlign::Left, None, window, cx);
                rows_above += entry.line.wrap_boundaries().len() + 1;
            }

            if let Some(thumb) = prepaint.thumb.take() {
                window.paint_quad(thumb);
            }

            if focus_handle.is_focused(window) {
                if let Some(cursor) = prepaint.cursor.take() {
                    window.paint_quad(cursor);
                }
            }
        });

        let _ = std::mem::take(&mut prepaint.lines);
        let content_size = prepaint.content_size;
        self.entity.update(cx, |editor, _cx| {
            editor.last_bounds = Some(bounds);
        });
        let _ = content_size;
    }
}

impl Render for Editor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let single_line = self.single_line;
        let wrap_long_lines = self.wrap_long_lines;
        let hitbox_id = self.hitbox_id.clone();
        div()
            .id(self.hitbox_id.clone())
            .debug_selector(move || hitbox_id.to_string())
            .flex()
            .key_context("Editor")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::submit))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .text_size(Theme::text_px(13.0))
            .line_height(px(row_height()))
            .child(
                div()
                    .id(self.element_id.clone())
                    .w_full()
                    .when(single_line && !wrap_long_lines, |this| {
                        this.overflow_x_scroll().whitespace_nowrap()
                    })
                    .child(EditorElement { entity }),
            )
    }
}

impl Editor {
    fn newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        self.insert_newline(cx);
    }
}
