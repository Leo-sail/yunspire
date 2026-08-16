pub mod assets;
pub mod markdown;
pub mod model;
pub mod mode;
pub mod renderer;
pub mod runtime;
pub mod theme;
pub mod transforms;
pub mod validation;

use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

use self::{
    assets::{is_valid_record_id, normalize_asset},
    markdown::{
        canonicalize_markdown, creation_blocks_from_markdown, parse_markdown, MarkdownBlock,
    },
    model::{
        CreationBlock, CreationCatalog, CreationDocumentV2, NormalizeCreationDocumentResult,
        ValidationReport, CREATION_SCHEMA_VERSION,
    },
    theme::{first_party_catalog, theme_exists},
    transforms::apply_layout_transforms,
    validation::validate_document,
};

fn generated_document_id(title: &str, markdown: &str) -> String {
    let slug = title
        .chars()
        .flat_map(char::to_lowercase)
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        let digest = format!(
            "{:x}",
            Sha256::digest(format!("{title}\n{markdown}").as_bytes())
        );
        format!("creation-{}", &digest[..16])
    } else {
        format!("creation-{}", slug.chars().take(140).collect::<String>())
    }
}

fn first_heading(markdown: &str) -> Option<String> {
    parse_markdown(markdown)
        .blocks
        .into_iter()
        .find_map(|block| {
            if let MarkdownBlock::Heading { level: 1, text } = block {
                (!text.trim().is_empty()).then(|| text.trim().to_string())
            } else {
                None
            }
        })
}

fn normalize_unique_strings(values: &mut Vec<String>, maximum_chars: usize) {
    let mut seen = BTreeSet::new();
    values.retain_mut(|value| {
        *value = value.trim().chars().take(maximum_chars).collect();
        !value.is_empty() && seen.insert(value.clone())
    });
}

fn normalize_content_type(value: &mut String) {
    *value = value.trim().to_ascii_lowercase();
    if !matches!(
        value.as_str(),
        "article" | "wechat" | "xiaohongshu" | "contract" | "paper"
    ) {
        *value = "article".to_string();
    }
}

fn normalized_optional_identifier(value: Option<String>, maximum_chars: usize) -> Option<String> {
    value
        .map(|item| item.trim().chars().take(maximum_chars).collect::<String>())
        .filter(|item| !item.is_empty())
}

fn preserve_block_identity(derived: &mut [CreationBlock], previous: &[CreationBlock]) {
    let by_hash = previous
        .iter()
        .filter_map(|block| block.text_hash.as_ref().map(|hash| (hash.clone(), block)))
        .collect::<BTreeMap<_, _>>();
    for block in derived {
        let previous_block = previous
            .iter()
            .find(|candidate| candidate.source_range == block.source_range)
            .or_else(|| {
                block
                    .text_hash
                    .as_ref()
                    .and_then(|hash| by_hash.get(hash).copied())
            });
        let Some(previous_block) = previous_block else {
            continue;
        };
        if is_valid_record_id(&previous_block.id) {
            block.id = previous_block.id.clone();
        }
        if matches!(previous_block.kind.as_str(), "component" | "image") {
            block.kind = previous_block.kind.clone();
            block.component_id = previous_block.component_id.clone();
            block.asset_id = previous_block.asset_id.clone();
            block.children = previous_block.children.clone();
            block.attributes = previous_block.attributes.clone();
        }
    }
}

pub(crate) fn normalize_document(
    mut document: CreationDocumentV2,
) -> Result<NormalizeCreationDocumentResult, String> {
    if !document.schema_version.is_empty()
        && !matches!(document.schema_version.as_str(), "1" | "1.0" | "2" | "2.0")
    {
        return Err(format!(
            "不支持的创作文稿版本 `{}`",
            document.schema_version
        ));
    }
    document.schema_version = CREATION_SCHEMA_VERSION.to_string();
    if !document.canonical_format.is_empty() && document.canonical_format != "markdown" {
        return Err("创作核心当前只接受 Markdown 权威正文".to_string());
    }
    document.canonical_format = "markdown".to_string();
    normalize_content_type(&mut document.content_type);

    document.title = document.title.trim().chars().take(240).collect();
    if document.title.is_empty() {
        document.title =
            first_heading(&document.canonical_markdown).unwrap_or_else(|| "未命名文稿".to_string());
    }
    document.id = document.id.trim().to_string();
    if !is_valid_record_id(&document.id) {
        document.id = generated_document_id(&document.title, &document.canonical_markdown);
    }
    document.revision = document.revision.max(1);

    document.layout.theme_id = document.layout.theme_id.trim().to_string();
    if document.layout.theme_id.is_empty() {
        document.layout.theme_id = "ink".to_string();
    }
    document.layout.theme_version = document.layout.theme_version.trim().to_string();
    if matches!(document.layout.theme_version.as_str(), "" | "1") {
        document.layout.theme_version = "1.0.0".to_string();
    }
    if !theme_exists(&document.layout.theme_id, &document.layout.theme_version) {
        document.layout.theme_id = "ink".to_string();
        document.layout.theme_version = "1.0.0".to_string();
    }
    if document.layout.target.is_empty() {
        document.layout.target = "wechatRichText".to_string();
    }
    if document.layout.typography.font_family.trim().is_empty() {
        document.layout.typography.font_family = "sans-serif".to_string();
    }
    if document.layout.typography.font_size == 0 {
        document.layout.typography.font_size = 16;
    }
    if document.layout.typography.line_height == 0.0 {
        document.layout.typography.line_height = 1.75;
    }
    if document.layout.features.external_links.is_empty() {
        document.layout.features.external_links = "preserve".to_string();
    }

    if document.metadata.language.is_empty() {
        document.metadata.language = "zh-CN".to_string();
    }
    normalize_unique_strings(&mut document.metadata.tags, 120);
    normalize_unique_strings(&mut document.metadata.wiki_links, 500);
    normalize_unique_strings(&mut document.publishing.targets, 20);
    if document.publishing.targets.is_empty() {
        document.publishing.targets.push("obsidian".to_string());
    }
    if document.publishing.status.is_empty() {
        document.publishing.status = "draft".to_string();
    }
    normalize_unique_strings(&mut document.publishing.title_candidates, 240);
    normalize_unique_strings(&mut document.publishing.infographic_asset_ids, 160);
    document.publishing.selected_title = document
        .publishing
        .selected_title
        .take()
        .map(|value| value.trim().chars().take(240).collect())
        .filter(|value: &String| !value.is_empty());

    if document.provenance.created_by.is_empty() {
        document.provenance.created_by = "user".to_string();
    }
    document.provenance.canonical_authority = "obsidianMarkdown".to_string();
    if document.provenance.derivation.is_empty() {
        document.provenance.derivation = "original".to_string();
    }
    normalize_unique_strings(&mut document.provenance.source_ids, 160);
    normalize_unique_strings(&mut document.provenance.model_run_ids, 160);

    let mut source_id_remap = BTreeMap::new();
    let mut normalized_source_ids = BTreeSet::new();
    for (index, source) in document.source_refs.iter_mut().enumerate() {
        let original_id = source.id.trim().to_string();
        let mut normalized_id = original_id.clone();
        if !is_valid_record_id(&normalized_id) || normalized_source_ids.contains(&normalized_id) {
            normalized_id = format!("source-{:04}", index + 1);
        }
        normalized_source_ids.insert(normalized_id.clone());
        if !original_id.is_empty() {
            source_id_remap
                .entry(original_id)
                .or_insert_with(|| normalized_id.clone());
        }
        source.id = normalized_id;
        source.kind = source.kind.trim().to_string();
        if source.kind.is_empty() {
            source.kind = "userInput".to_string();
        }
        source.r#ref = source.r#ref.trim().chars().take(4096).collect();
        if source.r#ref.is_empty() {
            source.r#ref = source.id.clone();
        }
        source.vault_id = normalized_optional_identifier(source.vault_id.take(), 160);
        source.relative_path = source
            .relative_path
            .take()
            .map(|value| value.trim().replace('\\', "/").chars().take(2048).collect())
            .filter(|value: &String| {
                !value.is_empty()
                    && !value.starts_with('/')
                    && !value.split('/').any(|part| part == "..")
            });
        source.content_hash = normalized_optional_identifier(source.content_hash.take(), 72);
        source.excerpt_hash = normalized_optional_identifier(source.excerpt_hash.take(), 72);
        source.trust = source.trust.trim().to_string();
        if source.trust.is_empty() {
            source.trust = "direct".to_string();
        }
    }
    let remap_source_id = |value: &str| {
        source_id_remap
            .get(value)
            .cloned()
            .unwrap_or_else(|| value.to_string())
    };
    document.provenance.source_ids = document
        .provenance
        .source_ids
        .iter()
        .map(|source_id| remap_source_id(source_id))
        .collect();
    for asset in &mut document.assets {
        asset.source_ref_id = asset
            .source_ref_id
            .take()
            .map(|source_id| remap_source_id(&source_id));
    }
    if !matches!(
        document.grounding_ledger.status.as_str(),
        "unverified" | "verified" | "stale" | "failed"
    ) {
        document.grounding_ledger.status = "unverified".to_string();
    }
    let mut ledger_block_ids = BTreeSet::new();
    for (index, block) in document.grounding_ledger.blocks.iter_mut().enumerate() {
        block.id = block.id.trim().to_string();
        if !is_valid_record_id(&block.id) || !ledger_block_ids.insert(block.id.clone()) {
            block.id = format!("grounding-block-{:04}", index + 1);
            ledger_block_ids.insert(block.id.clone());
        }
        block.source_ref_ids = block
            .source_ref_ids
            .iter()
            .map(|source_id| remap_source_id(source_id))
            .collect();
        normalize_unique_strings(&mut block.source_ref_ids, 160);
        if !matches!(
            block.verdict.as_str(),
            "supported" | "unsupported" | "uncertain"
        ) {
            block.verdict = "uncertain".to_string();
        }
        for evidence in &mut block.evidence {
            evidence.source_ref_id = remap_source_id(&evidence.source_ref_id);
            evidence.quote = evidence.quote.trim().chars().take(2_000).collect();
        }
        block
            .evidence
            .retain(|evidence| !evidence.source_ref_id.is_empty() && !evidence.quote.is_empty());
    }
    document.grounding_ledger.verified_at =
        normalized_optional_identifier(document.grounding_ledger.verified_at.take(), 64);
    document.grounding_ledger.content_hash =
        normalized_optional_identifier(document.grounding_ledger.content_hash.take(), 72);
    document.grounding_ledger.generation_trace_id =
        normalized_optional_identifier(document.grounding_ledger.generation_trace_id.take(), 160);
    document.grounding_ledger.verification_trace_id =
        normalized_optional_identifier(document.grounding_ledger.verification_trace_id.take(), 160);
    if document.provenance.source_ids.is_empty() {
        document.provenance.source_ids = document
            .source_refs
            .iter()
            .map(|source| source.id.clone())
            .collect();
    }
    for (index, asset) in document.assets.iter_mut().enumerate() {
        normalize_asset(asset, index);
    }

    if document.canonical_markdown.trim().is_empty() {
        document.canonical_markdown = format!("# {}\n", document.title);
    }
    document.canonical_markdown =
        canonicalize_markdown(&document.canonical_markdown).map_err(|errors| {
            errors
                .iter()
                .map(|error| format!("第 {} 行：{}", error.line, error.message))
                .collect::<Vec<_>>()
                .join("；")
        })?;
    let (transformed, transform) =
        apply_layout_transforms(&document.canonical_markdown, &document.layout)?;
    document.canonical_markdown = canonicalize_markdown(&transformed).map_err(|errors| {
        errors
            .iter()
            .map(|error| format!("第 {} 行：{}", error.line, error.message))
            .collect::<Vec<_>>()
            .join("；")
    })?;

    let canonical_content_hash = format!(
        "sha256:{:x}",
        Sha256::digest(document.canonical_markdown.as_bytes())
    );
    if document.grounding_ledger.status == "verified" {
        if document
            .grounding_ledger
            .content_hash
            .as_deref()
            .is_some_and(|hash| hash != canonical_content_hash)
        {
            document.grounding_ledger.status = "stale".to_string();
        }
        document.grounding_ledger.content_hash = Some(canonical_content_hash);
    }

    let previous_blocks = std::mem::take(&mut document.blocks);
    let mut derived_blocks = creation_blocks_from_markdown(&document.canonical_markdown);
    preserve_block_identity(&mut derived_blocks, &previous_blocks);
    document.blocks = derived_blocks;

    let report = validate_document(&document);
    document.validation_receipt = report.receipt.clone();
    document.readiness = Some(report.readiness.clone());
    Ok(NormalizeCreationDocumentResult {
        document,
        transform,
        readiness: report.readiness,
    })
}

#[tauri::command]
pub fn list_creation_catalog() -> CreationCatalog {
    first_party_catalog()
}

#[tauri::command]
pub fn normalize_creation_document(
    document: CreationDocumentV2,
) -> Result<NormalizeCreationDocumentResult, String> {
    normalize_document(document)
}

#[tauri::command]
pub fn validate_creation_document(document: CreationDocumentV2) -> ValidationReport {
    validate_document(&document)
}
