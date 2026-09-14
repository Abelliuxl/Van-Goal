use crate::ui::theme::Theme;
use gpui::{
    combine_highlights, div, prelude::*, px, relative, AnyElement, Div, FontStyle, FontWeight,
    HighlightStyle, InteractiveText, ParentElement, StrikethroughStyle, Styled, StyledText,
    UnderlineStyle,
};
use van_goal_core::markdown::{InlineStyle, MarkdownBlock};

const MAX_TABLE_COLUMNS: usize = 8;
const MAX_TABLE_ROWS: usize = 80;

/// Room above a heading, on top of the uniform gap between blocks. Headings are
/// what a reader navigates by, so they need more air before them than the
/// paragraphs they introduce.
const HEADING_SPACE_BEFORE: f32 = 16.0;
/// Room below a heading. Smaller than the space above it, which is what ties a
/// heading to the text it belongs to instead of letting it float between two
/// sections.
const HEADING_SPACE_AFTER: f32 = 4.0;
/// Extra breathing room around a paragraph. Paragraphs are the bulk of a reply,
/// and at the plain block gap two of them read as one wall of text.
const PARAGRAPH_SPACE: f32 = 4.0;

/// Render parsed markdown blocks as div-based layout. Strings wrap natively;
/// code blocks scroll horizontally and tables get a simple grid.
pub fn render_blocks(blocks: &[MarkdownBlock]) -> Div {
    div()
        .w_full()
        .min_w(px(0.0))
        .flex()
        .flex_col()
        .gap_2()
        .children(blocks.iter().enumerate().map(|(index, block)| {
            let seed = index as u64;
            div()
                .w_full()
                .min_w(px(0.0))
                .debug_selector(move || format!("markdown-block-{seed}"))
                .child(render_block(block, seed, index == 0))
        }))
}

/// `is_first` suppresses the space a block would leave above itself. A reply
/// that opens with a heading should not start with a hole at the top of its
/// bubble; the gap only exists to separate blocks from each other.
fn render_block(block: &MarkdownBlock, seed: u64, is_first: bool) -> AnyElement {
    match block {
        MarkdownBlock::Heading(level, text) => {
            // Sized against the 13px body text: a heading has to read as a
            // heading at a glance, and the old 18/16/14 steps were close enough
            // to body size to look like bold paragraphs.
            let (size, weight) = match level {
                1 => (24.0, gpui::FontWeight::BOLD),
                2 => (20.0, gpui::FontWeight::SEMIBOLD),
                _ => (17.0, gpui::FontWeight::SEMIBOLD),
            };
            div()
                .when(!is_first, |this| {
                    this.mt(Theme::text_px(HEADING_SPACE_BEFORE))
                })
                .mb(Theme::text_px(HEADING_SPACE_AFTER))
                .font_weight(weight)
                .text_size(Theme::text_px(size))
                .text_color(Theme::text())
                .child(render_inline(text, seed))
                .into_any()
        }
        MarkdownBlock::Paragraph(text) => div()
            .min_w(px(0.0))
            .my(Theme::text_px(PARAGRAPH_SPACE))
            .text_size(Theme::text_px(13.0))
            .text_color(Theme::text())
            .child(render_inline(text, seed))
            .into_any(),
        MarkdownBlock::Bullet(text) => div()
            .min_w(px(0.0))
            .flex()
            .flex_row()
            .gap_2()
            .child(
                div()
                    .text_size(Theme::text_px(13.0))
                    .text_color(crate::ui::theme::Theme::text_secondary())
                    .child("•"),
            )
            .child(
                div()
                    .text_size(Theme::text_px(13.0))
                    .text_color(crate::ui::theme::Theme::text())
                    .flex_1()
                    .min_w(px(0.0))
                    .child(render_inline(text, seed)),
            )
            .into_any(),
        MarkdownBlock::Numbered(number, text) => div()
            .min_w(px(0.0))
            .flex()
            .flex_row()
            .gap_2()
            .child(
                div()
                    .text_size(Theme::text_px(13.0))
                    .text_color(crate::ui::theme::Theme::text_secondary())
                    .min_w(px(18.0))
                    .child(format!("{number}.")),
            )
            .child(
                div()
                    .text_size(Theme::text_px(13.0))
                    .text_color(crate::ui::theme::Theme::text())
                    .flex_1()
                    .min_w(px(0.0))
                    .child(render_inline(text, seed)),
            )
            .into_any(),
        MarkdownBlock::Quote(text) => div()
            .min_w(px(0.0))
            .border_l_2()
            .border_color(crate::ui::theme::Theme::quote_bar())
            .pl_2()
            .text_size(Theme::text_px(13.0))
            .text_color(crate::ui::theme::Theme::text_secondary())
            .child(render_inline(text, seed))
            .into_any(),
        MarkdownBlock::Code(text) => div()
            .id(gpui::ElementId::NamedInteger(
                "code-block".into(),
                crate::ui::hash_id(text),
            ))
            .w_full()
            .overflow_x_scroll()
            .bg(crate::ui::theme::Theme::code_bg())
            .rounded_md()
            .border_1()
            .border_color(crate::ui::theme::Theme::border())
            .child(
                div()
                    .p_2()
                    .font_family("Menlo")
                    .text_size(Theme::text_px(12.0))
                    .text_color(crate::ui::theme::Theme::text())
                    .child(text.clone()),
            )
            .into_any(),
        MarkdownBlock::Table(headers, rows) => render_table(headers, rows, seed).into_any(),
        MarkdownBlock::Separator => div()
            .h(px(1.0))
            .w_full()
            .my(Theme::text_px(8.0))
            .bg(crate::ui::theme::Theme::border())
            .into_any(),
    }
}

fn render_inline(source: &str, seed: u64) -> AnyElement {
    // Give the text wrapper break opportunities before parsing, so highlight
    // ranges stay aligned with the bytes we actually render.
    let with_breaks = van_goal_core::markdown::add_break_opportunities(source);
    let parsed = van_goal_core::markdown::parse_inline(&with_breaks);
    let span_highlights = parsed.spans.iter().map(|span| {
        let style = match span.style {
            InlineStyle::Strong => HighlightStyle {
                font_weight: Some(FontWeight::BOLD),
                ..Default::default()
            },
            InlineStyle::Emphasis => HighlightStyle {
                font_style: Some(FontStyle::Italic),
                ..Default::default()
            },
            InlineStyle::Code => HighlightStyle {
                background_color: Some(crate::ui::theme::Theme::code_bg()),
                ..Default::default()
            },
            InlineStyle::Strikethrough => HighlightStyle {
                strikethrough: Some(StrikethroughStyle {
                    thickness: px(1.0),
                    ..Default::default()
                }),
                ..Default::default()
            },
        };
        (span.range.clone(), style)
    });
    let link_highlights = parsed.links.iter().map(|link| {
        (
            link.range.clone(),
            HighlightStyle {
                color: Some(crate::ui::theme::Theme::accent()),
                underline: Some(UnderlineStyle {
                    thickness: px(1.0),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
    });
    let highlights = combine_highlights(span_highlights, link_highlights).collect::<Vec<_>>();
    let styled = StyledText::new(parsed.text).with_highlights(highlights);

    if parsed.links.is_empty() {
        return styled.into_any();
    }

    let ranges = parsed
        .links
        .iter()
        .map(|link| link.range.clone())
        .collect::<Vec<_>>();
    let urls = parsed
        .links
        .into_iter()
        .map(|link| link.url)
        .collect::<Vec<_>>();
    InteractiveText::new(
        gpui::ElementId::NamedInteger(
            "markdown-inline".into(),
            crate::ui::hash_id(source).wrapping_add(seed),
        ),
        styled,
    )
    .on_click(ranges, move |index, _window, cx| {
        if let Some(url) = urls.get(index) {
            cx.open_url(url);
        }
    })
    .into_any()
}

fn render_table(headers: &[String], rows: &[Vec<String>], seed: u64) -> AnyElement {
    let column_count = headers
        .len()
        .max(rows.iter().map(Vec::len).max().unwrap_or(0))
        .clamp(1, MAX_TABLE_COLUMNS);
    let visible_rows: &[Vec<String>] = &rows[..rows.len().min(MAX_TABLE_ROWS)];
    let hidden_rows = rows.len().saturating_sub(MAX_TABLE_ROWS);
    // Same fractions for every row, so the columns of a table actually line up
    // (each row used to size its own cells to its own content, which made the
    // grid look ragged).
    let fractions = column_fractions(headers, visible_rows, column_count);

    let mut children: Vec<AnyElement> = Vec::new();

    // Header row.
    children.push(
        table_row(
            headers.to_vec(),
            &fractions,
            0,
            true,
            seed.wrapping_mul(1000),
        )
        .into_any(),
    );

    for (row_index, row) in visible_rows.iter().enumerate() {
        children.push(
            table_row(
                row.clone(),
                &fractions,
                row_index + 1,
                false,
                seed.wrapping_mul(1000)
                    .wrapping_add(((row_index + 1) * column_count) as u64),
            )
            .into_any(),
        );
    }

    let mut table = div()
        .id(gpui::ElementId::NamedInteger(
            "table".into(),
            crate::ui::hash_id(&headers.join("|")),
        ))
        .w_full()
        .min_w(px(0.0))
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded_md()
        .border_1()
        .border_color(crate::ui::theme::Theme::border())
        .children(children);

    if hidden_rows > 0 {
        table = table.child(
            div()
                .px_2()
                .py_1()
                .text_size(Theme::text_px(11.0))
                .text_color(crate::ui::theme::Theme::text_secondary())
                .child(format!("{hidden_rows} more row(s) hidden")),
        );
    }
    table.into_any()
}

/// Relative width of every column, taken from the widest cell it holds so that
/// wide columns (model names, descriptions) get more room than numeric ones.
fn column_fractions(headers: &[String], rows: &[Vec<String>], column_count: usize) -> Vec<f32> {
    let mut weights: Vec<f32> = Vec::with_capacity(column_count);
    for column in 0..column_count {
        let mut widest = headers
            .get(column)
            .map(|header| header.chars().count() as f32)
            .unwrap_or(0.0);
        for row in rows {
            if let Some(cell) = row.get(column) {
                widest = widest.max(cell.chars().count() as f32);
            }
        }
        weights.push(widest.clamp(4.0, 32.0));
    }
    let total: f32 = weights.iter().sum::<f32>().max(1.0);
    weights.into_iter().map(|weight| weight / total).collect()
}

fn table_row(
    cells: Vec<String>,
    fractions: &[f32],
    row_index: usize,
    is_header: bool,
    seed: u64,
) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .w_full()
        .children((0..fractions.len()).map(|column| {
            table_cell(
                cell_text(&cells, column),
                row_index,
                column,
                fractions.get(column).copied().unwrap_or(0.0),
                is_header,
                fractions.len(),
                seed.wrapping_add(column as u64),
            )
        }))
}

#[allow(clippy::too_many_arguments)]
fn table_cell(
    text: String,
    row_index: usize,
    column_index: usize,
    fraction: f32,
    is_header: bool,
    column_count: usize,
    seed: u64,
) -> gpui::AnyElement {
    let mut cell = div()
        .id(gpui::ElementId::NamedInteger(
            "table-cell".into(),
            crate::ui::hash_id(&format!("{row_index}:{column_index}:{text}")).wrapping_add(seed),
        ))
        .debug_selector(move || format!("table-cell-{row_index}-{column_index}"))
        .flex_basis(relative(fraction))
        .flex_shrink()
        .min_w(px(0.0))
        .overflow_hidden()
        .px_2()
        .py_1()
        .text_size(Theme::text_px(12.0))
        .whitespace_normal()
        .border_color(crate::ui::theme::Theme::border())
        .text_color(if is_header {
            crate::ui::theme::Theme::text()
        } else {
            crate::ui::theme::Theme::text_secondary()
        });
    if is_header {
        cell = cell
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .bg(crate::ui::theme::Theme::tool_bg());
    }
    if column_index + 1 < column_count {
        cell = cell.border_r_1();
    }
    if !is_header {
        cell = cell.border_t_1();
    }
    cell.child(render_inline(&text, seed)).into_any()
}

fn cell_text(cells: &[String], index: usize) -> String {
    cells
        .get(index)
        .map(|value| value.trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Render, TestAppContext, VisualTestContext, Window};

    struct TableFixture(&'static str);

    impl Render for TableFixture {
        fn render(
            &mut self,
            _window: &mut Window,
            _cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            div()
                .w(px(600.0))
                .child(render_blocks(&van_goal_core::markdown::parse(self.0)))
        }
    }

    fn cell_width(cx: &mut VisualTestContext, selector: &'static str) -> f32 {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} was not laid out"));
        f32::from(bounds.size.width)
    }

    fn cell_left(cx: &mut VisualTestContext, selector: &'static str) -> f32 {
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} was not laid out"));
        f32::from(bounds.origin.x)
    }

    /// Columns used to be sized per row from that row's own content, so a table
    /// came out as a ragged set of rows. Every row now shares the same column
    /// fractions, which this pins down.
    #[gpui::test]
    fn table_columns_line_up_across_rows(cx: &mut TestAppContext) {
        let markdown = "| model | pass | gemm |\n\
                        | --- | --- | --- |\n\
                        | DeepSeek V4 Flash (0731) | 6/6 | 0.409 |\n\
                        | GLM-5.3 | 5/5 | 0.046 |";
        let (_view, cx) = cx.add_window_view(|_window, _cx| TableFixture(markdown));
        cx.run_until_parked();

        let header_model = cell_width(cx, "table-cell-0-0");
        let row_model = cell_width(cx, "table-cell-1-0");
        let row_two_model = cell_width(cx, "table-cell-2-0");
        assert!(
            (header_model - row_model).abs() < 1.0,
            "model column differs between header ({header_model}) and row ({row_model})"
        );
        assert!(
            (header_model - row_two_model).abs() < 1.0,
            "model column differs between rows ({header_model} vs {row_two_model})"
        );
        assert!(
            (cell_left(cx, "table-cell-0-0") - cell_left(cx, "table-cell-2-0")).abs() < 1.0,
            "model column does not start at the same x in every row"
        );

        // Column 1 starts where column 0 ends, in every row.
        let model_right = cell_left(cx, "table-cell-1-0") + cell_width(cx, "table-cell-1-0");
        assert!(
            (cell_left(cx, "table-cell-1-1") - model_right).abs() < 1.0,
            "second column is not aligned with the first column's right edge"
        );

        // Wide content gets a wider column than short content.
        assert!(
            cell_width(cx, "table-cell-0-0") > cell_width(cx, "table-cell-0-1"),
            "column widths ignore content width"
        );
    }

    /// A long run of ASCII without spaces (a path, a hash, a compacted id) has
    /// to wrap inside the pane. Before, such rows were laid out at their
    /// min-content width, so the message looked like one long line running off
    /// the right edge.
    #[gpui::test]
    fn long_unbreakable_runs_wrap_inside_the_pane(cx: &mut TestAppContext) {
        for token in [
            // Path-like: gpui can break after the slashes once the row shrinks.
            "PHLDB1/COLEC10/RUNX5-12-22-ALPHA-BETA-GAMMA-DELTA-EPSILON",
            // Hash-like: no break opportunity at all, needs inserted ones.
            "0ec691c7175f4e50f6e7f758fef99ebd7222482fa424c2a8c1d2",
        ] {
            let markdown = format!("- 测试 ROS 检测手段 + qRT-PCR 检测 {token} 结束");
            let (_view, cx) = cx
                .add_window_view(|_window, _cx| TableFixture(Box::leak(markdown.into_boxed_str())));
            cx.run_until_parked();

            let block = cx
                .debug_bounds("markdown-block-0")
                .expect("block was not laid out");
            let width = f32::from(block.size.width);
            let height = f32::from(block.size.height);
            assert!(
                width <= 601.0,
                "{token}: block is {width}px wide, past the 600px pane"
            );
            assert!(
                height >= 2.0 * 20.0,
                "{token}: block is only {height}px tall, the token did not wrap"
            );
        }
    }

    /// Long cells have to wrap onto as many lines as they need. `line_clamp`
    /// made gpui keep everything past the clamped line on that last line, where
    /// it was cut off horizontally — which reads as "the text does not wrap".
    #[gpui::test]
    fn long_table_cells_wrap_instead_of_being_cut_off(cx: &mut TestAppContext) {
        let long = "词".repeat(400);
        let markdown = format!("| a | b |\n| --- | --- |\n| short | {long} |");
        let (_view, cx) =
            cx.add_window_view(|_window, _cx| TableFixture(Box::leak(markdown.into_boxed_str())));
        cx.run_until_parked();

        let cell = cx
            .debug_bounds("table-cell-1-1")
            .expect("long cell was not laid out");
        let height = f32::from(cell.size.height);
        // 400 CJK characters in a ~400px column need far more than four lines.
        assert!(
            height > 4.0 * 22.0,
            "long cell stopped at {height}px, so its text was cut off rather than wrapped"
        );
    }

    /// A cell must not be clipped to nothing when the table is narrow.
    #[gpui::test]
    fn narrow_tables_still_lay_out_columns(cx: &mut TestAppContext) {
        let markdown = "| 项目 | 状态 |\n| --- | --- |\n| nano-AgFe-抗氧化实验数据整理 | 进行中 |";
        let (_view, cx) = cx.add_window_view(|_window, _cx| TableFixture(markdown));
        cx.run_until_parked();
        let width = cell_width(cx, "table-cell-1-0");
        assert!(width > 8.0, "column collapsed to {width}px");
    }
}
