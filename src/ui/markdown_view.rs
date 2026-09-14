use crate::markdown::{InlineStyle, MarkdownBlock};
use gpui::{
    combine_highlights, div, prelude::*, px, AnyElement, Div, FontStyle, FontWeight,
    HighlightStyle, InteractiveText, ParentElement, StrikethroughStyle, Styled, StyledText,
    UnderlineStyle,
};

const MAX_TABLE_COLUMNS: usize = 8;
const MAX_TABLE_ROWS: usize = 80;
const CELL_MAX_WIDTH: f32 = 240.0;

/// Render parsed markdown blocks as div-based layout. Strings wrap natively;
/// code blocks scroll horizontally and tables get a simple grid.
pub fn render_blocks(blocks: &[MarkdownBlock]) -> Div {
    div().flex().flex_col().gap_2().children(
        blocks
            .iter()
            .enumerate()
            .map(|(index, block)| render_block(block, index as u64)),
    )
}

fn render_block(block: &MarkdownBlock, seed: u64) -> AnyElement {
    match block {
        MarkdownBlock::Heading(level, text) => {
            let (size, weight) = match level {
                1 => (px(18.0), gpui::FontWeight::BOLD),
                2 => (px(16.0), gpui::FontWeight::SEMIBOLD),
                _ => (px(14.0), gpui::FontWeight::SEMIBOLD),
            };
            div()
                .font_weight(weight)
                .text_size(size)
                .text_color(crate::ui::theme::Theme::text())
                .child(render_inline(text, seed))
                .into_any()
        }
        MarkdownBlock::Paragraph(text) => div()
            .text_size(px(13.0))
            .text_color(crate::ui::theme::Theme::text())
            .child(render_inline(text, seed))
            .into_any(),
        MarkdownBlock::Bullet(text) => div()
            .flex()
            .flex_row()
            .gap_2()
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(crate::ui::theme::Theme::text_secondary())
                    .child("•"),
            )
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(crate::ui::theme::Theme::text())
                    .flex_1()
                    .child(render_inline(text, seed)),
            )
            .into_any(),
        MarkdownBlock::Numbered(number, text) => div()
            .flex()
            .flex_row()
            .gap_2()
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(crate::ui::theme::Theme::text_secondary())
                    .min_w(px(18.0))
                    .child(format!("{number}.")),
            )
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(crate::ui::theme::Theme::text())
                    .flex_1()
                    .child(render_inline(text, seed)),
            )
            .into_any(),
        MarkdownBlock::Quote(text) => div()
            .border_l_2()
            .border_color(crate::ui::theme::Theme::quote_bar())
            .pl_2()
            .text_size(px(13.0))
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
                    .text_size(px(12.0))
                    .text_color(crate::ui::theme::Theme::text())
                    .child(text.clone()),
            )
            .into_any(),
        MarkdownBlock::Table(headers, rows) => render_table(headers, rows, seed).into_any(),
        MarkdownBlock::Separator => div()
            .h(px(1.0))
            .w_full()
            .bg(crate::ui::theme::Theme::border())
            .into_any(),
    }
}

fn render_inline(source: &str, seed: u64) -> AnyElement {
    let parsed = crate::markdown::parse_inline(source);
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
        .min(MAX_TABLE_COLUMNS)
        .max(1);
    let visible_rows: &[Vec<String>] = &rows[..rows.len().min(MAX_TABLE_ROWS)];
    let hidden_rows = rows.len().saturating_sub(MAX_TABLE_ROWS);

    let mut children: Vec<AnyElement> = Vec::new();

    // Header row.
    children.push(
        div()
            .flex()
            .flex_row()
            .bg(crate::ui::theme::Theme::tool_bg())
            .children((0..column_count).map(|column| {
                table_cell(
                    cell_text(headers, column),
                    true,
                    seed.wrapping_mul(1000).wrapping_add(column as u64),
                )
            }))
            .into_any(),
    );
    children.push(
        div()
            .h(px(1.0))
            .w_full()
            .bg(crate::ui::theme::Theme::border())
            .into_any(),
    );

    for (row_index, row) in visible_rows.iter().enumerate() {
        children.push(
            div()
                .flex()
                .flex_row()
                .children((0..column_count).map(|column| {
                    table_cell(
                        cell_text(row, column),
                        false,
                        seed.wrapping_mul(1000)
                            .wrapping_add(((row_index + 1) * column_count + column) as u64),
                    )
                }))
                .into_any(),
        );
        if row_index + 1 < visible_rows.len() {
            children.push(
                div()
                    .h(px(1.0))
                    .w_full()
                    .bg(crate::ui::theme::Theme::border())
                    .into_any(),
            );
        }
    }

    let mut table = div()
        .id(gpui::ElementId::NamedInteger(
            "table".into(),
            crate::ui::hash_id(&headers.join("|")),
        ))
        .w_full()
        .overflow_x_scroll()
        .rounded_md()
        .border_1()
        .border_color(crate::ui::theme::Theme::border())
        .flex()
        .flex_col()
        .children(children);

    if hidden_rows > 0 {
        table = table.child(
            div()
                .px_2()
                .py_1()
                .text_size(px(11.0))
                .text_color(crate::ui::theme::Theme::text_secondary())
                .child(format!("{hidden_rows} more row(s) hidden")),
        );
    }
    table.into_any()
}

fn table_cell(text: String, is_header: bool, seed: u64) -> gpui::AnyElement {
    let mut cell = div()
        .px_2()
        .py_1()
        .min_w(px(56.0))
        .max_w(px(CELL_MAX_WIDTH))
        .text_size(px(12.0))
        .text_ellipsis()
        .whitespace_nowrap()
        .text_color(if is_header {
            crate::ui::theme::Theme::text()
        } else {
            crate::ui::theme::Theme::text_secondary()
        });
    if is_header {
        cell = cell.font_weight(gpui::FontWeight::SEMIBOLD);
    }
    cell.child(render_inline(&text, seed)).into_any()
}

fn cell_text(cells: &[String], index: usize) -> String {
    cells
        .get(index)
        .map(|value| value.trim().to_string())
        .unwrap_or_default()
}
