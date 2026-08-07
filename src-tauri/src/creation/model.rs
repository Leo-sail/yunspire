//! Stable data contracts for the v0.3 creation runtime.
//!
//! These structures mirror the JSON Schemas in `docs/schemas`. Markdown is
//! the canonical authority; blocks, validation receipts and readiness reports
//! are deterministic derived data.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CREATION_SCHEMA_VERSION: &str = "2.0";
pub const MANIFEST_SCHEMA_VERSION: &str = "1.0";
pub const CREATION_RUNTIME_VERSION: &str = "0.3.0";

fn creation_schema_version() -> String {
    CREATION_SCHEMA_VERSION.to_string()
}

fn manifest_schema_version() -> String {
    MANIFEST_SCHEMA_VERSION.to_string()
}

fn canonical_format() -> String {
    "markdown".to_string()
}

fn default_content_type() -> String {
    "article".to_string()
}

fn default_grounding_status() -> String {
    "unverified".to_string()
}

fn default_theme_id() -> String {
    "ink".to_string()
}

fn default_semver() -> String {
    "1.0.0".to_string()
}

fn default_layout_target() -> String {
    "wechatRichText".to_string()
}

fn default_language() -> String {
    "zh-CN".to_string()
}

fn default_font_family() -> String {
    "sans-serif".to_string()
}

fn default_font_size() -> u8 {
    16
}

fn default_line_height() -> f32 {
    1.75
}

fn default_heading_scale() -> Option<f32> {
    Some(1.25)
}

fn default_external_links() -> String {
    "preserve".to_string()
}

fn default_created_by() -> String {
    "user".to_string()
}

fn canonical_authority() -> String {
    "obsidianMarkdown".to_string()
}

fn default_derivation() -> String {
    "original".to_string()
}

fn default_publishing_status() -> String {
    "draft".to_string()
}

fn default_validator_version() -> String {
    CREATION_RUNTIME_VERSION.to_string()
}

fn default_manifest_type_theme() -> String {
    "theme".to_string()
}

fn default_manifest_type_component() -> String {
    "component".to_string()
}

fn default_manifest_status() -> String {
    "active".to_string()
}

fn default_source_policy() -> String {
    "yunspire_first_party".to_string()
}

fn default_authored_by() -> String {
    "Yunspire".to_string()
}

fn default_repository() -> String {
    "https://github.com/Leo-sail/yunspire".to_string()
}

fn default_license_scope() -> String {
    "yunspire_first_party_project_asset".to_string()
}

fn default_publication_claim() -> String {
    "exportOnly".to_string()
}

fn default_readiness_target() -> String {
    "wechat".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CreationDocumentV2 {
    #[serde(default = "creation_schema_version")]
    pub schema_version: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub title: String,
    #[serde(default = "canonical_format")]
    pub canonical_format: String,
    #[serde(default = "default_content_type")]
    pub content_type: String,
    #[serde(default)]
    pub canonical_markdown: String,
    #[serde(default)]
    pub blocks: Vec<CreationBlock>,
    #[serde(default)]
    pub assets: Vec<CreationAsset>,
    #[serde(default)]
    pub source_refs: Vec<CreationSourceRef>,
    #[serde(default)]
    pub layout: CreationLayout,
    #[serde(default)]
    pub metadata: CreationMetadata,
    #[serde(default)]
    pub publishing: CreationPublishing,
    #[serde(default)]
    pub provenance: CreationProvenance,
    #[serde(default)]
    pub grounding_ledger: GroundingLedger,
    #[serde(default)]
    pub validation_receipt: ValidationReceipt,
    #[serde(default)]
    pub readiness: Option<ReadinessReport>,
}

impl Default for CreationDocumentV2 {
    fn default() -> Self {
        Self {
            schema_version: creation_schema_version(),
            id: String::new(),
            revision: 0,
            title: String::new(),
            canonical_format: canonical_format(),
            content_type: default_content_type(),
            canonical_markdown: String::new(),
            blocks: Vec::new(),
            assets: Vec::new(),
            source_refs: Vec::new(),
            layout: CreationLayout::default(),
            metadata: CreationMetadata::default(),
            publishing: CreationPublishing::default(),
            provenance: CreationProvenance::default(),
            grounding_ledger: GroundingLedger::default(),
            validation_receipt: ValidationReceipt::default(),
            readiness: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GroundingLedger {
    #[serde(default = "default_grounding_status")]
    pub status: String,
    #[serde(default)]
    pub blocks: Vec<GroundingLedgerBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_trace_id: Option<String>,
}

impl Default for GroundingLedger {
    fn default() -> Self {
        Self {
            status: default_grounding_status(),
            blocks: Vec::new(),
            verified_at: None,
            content_hash: None,
            generation_trace_id: None,
            verification_trace_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct GroundingLedgerBlock {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub source_ref_ids: Vec<String>,
    #[serde(default)]
    pub verdict: String,
    #[serde(default)]
    pub evidence: Vec<GroundingEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct GroundingEvidence {
    #[serde(default)]
    pub source_ref_id: String,
    #[serde(default)]
    pub quote: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CreationBlock {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub level: Option<u8>,
    #[serde(default)]
    pub component_id: Option<String>,
    #[serde(default)]
    pub asset_id: Option<String>,
    #[serde(default)]
    pub source_range: SourceRange,
    #[serde(default)]
    pub children: Vec<String>,
    #[serde(default)]
    pub text_hash: Option<String>,
    #[serde(default)]
    pub attributes: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SourceRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CreationAsset {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub relative_path: Option<String>,
    #[serde(default)]
    pub content_hash: Option<String>,
    #[serde(default)]
    pub alt: Option<String>,
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default)]
    pub source_ref_id: Option<String>,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CreationSourceRef {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub r#ref: String,
    /// Stable Vault identity for cross-Vault references. `ref` remains the
    /// human-readable locator while this pair is used for deterministic
    /// recovery and hash verification.
    #[serde(default)]
    pub vault_id: Option<String>,
    #[serde(default)]
    pub relative_path: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub excerpt: Option<String>,
    #[serde(default)]
    pub content_hash: Option<String>,
    #[serde(default)]
    pub excerpt_hash: Option<String>,
    #[serde(default)]
    pub captured_at: Option<String>,
    #[serde(default)]
    pub trust: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CreationLayout {
    #[serde(default = "default_theme_id")]
    pub theme_id: String,
    #[serde(default = "default_semver")]
    pub theme_version: String,
    #[serde(default = "default_layout_target")]
    pub target: String,
    #[serde(default)]
    pub typography: CreationTypography,
    #[serde(default)]
    pub tokens: BTreeMap<String, Value>,
    #[serde(default)]
    pub features: CreationLayoutFeatures,
}

impl Default for CreationLayout {
    fn default() -> Self {
        Self {
            theme_id: default_theme_id(),
            theme_version: default_semver(),
            target: default_layout_target(),
            typography: CreationTypography::default(),
            tokens: BTreeMap::new(),
            features: CreationLayoutFeatures::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CreationTypography {
    #[serde(default = "default_font_family")]
    pub font_family: String,
    #[serde(default = "default_font_size")]
    pub font_size: u8,
    #[serde(default = "default_line_height")]
    pub line_height: f32,
    #[serde(
        default = "default_heading_scale",
        skip_serializing_if = "Option::is_none"
    )]
    pub heading_scale: Option<f32>,
}

impl Default for CreationTypography {
    fn default() -> Self {
        Self {
            font_family: default_font_family(),
            font_size: default_font_size(),
            line_height: default_line_height(),
            heading_scale: default_heading_scale(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CreationLayoutFeatures {
    #[serde(default)]
    pub auto_numbering: bool,
    #[serde(default)]
    pub keyword_underline: bool,
    #[serde(default)]
    pub table_of_contents: bool,
    #[serde(default)]
    pub introduction: bool,
    #[serde(default)]
    pub signature: bool,
    #[serde(default)]
    pub cjk_spacing: bool,
    #[serde(default = "default_external_links")]
    pub external_links: String,
}

impl Default for CreationLayoutFeatures {
    fn default() -> Self {
        Self {
            auto_numbering: false,
            keyword_underline: false,
            table_of_contents: false,
            introduction: false,
            signature: false,
            cjk_spacing: false,
            external_links: default_external_links(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CreationMetadata {
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub properties: BTreeMap<String, Value>,
    #[serde(default)]
    pub wiki_links: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brand_profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

impl Default for CreationMetadata {
    fn default() -> Self {
        Self {
            language: default_language(),
            tags: Vec::new(),
            properties: BTreeMap::new(),
            wiki_links: Vec::new(),
            brand_profile_id: None,
            author: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CreationPublishing {
    #[serde(default = "default_publishing_targets")]
    pub targets: Vec<String>,
    #[serde(default = "default_publishing_status")]
    pub status: String,
    #[serde(default)]
    pub title_candidates: Vec<String>,
    #[serde(default)]
    pub selected_title: Option<String>,
    #[serde(default)]
    pub cover_asset_id: Option<String>,
    #[serde(default)]
    pub infographic_asset_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_exported_at: Option<String>,
}

fn default_publishing_targets() -> Vec<String> {
    vec!["obsidian".to_string()]
}

impl Default for CreationPublishing {
    fn default() -> Self {
        Self {
            targets: default_publishing_targets(),
            status: default_publishing_status(),
            title_candidates: Vec::new(),
            selected_title: None,
            cover_asset_id: None,
            infographic_asset_ids: Vec::new(),
            last_exported_at: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CreationProvenance {
    #[serde(default = "default_created_by")]
    pub created_by: String,
    #[serde(default = "canonical_authority")]
    pub canonical_authority: String,
    #[serde(default)]
    pub source_ids: Vec<String>,
    #[serde(default = "default_derivation")]
    pub derivation: String,
    #[serde(default)]
    pub model_run_ids: Vec<String>,
}

impl Default for CreationProvenance {
    fn default() -> Self {
        Self {
            created_by: default_created_by(),
            canonical_authority: canonical_authority(),
            source_ids: Vec::new(),
            derivation: default_derivation(),
            model_run_ids: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ValidationReceipt {
    #[serde(default)]
    pub schema_valid: bool,
    #[serde(default)]
    pub ast_valid: bool,
    #[serde(default)]
    pub html_valid: bool,
    #[serde(default)]
    pub issues: Vec<ValidationIssue>,
    #[serde(default)]
    pub validated_at: String,
    #[serde(default = "default_validator_version")]
    pub validator_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
}

impl Default for ValidationReceipt {
    fn default() -> Self {
        Self {
            schema_valid: false,
            ast_valid: false,
            html_valid: false,
            issues: Vec::new(),
            validated_at: String::new(),
            validator_version: default_validator_version(),
            content_hash: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ValidationIssue {
    pub code: String,
    pub severity: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessReport {
    #[serde(default = "manifest_schema_version")]
    pub schema_version: String,
    pub id: String,
    pub document_id: String,
    pub document_revision: u64,
    #[serde(default = "default_readiness_target")]
    pub target: String,
    pub status: String,
    #[serde(default = "default_publication_claim")]
    pub publication_claim: String,
    pub checks: Vec<ReadinessCheck>,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
    pub assets: ReadinessAssetSummary,
    pub validation: ReadinessValidation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<ReadinessOutput>,
    pub generated_at: String,
    #[serde(default = "default_validator_version")]
    pub engine_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessCheck {
    pub id: String,
    pub category: String,
    pub status: String,
    pub deterministic: bool,
    pub detail: String,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessAssetSummary {
    pub total: usize,
    pub ready: usize,
    pub upload_required: usize,
    pub failed: usize,
    pub missing_alt: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessValidation {
    pub schema_valid: bool,
    pub ast_valid: bool,
    pub html_valid: bool,
    pub cjk_spacing_valid: bool,
    pub citations_resolved: bool,
    pub title_selected: bool,
    pub cover_ready: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessOutput {
    pub format: String,
    pub relative_path: String,
    pub content_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TransformReport {
    pub changed: bool,
    pub applied: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NormalizeCreationDocumentResult {
    pub document: CreationDocumentV2,
    pub transform: TransformReport,
    pub readiness: ReadinessReport,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ValidationReport {
    pub valid: bool,
    pub issues: Vec<ValidationIssue>,
    pub receipt: ValidationReceipt,
    pub readiness: ReadinessReport,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThemeManifest {
    #[serde(default = "manifest_schema_version")]
    pub schema_version: String,
    #[serde(default = "default_manifest_type_theme")]
    pub manifest_type: String,
    #[serde(default = "default_semver")]
    pub catalog_version: String,
    pub id: String,
    #[serde(default = "default_semver")]
    pub version: String,
    pub display_name: String,
    pub description: String,
    #[serde(default = "default_manifest_status")]
    pub status: String,
    pub category: String,
    pub tags: Vec<String>,
    pub legacy_ids: Vec<String>,
    pub palette: ThemePalette,
    pub typography: ThemeTypography,
    pub spacing: ThemeSpacing,
    pub features: ThemeFeatures,
    pub supported_component_ids: Vec<String>,
    pub renderers: ManifestRenderers,
    pub compatibility: ThemeCompatibility,
    pub source: ManifestSource,
    pub license: ManifestLicense,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ThemePalette {
    pub accent: String,
    pub accent_soft: String,
    pub text: String,
    pub muted: String,
    pub border: String,
    pub quote: String,
    pub heading: String,
    pub background: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ThemeTypography {
    pub default_family: String,
    pub fallback_stack: String,
    pub base_size: u8,
    pub line_height: f32,
    pub heading_weight: u16,
    pub body_weight: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ThemeSpacing {
    pub paragraph: u16,
    pub section: u16,
    pub page_x: u16,
    pub page_y: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ThemeFeatures {
    pub auto_numbering: bool,
    pub keyword_underline: bool,
    pub table_of_contents: bool,
    pub introduction: bool,
    pub signature: bool,
    pub span_leaf: bool,
    pub cjk_spacing: bool,
    pub external_link_footnotes: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ManifestRenderers {
    pub markdown: String,
    pub html: String,
    pub wechat_rich_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ThemeCompatibility {
    pub targets: Vec<String>,
    pub wechat_certification: String,
    pub min_runtime_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ManifestSource {
    #[serde(default = "default_source_policy")]
    pub policy: String,
    #[serde(default = "default_authored_by")]
    pub authored_by: String,
    #[serde(default = "default_repository")]
    pub repository: String,
    #[serde(default)]
    pub upstream_code_copied: bool,
    pub research_boundary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ManifestLicense {
    #[serde(default = "default_license_scope")]
    pub scope: String,
    pub notice: String,
    #[serde(default)]
    pub third_party_assets: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ComponentManifest {
    #[serde(default = "manifest_schema_version")]
    pub schema_version: String,
    #[serde(default = "default_manifest_type_component")]
    pub manifest_type: String,
    #[serde(default = "default_semver")]
    pub catalog_version: String,
    pub id: String,
    #[serde(default = "default_semver")]
    pub version: String,
    pub display_name: String,
    pub description: String,
    #[serde(default = "default_manifest_status")]
    pub status: String,
    pub category: String,
    pub legacy_ids: Vec<String>,
    pub block_kind: String,
    pub slots: Vec<ComponentSlot>,
    pub semantics: ComponentSemantics,
    pub constraints: ComponentConstraints,
    pub renderers: ManifestRenderers,
    pub compatibility: ComponentCompatibility,
    pub source: ManifestSource,
    pub license: ManifestLicense,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ComponentSlot {
    pub id: String,
    pub kind: String,
    pub required: bool,
    pub max_length: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ComponentSemantics {
    pub role: String,
    pub aria_label: String,
    pub markdown_fallback: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ComponentConstraints {
    pub min_items: usize,
    pub max_items: usize,
    pub allow_nested_components: bool,
    pub span_leaf: bool,
    pub allow_scripts: bool,
    pub allow_external_styles: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ComponentCompatibility {
    pub targets: Vec<String>,
    pub min_runtime_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CreationCatalog {
    pub catalog_version: String,
    pub themes: Vec<ThemeManifest>,
    pub components: Vec<ComponentManifest>,
    pub templates: Vec<Value>,
}
