use std::ops::Range;

/// Markdown block parser, a direct port of the SwiftUI MarkdownBlockParser:
/// headings, paragraphs, bullets, numbered lists, quotes, fenced code, tables
/// and separators. Inline markup is rendered by the view layer.
#[derive(Clone, Debug, PartialEq)]
pub enum MarkdownBlock {
    Heading(i32, String),
    Paragraph(String),
    Bullet(String),
    Numbered(i64, String),
    Quote(String),
    Code(String),
    Table(Vec<String>, Vec<Vec<String>>),
    Separator,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InlineStyle {
    Strong,
    Emphasis,
    Code,
    Strikethrough,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineSpan {
    pub range: Range<usize>,
    pub style: InlineStyle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineLink {
    pub range: Range<usize>,
    pub url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InlineMarkdown {
    pub text: String,
    pub spans: Vec<InlineSpan>,
    pub links: Vec<InlineLink>,
}

/// Parse the inline Markdown used inside headings, paragraphs, list items,
/// quotes and table cells. Delimiters are removed from the visible text while
/// byte ranges are retained for GPUI text highlighting and link hit testing.
/// Characters that already read as a break point for people: paths, ids and
/// URLs break after them in every browser, so we let gpui break there too.
const BREAK_AFTER: &[char] = &[
    '/', '\\', '-', '_', '.', ':', '?', '&', '=', ',', ';', '+', '|', '~', '@', '#', '%',
];

/// Longest run without a separator before we force a break anyway. Keeps
/// hashes and ids from being laid out as a single 500px line.
const MAX_UNBREAKABLE_RUN: usize = 14;

/// gpui's text wrapper only breaks at unicode line-break opportunities, so a
/// long ASCII run without spaces (a path, a hash, a compacted id) is laid out as
/// a single line, overflows the transcript and looks like "the message does not
/// wrap". Inserting a zero-width space after punctuation and inside very long
/// runs gives the wrapper somewhere to break. Zero width means the rendered text
/// is unchanged.
pub fn add_break_opportunities(text: &str) -> std::borrow::Cow<'_, str> {
    const ZWSP: char = '\u{200b}';

    if !text.contains(|c: char| c.is_ascii_alphanumeric()) {
        return std::borrow::Cow::Borrowed(text);
    }

    let mut out = String::with_capacity(text.len() + text.len() / 8 + 8);
    let mut run = 0usize;
    let mut changed = false;
    for c in text.chars() {
        out.push(c);
        if c.is_whitespace() || !c.is_ascii() {
            // CJK and spaces are already break opportunities.
            run = 0;
            continue;
        }
        run += 1;
        if BREAK_AFTER.contains(&c) {
            // A separator ends the run whether or not we added a break, so a
            // word like `D0/D3/D7` is already text the wrapper can break.
            if run >= 4 {
                out.push(ZWSP);
                changed = true;
            }
            run = 0;
        } else if run >= MAX_UNBREAKABLE_RUN {
            out.push(ZWSP);
            run = 0;
            changed = true;
        }
    }

    if changed {
        std::borrow::Cow::Owned(out)
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

pub fn parse_inline(content: &str) -> InlineMarkdown {
    let mut output = InlineMarkdown::default();
    parse_inline_into(content, &mut output);
    output
}

fn parse_inline_into(content: &str, output: &mut InlineMarkdown) {
    let mut index = 0usize;
    while index < content.len() {
        let rest = &content[index..];

        if rest.starts_with('\\') {
            let escaped_start = index + 1;
            if let Some(character) = content[escaped_start..].chars().next() {
                if r#"\\`*_{}[]()#+-.!>|~"#.contains(character) {
                    output.text.push(character);
                    index = escaped_start + character.len_utf8();
                    continue;
                }
            }
        }

        if rest.starts_with('`') {
            let marker_len = rest.bytes().take_while(|byte| *byte == b'`').count();
            let marker = &rest[..marker_len];
            if let Some(close) = find_unescaped(content, marker, index + marker_len) {
                let mut code = &content[index + marker_len..close];
                if code.starts_with(' ') && code.ends_with(' ') && code.len() > 1 {
                    code = &code[1..code.len() - 1];
                }
                append_literal(output, code, InlineStyle::Code);
                index = close + marker_len;
                continue;
            }
        }

        if rest.starts_with("**") || rest.starts_with("__") {
            let marker = &rest[..2];
            if let Some(close) = find_strong_close(content, marker, index + 2) {
                append_nested(output, &content[index + 2..close], InlineStyle::Strong);
                index = close + 2;
                continue;
            }
        }

        if rest.starts_with("~~") {
            if let Some(close) = find_unescaped(content, "~~", index + 2) {
                append_nested(
                    output,
                    &content[index + 2..close],
                    InlineStyle::Strikethrough,
                );
                index = close + 2;
                continue;
            }
        }

        if rest.starts_with("![") {
            if let Some((label, url, end)) = parse_link_at(content, index + 1) {
                let start = output.text.len();
                output.text.push_str("Image: ");
                parse_inline_into(label, output);
                let range = start..output.text.len();
                if is_link_target(url) {
                    output.links.push(InlineLink {
                        range,
                        url: url.to_string(),
                    });
                }
                index = end;
                continue;
            }
        }

        if rest.starts_with('[') {
            if let Some((label, url, end)) = parse_link_at(content, index) {
                let start = output.text.len();
                parse_inline_into(label, output);
                let range = start..output.text.len();
                if !range.is_empty() && is_link_target(url) {
                    output.links.push(InlineLink {
                        range,
                        url: url.to_string(),
                    });
                }
                index = end;
                continue;
            }
        }

        if rest.starts_with('<') {
            if let Some(close_offset) = rest.find('>') {
                let candidate = &rest[1..close_offset];
                if is_link_target(candidate) {
                    let start = output.text.len();
                    output.text.push_str(candidate);
                    output.links.push(InlineLink {
                        range: start..output.text.len(),
                        url: candidate.to_string(),
                    });
                    index += close_offset + 1;
                    continue;
                }
            }
        }

        if rest.starts_with("https://") || rest.starts_with("http://") {
            let end_offset = rest
                .char_indices()
                .find_map(|(offset, character)| {
                    (offset > 0 && character.is_whitespace()).then_some(offset)
                })
                .unwrap_or(rest.len());
            let mut candidate = &rest[..end_offset];
            while candidate
                .chars()
                .next_back()
                .is_some_and(|character| ['.', ',', ';', ':'].contains(&character))
            {
                candidate = &candidate[..candidate.len() - 1];
            }
            if !candidate.is_empty() {
                let start = output.text.len();
                output.text.push_str(candidate);
                output.links.push(InlineLink {
                    range: start..output.text.len(),
                    url: candidate.to_string(),
                });
                index += candidate.len();
                continue;
            }
        }

        if (rest.starts_with('*') || rest.starts_with('_')) && can_open_emphasis(content, index) {
            let marker = &rest[..1];
            if let Some(close) = find_emphasis_close(content, marker, index + 1) {
                append_nested(output, &content[index + 1..close], InlineStyle::Emphasis);
                index = close + 1;
                continue;
            }
        }

        let character = rest.chars().next().expect("valid UTF-8 boundary");
        output.text.push(character);
        index += character.len_utf8();
    }
}

fn append_literal(output: &mut InlineMarkdown, text: &str, style: InlineStyle) {
    let start = output.text.len();
    output.text.push_str(text);
    if start < output.text.len() {
        output.spans.push(InlineSpan {
            range: start..output.text.len(),
            style,
        });
    }
}

fn append_nested(output: &mut InlineMarkdown, text: &str, style: InlineStyle) {
    let start = output.text.len();
    parse_inline_into(text, output);
    if start < output.text.len() {
        output.spans.push(InlineSpan {
            range: start..output.text.len(),
            style,
        });
    }
}

fn find_unescaped(content: &str, marker: &str, mut from: usize) -> Option<usize> {
    while from <= content.len() {
        let offset = content[from..].find(marker)?;
        let candidate = from + offset;
        let slash_count = content[..candidate]
            .bytes()
            .rev()
            .take_while(|byte| *byte == b'\\')
            .count();
        if slash_count % 2 == 0 {
            return Some(candidate);
        }
        from = candidate + marker.len();
    }
    None
}

fn find_strong_close(content: &str, marker: &str, from: usize) -> Option<usize> {
    let close = find_unescaped(content, marker, from)?;
    let marker_character = marker.chars().next()?;
    if content[close + marker.len()..].starts_with(marker_character) {
        Some(close + marker_character.len_utf8())
    } else {
        Some(close)
    }
}

fn can_open_emphasis(content: &str, index: usize) -> bool {
    let marker = content[index..].chars().next().unwrap_or('*');
    let next = content[index + marker.len_utf8()..].chars().next();
    if next.is_none_or(char::is_whitespace) {
        return false;
    }
    if marker == '_' {
        let previous = content[..index].chars().next_back();
        if previous.is_some_and(char::is_alphanumeric) && next.is_some_and(char::is_alphanumeric) {
            return false;
        }
    }
    true
}

fn find_emphasis_close(content: &str, marker: &str, mut from: usize) -> Option<usize> {
    while let Some(candidate) = find_unescaped(content, marker, from) {
        let before = content[..candidate].chars().next_back();
        let after = content[candidate + marker.len()..].chars().next();
        let doubled = content[candidate + marker.len()..].starts_with(marker);
        let intraword_underscore = marker == "_"
            && before.is_some_and(char::is_alphanumeric)
            && after.is_some_and(char::is_alphanumeric);
        if before.is_some_and(|character| !character.is_whitespace())
            && !doubled
            && !intraword_underscore
        {
            return Some(candidate);
        }
        from = candidate + marker.len();
    }
    None
}

fn parse_link_at(content: &str, open: usize) -> Option<(&str, &str, usize)> {
    if !content[open..].starts_with('[') {
        return None;
    }
    let label_end = find_unescaped(content, "](", open + 1)?;
    let url_start = label_end + 2;
    let url_end = find_unescaped(content, ")", url_start)?;
    let label = &content[open + 1..label_end];
    let url = content[url_start..url_end].trim();
    Some((label, url, url_end + 1))
}

fn is_link_target(value: &str) -> bool {
    value.starts_with("https://") || value.starts_with("http://") || value.starts_with("mailto:")
}

pub fn parse(content: &str) -> Vec<MarkdownBlock> {
    let normalized = normalize(content);
    let lines: Vec<&str> = normalized.lines().collect();
    let mut blocks: Vec<MarkdownBlock> = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    let mut code_lines: Vec<String> = Vec::new();
    let mut in_code_block = false;

    fn flush_paragraph(blocks: &mut Vec<MarkdownBlock>, paragraph: &mut Vec<String>) {
        let text = paragraph
            .iter()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
        if !text.is_empty() {
            blocks.push(MarkdownBlock::Paragraph(text));
        }
        paragraph.clear();
    }

    let mut index = 0usize;
    while index < lines.len() {
        let raw_line = lines[index];
        let line = raw_line.trim();

        if line.starts_with("```") {
            if in_code_block {
                blocks.push(MarkdownBlock::Code(code_lines.join("\n")));
                code_lines.clear();
                in_code_block = false;
            } else {
                flush_paragraph(&mut blocks, &mut paragraph);
                in_code_block = true;
            }
            index += 1;
            continue;
        }

        if in_code_block {
            code_lines.push(raw_line.to_string());
            index += 1;
            continue;
        }

        if line.is_empty() {
            flush_paragraph(&mut blocks, &mut paragraph);
            index += 1;
            continue;
        }

        if is_table_start(&lines, index) {
            flush_paragraph(&mut blocks, &mut paragraph);
            let headers = split_table_line(lines[index]);
            index += 2;
            let mut rows: Vec<Vec<String>> = Vec::new();
            while index < lines.len() {
                let candidate = lines[index].trim();
                if !is_table_line(candidate) || is_table_separator(candidate) {
                    break;
                }
                rows.push(split_table_line(candidate));
                index += 1;
            }
            blocks.push(MarkdownBlock::Table(headers, rows));
            continue;
        }

        if line.len() >= 3 && line.chars().all(|c| c == '-' || c == '*' || c == '_') {
            flush_paragraph(&mut blocks, &mut paragraph);
            blocks.push(MarkdownBlock::Separator);
            index += 1;
            continue;
        }

        if let Some(captures) = match_regex(line, r"^(#{1,6})\s+(.+)$") {
            if captures.len() == 2 {
                flush_paragraph(&mut blocks, &mut paragraph);
                let level = captures[0].chars().filter(|c| *c == '#').count() as i32;
                blocks.push(MarkdownBlock::Heading(level, captures[1].clone()));
                index += 1;
                continue;
            }
        }

        if let Some(captures) = match_regex(line, r"^(\d{1,3})[.)]\s+(.+)$") {
            if captures.len() == 2 {
                if let Ok(number) = captures[0].parse::<i64>() {
                    flush_paragraph(&mut blocks, &mut paragraph);
                    blocks.push(MarkdownBlock::Numbered(number, captures[1].clone()));
                    index += 1;
                    continue;
                }
            }
        }

        if let Some(captures) = match_regex(line, r"^(\d{1,3})([A-Za-z_][A-Za-z0-9_.\-].*)$") {
            if captures.len() == 2 {
                if let Ok(number) = captures[0].parse::<i64>() {
                    flush_paragraph(&mut blocks, &mut paragraph);
                    blocks.push(MarkdownBlock::Numbered(number, captures[1].clone()));
                    index += 1;
                    continue;
                }
            }
        }

        if let Some(captures) = match_regex(line, r"^[-*•]\s+(.+)$") {
            if !captures.is_empty() {
                flush_paragraph(&mut blocks, &mut paragraph);
                blocks.push(MarkdownBlock::Bullet(captures[0].clone()));
                index += 1;
                continue;
            }
        }

        if let Some(captures) = match_regex(line, r"^>\s?(.+)$") {
            if !captures.is_empty() {
                flush_paragraph(&mut blocks, &mut paragraph);
                blocks.push(MarkdownBlock::Quote(captures[0].clone()));
                index += 1;
                continue;
            }
        }

        paragraph.push(raw_line.to_string());
        index += 1;
    }

    if in_code_block {
        blocks.push(MarkdownBlock::Code(code_lines.join("\n")));
    }
    flush_paragraph(&mut blocks, &mut paragraph);

    if blocks.is_empty() {
        blocks.push(MarkdownBlock::Paragraph(normalized));
    }
    blocks
}

/// Does the content contain a markdown table (used to pick streaming font)?
pub fn contains_markdown_table(content: &str) -> bool {
    let lines: Vec<&str> = content.split('\n').collect();
    if lines.len() < 2 {
        return false;
    }
    for index in 0..lines.len() - 1 {
        if lines[index].contains('|') && is_table_separator(lines[index + 1]) {
            return true;
        }
    }
    false
}

fn normalize(content: &str) -> String {
    let mut text = content.replace("\r\n", "\n").replace('\r', "\n");

    if !text.contains('\n') && text.contains("\\n") {
        text = text.replace("\\n", "\n");
    }

    if !text.contains('\n') {
        // Split "3Step" into "3. Step" for single-line agent output. The
        // look-around the Swift version used is unsupported by the regex
        // crate, so the boundary character is captured and re-emitted.
        let re = regex::Regex::new(r"(^|[^A-Za-z0-9.])([1-9][0-9]?)([A-Za-z_][A-Za-z0-9_.\-]+)");
        if let Ok(re) = re {
            text = re
                .replace_all(&text, "${1}\n$2. $3")
                .trim_matches(|c: char| c.is_whitespace())
                .to_string();
        }
        return text;
    }

    text.trim().to_string()
}

fn match_regex(text: &str, pattern: &str) -> Option<Vec<String>> {
    let re = regex::Regex::new(pattern).ok()?;
    let captures = re.captures(text)?;
    let mut out = Vec::new();
    for group in captures.iter().skip(1) {
        out.push(group?.as_str().to_string());
    }
    Some(out)
}

fn is_table_start(lines: &[&str], index: usize) -> bool {
    if index + 1 >= lines.len() {
        return false;
    }
    is_table_line(lines[index]) && is_table_separator(lines[index + 1])
}

fn is_table_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.contains('|') && split_table_line(trimmed).len() >= 2
}

fn is_table_separator(line: &str) -> bool {
    let cells = split_table_line(line);
    if cells.len() < 2 {
        return false;
    }
    cells.iter().all(|cell| {
        let value = cell.trim();
        if value.is_empty() || !value.contains('-') {
            return false;
        }
        value.chars().all(|c| c == '-' || c == ':')
    })
}

fn split_table_line(line: &str) -> Vec<String> {
    let mut trimmed = line.trim();
    if trimmed.starts_with('|') {
        trimmed = &trimmed[1..];
    }
    if trimmed.ends_with('|') && !trimmed.is_empty() {
        trimmed = &trimmed[..trimmed.len() - 1];
    }
    trimmed
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_headings_bullets_code() {
        let blocks = parse("# Title\n\n- item one\n- item two\n\n```rust\nfn a() {}\n```");
        assert_eq!(
            blocks,
            vec![
                MarkdownBlock::Heading(1, "Title".into()),
                MarkdownBlock::Bullet("item one".into()),
                MarkdownBlock::Bullet("item two".into()),
                MarkdownBlock::Code("fn a() {}".into()),
            ]
        );
    }

    #[test]
    fn parses_table() {
        let blocks = parse("| a | b |\n| --- | --- |\n| 1 | 2 |\n| 3 | 4 |");
        match &blocks[0] {
            MarkdownBlock::Table(headers, rows) => {
                assert_eq!(headers, &vec!["a".to_string(), "b".to_string()]);
                assert_eq!(rows.len(), 2);
            }
            other => panic!("expected table, got {other:?}"),
        }
    }

    /// Table shapes that actually show up in agent transcripts, kept as a
    /// regression net for the parser: one-dash separators, inline markup in
    /// cells, emoji/checkmarks, CJK text, a table directly under a heading
    /// (no blank line) and two tables in one message.
    #[test]
    fn parses_the_table_shapes_agents_actually_emit() {
        let samples = [
            "| 验证方式 | 结果 |\n|---|---|\n| `openclaw devices list` 表格 | Paired = **4** ✅ |\n| JSON 权威查询 | paired = 4，**不含** 43cbc108 ✅ |",
            "## KernelBenchCUDA（4 个题目）\n| 模型 | 通过率 | GLM-5.2 Fused MoE | DeepSeek NSA |\n|---|---|---|---|\n| **DeepSeek V4 Flash (0731)** | 4/4 | 0.041 | 0.046 |\n| **GLM-5.3** | 1/1 | 0.100 | — |",
            "| 项目 | 任务进度 | 进行中 |\n| :--- | ---: | :---: |\n| **nano-AgFe-抗氧化** | 6/9 | 测试ROS检测手段 |\n| PEEK亲水改性 | 4/4 ✅ | — |",
            "text before\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\nmiddle\n\n| c | d |\n| --- | --- |\n| 3 | 4 |",
        ];
        for sample in samples {
            let tables = parse(sample)
                .into_iter()
                .filter(|block| matches!(block, MarkdownBlock::Table(..)))
                .count();
            assert!(tables > 0, "no table parsed out of:\n{sample}");
        }
    }

    #[test]
    fn table_cells_keep_their_inline_markup() {
        let blocks = parse("| a | b |\n| --- | --- |\n| **bold** | `code` |");
        let MarkdownBlock::Table(headers, rows) = &blocks[0] else {
            panic!("expected table, got {blocks:?}");
        };
        assert_eq!(headers.len(), 2);
        assert_eq!(rows[0], vec!["**bold**".to_string(), "`code`".to_string()]);
    }

    #[test]
    fn numbered_list_and_quote() {
        let blocks = parse("1. first\n2. second\n> quoted");
        assert_eq!(
            blocks,
            vec![
                MarkdownBlock::Numbered(1, "first".into()),
                MarkdownBlock::Numbered(2, "second".into()),
                MarkdownBlock::Quote("quoted".into()),
            ]
        );
    }

    #[test]
    fn compact_numbered_mid_sentence() {
        let blocks = parse("do 3Step one now");
        assert_eq!(
            blocks,
            vec![
                MarkdownBlock::Paragraph("do".into()),
                MarkdownBlock::Numbered(3, "Step one now".into()),
            ]
        );
    }

    #[test]
    fn compact_numbered_only_in_single_line_text() {
        // single-line: "3Step one" becomes a numbered item
        let blocks = parse("3Step one");
        assert_eq!(blocks[0], MarkdownBlock::Numbered(3, "Step one".into()));
        // multi-line keeps "12.4 GB" intact
        let blocks = parse("Disk shows 12.4 GB free.\nAll good.");
        assert!(matches!(blocks[0], MarkdownBlock::Paragraph(_)));
    }

    #[test]
    fn parses_common_inline_markdown_without_showing_delimiters() {
        let inline = parse_inline(
            "Use **bold and *italic***, `cargo test`, ~~old~~, and [docs](https://example.com).",
        );
        assert_eq!(
            inline.text,
            "Use bold and italic, cargo test, old, and docs."
        );
        assert!(inline
            .spans
            .iter()
            .any(|span| span.style == InlineStyle::Strong
                && &inline.text[span.range.clone()] == "bold and italic"));
        assert!(inline
            .spans
            .iter()
            .any(|span| span.style == InlineStyle::Emphasis
                && &inline.text[span.range.clone()] == "italic"));
        assert!(inline
            .spans
            .iter()
            .any(|span| span.style == InlineStyle::Code
                && &inline.text[span.range.clone()] == "cargo test"));
        assert_eq!(inline.links.len(), 1);
        assert_eq!(&inline.text[inline.links[0].range.clone()], "docs");
        assert_eq!(inline.links[0].url, "https://example.com");
    }

    #[test]
    fn inline_parser_preserves_escapes_and_intraword_underscores() {
        let inline = parse_inline(r"\*literal\* and snake_case plus <https://example.com>");
        assert_eq!(
            inline.text,
            "*literal* and snake_case plus https://example.com"
        );
        assert_eq!(inline.links.len(), 1);
    }
}

#[cfg(test)]
mod break_opportunity_tests {
    use super::add_break_opportunities;

    /// Whatever we insert must disappear again, i.e. the visible text is
    /// unchanged.
    fn strip(text: &str) -> String {
        text.chars().filter(|c| *c != '\u{200b}').collect()
    }

    #[test]
    fn short_and_cjk_text_is_left_alone() {
        let cjk = "测试检测手段完成情况，三条未完成";
        assert!(
            matches!(add_break_opportunities(cjk), std::borrow::Cow::Borrowed(_)),
            "pure CJK already wraps anywhere"
        );
        // Short ASCII words keep their own break points instead of gaining one.
        assert!(matches!(
            add_break_opportunities("nano ROS BMSC 24 孔板"),
            std::borrow::Cow::Borrowed(_)
        ));
        assert!(matches!(
            add_break_opportunities("ROS 检测 D0/D3/D7"),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn unbreakable_runs_gain_zero_width_breaks() {
        let hash = "0ec691c7175f4e50f6e7f758fef99ebd7222482fa424c2a8c1d2";
        let broken = add_break_opportunities(hash);
        assert_ne!(broken.as_ref(), hash, "a 52 char hash needs break points");
        assert_eq!(strip(&broken), hash, "visible text must not change");
        assert!(broken.chars().filter(|c| *c == '\u{200b}').count() >= 3);
    }

    #[test]
    fn paths_break_after_separators() {
        let path = "PHLDB1/COLEC10/RUNX5-12-22";
        let broken = add_break_opportunities(path);
        assert_eq!(strip(&broken), path);
        assert!(
            broken.contains("PHLDB1/\u{200b}"),
            "no break after the first slash"
        );
        assert!(broken.contains("COLEC10/\u{200b}"));
    }

    #[test]
    fn markdown_syntax_still_parses() {
        let source = "**bold** and `0ec691c7175f4e50f6e7f758fef99ebd7222482fa424c2a8c1d2` end";
        let broken = add_break_opportunities(source);
        assert_eq!(strip(&broken), source);
        let parsed = super::parse_inline(&broken);
        assert!(parsed
            .spans
            .iter()
            .any(|span| span.style == super::InlineStyle::Strong));
        assert!(parsed
            .spans
            .iter()
            .any(|span| span.style == super::InlineStyle::Code));
    }
}
