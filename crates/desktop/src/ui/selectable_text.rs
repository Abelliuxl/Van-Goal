use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    App, Bounds, CursorStyle, Element, ElementId, GlobalElementId, HighlightStyle, HitboxBehavior,
    IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    SharedString, StyledText, TextLayout, Window,
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
    /// A left press landed outside this block. Clears an active selection,
    /// except when the press is on the open context menu — the menu's own
    /// items have yet to act on the selection.
    fn press_outside(&self, position: gpui::Point<Pixels>, cx: &mut App);
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
/// this block measures and wraps exactly like the plain text it replaced. The
/// selection is folded into the runs as their background colour, so it is
/// painted by the same pass as the glyphs and lines up with every wrapped row
/// and every inline-code span by construction.
pub struct SelectableText {
    id: ElementId,
    /// Selection identity: a block cannot hold another block's selection.
    /// Stable across frames so a repaint keeps the highlight.
    key: u64,
    text: SharedString,
    /// The block's own highlights; the selection is re-cut against them per
    /// frame (see [`selection_over`]).
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    /// The styled text this block delegates to.
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
        let styled = StyledText::new(text.clone()).with_highlights(highlights.clone());
        Self {
            id: id.into(),
            key,
            text,
            highlights,
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
        //
        // The selection ships inside the runs as their background colour, so
        // the same pass that paints the glyphs paints the selection: it aligns
        // with every wrapped row and every inline-code span by construction. A
        // separately painted highlight underneath loses that fight to the code
        // spans' own backgrounds, which is what made selected rows and spans
        // look unselected.
        let selection = self
            .host
            .as_ref()
            .and_then(|host| host.selection(cx))
            .filter(|(key, _, _)| *key == self.key)
            .map(|(_, range, _)| range);
        self.styled = StyledText::new(self.text.clone()).with_highlights(selection_over(
            self.text.len(),
            self.highlights.clone(),
            selection,
        ));
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
        // The selection ships inside the runs (see request_layout), so the
        // glyphs are all this pass paints.
        self.styled
            .paint(None, None, bounds, &mut (), &mut (), window, cx);
        window.set_cursor_style(CursorStyle::IBeam, &prepaint.hitbox);

        let Some(host) = self.host.clone() else {
            return;
        };

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
                        host.press_outside(event.position, cx);
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
    hitbox: gpui::Hitbox,
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

/// Tile the block's text into disjoint pieces against both the styled spans
/// and the selection, and hand every covered piece its style — with the
/// selection background winning over a code span's own. Deterministic, so the
/// selection always reads as one solid range.
fn selection_over(
    text_len: usize,
    highlights: Vec<(Range<usize>, HighlightStyle)>,
    selection: Option<Range<usize>>,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let Some(selection) = selection else {
        return highlights;
    };
    let mut cuts = vec![0usize, text_len, selection.start, selection.end];
    for (range, _) in &highlights {
        cuts.push(range.start);
        cuts.push(range.end);
    }
    cuts.retain(|cut| *cut <= text_len);
    cuts.sort_unstable();
    cuts.dedup();

    let mut out = Vec::new();
    for pair in cuts.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        if from >= to {
            continue;
        }
        // The pieces are disjoint and sorted, so at most one spans a window.
        let containing = highlights
            .iter()
            .find(|(range, _)| range.contains(&from))
            .map(|(_, style)| *style)
            .unwrap_or_default();
        let mut style = containing;
        if from >= selection.start && to <= selection.end {
            style.background_color = Some(Theme::selection());
        }
        out.push((from..to, style));
    }
    out
}

#[cfg(test)]
const ROW_HEIGHT: f32 = 20.0;

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{div, point, prelude::*, px, Context, TestAppContext};
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

        fn press_outside(&self, _position: gpui::Point<Pixels>, _cx: &mut App) {
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
        let (_root, cx) = cx.add_window_view(|_window, _cx| Fixture {
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
        let (start, end) = _root.update_in(cx, |_, window, _cx| {
            let style = window.text_style();
            let font_size = style.font_size.to_pixels(window.rem_size());
            let run = style.to_run(17);
            let lines = window
                .text_system()
                .shape_text(text.clone(), font_size, &[run], Some(px(320.0)), None)
                .unwrap();
            (
                lines[0].position_for_index(6, px(ROW_HEIGHT)).unwrap(),
                lines[0].position_for_index(11, px(ROW_HEIGHT)).unwrap(),
            )
        });
        let at = |local: gpui::Point<Pixels>| {
            point(px(0.0) + local.x, px(0.0) + local.y + px(ROW_HEIGHT / 2.0))
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
