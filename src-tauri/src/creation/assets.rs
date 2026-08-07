//! Deterministic creation-asset normalization and local safety checks.

use std::path::{Component, Path};

use super::model::{CreationAsset, ValidationIssue};

const ASSET_KINDS: &[&str] = &[
    "image",
    "video",
    "audio",
    "file",
    "cover",
    "infographic",
    "gallery",
    "longImage",
];
const ASSET_STATES: &[&str] = &[
    "draft",
    "local",
    "localized",
    "upload_required",
    "ready",
    "failed",
];

pub fn is_valid_record_id(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric())
        && value.chars().count() <= 160
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
}

pub fn is_safe_relative_path(value: &str) -> bool {
    if value.is_empty()
        || value.contains('\0')
        || value.starts_with('/')
        || value.starts_with('\\')
        || value.get(1..2) == Some(":")
    {
        return false;
    }
    Path::new(value)
        .components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

pub fn asset_is_visual(asset: &CreationAsset) -> bool {
    matches!(
        asset.kind.as_str(),
        "image" | "cover" | "infographic" | "gallery" | "longImage"
    )
}

fn inferred_kind(mime_type: Option<&str>) -> &'static str {
    match mime_type.unwrap_or_default() {
        mime if mime.starts_with("image/") => "image",
        mime if mime.starts_with("video/") => "video",
        mime if mime.starts_with("audio/") => "audio",
        _ => "file",
    }
}

pub fn normalize_asset(asset: &mut CreationAsset, index: usize) {
    asset.id = asset.id.trim().to_string();
    if asset.id.is_empty() {
        asset.id = format!("asset-{:04}", index + 1);
    }
    asset.name = asset.name.trim().chars().take(240).collect();
    if asset.name.is_empty() {
        asset.name = format!("未命名素材 {}", index + 1);
    }
    asset.mime_type = asset
        .mime_type
        .take()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());
    asset.kind = asset.kind.trim().to_string();
    if asset.kind.is_empty() {
        asset.kind = inferred_kind(asset.mime_type.as_deref()).to_string();
    }
    asset.state = asset.state.trim().to_string();
    if asset.state.is_empty() {
        asset.state = "draft".to_string();
    }
    asset.relative_path = asset
        .relative_path
        .take()
        .map(|value| value.trim().replace('\\', "/"))
        .filter(|value| !value.is_empty());
    asset.alt = asset
        .alt
        .take()
        .map(|value| value.trim().chars().take(500).collect())
        .filter(|value: &String| !value.is_empty());
    asset.caption = asset
        .caption
        .take()
        .map(|value| value.trim().chars().take(1000).collect())
        .filter(|value: &String| !value.is_empty());
}

pub fn validate_asset(asset: &CreationAsset) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    let mut issue = |code: &str, severity: &str, message: String| {
        issues.push(ValidationIssue {
            code: code.to_string(),
            severity: severity.to_string(),
            message,
            block_id: None,
        });
    };
    if !is_valid_record_id(&asset.id) {
        issue(
            "asset.invalid-id",
            "error",
            format!("素材 ID `{}` 不符合本地记录格式", asset.id),
        );
    }
    if !ASSET_KINDS.contains(&asset.kind.as_str()) {
        issue(
            "asset.invalid-kind",
            "error",
            format!("素材 `{}` 的类型 `{}` 不受支持", asset.id, asset.kind),
        );
    }
    if asset.name.is_empty() || asset.name.chars().count() > 240 {
        issue(
            "asset.invalid-name",
            "error",
            format!("素材 `{}` 缺少有效名称", asset.id),
        );
    }
    if !ASSET_STATES.contains(&asset.state.as_str()) {
        issue(
            "asset.invalid-state",
            "error",
            format!("素材 `{}` 的状态 `{}` 不受支持", asset.id, asset.state),
        );
    }
    if let Some(path) = asset.relative_path.as_deref() {
        if path.len() > 2048 || !is_safe_relative_path(path) {
            issue(
                "asset.unsafe-path",
                "error",
                format!("素材 `{}` 的相对路径不安全", asset.id),
            );
        }
    }
    if asset
        .mime_type
        .as_ref()
        .is_some_and(|mime_type| mime_type.len() > 120 || !mime_type.contains('/'))
    {
        issue(
            "asset.invalid-mime",
            "error",
            format!("素材 `{}` 的 MIME 类型无效", asset.id),
        );
    }
    if asset_is_visual(asset) && asset.alt.as_deref().is_none_or(str::is_empty) {
        issue(
            "asset.missing-alt",
            "warning",
            format!("视觉素材 `{}` 缺少替代文本", asset.id),
        );
    }
    issues
}
