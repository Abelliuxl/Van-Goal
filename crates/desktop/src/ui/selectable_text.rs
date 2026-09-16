use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    fill, point, px, App, Bounds, CursorStyle, Element, ElementId, GlobalElementId, HighlightStyle,
    Hitbox, HitboxBehavior, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, PaintQuad, Pixels, SharedString, StyledText, TextLayout, Window,
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

/// One text block the user can select with the mouse: a paragraph, a list
/// item, a quote, a table cell or a code block.
///
/// Layout and glyph painting are delegated to gpui's own [`StyledText`], so
/// this block measures and wraps exactly like the plain text it replaced; the
/// only additions are the selection highlight underneath and the mouse
/// handling. Without a host it paints the text and nothing more, which is the
/// plain renderer's behaviour.
pub struct SelectableText {
    id: ElementId,
    /// Selection identity: a block cannot hold another block's selection.
    /// Stable across frames so a repaint keeps the highlight.
    key: u64,
    text: SharedString,
    /// The styled text this block delegates to, carrying the same highlights
    /// a plain `StyledText` would take.
    styled: StyledText,
    /// `(byte range in `text`, destination)` pairs, sorted by range start.
    links: Vec<(Range<usize>, String)>,
    host: Option<Rc<dyn SelectionHost>>,
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
        let text = text.into();
        let styled = StyledText::new(text.clone()).with_highlights(highlights);
        Self {
            id: id.into(),
            key,
            text,
            styled,
            links,
            host,
        }
    }
}

impl IntoElement for SelectableText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SelectableText {
    type RequestLayoutState = <StyledText as Element>::RequestLayoutState;
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
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        // The inner StyledText owns measurement in every layout pass
        // (intrinsic, min-content, final) — rolling that by hand is how
        // wrapped heights and widths drifted from their boxes before.
        self.styled.request_layout(None, _inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        _global_id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.styled
            .prepaint(None, _inspector_id, bounds, state, window, cx);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        PrepaintState {
            layout: self.styled.layout().clone(),
            text: self.text.clone(),
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
        let Some(host) = self.host.clone() else {
            self.styled
                .paint(None, None, bounds, &mut (), &mut (), window, cx);
            return;
        };

        // The highlight underneath the text has to be painted first, and the
        // inner element paints its glyphs in this same pass: the selection
        // quads therefore go down first.
        let selection = host.selection(cx).filter(|(key, _, _)| *key == self.key);
        if let Some((_, range, _)) = &selection {
            for quad in selection_quads(&prepaint.layout, &prepaint.text, range.clone(), bounds) {
                window.paint_quad(quad);
            }
        }
        self.styled
            .paint(None, None, bounds, &mut (), &mut (), window, cx);
        window.set_cursor_style(CursorStyle::IBeam, &prepaint.hitbox);

        window.with_element_state::<Drag, _>(global_id.unwrap(), |state, window| {
            let drag = state.unwrap_or_default();
            let drag_down = drag.clone();
            let key = self.key;
            let links = Rc::new(self.links.clone());
            let drag_move = drag.clone();
            let drag_up = drag.clone();
            let hitbox = prepaint.hitbox.clone();
            let layout = prepaint.layout.clone();
            let text = prepaint.text.clone();

            window.on_mouse_event({
                let layout = layout.clone();
                let text = text.clone();
                let host = host.clone();
                move |event: &MouseDownEvent, phase, window, cx| {
                    if event.button == MouseButton::Right {
                        // The menu is for the selection: a right-click over a
                        // block with an active selection of its own opens the
                        // copy / quote menu at the pointer.
                        if phase.bubble() && hitbox.is_hovered(window) {
                            if let Some((selection_key, _, selection_text)) = host.selection(cx) {
                                if selection_key == key {
                                    host.open_context_menu(event.position, selection_text, cx);
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
                        let index = index_at(&layout, event.position);
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
                let layout = layout.clone();
                let text = text.clone();
                let host = host.clone();
                move |event: &MouseMoveEvent, phase, _window, cx| {
                    if phase.capture() || !drag_move.pending.get() {
                        return;
                    }
                    let Some(anchor) = drag_move.anchor.get() else {
                        return;
                    };
                    let head = index_at(&layout, event.position);
                    let range = anchored_range(anchor, head);
                    host.set_selection(
                        key,
                        range.clone(),
                        text.get(range.clone()).unwrap_or("").to_string(),
                        cx,
                    );
                }
            });

            window.on_mouse_event({
                let layout = layout.clone();
                move |event: &MouseUpEvent, phase, _window, cx| {
                    if !phase.bubble() {
                        return;
                    }
                    drag_up.pending.set(false);
                    let Some(anchor) = drag_up.anchor.get() else {
                        return;
                    };
                    drag_up.anchor.set(None);
                    // Pressing and releasing on one character of a link opens
                    // it; a drag that moved is a selection, and does not.
                    let head = index_at(&layout, event.position);
                    if anchor == head {
                        for (range, url) in links.iter() {
                            if range.contains(&head) {
                                cx.open_url(url);
                                break;
                            }
                        }
                    }
                }
            });
            ((), drag)
        });
    }
}

pub struct PrepaintState {
    layout: TextLayout,
    text: SharedString,
    hitbox: Hitbox,
}

/// Byte index of a window position, through the shared layout.
fn index_at(layout: &TextLayout, position: gpui::Point<Pixels>) -> usize {
    match layout.index_for_position(position) {
        Ok(index) | Err(index) => index,
    }
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

/// One highlight quad per wrapped row the selection covers, positioned from
/// the same layout the glyphs were painted from.
fn selection_quads(
    layout: &TextLayout,
    text: &str,
    range: Range<usize>,
    bounds: Bounds<Pixels>,
) -> Vec<PaintQuad> {
    let line_height = layout.line_height();
    let (start, end) = (range.start.min(range.end), range.start.max(range.end));
    if start >= end {
        return Vec::new();
    }

    let mut quads = Vec::new();
    let mut byte = 0usize;
    let mut rows_above = 0usize;
    for line in text.split('\n') {
        let line_start = byte;
        let relative_len = line.len();
        if let Some(wrapped) = layout.line_layout_for_index(line_start) {
            // Byte ranges of each wrapped row inside this logical line: the
            // wrap boundaries carry the glyph they wrap before.
            let mut row_ends: Vec<usize> = wrapped
                .wrap_boundaries
                .iter()
                .map(|boundary| {
                    wrapped.unwrapped_layout.runs[boundary.run_ix].glyphs[boundary.glyph_ix].index
                })
                .collect();
            row_ends.push(relative_len);

            let mut row_start = 0usize;
            for row_end in row_ends.iter() {
                let intersect_start = start.max(line_start + row_start);
                let intersect_end = end.min(line_start + row_end);
                if intersect_start < intersect_end {
                    let local_from = intersect_start - line_start;
                    let local_to = intersect_end - line_start;
                    if let (Some(from), Some(to)) = (
                        wrapped.position_for_index(local_from, line_height),
                        wrapped.position_for_index(local_to, line_height),
                    ) {
                        // `from.y` is the wrapped row's offset inside this
                        // logical line; the rows of the logical lines before
                        // it come from the running count.
                        let y = bounds.top() + rows_above as f32 * line_height + from.y;
                        quads.push(highlight_quad(Bounds::new(
                            point(bounds.left() + from.x, y),
                            gpui::size((to.x - from.x).max(px(2.0)), line_height),
                        )));
                    }
                }
                row_start = *row_end;
            }
            rows_above += row_ends.len();
        }
        byte += relative_len + 1;
    }
    quads
}

fn highlight_quad(bounds: Bounds<Pixels>) -> PaintQuad {
    fill(bounds, Theme::selection())
}
