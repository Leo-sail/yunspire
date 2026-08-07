//! Creation-document AST, cross-reference, HTML and publication-readiness gates.

use std::collections::BTreeSet;

use chrono::Utc;
use sha2::{Digest, Sha256};

use super::{
    assets::{asset_is_visual, is_valid_record_id, validate_asset},
    markdown::parse_markdown,
    model::{
        CreationDocumentV2, ReadinessAssetSummary, ReadinessCheck, ReadinessReport,
        ReadinessValidation, ValidationIssue, ValidationReceipt, ValidationReport,
        CREATION_RUNTIME_VERSION, CREATION_SCHEMA_VERSION, MANIFEST_SCHEMA_VERSION,
    },
    renderer::{render_document_html, validate_html_whitelist},
    theme::{component_exists, theme_exists},
    transforms::apply_cjk_spacing,
};

const BLOCK_KINDS: &[&str] = &[
    "heading",
    "paragraph",
    "list",
    "quote",
    "code",
    "table",
    "image",
    "component",
    "divider",
    "html",
];
const LAYOUT_TARGETS: &[&str] = &["wechatRichText", "markdown", "html", "multiTarget"];
const PUBLISHING_TARGETS: &[&str] = &["obsidian", "wechat", "html", "markdown", "pdf", "image"];
const CONTENT_TYPES: &[&str] = &["article", "wechat", "xiaohongshu", "contract", "paper"];

fn issue(
    code: &str,
    severity: &str,
    message: impl Into<String>,
    block_id: Option<&str>,
) -> ValidationIssue {
    ValidationIssue {
        code: code.to_string(),
        severity: severity.to_string(),
        message: message.into(),
        block_id: block_id.map(str::to_string),
    }
}

fn valid_hash(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    })
}

fn is_scalar(value: &serde_json::Value) -> bool {
    matches!(
        value,
        serde_json::Value::String(_)
            | serde_json::Value::Number(_)
            | serde_json::Value::Bool(_)
            | serde_json::Value::Null
    )
}

fn valid_language(value: &str) -> bool {
    let Some((base, region)) = value.split_once('-') else {
        return (2..=3).contains(&value.len()) && value.chars().all(|c| c.is_ascii_lowercase());
    };
    (2..=3).contains(&base.len())
        && base.chars().all(|c| c.is_ascii_lowercase())
        && region.len() == 2
        && region.chars().all(|c| c.is_ascii_uppercase())
}

fn unique_nonempty(values: &[String], maximum_length: usize) -> bool {
    let mut seen = BTreeSet::new();
    values.iter().all(|value| {
        !value.is_empty() && value.chars().count() <= maximum_length && seen.insert(value)
    })
}

struct Inspection {
    issues: Vec<ValidationIssue>,
    schema_valid: bool,
    ast_valid: bool,
    html_valid: bool,
}

fn inspect_document(document: &CreationDocumentV2) -> Inspection {
    let mut issues = Vec::new();
    let mut schema_valid = true;
    let mut schema_issue = |value: ValidationIssue| {
        if value.severity == "error" {
            schema_valid = false;
        }
        issues.push(value);
    };

    if document.schema_version != CREATION_SCHEMA_VERSION {
        schema_issue(issue(
            "document.schema-version",
            "error",
            format!("仅支持 CreationDocumentV2 {CREATION_SCHEMA_VERSION}"),
            None,
        ));
    }
    if !is_valid_record_id(&document.id) {
        schema_issue(issue(
            "document.invalid-id",
            "error",
            "文稿 ID 不符合本地记录格式",
            None,
        ));
    }
    if document.revision == 0 {
        schema_issue(issue(
            "document.invalid-revision",
            "error",
            "文稿修订号必须大于或等于 1",
            None,
        ));
    }
    if document.title.is_empty() || document.title.chars().count() > 240 {
        schema_issue(issue(
            "document.invalid-title",
            "error",
            "文稿标题不能为空且不能超过 240 个字符",
            None,
        ));
    }
    if document.canonical_format != "markdown" {
        schema_issue(issue(
            "document.canonical-format",
            "error",
            "权威正文格式必须是 markdown",
            None,
        ));
    }
    if !CONTENT_TYPES.contains(&document.content_type.as_str()) {
        schema_issue(issue(
            "document.invalid-content-type",
            "error",
            "内容类型必须是 article、wechat、xiaohongshu、contract 或 paper",
            None,
        ));
    }
    let markdown_lower = document.canonical_markdown.to_ascii_lowercase();
    if markdown_lower.contains("data:image/") && markdown_lower.contains(";base64,") {
        schema_issue(issue(
            "document.embedded-binary-asset",
            "error",
            "Markdown 正文不能内嵌 Base64 图片；请先将图片保存为耐久素材并使用相对路径引用",
            None,
        ));
    }
    if !theme_exists(&document.layout.theme_id, &document.layout.theme_version) {
        schema_issue(issue(
            "layout.unknown-theme",
            "error",
            format!(
                "主题 `{}@{}` 不在第一方目录中",
                document.layout.theme_id, document.layout.theme_version
            ),
            None,
        ));
    }
    if !LAYOUT_TARGETS.contains(&document.layout.target.as_str()) {
        schema_issue(issue(
            "layout.invalid-target",
            "error",
            "布局目标无效",
            None,
        ));
    }
    let typography = &document.layout.typography;
    if typography.font_family.is_empty()
        || typography.font_size < 10
        || typography.font_size > 32
        || !(1.0..=3.0).contains(&typography.line_height)
        || typography
            .heading_scale
            .is_some_and(|scale| !(0.5..=3.0).contains(&scale))
    {
        schema_issue(issue(
            "layout.invalid-typography",
            "error",
            "排版字体、字号或行高超出允许范围",
            None,
        ));
    }
    if document.layout.tokens.len() > 100
        || document
            .layout
            .tokens
            .values()
            .any(|value| !is_scalar(value))
    {
        schema_issue(issue(
            "layout.invalid-tokens",
            "error",
            "布局 token 只能包含最多 100 个标量值",
            None,
        ));
    }
    if !["preserve", "footnote", "remove"]
        .contains(&document.layout.features.external_links.as_str())
    {
        schema_issue(issue(
            "layout.invalid-external-links",
            "error",
            "外链策略必须是 preserve、footnote 或 remove",
            None,
        ));
    }

    if !valid_language(&document.metadata.language)
        || !unique_nonempty(&document.metadata.tags, 120)
        || !unique_nonempty(&document.metadata.wiki_links, 500)
        || document.metadata.properties.len() > 100
        || document
            .metadata
            .properties
            .values()
            .any(|value| !is_scalar(value))
    {
        schema_issue(issue(
            "metadata.invalid",
            "error",
            "语言、标签、属性或 Wiki 链接不符合契约",
            None,
        ));
    }

    let mut block_ids = BTreeSet::new();
    let mut referenced_assets = BTreeSet::new();
    for block in &document.blocks {
        if !is_valid_record_id(&block.id) || !block_ids.insert(block.id.as_str()) {
            schema_issue(issue(
                "block.invalid-id",
                "error",
                format!("块 ID `{}` 无效或重复", block.id),
                Some(&block.id),
            ));
        }
        if !BLOCK_KINDS.contains(&block.kind.as_str()) {
            schema_issue(issue(
                "block.invalid-kind",
                "error",
                format!("块 `{}` 的类型 `{}` 不受支持", block.id, block.kind),
                Some(&block.id),
            ));
        }
        if block.kind == "heading" && !matches!(block.level, Some(1..=6)) {
            schema_issue(issue(
                "block.invalid-heading-level",
                "error",
                format!("标题块 `{}` 缺少有效层级", block.id),
                Some(&block.id),
            ));
        }
        if block.source_range.start > block.source_range.end
            || block.source_range.end > document.canonical_markdown.len()
        {
            schema_issue(issue(
                "block.invalid-source-range",
                "error",
                format!("块 `{}` 的 Markdown 来源范围无效", block.id),
                Some(&block.id),
            ));
        }
        if block.kind == "component" && !block.component_id.as_deref().is_some_and(component_exists)
        {
            schema_issue(issue(
                "block.unknown-component",
                "error",
                format!("组件块 `{}` 没有可用的第一方组件", block.id),
                Some(&block.id),
            ));
        }
        if let Some(asset_id) = block.asset_id.as_deref() {
            referenced_assets.insert(asset_id);
        }
        if block
            .text_hash
            .as_deref()
            .is_some_and(|hash| !valid_hash(hash))
            || block.attributes.len() > 40
            || block.attributes.values().any(|value| !is_scalar(value))
        {
            schema_issue(issue(
                "block.invalid-derived-data",
                "error",
                format!("块 `{}` 的哈希或属性无效", block.id),
                Some(&block.id),
            ));
        }
        if let (Some(hash), Some(source)) = (
            block.text_hash.as_deref(),
            document
                .canonical_markdown
                .get(block.source_range.start..block.source_range.end),
        ) {
            let expected = format!("sha256:{:x}", Sha256::digest(source.as_bytes()));
            if hash != expected {
                schema_issue(issue(
                    "block.text-hash-mismatch",
                    "error",
                    format!("块 `{}` 的文本哈希与权威 Markdown 不一致", block.id),
                    Some(&block.id),
                ));
            }
        }
    }
    for block in &document.blocks {
        for child_id in &block.children {
            if !block_ids.contains(child_id.as_str()) {
                schema_issue(issue(
                    "block.missing-child",
                    "error",
                    format!("块 `{}` 引用了不存在的子块 `{child_id}`", block.id),
                    Some(&block.id),
                ));
            }
        }
    }

    let mut asset_ids = BTreeSet::new();
    for asset in &document.assets {
        if !asset_ids.insert(asset.id.as_str()) {
            schema_issue(issue(
                "asset.duplicate-id",
                "error",
                format!("素材 ID `{}` 重复", asset.id),
                None,
            ));
        }
        for asset_issue in validate_asset(asset) {
            schema_issue(asset_issue);
        }
    }

    let mut source_ids = BTreeSet::new();
    for source in &document.source_refs {
        let hashes_valid = source.content_hash.as_deref().is_none_or(valid_hash)
            && source.excerpt_hash.as_deref().is_none_or(valid_hash);
        if !is_valid_record_id(&source.id)
            || !source_ids.insert(source.id.as_str())
            || source.r#ref.is_empty()
            || source.r#ref.len() > 4096
            || ![
                "vaultNote",
                "knowledgeRecord",
                "url",
                "file",
                "userInput",
                "generated",
            ]
            .contains(&source.kind.as_str())
            || ![
                "direct",
                "inferred",
                "generated",
                "unverified",
                "conflicted",
            ]
            .contains(&source.trust.as_str())
            || !hashes_valid
        {
            schema_issue(issue(
                "source.invalid",
                "error",
                format!("来源 `{}` 不符合来源引用契约", source.id),
                None,
            ));
        }
    }

    let ledger = &document.grounding_ledger;
    if !["unverified", "verified", "stale", "failed"].contains(&ledger.status.as_str()) {
        schema_issue(issue(
            "grounding.invalid",
            "error",
            "证据账本状态无效",
            None,
        ));
    }
    let mut grounding_block_ids = BTreeSet::new();
    for block in &ledger.blocks {
        let block_sources_valid = unique_nonempty(&block.source_ref_ids, 160)
            && block
                .source_ref_ids
                .iter()
                .all(|source_id| source_ids.contains(source_id.as_str()));
        let evidence_valid = block.evidence.iter().all(|evidence| {
            !evidence.quote.is_empty()
                && evidence.quote.chars().count() <= 2_000
                && source_ids.contains(evidence.source_ref_id.as_str())
                && block.source_ref_ids.contains(&evidence.source_ref_id)
        });
        if !is_valid_record_id(&block.id)
            || !grounding_block_ids.insert(block.id.as_str())
            || !["supported", "unsupported", "uncertain"].contains(&block.verdict.as_str())
            || !block_sources_valid
            || !evidence_valid
            || (block.verdict == "supported" && block.evidence.is_empty())
        {
            schema_issue(issue(
                "grounding.invalid-block",
                "error",
                format!("证据账本块 `{}` 无法回溯到有效来源", block.id),
                Some(&block.id),
            ));
        }
    }
    if ledger.status == "verified" {
        let expected_hash = format!(
            "sha256:{:x}",
            Sha256::digest(document.canonical_markdown.as_bytes())
        );
        let verified_at_valid = ledger
            .verified_at
            .as_deref()
            .is_some_and(|value| chrono::DateTime::parse_from_rfc3339(value).is_ok());
        if ledger.blocks.is_empty()
            || ledger
                .blocks
                .iter()
                .any(|block| block.verdict != "supported")
            || !verified_at_valid
            || ledger.content_hash.as_deref() != Some(expected_hash.as_str())
        {
            schema_issue(issue(
                "grounding.verification-stale",
                "error",
                "已核验证据账本必须覆盖正文、绑定当前正文哈希并记录核验时间",
                None,
            ));
        }
    } else if ledger
        .content_hash
        .as_deref()
        .is_some_and(|value| !valid_hash(value))
    {
        schema_issue(issue(
            "grounding.invalid-content-hash",
            "error",
            "证据账本正文哈希无效",
            None,
        ));
    }
    for asset_id in referenced_assets {
        if !asset_ids.contains(asset_id) {
            schema_issue(issue(
                "block.missing-asset",
                "error",
                format!("块引用了不存在的素材 `{asset_id}`"),
                None,
            ));
        }
    }

    for source_id in &document.provenance.source_ids {
        if !source_ids.contains(source_id.as_str()) {
            schema_issue(issue(
                "provenance.missing-source",
                "error",
                format!("来源链引用了不存在的来源 `{source_id}`"),
                None,
            ));
        }
    }
    for asset in &document.assets {
        if asset
            .source_ref_id
            .as_deref()
            .is_some_and(|source_id| !source_ids.contains(source_id))
        {
            schema_issue(issue(
                "asset.missing-source",
                "error",
                format!("素材 `{}` 引用了不存在的来源", asset.id),
                None,
            ));
        }
    }

    if document.publishing.targets.is_empty()
        || !unique_nonempty(&document.publishing.targets, 20)
        || document
            .publishing
            .targets
            .iter()
            .any(|target| !PUBLISHING_TARGETS.contains(&target.as_str()))
        || ![
            "draft",
            "preparing",
            "readyForExport",
            "exported",
            "blocked",
        ]
        .contains(&document.publishing.status.as_str())
    {
        schema_issue(issue(
            "publishing.invalid",
            "error",
            "发布目标或发布状态无效",
            None,
        ));
    }
    if !["user", "assistant", "import", "system"].contains(&document.provenance.created_by.as_str())
        || document.provenance.canonical_authority != "obsidianMarkdown"
        || !["original", "modelCandidate", "imported", "revised"]
            .contains(&document.provenance.derivation.as_str())
    {
        schema_issue(issue(
            "provenance.invalid",
            "error",
            "文稿来源链声明无效",
            None,
        ));
    }

    let ast = parse_markdown(&document.canonical_markdown);
    let ast_valid = ast.errors.is_empty();
    for error in ast.errors {
        issues.push(issue(
            error.code,
            "error",
            format!("第 {} 行：{}", error.line, error.message),
            None,
        ));
    }

    let mut html_valid = false;
    if ast_valid {
        match render_document_html(document) {
            Ok(html) => {
                let html_issues = validate_html_whitelist(&html);
                html_valid = html_issues.is_empty();
                issues.extend(html_issues);
            }
            Err(error) => issues.push(issue("html.render-failed", "error", error, None)),
        }
    }
    issues.sort_by(|left, right| {
        left.severity
            .cmp(&right.severity)
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.block_id.cmp(&right.block_id))
    });
    issues.dedup_by(|left, right| {
        left.code == right.code && left.message == right.message && left.block_id == right.block_id
    });
    Inspection {
        issues,
        schema_valid,
        ast_valid,
        html_valid,
    }
}

fn readiness_target(document: &CreationDocumentV2) -> String {
    match document.content_type.as_str() {
        "wechat" => return "wechat".to_string(),
        "xiaohongshu" => return "image".to_string(),
        "contract" | "paper" => return "pdf".to_string(),
        _ => {}
    }
    for target in ["wechat", "html", "markdown", "pdf", "image"] {
        if document
            .publishing
            .targets
            .iter()
            .any(|candidate| candidate == target)
        {
            return target.to_string();
        }
    }
    "markdown".to_string()
}

fn content_type_structure_valid(document: &CreationDocumentV2) -> bool {
    let markdown = document.canonical_markdown.as_str();
    match document.content_type.as_str() {
        "contract" => {
            ["合同", "协议", "甲方", "乙方", "权利", "义务", "责任"]
                .iter()
                .filter(|term| markdown.contains(**term))
                .count()
                >= 3
        }
        "paper" => markdown.contains("摘要") && markdown.contains("关键词"),
        "xiaohongshu" => {
            markdown
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count()
                >= 3
        }
        "article" | "wechat" => markdown.lines().any(|line| line.starts_with("# ")),
        _ => false,
    }
}

fn check(
    id: &str,
    category: &str,
    passed: bool,
    warning_only: bool,
    pass_detail: &str,
    fail_detail: &str,
) -> ReadinessCheck {
    ReadinessCheck {
        id: id.to_string(),
        category: category.to_string(),
        status: if passed {
            "pass"
        } else if warning_only {
            "warn"
        } else {
            "fail"
        }
        .to_string(),
        deterministic: true,
        detail: if passed { pass_detail } else { fail_detail }.to_string(),
        evidence_refs: Vec::new(),
    }
}

fn build_readiness(
    document: &CreationDocumentV2,
    inspection: &Inspection,
    generated_at: &str,
) -> ReadinessReport {
    let report_document_id = if is_valid_record_id(&document.id) {
        document.id.clone()
    } else {
        "invalid-document".to_string()
    };
    let report_revision = document.revision.max(1);
    let target = readiness_target(document);
    let assets = ReadinessAssetSummary {
        total: document.assets.len(),
        ready: document
            .assets
            .iter()
            .filter(|asset| matches!(asset.state.as_str(), "ready" | "localized"))
            .count(),
        upload_required: document
            .assets
            .iter()
            .filter(|asset| asset.state == "upload_required")
            .count(),
        failed: document
            .assets
            .iter()
            .filter(|asset| asset.state == "failed")
            .count(),
        missing_alt: document
            .assets
            .iter()
            .filter(|asset| {
                asset_is_visual(asset) && asset.alt.as_deref().is_none_or(str::is_empty)
            })
            .count(),
    };
    let title_selected = document
        .publishing
        .selected_title
        .as_deref()
        .is_some_and(|title| !title.trim().is_empty())
        || !document.title.trim().is_empty();
    let sources_resolved = document
        .source_refs
        .iter()
        .all(|source| !matches!(source.trust.as_str(), "unverified" | "conflicted"));
    let grounding_current = document.provenance.created_by != "assistant"
        || document.grounding_ledger.status == "verified";
    let citations_resolved = sources_resolved && grounding_current;
    let cjk_spacing_valid = !document.layout.features.cjk_spacing
        || apply_cjk_spacing(&document.canonical_markdown)
            .is_ok_and(|spaced| spaced == document.canonical_markdown);
    let cover_ready = document
        .publishing
        .cover_asset_id
        .as_deref()
        .and_then(|cover_id| document.assets.iter().find(|asset| asset.id == cover_id))
        .is_some_and(|asset| matches!(asset.state.as_str(), "ready" | "localized"));
    let cover_required = matches!(target.as_str(), "wechat" | "image");

    let mut checks = vec![
        check(
            "document.schema",
            "structure",
            inspection.schema_valid,
            false,
            "CreationDocumentV2 契约有效",
            "CreationDocumentV2 契约无效",
        ),
        check(
            "document.ast",
            "structure",
            inspection.ast_valid,
            false,
            "Markdown AST 有效",
            "Markdown AST 无效",
        ),
        check(
            "document.html",
            "safety",
            inspection.html_valid,
            false,
            "最终 HTML 通过白名单校验",
            "最终 HTML 未通过白名单校验",
        ),
        check(
            "document.title",
            "content",
            title_selected,
            false,
            "标题已确定",
            "尚未确定标题",
        ),
        check(
            "document.citations",
            "citation",
            citations_resolved,
            true,
            "来源引用没有未核实或冲突项",
            "仍有未核实或冲突来源需要人工确认",
        ),
        check(
            "document.grounding",
            "citation",
            grounding_current,
            false,
            "AI 正文已通过本地证据核验",
            "AI 正文的证据账本缺失、失败或已经过期",
        ),
        check(
            "content.type-structure",
            "content",
            content_type_structure_valid(document),
            true,
            "正文结构符合所选内容类型的基础要求",
            "正文结构与所选内容类型不完全匹配，需要复核",
        ),
        check(
            "layout.cjk-spacing",
            "layout",
            cjk_spacing_valid,
            true,
            "CJK 间距符合当前策略",
            "CJK 间距仍可修复",
        ),
        check(
            "assets.integrity",
            "asset",
            assets.failed == 0,
            false,
            "没有失败素材",
            "存在失败素材",
        ),
        check(
            "assets.upload",
            "asset",
            assets.upload_required == 0,
            true,
            "素材无需额外上传",
            "仍有素材需要上传到目标平台",
        ),
        check(
            "assets.alt",
            "asset",
            assets.missing_alt == 0,
            true,
            "视觉素材均有替代文本",
            "部分视觉素材缺少替代文本",
        ),
    ];
    if cover_required {
        checks.push(check(
            "assets.cover",
            "asset",
            cover_ready,
            true,
            "封面素材已就绪",
            "封面素材尚未就绪",
        ));
    }
    let mut blockers = inspection
        .issues
        .iter()
        .filter(|issue| issue.severity == "error")
        .map(|issue| issue.message.clone())
        .collect::<Vec<_>>();
    blockers.extend(
        checks
            .iter()
            .filter(|check| check.status == "fail")
            .map(|check| check.detail.clone()),
    );
    let mut warnings = inspection
        .issues
        .iter()
        .filter(|issue| issue.severity == "warning")
        .map(|issue| issue.message.clone())
        .collect::<Vec<_>>();
    warnings.extend(
        checks
            .iter()
            .filter(|check| check.status == "warn")
            .map(|check| check.detail.clone()),
    );
    blockers.sort();
    blockers.dedup();
    warnings.sort();
    warnings.dedup();
    let status = if !blockers.is_empty() {
        "blocked"
    } else if !warnings.is_empty() {
        "reviewRequired"
    } else if document.publishing.status == "exported" {
        "exported"
    } else {
        "readyForExport"
    }
    .to_string();
    let readiness_id = format!("readiness-{report_document_id}-{report_revision}");
    let readiness_id = if readiness_id.chars().count() <= 160 {
        readiness_id
    } else {
        format!(
            "readiness-{}-{}",
            report_document_id.chars().take(130).collect::<String>(),
            report_revision
        )
    };
    ReadinessReport {
        schema_version: MANIFEST_SCHEMA_VERSION.to_string(),
        id: readiness_id,
        document_id: report_document_id,
        document_revision: report_revision,
        target,
        status,
        publication_claim: "exportOnly".to_string(),
        checks,
        blockers,
        warnings,
        assets,
        validation: ReadinessValidation {
            schema_valid: inspection.schema_valid,
            ast_valid: inspection.ast_valid,
            html_valid: inspection.html_valid,
            cjk_spacing_valid,
            citations_resolved,
            title_selected,
            cover_ready,
        },
        output: None,
        generated_at: generated_at.to_string(),
        engine_version: CREATION_RUNTIME_VERSION.to_string(),
    }
}

pub fn validate_document(document: &CreationDocumentV2) -> ValidationReport {
    let inspection = inspect_document(document);
    let validated_at = Utc::now().to_rfc3339();
    let receipt = ValidationReceipt {
        schema_valid: inspection.schema_valid,
        ast_valid: inspection.ast_valid,
        html_valid: inspection.html_valid,
        issues: inspection.issues.clone(),
        validated_at: validated_at.clone(),
        validator_version: CREATION_RUNTIME_VERSION.to_string(),
        content_hash: Some(format!(
            "sha256:{:x}",
            Sha256::digest(document.canonical_markdown.as_bytes())
        )),
    };
    let readiness = build_readiness(document, &inspection, &validated_at);
    let valid = receipt.schema_valid
        && receipt.ast_valid
        && receipt.html_valid
        && !receipt.issues.iter().any(|issue| issue.severity == "error");
    ValidationReport {
        valid,
        issues: receipt.issues.clone(),
        receipt,
        readiness,
    }
}
