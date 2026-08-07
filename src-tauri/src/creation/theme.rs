//! Compile-time first-party creation catalog.
//!
//! The JSON manifests are the source of truth shared with schema validation.
//! Embedding them keeps catalog listing deterministic and independent of the
//! filesystem or network at runtime.

use std::sync::OnceLock;

use super::model::CreationCatalog;

const RUNTIME_BUNDLE: &str =
    include_str!("../../../resources/creation/catalog/runtime-bundle.json");
static FIRST_PARTY_CATALOG: OnceLock<CreationCatalog> = OnceLock::new();

fn bundled_catalog() -> &'static CreationCatalog {
    FIRST_PARTY_CATALOG.get_or_init(|| {
        serde_json::from_str(RUNTIME_BUNDLE).unwrap_or_else(|error| {
            panic!("bundled creation runtime catalog must be valid: {error}")
        })
    })
}

pub fn first_party_catalog() -> CreationCatalog {
    bundled_catalog().clone()
}

pub fn theme_exists(theme_id: &str, theme_version: &str) -> bool {
    bundled_catalog().themes.iter().any(|theme| {
        theme.id == theme_id
            && (theme.version == theme_version
                || (matches!(theme.id.as_str(), "ink" | "jade" | "vermilion" | "graphite")
                    && theme_version == "1.0.0"))
    })
}

pub fn component_exists(component_id: &str) -> bool {
    bundled_catalog().components.iter().any(|component| {
        component.id == component_id
            || component
                .legacy_ids
                .iter()
                .any(|legacy_id| legacy_id == component_id)
    })
}
