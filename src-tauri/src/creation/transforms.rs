//! Deterministic transforms applied after AI generation.

use std::collections::{BTreeMap, BTreeSet};

use regex::{Captures, Regex};

use super::{
    markdown::{parse_markdown, serialize_markdown, MarkdownAst, MarkdownBlock},
    model::{CreationLayout, TransformReport},
};

const TOC_START: &str = "<!-- yunspire:toc:start -->";
const TOC_END: &str = "<!-- yunspire:toc:end -->";

fn parse_valid(markdown: &str) -> Result<MarkdownAst, String> {
    let ast = parse_markdown(markdown);
    if ast.errors.is_empty() {
        Ok(ast)
    } else {
        Err(ast
            .errors
            .iter()
            .map(|error| format!("第 {} 行：{}", error.line, error.message))
            .collect::<Vec<_>>()
            .join("；"))
    }
}

fn strip_generated_number(text: &str) -> String {
    let prefix = Regex::new(r"^\d+(?:\.\d+)*[.、]?\s+").expect("valid numbering regex");
    prefix.replace(text.trim(), "").trim().to_string()
}

pub fn apply_section_numbering(markdown: &str, start_level: u8) -> Result<String, String> {
    let mut ast = parse_valid(markdown)?;
    let start_level = start_level.clamp(1, 6);
    let mut counters = [0_u32; 7];

    for block in &mut ast.blocks {
        let MarkdownBlock::Heading { level, text } = block else {
            continue;
        };
        if *level < start_level {
            continue;
        }

        let level_index = *level as usize;
        for counter in counters
            .iter_mut()
            .take(level_index)
            .skip(start_level as usize)
        {
            if *counter == 0 {
                *counter = 1;
            }
        }
        counters[level_index] += 1;
        for counter in counters.iter_mut().skip(level_index + 1) {
            *counter = 0;
        }

        let number = counters[start_level as usize..=level_index]
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(".");
        *text = format!("{number} {}", strip_generated_number(text));
    }

    Ok(serialize_markdown(&ast))
}

fn strip_existing_toc(markdown: &str) -> Result<String, String> {
    let lines = markdown
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut output = Vec::with_capacity(lines.len());
    let mut in_toc = false;
    let mut found_end = false;
    for line in lines {
        if line.trim() == TOC_START {
            if in_toc {
                return Err("目录标记发生嵌套".to_string());
            }
            in_toc = true;
            found_end = false;
            continue;
        }
        if line.trim() == TOC_END {
            if !in_toc {
                return Err("目录结束标记没有对应的开始标记".to_string());
            }
            in_toc = false;
            found_end = true;
            continue;
        }
        if !in_toc {
            output.push(line);
        }
    }
    if in_toc && !found_end {
        return Err("目录开始标记没有对应的结束标记".to_string());
    }
    Ok(output.join("\n"))
}

fn heading_slug(text: &str, seen: &mut BTreeMap<String, usize>) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for character in text.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() || is_cjk(character) {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(character);
        } else if character.is_whitespace() || matches!(character, '-' | '_' | '.') {
            pending_dash = true;
        }
    }
    if slug.is_empty() {
        slug.push_str("section");
    }
    let count = seen.entry(slug.clone()).or_insert(0);
    *count += 1;
    if *count > 1 {
        slug.push('-');
        slug.push_str(&count.to_string());
    }
    slug
}

pub fn insert_table_of_contents(
    markdown: &str,
    max_level: u8,
    title: &str,
) -> Result<(String, bool), String> {
    let without_toc = strip_existing_toc(markdown)?;
    let mut ast = parse_valid(&without_toc)?;
    let max_level = max_level.clamp(2, 6);
    let mut seen = BTreeMap::new();
    let headings = ast
        .blocks
        .iter()
        .filter_map(|block| match block {
            MarkdownBlock::Heading { level, text } if (2..=max_level).contains(level) => {
                Some((*level, text.clone(), heading_slug(text, &mut seen)))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if headings.is_empty() {
        return Ok((serialize_markdown(&ast), false));
    }

    let base_level = headings
        .iter()
        .map(|(level, _, _)| *level)
        .min()
        .unwrap_or(2);
    let list = headings
        .into_iter()
        .map(|(level, text, slug)| {
            format!(
                "{}- [{}](#{})",
                "  ".repeat(level.saturating_sub(base_level) as usize),
                text,
                slug
            )
        })
        .collect::<Vec<_>>();

    let insertion_index = ast
        .blocks
        .iter()
        .position(|block| matches!(block, MarkdownBlock::Heading { level: 1, .. }))
        .map(|index| index + 1)
        .or_else(|| {
            ast.blocks
                .first()
                .filter(|block| matches!(block, MarkdownBlock::Frontmatter(_)))
                .map(|_| 1)
        })
        .unwrap_or(0);
    let toc_blocks = vec![
        MarkdownBlock::Html(vec![TOC_START.to_string()]),
        MarkdownBlock::Heading {
            level: 2,
            text: title.trim().to_string(),
        },
        MarkdownBlock::List(list),
        MarkdownBlock::Html(vec![TOC_END.to_string()]),
    ];
    ast.blocks
        .splice(insertion_index..insertion_index, toc_blocks);
    Ok((serialize_markdown(&ast), true))
}

fn map_block_lines(block: &mut MarkdownBlock, transform: &mut impl FnMut(&str) -> String) {
    match block {
        MarkdownBlock::Heading { text, .. } => *text = transform(text),
        MarkdownBlock::Paragraph(lines)
        | MarkdownBlock::Blockquote(lines)
        | MarkdownBlock::List(lines)
        | MarkdownBlock::Table(lines) => {
            for line in lines {
                *line = transform(line);
            }
        }
        MarkdownBlock::FootnoteDefinition { body, .. } => {
            for line in body {
                *line = transform(line);
            }
        }
        MarkdownBlock::Frontmatter(_)
        | MarkdownBlock::CodeFence { .. }
        | MarkdownBlock::ThematicBreak
        | MarkdownBlock::Html(_) => {}
    }
}

fn normalized_label_prefix(prefix: &str) -> String {
    let normalized = prefix
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
        .collect::<String>();
    if normalized.is_empty() {
        "source".to_string()
    } else {
        normalized
    }
}

pub fn external_links_to_footnotes(markdown: &str, prefix: &str) -> Result<String, String> {
    let mut ast = parse_valid(markdown)?;
    let link =
        Regex::new(r"(!?)\[([^\]\n]+)\]\((https?://[^)\s]+)\)").expect("valid external link regex");
    let mut used_labels = ast
        .blocks
        .iter()
        .filter_map(|block| match block {
            MarkdownBlock::FootnoteDefinition { label, .. } => Some(label.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let prefix = normalized_label_prefix(prefix);
    let mut links_by_url = BTreeMap::<String, String>::new();
    let mut definitions = Vec::<(String, String)>::new();
    let mut sequence = 1_u32;

    for block in &mut ast.blocks {
        map_block_lines(block, &mut |line_text| {
            link.replace_all(line_text, |captures: &Captures<'_>| {
                if captures.get(1).is_some_and(|value| value.as_str() == "!") {
                    return captures[0].to_string();
                }
                let text = captures.get(2).map_or("", |value| value.as_str());
                let url = captures.get(3).map_or("", |value| value.as_str());
                let label = links_by_url.entry(url.to_string()).or_insert_with(|| loop {
                    let candidate = format!("{prefix}-{sequence}");
                    sequence += 1;
                    if used_labels.insert(candidate.clone()) {
                        definitions.push((candidate.clone(), url.to_string()));
                        break candidate;
                    }
                });
                format!("[{text}][^{label}]")
            })
            .into_owned()
        });
    }
    ast.blocks
        .extend(
            definitions
                .into_iter()
                .map(|(label, url)| MarkdownBlock::FootnoteDefinition {
                    label,
                    body: vec![url],
                }),
        );
    Ok(serialize_markdown(&ast))
}

pub fn remove_external_links(markdown: &str) -> Result<String, String> {
    let mut ast = parse_valid(markdown)?;
    let link =
        Regex::new(r"(!?)\[([^\]\n]+)\]\((https?://[^)\s]+)\)").expect("valid external link regex");
    for block in &mut ast.blocks {
        map_block_lines(block, &mut |line_text| {
            link.replace_all(line_text, |captures: &Captures<'_>| {
                captures
                    .get(2)
                    .map_or("", |value| value.as_str())
                    .to_string()
            })
            .into_owned()
        });
    }
    Ok(serialize_markdown(&ast))
}

fn is_cjk(character: char) -> bool {
    matches!(
        character as u32,
        0x2e80..=0x2eff
            | 0x3040..=0x30ff
            | 0x31c0..=0x31ef
            | 0x3400..=0x4dbf
            | 0x4e00..=0x9fff
            | 0xac00..=0xd7af
            | 0xf900..=0xfaff
            | 0x20000..=0x3134f
    )
}

fn add_cjk_ascii_spacing(value: &str) -> String {
    let characters = value.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(value.len() + value.len() / 16);
    for (index, character) in characters.iter().copied().enumerate() {
        if let Some(previous) = index
            .checked_sub(1)
            .and_then(|previous| characters.get(previous))
        {
            if (is_cjk(*previous) && character.is_ascii_alphanumeric())
                || (previous.is_ascii_alphanumeric() && is_cjk(character))
            {
                output.push(' ');
            }
        }
        output.push(character);
    }
    output
}

fn add_cjk_spacing_preserving_syntax(value: &str) -> String {
    let protected = Regex::new(
        r#"!?(?:\[[^\]]*\]\([^)]*\)|\[[^\]]*\]\[\^[^\]]+\])|`[^`]*`|https?://[^\s<>]+|<[^>]+>"#,
    )
    .expect("valid protected Markdown syntax regex");
    let mut output = String::with_capacity(value.len() + value.len() / 16);
    let mut cursor = 0;
    for matched in protected.find_iter(value) {
        output.push_str(&add_cjk_ascii_spacing(&value[cursor..matched.start()]));
        output.push_str(matched.as_str());
        cursor = matched.end();
    }
    output.push_str(&add_cjk_ascii_spacing(&value[cursor..]));
    output
}

pub fn apply_cjk_spacing(markdown: &str) -> Result<String, String> {
    let mut ast = parse_valid(markdown)?;
    for block in &mut ast.blocks {
        map_block_lines(block, &mut add_cjk_spacing_preserving_syntax);
    }
    Ok(serialize_markdown(&ast))
}

pub fn apply_layout_transforms(
    markdown: &str,
    layout: &CreationLayout,
) -> Result<(String, TransformReport), String> {
    let original = markdown.to_string();
    let mut output = if layout.features.table_of_contents {
        strip_existing_toc(markdown)?
    } else {
        markdown.to_string()
    };
    let mut report = TransformReport::default();

    if layout.features.external_links == "footnote" {
        output = external_links_to_footnotes(&output, "source")?;
        report
            .applied
            .push("external-links-to-footnotes".to_string());
    } else if layout.features.external_links == "remove" {
        output = remove_external_links(&output)?;
        report.applied.push("remove-external-links".to_string());
    }
    if layout.features.auto_numbering {
        output = apply_section_numbering(&output, 2)?;
        report.applied.push("section-numbering".to_string());
    }
    if layout.features.table_of_contents {
        let (with_toc, inserted) = insert_table_of_contents(&output, 3, "目录")?;
        output = with_toc;
        report.applied.push("table-of-contents".to_string());
        if !inserted {
            report
                .warnings
                .push("没有可用于生成目录的二级至六级标题".to_string());
        }
    }
    if layout.features.cjk_spacing {
        output = apply_cjk_spacing(&output)?;
        report.applied.push("cjk-spacing".to_string());
    }
    report.changed = output != original;
    Ok((output, report))
}
