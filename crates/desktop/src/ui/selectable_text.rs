use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    fill, point, px, size, App, Bounds, CursorStyle, Element, ElementId, GlobalElementId,
    HighlightStyle, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PaintQuad, Pixels, SharedString, TextAlign, TextRun, TextStyle, Window,
    WrappedLine,
};

use crate::ui::theme::Theme;

/// Where a [`SelectableText`] keeps its selection.
///
/// Implemented on the transcript view: one selection is active at a time, and
/// clicking another block replaces it instead of leaving two highlights.
pub trait SelectionHost {
    /// The active selection's key, byte range and rendered text, if any.
    fn selection(&self, cx: &App) -> Option<(u64, Range<usize>, String)>;
    /// Replace the active selection with this one and request a redraw.
    fn set_selection(&self, key: u64, range: Range<usize>, text: String, cx: &mut App);
    /// Drop the active selection and request a redraw.
    fn clear_selection(&self, cx: &mut App);
    /// Take focus, so a following ⌘C reaches the transcript's copy action
    /// instead of the composer's.
    fn focus_transcript(&self, window: &mut Window, cx: &mut App);
    /// Right-click on a selection: open the transcript's context menu for it.
    /// `position` is in window coordinates.
    fn open_context_menu(&self, position: gpui::Point<Pixels>, text: String, cx: &mut App);
}

/// A point inside the first text row, for tests that probe positions.
#[cfg(test)]
const ROW_HEIGHT: f32 = 20.0;

#[cfg(test)]
fn row_height() -> Pixels {
    px(ROW_HEIGHT)
}

/// One text block the user can select with the mouse: a paragraph, a list
/// item, a quote, a table cell or a code block.
///
/// It paints what a plain `StyledText` paints — the runs and their highlights —
/// plus the selection underneath when this block holds the transcript's active
/// selection. Without a host it paints the text and nothing more, which is the
/// plain renderer's behaviour, so the same block type serves both uses.
pub struct SelectableText {
    id: ElementId,
    /// Selection identity: a block cannot hold another block's selection.
    /// Stable across frames so a repaint keeps the highlight.
    key: u64,
    text: SharedString,
    /// Styling per byte range, resolved against the surrounding text style at
    /// layout time — the same delayed form `StyledText` takes its highlights in.
    /// Ranges are disjoint and sorted; build from `combine_highlights` output.
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    /// `(byte range in `text`, destination)` pairs, disjoint and sorted.
    links: Vec<(Range<usize>, String)>,
    /// Long lines wrap at the block's width; a code block scrolls sideways
    /// instead, which is what not wrapping means here.
    wrap: bool,
    host: Option<Rc<dyn SelectionHost>>,
    /// Shaped once per frame at layout time, when the width is known; prepaint
    /// and the paint pass reuse it instead of shaping a second time.
    shaped: Rc<RefCell<Option<ShapedText>>>,
}

/// The shaped lines and the size they were measured at, shared between the
/// layout measurement and the paint pass of one frame.
struct ShapedText {
    lines: Vec<WrappedLine>,
    size: gpui::Size<Pixels>,
    wrap_width: Pixels,
    line_height: Pixels,
}

/// One drag, held across frames under the element's id.
#[derive(Default, Clone)]
struct Drag {
    /// True between mouse-down and mouse-up on this block.
    pending: Rc<Cell<bool>>,
    /// The character the mouse went down on. A click — down and up on the same
    /// character — opens a link; a drag is a selection. This is the
    /// distinction, and it is what keeps links clickable.
    anchor: Rc<Cell<Option<usize>>>,
}

impl SelectableText {
    pub fn new(
        id: impl Into<ElementId>,
        key: u64,
        text: impl Into<SharedString>,
        highlights: Vec<(Range<usize>, HighlightStyle)>,
        links: Vec<(Range<usize>, String)>,
        host: Option<Rc<dyn SelectionHost>>,
    ) -> Self {
        Self {
            id: id.into(),
            key,
            text: text.into(),
            highlights,
            links,
            wrap: true,
            host,
            shaped: Rc::new(RefCell::new(None)),
        }
    }

    /// A block whose long lines scroll horizontally instead of wrapping — a
    /// code block inside `overflow_x_scroll`.
    pub fn without_wrap(mut self) -> Self {
        self.wrap = false;
        self
    }
}

impl IntoElement for SelectableText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SelectableText {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        _cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        // Shape at layout time, when the width is known, and report the real
        // wrapped height — a height guessed from line counts is how wrapped
        // paragraphs ended up painted over the block below them.
        let text_style = window.text_style();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let line_height = text_style
            .line_height
            .to_pixels(font_size.into(), window.rem_size());
        let runs = compute_runs(&self.text, &text_style, &self.highlights);
        let text = self.text.clone();
        let wrap = self.wrap;
        let shaped = self.shaped.clone();

        let layout_id = window.request_measured_layout(Default::default(), {
            move |known, available, window, _cx| {
                let wrap_width = if wrap {
                    known.width.or(match available.width {
                        gpui::AvailableSpace::Definite(width) => Some(width),
                        _ => None,
                    })
                } else {
                    None
                };
                if let Some(shaped) = shaped.borrow().as_ref() {
                    if shaped.line_height == line_height
                        && shaped.wrap_width == wrap_width.unwrap_or(px(0.0))
                    {
                        return shaped.size;
                    }
                }
                let lines = window
                    .text_system()
                    .shape_text(text.clone(), font_size, &runs, wrap_width, None)
                    .unwrap_or_default();
                let mut size: gpui::Size<Pixels> = gpui::Size::default();
                for line in &lines {
                    let line_size = line.size(line_height);
                    size.height += line_size.height;
                    size.width = size.width.max(line_size.width).ceil();
                }
                *shaped.borrow_mut() = Some(ShapedText {
                    lines: lines.to_vec(),
                    size,
                    wrap_width: wrap_width.unwrap_or(px(0.0)),
                    line_height,
                });
                size
            }
        });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let hitbox = window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal);
        PrepaintState {
            shaped: self.shaped.clone(),
            hitbox,
        }
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let shaped = prepaint.shaped.borrow();
        let shaped = shaped
            .as_ref()
            .expect("paint ran before the layout measurement");
        let lines = shaped.lines.clone();
        let line_height = shaped.line_height;

        let Some(host) = self.host.clone() else {
            paint_lines(&lines, bounds, line_height, window, cx);
            return;
        };

        // The highlight underneath the text has to be painted first, and this
        // element's text is painted in this same paint pass: the selection
        // quads therefore go down before the lines.
        let selection = host.selection(cx).filter(|(key, _, _)| *key == self.key);
        if let Some((_, range, _)) = &selection {
            for quad in selection_quads(&self.text, &lines, range.clone(), bounds, line_height) {
                window.paint_quad(quad);
            }
        }
        paint_lines(&lines, bounds, line_height, window, cx);
        window.set_cursor_style(CursorStyle::IBeam, &prepaint.hitbox);

        window.with_element_state::<Drag, _>(global_id.unwrap(), |state, window| {
            let drag = state.unwrap_or_default();
            let drag_down = drag.clone();
            // The three listeners share one shaping pass; Rc keeps the captured
            // line layouts cheap to clone into each closure.
            let text = self.text.clone();
            let lines = Rc::new(lines);
            let key = self.key;
            let links = Rc::new(self.links.clone());
            let drag_move = drag.clone();
            let drag_up = drag.clone();
            let hitbox = prepaint.hitbox.clone();

            window.on_mouse_event({
                let text = text.clone();
                let lines = lines.clone();
                let host = host.clone();
                move |event: &MouseDownEvent, phase, window, cx| {
                    if event.button == MouseButton::Right {
                        // The menu is for the selection: a right-click over a
                        // block with an active selection of its own opens the
                        // copy / quote menu at the pointer.
                        if phase.bubble() && hitbox.is_hovered(window) {
                            if let Some((selection_key, _, text)) = host.selection(cx) {
                                if selection_key == key {
                                    host.open_context_menu(event.position, text, cx);
                                    window.prevent_default();
                                }
                            }
                        }
                        return;
                    }
                    if event.button != MouseButton::Left {
                        return;
                    }
                    if phase.bubble() && hitbox.is_hovered(window) {
                        let index = index_for_position(&text, &lines, event.position, line_height);
                        drag_down.pending.set(true);
                        drag_down.anchor.set(Some(index));
                        let range = match event.click_count {
                            2 => word_range_around(&text, index),
                            _ => index..index,
                        };
                        host.set_selection(
                            key,
                            range.clone(),
                            text.get(range.clone()).unwrap_or("").to_string(),
                            cx,
                        );
                        host.focus_transcript(window, cx);
                        window.prevent_default();
                    } else if phase.capture() {
                        drag_down.anchor.set(None);
                        host.clear_selection(cx);
                    }
                }
            });

            window.on_mouse_event({
                let text = text.clone();
                let lines = lines.clone();
                let host = host.clone();
                move |event: &MouseMoveEvent, phase, _window, cx| {
                    if phase.capture() || !drag_move.pending.get() {
                        return;
                    }
                    let Some(anchor) = drag_move.anchor.get() else {
                        return;
                    };
                    let head = index_for_position(&text, &lines, event.position, line_height);
                    let range = anchored_range(anchor, head);
                    host.set_selection(
                        key,
                        range.clone(),
                        text.get(range.clone()).unwrap_or("").to_string(),
                        cx,
                    );
                }
            });

            window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, cx| {
                if !phase.bubble() {
                    return;
                }
                drag_up.pending.set(false);
                let Some(anchor) = drag_up.anchor.get() else {
                    return;
                };
                drag_up.anchor.set(None);
                // Pressing and releasing on one character of a link opens it;
                // a drag that moved is a selection, and does not.
                let head = index_for_position(&text, &lines, event.position, line_height);
                if anchor == head {
                    for (range, url) in links.iter() {
                        if range.contains(&head) {
                            cx.open_url(url);
                            break;
                        }
                    }
                }
            });
            ((), drag)
        });
    }
}

pub struct PrepaintState {
    shaped: Rc<RefCell<Option<ShapedText>>>,
    hitbox: gpui::Hitbox,
}

fn paint_lines(
    lines: &[WrappedLine],
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    let mut rows_above = 0usize;
    for line in lines {
        let origin = point(
            bounds.left(),
            bounds.top() + line_height * rows_above as f32,
        );
        line.paint(origin, line_height, TextAlign::Left, None, window, cx)
            .ok();
        rows_above += line.wrap_boundaries().len() + 1;
    }
}

/// Byte index of a window position inside `lines`: the wrapped row the pointer
/// is in decides which logical line owns the position, and the closest
/// character boundary wins.
fn index_for_position(
    text: &str,
    lines: &[WrappedLine],
    position: gpui::Point<Pixels>,
    line_height: Pixels,
) -> usize {
    let clamp = |offset: usize| {
        let mut offset = offset.min(text.len());
        while offset > 0 && !text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    };
    if position.y < px(0.0) {
        return 0;
    }

    let mut byte_start = 0usize;
    let mut rows_above = 0usize;
    for line in lines {
        let rows = line.wrap_boundaries().len() + 1;
        let top = line_height * rows_above as f32;
        let bottom = top + line_height * rows as f32;
        if position.y >= top && position.y <= bottom {
            let local = point(position.x, position.y - top);
            let offset = line
                .closest_index_for_position(local, line_height)
                .unwrap_or_else(|closest| closest);
            return clamp(byte_start + offset);
        }
        rows_above += rows;
        // Advance past this line's text and the newline that follows it.
        let remainder = &text[byte_start.min(text.len())..];
        match remainder.find('\n') {
            Some(newline) => byte_start += newline + 1,
            None => byte_start += remainder.len(),
        }
    }
    text.len()
}

/// The byte range a selection covers, from its two ends in either order.
fn anchored_range(anchor: usize, head: usize) -> Range<usize> {
    if head < anchor {
        head..anchor
    } else {
        anchor..head
    }
}

/// The word around `index`, where a word is a run of characters that are not
/// whitespace. Double-click semantics, kept simple on purpose.
fn word_range_around(text: &str, index: usize) -> Range<usize> {
    let index = index.min(text.len());
    let start = text[..index]
        .char_indices()
        .rev()
        .take_while(|(_, character)| !character.is_whitespace())
        .map(|(offset, _)| offset)
        .last()
        .unwrap_or(0);
    let end = text[index..]
        .char_indices()
        .find(|(_, character)| character.is_whitespace())
        .map(|(offset, _)| index + offset)
        .unwrap_or(text.len());
    start..end.max(start)
}

/// One highlight quad per covered row: exactly what is selected when it fits
/// on one wrapped row, otherwise a full-width highlight for the block — the
/// same trade the editor's selection makes.
fn selection_quads(
    text: &str,
    lines: &[WrappedLine],
    range: Range<usize>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
) -> Vec<PaintQuad> {
    let mut quads = Vec::new();
    let start = range.start.min(range.end);
    let end = range.start.max(range.end);
    if start >= end {
        return quads;
    }

    let mut byte_start = 0usize;
    let mut rows_above = 0usize;
    for line in lines {
        let line_end = byte_start + line.len();
        let intersect_start = start.max(byte_start);
        let intersect_end = end.min(line_end);
        if intersect_start < intersect_end {
            let rows = line.wrap_boundaries().len() + 1;
            let start_local = line.position_for_index(intersect_start - byte_start, line_height);
            let end_local = line.position_for_index(intersect_end - byte_start, line_height);
            if let (Some(start_local), Some(end_local)) = (start_local, end_local) {
                if (start_local.y - end_local.y).abs() < px(0.5) {
                    let y = bounds.top() + line_height * rows_above as f32 + start_local.y;
                    quads.push(highlight_quad(Bounds::new(
                        point(bounds.left() + start_local.x, y),
                        size((end_local.x - start_local.x).max(px(2.0)), line_height),
                    )));
                } else {
                    let y = bounds.top() + line_height * rows_above as f32;
                    quads.push(highlight_quad(Bounds::new(
                        point(bounds.left(), y),
                        size(bounds.size.width, line_height * rows as f32),
                    )));
                }
            }
        }
        rows_above += line.wrap_boundaries().len() + 1;
        // Advance past this line's text and the newline that follows it.
        let remainder = &text[byte_start.min(text.len())..];
        match remainder.find('\n') {
            Some(newline) => byte_start += newline + 1,
            None => byte_start += remainder.len(),
        }
    }
    quads
}

fn highlight_quad(bounds: Bounds<Pixels>) -> PaintQuad {
    fill(bounds, Theme::selection())
}

/// Fold disjoint, sorted `(range, highlight)` pairs into `TextRun`s, one per
/// style change, the way `StyledText` builds runs for its own default style.
fn compute_runs(
    text: &str,
    base: &TextStyle,
    highlights: &[(Range<usize>, HighlightStyle)],
) -> Vec<TextRun> {
    let mut runs = Vec::new();
    let mut index = 0usize;
    for (range, highlight) in highlights {
        if range.end <= index {
            continue;
        }
        if range.start > index {
            runs.push(base.clone().to_run(range.start - index));
        }
        let end = range.end.min(text.len());
        runs.push(
            base.clone()
                .highlight(*highlight)
                .to_run(end - range.start.max(index)),
        );
        index = end;
    }
    if index < text.len() {
        runs.push(base.clone().to_run(text.len() - index));
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{div, prelude::*, Context, Render, TestAppContext};
    use std::cell::RefCell;

    type StoredSelection = Option<(u64, Range<usize>, String)>;

    /// A stand-in for the transcript: records whatever a block stores through
    /// the host, so the selection layer is testable on its own.
    #[derive(Default)]
    struct FakeSink {
        stored: RefCell<StoredSelection>,
    }

    impl SelectionHost for FakeSink {
        fn selection(&self, _cx: &App) -> Option<(u64, Range<usize>, String)> {
            self.stored
                .borrow()
                .as_ref()
                .map(|(key, range, text)| (*key, range.clone(), text.clone()))
        }

        fn set_selection(&self, key: u64, range: Range<usize>, text: String, _cx: &mut App) {
            *self.stored.borrow_mut() = Some((key, range, text));
        }

        fn clear_selection(&self, _cx: &mut App) {
            *self.stored.borrow_mut() = None;
        }

        fn focus_transcript(&self, _window: &mut Window, _cx: &mut App) {}

        fn open_context_menu(&self, _position: gpui::Point<Pixels>, _text: String, _cx: &mut App) {}
    }

    struct Fixture {
        sink: Rc<FakeSink>,
        text: SharedString,
        width: f32,
    }

    impl Render for Fixture {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("selectable-probe")
                .debug_selector(|| "selectable-probe".into())
                .w(px(self.width))
                .child(
                    SelectableText::new(
                        "probe",
                        42,
                        self.text.clone(),
                        Vec::new(),
                        Vec::new(),
                        Some(self.sink.clone()),
                    )
                    .into_any(),
                )
        }
    }

    /// Dragging across part of a block selects those bytes: the host receives
    /// the range and the text it picks out.
    #[gpui::test]
    fn a_drag_within_a_block_stores_the_selection_on_the_host(cx: &mut TestAppContext) {
        let sink = Rc::new(FakeSink::default());
        let text = SharedString::from("hello brave world");
        let (root, cx) = cx.add_window_view(|_window, _cx| Fixture {
            sink: sink.clone(),
            text: text.clone(),
            width: 320.0,
        });
        for _ in 0..3 {
            cx.update(|window, _cx| window.refresh());
            cx.run_until_parked();
        }

        // Pixel positions of bytes 6 and 11, shaped the same way the element
        // shapes its own text.
        let (start, end) = root.update_in(cx, |_, window, _cx| {
            // Exactly the shaping the element itself does, so the positions
            // line up with its layout.
            let style = window.text_style();
            let font_size = style.font_size.to_pixels(window.rem_size());
            let run = style.to_run(17);
            let lines = window
                .text_system()
                .shape_text(text, font_size, &[run], Some(px(320.0)), None)
                .unwrap();
            (
                lines[0].position_for_index(6, px(ROW_HEIGHT)).unwrap(),
                lines[0].position_for_index(11, px(ROW_HEIGHT)).unwrap(),
            )
        });
        let at = |local: gpui::Point<Pixels>| {
            point(px(0.0) + local.x, px(0.0) + local.y + row_height() / 2.0)
        };

        cx.simulate_mouse_down(at(start), MouseButton::Left, gpui::Modifiers::default());
        cx.simulate_mouse_move(at(end), Some(MouseButton::Left), gpui::Modifiers::default());
        cx.simulate_mouse_up(at(end), MouseButton::Left, gpui::Modifiers::default());
        cx.run_until_parked();

        let stored = sink.stored.borrow().clone();
        let (key, range, text) = stored.expect("no selection was stored");
        assert_eq!(key, 42);
        assert_eq!(range, 6..11, "selection picked {:?}", range);
        assert_eq!(text, "brave");

        // A click elsewhere clears it.
        cx.simulate_mouse_down(
            gpui::point(px(300.0), px(50.0)),
            MouseButton::Left,
            gpui::Modifiers::default(),
        );
        cx.run_until_parked();
        assert!(
            sink.stored.borrow().is_none(),
            "selection survived a click outside the block"
        );
    }

    /// A block that wraps must report the height of every wrapped row. The
    /// height used to be guessed from the *logical* line count, so a paragraph
    /// that wrapped onto six rows claimed one and painted over the block below
    /// it — the transcript read as overlapping garbage.
    #[gpui::test]
    fn wrapped_blocks_measure_the_rows_they_need(cx: &mut TestAppContext) {
        let sink = Rc::new(FakeSink::default());
        let text = SharedString::from("一".repeat(240));
        let (_root, cx) = cx.add_window_view(|_window, _cx| Fixture {
            sink: sink.clone(),
            text,
            width: 220.0,
        });
        for _ in 0..3 {
            cx.update(|window, _cx| window.refresh());
            cx.run_until_parked();
        }
        let bounds = cx
            .debug_bounds("selectable-probe")
            .unwrap_or_else(|| panic!("probe was not laid out"));

        // 240 CJK characters in a 220px column need far more than two rows.
        assert!(
            f32::from(bounds.size.height) > 8.0 * 20.0,
            "wrapped block claimed only {}px of height",
            bounds.size.height
        );
    }
}
