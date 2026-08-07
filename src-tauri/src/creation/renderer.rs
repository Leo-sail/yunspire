//! Deterministic Markdown-to-HTML renderer and final HTML allowlist gate.

use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;

use super::{
    markdown::{parse_markdown, MarkdownBlock},
    model::{CreationDocumentV2, ValidationIssue},
};

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn safe_url(value: &str, image: bool) -> bool {
    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_control) {
        return false;
    }
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        return true;
    }
    if image
        && [
            "data:image/png;base64,",
            "data:image/jpeg;base64,",
            "data:image/gif;base64,",
            "data:image/webp;base64,",
        ]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
    {
        return true;
    }
    value.starts_with('#')
        || value.starts_with('/')
        || value.starts_with("./")
        || (!value.contains(':') && !value.starts_with(".."))
}

fn delimited<'a>(source: &'a str, opening: &str, closing: &str) -> Option<(&'a str, usize)> {
    let rest = source.strip_prefix(opening)?;
    let end = rest.find(closing)?;
    Some((&rest[..end], opening.len() + end + closing.len()))
}

fn markdown_link(source: &str, image: bool) -> Option<(&str, &str, usize)> {
    let prefix = if image { "![" } else { "[" };
    let rest = source.strip_prefix(prefix)?;
    let label_end = rest.find("](")?;
    let after_label = &rest[label_end + 2..];
    let url_end = after_label.find(')')?;
    Some((
        &rest[..label_end],
        &after_label[..url_end],
        prefix.len() + label_end + 2 + url_end + 1,
    ))
}

fn render_inline(source: &str) -> String {
    let mut output = String::with_capacity(source.len() + source.len() / 4);
    let mut cursor = 0;
    while cursor < source.len() {
        let rest = &source[cursor..];
        if let Some((alt, url, consumed)) = markdown_link(rest, true) {
            if safe_url(url, true) {
                output.push_str(&format!(
                    "<img src=\"{}\" alt=\"{}\">",
                    escape_html(url),
                    escape_html(alt)
                ));
                cursor += consumed;
                continue;
            }
        }
        if let Some((label, url, consumed)) = markdown_link(rest, false) {
            if safe_url(url, false) {
                output.push_str(&format!(
                    "<a href=\"{}\">{}</a>",
                    escape_html(url),
                    render_inline(label)
                ));
                cursor += consumed;
                continue;
            }
        }
        if let Some((code, consumed)) = delimited(rest, "`", "`") {
            output.push_str("<code>");
            output.push_str(&escape_html(code));
            output.push_str("</code>");
            cursor += consumed;
            continue;
        }
        if let Some((strong, consumed)) = delimited(rest, "**", "**") {
            output.push_str("<strong>");
            output.push_str(&render_inline(strong));
            output.push_str("</strong>");
            cursor += consumed;
            continue;
        }
        if let Some((emphasis, consumed)) = delimited(rest, "*", "*") {
            output.push_str("<em>");
            output.push_str(&render_inline(emphasis));
            output.push_str("</em>");
            cursor += consumed;
            continue;
        }
        if let Some(label_end) = rest.find("][^") {
            if rest.starts_with('[') {
                let label = &rest[1..label_end];
                let reference = &rest[label_end + 3..];
                if let Some(reference_end) = reference.find(']') {
                    output.push_str(&render_inline(label));
                    output.push_str("<sup>");
                    output.push_str(&escape_html(&reference[..reference_end]));
                    output.push_str("</sup>");
                    cursor += label_end + 3 + reference_end + 1;
                    continue;
                }
            }
        }
        if let Some(reference) = rest.strip_prefix("[^") {
            if let Some(end) = reference.find(']') {
                output.push_str("<sup>");
                output.push_str(&escape_html(&reference[..end]));
                output.push_str("</sup>");
                cursor += end + 3;
                continue;
            }
        }
        let character = rest.chars().next().expect("cursor is on a char boundary");
        output.push_str(&escape_html(&character.to_string()));
        cursor += character.len_utf8();
    }
    output
}

fn heading_slug(text: &str, seen: &mut BTreeMap<String, usize>) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for character in text.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
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

fn list_item_text(line: &str) -> &str {
    let trimmed = line.trim_start();
    if let Some(rest) = ["- ", "* ", "+ "]
        .iter()
        .find_map(|prefix| trimmed.strip_prefix(prefix))
    {
        return rest;
    }
    let digits = trimmed
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .count();
    let remainder = &trimmed[digits..];
    remainder
        .strip_prefix('.')
        .or_else(|| remainder.strip_prefix(')'))
        .map(str::trim_start)
        .unwrap_or(trimmed)
}

fn table_cells(line: &str) -> Vec<&str> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .collect()
}

pub fn render_document_html(document: &CreationDocumentV2) -> Result<String, String> {
    let ast = parse_markdown(&document.canonical_markdown);
    if !ast.errors.is_empty() {
        return Err(ast
            .errors
            .iter()
            .map(|error| format!("第 {} 行：{}", error.line, error.message))
            .collect::<Vec<_>>()
            .join("；"));
    }
    let mut output = String::from("<article>");
    let mut slugs = BTreeMap::new();
    for block in ast.blocks {
        match block {
            MarkdownBlock::Frontmatter(_) => {}
            MarkdownBlock::Heading { level, text } => {
                let slug = heading_slug(&text, &mut slugs);
                output.push_str(&format!(
                    "<h{level} id=\"{}\">{}</h{level}>",
                    escape_html(&slug),
                    render_inline(&text)
                ));
            }
            MarkdownBlock::Paragraph(lines) => {
                output.push_str("<p>");
                output.push_str(
                    &lines
                        .iter()
                        .map(|line| render_inline(line))
                        .collect::<Vec<_>>()
                        .join("<br>"),
                );
                output.push_str("</p>");
            }
            MarkdownBlock::Blockquote(lines) => {
                output.push_str("<blockquote>");
                output.push_str(
                    &lines
                        .iter()
                        .map(|line| {
                            render_inline(line.trim_start().trim_start_matches('>').trim_start())
                        })
                        .collect::<Vec<_>>()
                        .join("<br>"),
                );
                output.push_str("</blockquote>");
            }
            MarkdownBlock::List(lines) => {
                let ordered = lines.first().is_some_and(|line| {
                    line.trim_start()
                        .chars()
                        .next()
                        .is_some_and(|character| character.is_ascii_digit())
                });
                let tag = if ordered { "ol" } else { "ul" };
                output.push_str(&format!("<{tag}>"));
                for line in lines.iter().filter(|line| !line.trim().is_empty()) {
                    output.push_str("<li>");
                    output.push_str(&render_inline(list_item_text(line)));
                    output.push_str("</li>");
                }
                output.push_str(&format!("</{tag}>"));
            }
            MarkdownBlock::CodeFence { body, .. } => {
                output.push_str("<pre><code>");
                output.push_str(&escape_html(&body.join("\n")));
                output.push_str("</code></pre>");
            }
            MarkdownBlock::Table(lines) => {
                let mut rows = lines.into_iter();
                let header = rows
                    .next()
                    .map(|line| {
                        table_cells(&line)
                            .into_iter()
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let _delimiter = rows.next();
                output.push_str("<table><thead><tr>");
                for cell in header {
                    output.push_str("<th>");
                    output.push_str(&render_inline(&cell));
                    output.push_str("</th>");
                }
                output.push_str("</tr></thead><tbody>");
                for row in rows {
                    output.push_str("<tr>");
                    for cell in table_cells(&row) {
                        output.push_str("<td>");
                        output.push_str(&render_inline(cell));
                        output.push_str("</td>");
                    }
                    output.push_str("</tr>");
                }
                output.push_str("</tbody></table>");
            }
            MarkdownBlock::ThematicBreak => output.push_str("<hr>"),
            MarkdownBlock::FootnoteDefinition { label, body } => {
                output.push_str("<p><sup>");
                output.push_str(&escape_html(&label));
                output.push_str("</sup> ");
                output.push_str(
                    &body
                        .iter()
                        .map(|line| render_inline(line))
                        .collect::<Vec<_>>()
                        .join("<br>"),
                );
                output.push_str("</p>");
            }
            MarkdownBlock::Html(lines) => {
                if lines.iter().all(|line| line.trim().starts_with("<!--")) {
                    continue;
                }
                output.push_str("<p>");
                output.push_str(&escape_html(&lines.join("\n")));
                output.push_str("</p>");
            }
        }
    }
    output.push_str("</article>");
    Ok(output)
}

fn issue(code: &str, message: impl Into<String>) -> ValidationIssue {
    ValidationIssue {
        code: code.to_string(),
        severity: "error".to_string(),
        message: message.into(),
        block_id: None,
    }
}

pub fn validate_html_whitelist(html: &str) -> Vec<ValidationIssue> {
    let allowed_tags = [
        "a",
        "article",
        "blockquote",
        "br",
        "code",
        "em",
        "figcaption",
        "figure",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "hr",
        "img",
        "li",
        "ol",
        "p",
        "pre",
        "section",
        "span",
        "strong",
        "sup",
        "table",
        "tbody",
        "td",
        "th",
        "thead",
        "tr",
        "ul",
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    let tag_pattern =
        Regex::new(r"(?is)<\s*(/?)\s*([a-z][a-z0-9-]*)\b([^>]*)>").expect("valid HTML tag regex");
    let attribute_pattern = Regex::new(
        r#"(?i)([a-z_:][a-z0-9_.:-]*)(?:\s*=\s*(?:\"([^\"]*)\"|'([^']*)'|([^\s\"'=<>`]+)))?"#,
    )
    .expect("valid HTML attribute regex");
    let mut issues = Vec::new();
    let mut open_tags = Vec::<String>::new();

    let lower = html.to_ascii_lowercase();
    if lower.contains("javascript:") || lower.contains("vbscript:") {
        issues.push(issue("html.unsafe-url", "HTML 包含不安全的脚本 URL"));
    }
    if lower.contains("<script") || lower.contains("<style") || lower.contains("<link") {
        issues.push(issue(
            "html.forbidden-element",
            "HTML 包含脚本或外部样式元素",
        ));
    }

    for captures in tag_pattern.captures_iter(html) {
        let closing = captures
            .get(1)
            .is_some_and(|value| !value.as_str().is_empty());
        let tag = captures
            .get(2)
            .map(|value| value.as_str().to_ascii_lowercase())
            .unwrap_or_default();
        if !allowed_tags.contains(tag.as_str()) {
            issues.push(issue(
                "html.tag-not-allowed",
                format!("HTML 标签 `<{tag}>` 不在白名单中"),
            ));
            continue;
        }
        if closing {
            if matches!(tag.as_str(), "br" | "hr" | "img") {
                issues.push(issue(
                    "html.invalid-closing-tag",
                    format!("空元素 `<{tag}>` 不应有结束标签"),
                ));
            } else if open_tags.pop().as_deref() != Some(tag.as_str()) {
                issues.push(issue(
                    "html.unbalanced-tags",
                    format!("HTML 结束标签 `</{tag}>` 与当前结构不匹配"),
                ));
            }
            continue;
        }
        let raw_attributes = captures.get(3).map_or("", |value| value.as_str());
        let mut consumed = 0;
        for attribute in attribute_pattern.captures_iter(raw_attributes) {
            let matched = attribute.get(0).expect("attribute match exists");
            if !raw_attributes[consumed..matched.start()]
                .trim_matches(|character: char| character.is_whitespace() || character == '/')
                .is_empty()
            {
                issues.push(issue(
                    "html.malformed-attribute",
                    "HTML 属性语法无法安全解析",
                ));
            }
            consumed = matched.end();
            let name = attribute
                .get(1)
                .map(|value| value.as_str().to_ascii_lowercase())
                .unwrap_or_default();
            let value = attribute
                .get(2)
                .or_else(|| attribute.get(3))
                .or_else(|| attribute.get(4))
                .map_or("", |value| value.as_str());
            let allowed = match tag.as_str() {
                "a" => matches!(name.as_str(), "href" | "title"),
                "img" => matches!(name.as_str(), "src" | "alt" | "width" | "height"),
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => name == "id",
                "ol" => name == "start",
                "td" | "th" => matches!(name.as_str(), "colspan" | "rowspan"),
                _ => false,
            };
            if name.starts_with("on") || !allowed {
                issues.push(issue(
                    "html.attribute-not-allowed",
                    format!("标签 `<{tag}>` 不允许属性 `{name}`"),
                ));
            }
            if matches!(name.as_str(), "href" | "src") && !safe_url(value, name == "src") {
                issues.push(issue(
                    "html.unsafe-url",
                    format!("标签 `<{tag}>` 包含不安全 URL"),
                ));
            }
        }
        if !raw_attributes[consumed..]
            .trim_matches(|character: char| character.is_whitespace() || character == '/')
            .is_empty()
        {
            issues.push(issue(
                "html.malformed-attribute",
                "HTML 属性语法无法安全解析",
            ));
        }
        if !matches!(tag.as_str(), "br" | "hr" | "img") {
            open_tags.push(tag);
        }
    }
    if !open_tags.is_empty() {
        issues.push(issue(
            "html.unbalanced-tags",
            format!("HTML 仍有未闭合标签：{}", open_tags.join(", ")),
        ));
    }
    issues.sort_by(|left, right| {
        left.code
            .cmp(&right.code)
            .then_with(|| left.message.cmp(&right.message))
    });
    issues.dedup_by(|left, right| left.code == right.code && left.message == right.message);
    issues
}
