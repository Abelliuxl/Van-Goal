//! Markdown, parsed by core and handed to the Go side as blocks of styled runs.
//!
//! The parser reports inline styles as byte ranges into the text with the
//! delimiters removed. Byte offsets are fragile currency across a language
//! boundary, so each run carries its own text and the renderer only appends.

use serde_json::{json, Value};
use van_goal_core::markdown::{self, InlineStyle, MarkdownBlock};

pub fn blocks(text: &str) -> Vec<Value> {
    markdown::parse(text)
        .into_iter()
        .map(|block| match block {
            MarkdownBlock::Heading(level, text) => {
                json!({ "kind": "heading", "level": level, "runs": runs(&text) })
            }
            MarkdownBlock::Paragraph(text) => json!({ "kind": "paragraph", "runs": runs(&text) }),
            MarkdownBlock::Bullet(text) => json!({ "kind": "bullet", "runs": runs(&text) }),
            MarkdownBlock::Numbered(number, text) => {
                json!({ "kind": "numbered", "number": number, "runs": runs(&text) })
            }
            MarkdownBlock::Quote(text) => json!({ "kind": "quote", "runs": runs(&text) }),
            MarkdownBlock::Code(text) => json!({ "kind": "code", "text": text }),
            MarkdownBlock::Table(headers, rows) => json!({
                "kind": "table",
                "headers": headers.iter().map(|cell| runs(cell)).collect::<Vec<_>>(),
                "rows": rows
                    .iter()
                    .map(|row| row.iter().map(|cell| runs(cell)).collect::<Vec<_>>())
                    .collect::<Vec<_>>(),
            }),
            MarkdownBlock::Separator => json!({ "kind": "separator" }),
        })
        .collect()
}

pub fn runs(text: &str) -> Vec<Value> {
    let inline = markdown::parse_inline(text);
    let visible = inline.text.as_str();
    let mut cuts: Vec<usize> = vec![0, visible.len()];
    for style in &inline.spans {
        cuts.push(style.range.start);
        cuts.push(style.range.end);
    }
    for link in &inline.links {
        cuts.push(link.range.start);
        cuts.push(link.range.end);
    }
    cuts.retain(|cut| *cut <= visible.len() && visible.is_char_boundary(*cut));
    cuts.sort_unstable();
    cuts.dedup();

    let mut out = Vec::with_capacity(cuts.len().saturating_sub(1));
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        if start >= end {
            continue;
        }
        let mut run = json!({ "text": &visible[start..end] });
        for style in inline
            .spans
            .iter()
            .filter(|style| style.range.start <= start && style.range.end >= end)
        {
            let key = match style.style {
                InlineStyle::Strong => "bold",
                InlineStyle::Emphasis => "italic",
                InlineStyle::Code => "code",
                InlineStyle::Strikethrough => "strike",
            };
            run[key] = json!(true);
        }
        if let Some(link) = inline
            .links
            .iter()
            .find(|link| link.range.start <= start && link.range.end >= end)
        {
            run["url"] = json!(link.url);
        }
        out.push(run);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_runs_carry_their_own_text_and_nothing_is_lost() {
        let parsed = runs("你好 👋 **粗体** 和 `代码` 还有 [链接](https://example.com)");
        let text: String = parsed
            .iter()
            .map(|run| run["text"].as_str().unwrap())
            .collect();
        assert_eq!(text, "你好 👋 粗体 和 代码 还有 链接");
        assert!(parsed.iter().any(|run| run["bold"] == true));
        assert!(parsed.iter().any(|run| run["code"] == true));
        assert!(parsed
            .iter()
            .any(|run| run["url"].as_str() == Some("https://example.com")));
    }

    #[test]
    fn block_kinds_survive() {
        let parsed = blocks("# 标题\n\n- 一项\n\n```\n代码\n```\n");
        let kinds: Vec<&str> = parsed.iter().map(|b| b["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["heading", "bullet", "code"]);
        assert_eq!(parsed[2]["text"], "代码");
    }
}
