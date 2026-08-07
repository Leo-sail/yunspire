//! Small, deterministic Markdown block parser used by the creation runtime.
//!
//! It intentionally models block structure only. Inline Markdown is kept as
//! source text so Obsidian remains the authoritative representation and a
//! parse/serialize cycle does not invent rich-text semantics.

use serde_json::json;
use sha2::{Digest, Sha256};

use super::model::{CreationBlock, SourceRange};

#[derive(Debug, Clone, PartialEq)]
pub struct MarkdownAst {
    pub blocks: Vec<MarkdownBlock>,
    pub errors: Vec<MarkdownParseError>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MarkdownParseError {
    pub line: usize,
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MarkdownBlock {
    Frontmatter(Vec<String>),
    Heading {
        level: u8,
        text: String,
    },
    Paragraph(Vec<String>),
    Blockquote(Vec<String>),
    List(Vec<String>),
    CodeFence {
        fence: String,
        info: String,
        body: Vec<String>,
        closed: bool,
    },
    Table(Vec<String>),
    ThematicBreak,
    FootnoteDefinition {
        label: String,
        body: Vec<String>,
    },
    Html(Vec<String>),
}

impl MarkdownBlock {
    pub fn block_kind(&self) -> &'static str {
        match self {
            Self::Frontmatter(_) => "html",
            Self::Heading { .. } => "heading",
            Self::Paragraph(_) => "paragraph",
            Self::Blockquote(_) => "quote",
            Self::List(_) => "list",
            Self::CodeFence { .. } => "code",
            Self::Table(_) => "table",
            Self::ThematicBreak => "divider",
            Self::FootnoteDefinition { .. } => "paragraph",
            Self::Html(_) => "html",
        }
    }

    pub fn to_markdown(&self) -> String {
        match self {
            Self::Frontmatter(lines)
            | Self::Paragraph(lines)
            | Self::Blockquote(lines)
            | Self::List(lines)
            | Self::Table(lines)
            | Self::Html(lines) => lines.join("\n"),
            Self::Heading { level, text } => format!("{} {}", "#".repeat(*level as usize), text),
            Self::CodeFence {
                fence,
                info,
                body,
                closed,
            } => {
                let mut lines = vec![format!("{fence}{info}")];
                lines.extend(body.iter().cloned());
                if *closed {
                    lines.push(fence.clone());
                }
                lines.join("\n")
            }
            Self::ThematicBreak => "---".to_string(),
            Self::FootnoteDefinition { label, body } => {
                let mut lines = Vec::with_capacity(body.len().max(1));
                let first = body.first().map(String::as_str).unwrap_or_default();
                lines.push(format!("[^{label}]: {first}").trim_end().to_string());
                lines.extend(body.iter().skip(1).map(|line| format!("    {line}")));
                lines.join("\n")
            }
        }
    }
}

fn normalized_lines(markdown: &str) -> Vec<String> {
    markdown
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(|line| line.trim_end().to_string())
        .collect()
}

fn heading(line: &str) -> Option<(u8, String)> {
    let trimmed = line.trim_start();
    let count = trimmed
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if !(1..=6).contains(&count)
        || !trimmed[count..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
    {
        return None;
    }
    let text = trimmed[count..]
        .trim()
        .trim_end_matches('#')
        .trim_end()
        .to_string();
    Some((count as u8, text))
}

fn fence_start(line: &str) -> Option<(char, usize, String)> {
    let trimmed = line.trim_start();
    let marker = trimmed.chars().next()?;
    if marker != '`' && marker != '~' {
        return None;
    }
    let count = trimmed
        .chars()
        .take_while(|character| *character == marker)
        .count();
    if count < 3 {
        return None;
    }
    Some((marker, count, trimmed[count..].trim().to_string()))
}

fn fence_end(line: &str, marker: char, minimum: usize) -> bool {
    let trimmed = line.trim();
    trimmed.chars().count() >= minimum && trimmed.chars().all(|character| character == marker)
}

fn footnote_start(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim_start();
    let rest = trimmed.strip_prefix("[^")?;
    let closing = rest.find("]:")?;
    let label = rest[..closing].trim();
    if label.is_empty() {
        return None;
    }
    Some((
        label.to_string(),
        rest[closing + 2..].trim_start().to_string(),
    ))
}

fn is_thematic_break(line: &str) -> bool {
    let compact = line
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    compact.len() >= 3
        && compact.chars().next().is_some_and(|marker| {
            matches!(marker, '-' | '*' | '_') && compact.chars().all(|c| c == marker)
        })
}

fn is_list_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if ["- ", "* ", "+ "]
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
    {
        return true;
    }
    let digit_count = trimmed
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .count();
    let mut remainder = trimmed[digit_count..].chars();
    digit_count > 0
        && remainder
            .next()
            .is_some_and(|character| matches!(character, '.' | ')'))
        && remainder.next().is_some_and(char::is_whitespace)
}

fn is_table_delimiter(line: &str) -> bool {
    let trimmed = line.trim().trim_matches('|');
    let cells = trimmed.split('|').map(str::trim).collect::<Vec<_>>();
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let value = cell.trim_matches(':');
            value.len() >= 3 && value.chars().all(|character| character == '-')
        })
}

fn starts_new_block(lines: &[String], index: usize) -> bool {
    let line = &lines[index];
    if line.trim().is_empty()
        || heading(line).is_some()
        || fence_start(line).is_some()
        || footnote_start(line).is_some()
        || is_thematic_break(line)
        || line.trim_start().starts_with('>')
        || is_list_line(line)
        || line.trim_start().starts_with('<')
    {
        return true;
    }
    line.contains('|')
        && lines
            .get(index + 1)
            .is_some_and(|candidate| is_table_delimiter(candidate))
}

pub fn parse_markdown(markdown: &str) -> MarkdownAst {
    let mut lines = normalized_lines(markdown);
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }

    let mut blocks = Vec::new();
    let mut errors = Vec::new();
    let mut index = 0;

    if lines.first().is_some_and(|line| line.trim() == "---") {
        let start = index;
        index += 1;
        while index < lines.len() && lines[index].trim() != "---" {
            index += 1;
        }
        if index < lines.len() {
            index += 1;
            blocks.push(MarkdownBlock::Frontmatter(lines[start..index].to_vec()));
        } else {
            errors.push(MarkdownParseError {
                line: 1,
                code: "unclosed-frontmatter",
                message: "YAML frontmatter 缺少结束分隔符".to_string(),
            });
            blocks.push(MarkdownBlock::Frontmatter(lines[start..].to_vec()));
            index = lines.len();
        }
    }

    while index < lines.len() {
        if lines[index].trim().is_empty() {
            index += 1;
            continue;
        }

        if let Some((marker, count, info)) = fence_start(&lines[index]) {
            let opening_line = index + 1;
            let fence = marker.to_string().repeat(count);
            index += 1;
            let body_start = index;
            while index < lines.len() && !fence_end(&lines[index], marker, count) {
                index += 1;
            }
            let body = lines[body_start..index].to_vec();
            let closed = index < lines.len();
            if closed {
                index += 1;
            } else {
                errors.push(MarkdownParseError {
                    line: opening_line,
                    code: "unclosed-code-fence",
                    message: "代码块缺少结束围栏".to_string(),
                });
            }
            blocks.push(MarkdownBlock::CodeFence {
                fence,
                info,
                body,
                closed,
            });
            continue;
        }

        if let Some((level, text)) = heading(&lines[index]) {
            blocks.push(MarkdownBlock::Heading { level, text });
            index += 1;
            continue;
        }

        if let Some((label, first)) = footnote_start(&lines[index]) {
            index += 1;
            let mut body = vec![first];
            while index < lines.len()
                && (lines[index].starts_with("    ") || lines[index].starts_with('\t'))
            {
                body.push(lines[index].trim_start().to_string());
                index += 1;
            }
            blocks.push(MarkdownBlock::FootnoteDefinition { label, body });
            continue;
        }

        if is_thematic_break(&lines[index]) {
            blocks.push(MarkdownBlock::ThematicBreak);
            index += 1;
            continue;
        }

        if lines[index].trim_start().starts_with('>') {
            let start = index;
            while index < lines.len() && lines[index].trim_start().starts_with('>') {
                index += 1;
            }
            blocks.push(MarkdownBlock::Blockquote(lines[start..index].to_vec()));
            continue;
        }

        if is_list_line(&lines[index]) {
            let start = index;
            index += 1;
            while index < lines.len()
                && (is_list_line(&lines[index])
                    || lines[index].starts_with("  ")
                    || lines[index].starts_with('\t'))
            {
                index += 1;
            }
            blocks.push(MarkdownBlock::List(lines[start..index].to_vec()));
            continue;
        }

        if lines[index].contains('|')
            && lines
                .get(index + 1)
                .is_some_and(|candidate| is_table_delimiter(candidate))
        {
            let start = index;
            index += 2;
            while index < lines.len()
                && lines[index].contains('|')
                && !lines[index].trim().is_empty()
            {
                index += 1;
            }
            blocks.push(MarkdownBlock::Table(lines[start..index].to_vec()));
            continue;
        }

        if lines[index].trim_start().starts_with('<') {
            let start = index;
            index += 1;
            while index < lines.len()
                && !lines[index].trim().is_empty()
                && !starts_new_block(&lines, index)
            {
                index += 1;
            }
            blocks.push(MarkdownBlock::Html(lines[start..index].to_vec()));
            continue;
        }

        let start = index;
        index += 1;
        while index < lines.len() && !starts_new_block(&lines, index) {
            index += 1;
        }
        blocks.push(MarkdownBlock::Paragraph(lines[start..index].to_vec()));
    }

    MarkdownAst { blocks, errors }
}

pub fn serialize_markdown(ast: &MarkdownAst) -> String {
    if ast.blocks.is_empty() {
        return String::new();
    }
    let mut output = ast
        .blocks
        .iter()
        .map(MarkdownBlock::to_markdown)
        .collect::<Vec<_>>()
        .join("\n\n");
    output.push('\n');
    output
}

pub fn canonicalize_markdown(markdown: &str) -> Result<String, Vec<MarkdownParseError>> {
    let ast = parse_markdown(markdown);
    if ast.errors.is_empty() {
        Ok(serialize_markdown(&ast))
    } else {
        Err(ast.errors)
    }
}

pub fn creation_blocks_from_markdown(markdown: &str) -> Vec<CreationBlock> {
    let ast = parse_markdown(markdown);
    let mut cursor = 0_usize;
    ast.blocks
        .iter()
        .enumerate()
        .map(|(index, block)| {
            let kind = block.block_kind();
            let source = block.to_markdown();
            let start = cursor;
            let end = start + source.len();
            cursor = end + 2;
            let mut attributes = std::collections::BTreeMap::new();
            let level = match block {
                MarkdownBlock::Heading { level, .. } => Some(*level),
                _ => None,
            };
            if let MarkdownBlock::CodeFence { info, .. } = block {
                if !info.is_empty() {
                    attributes.insert("language".to_string(), json!(info));
                }
            }
            if let MarkdownBlock::FootnoteDefinition { label, .. } = block {
                attributes.insert("footnoteLabel".to_string(), json!(label));
            }
            CreationBlock {
                id: format!("block-{:04}-{kind}", index + 1),
                kind: kind.to_string(),
                level,
                component_id: None,
                asset_id: None,
                source_range: SourceRange { start, end },
                children: Vec::new(),
                text_hash: Some(format!("sha256:{:x}", Sha256::digest(source.as_bytes()))),
                attributes,
            }
        })
        .collect()
}
