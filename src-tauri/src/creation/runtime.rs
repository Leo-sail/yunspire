//! Durable Creation Studio runtime.
//!
//! The implementation lives in this module so Creation-specific tables and
//! commands do not depend on the client workspace snapshot.  Tables are
//! created idempotently by [`migrate`] from the shared runtime database.

use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tauri::State;
use uuid::Uuid;

use crate::runtime_db::RuntimeDatabase;

use super::{
    assets::is_valid_record_id,
    model::{CreationDocumentV2, GroundingLedgerBlock},
    normalize_document,
    validation::validate_document,
};

const ACTIVE_RUN_STATES: &[&str] = &["queued", "running", "awaitingReview"];
const CREATION_CAPABILITIES: &[&str] = &["creation.generate", "creation.edit"];
/// A single IPC/database event is bounded for memory safety. There is no run-level
/// aggregate byte or event-count limit; callers continue with the next sequence.
const MAX_STREAM_EVENT_BYTES: usize = 1024 * 1024;
const DEFAULT_STREAM_PAGE_EVENTS: usize = 128;
const MAX_STREAM_PAGE_EVENTS: usize = 512;
const DEFAULT_STREAM_PAGE_BYTES: usize = 2 * 1024 * 1024;
const MAX_STREAM_PAGE_BYTES: usize = 4 * 1024 * 1024;
const STREAM_EVENT_TYPES: &[&str] = &[
    "streamStarted",
    "contentDelta",
    "contentSnapshot",
    "progress",
    "artifact",
    "diagnostic",
    "streamCompleted",
    "streamFailed",
    "streamCancelled",
    "heartbeat",
];

pub(crate) fn migrate(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS creation_writing_runs (
               workspace_scope TEXT NOT NULL,
               id TEXT NOT NULL,
               document_id TEXT NOT NULL,
               document_revision INTEGER NOT NULL CHECK(document_revision > 0),
               capability TEXT NOT NULL CHECK(capability IN ('creation.generate', 'creation.edit')),
               state TEXT NOT NULL CHECK(state IN ('queued', 'running', 'awaitingReview', 'succeeded', 'failed', 'cancelled')),
               input_hash TEXT NOT NULL,
               output_hash TEXT,
               run_json TEXT NOT NULL,
               base_document_json TEXT NOT NULL,
               candidate_document_json TEXT,
               stream_id TEXT NOT NULL,
               operation_id TEXT NOT NULL,
               creation_mode TEXT NOT NULL DEFAULT 'quick' CHECK(creation_mode IN ('quick', 'professional')),
               last_sequence INTEGER NOT NULL DEFAULT -1 CHECK(last_sequence >= -1),
               version INTEGER NOT NULL DEFAULT 1 CHECK(version > 0),
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               completed_at TEXT,
               PRIMARY KEY(workspace_scope, id),
               FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
             );
             CREATE UNIQUE INDEX IF NOT EXISTS idx_creation_writing_run_document_lock
               ON creation_writing_runs(workspace_scope, document_id)
               WHERE state IN ('queued', 'running', 'awaitingReview');
             CREATE INDEX IF NOT EXISTS idx_creation_writing_run_recovery
               ON creation_writing_runs(workspace_scope, state, updated_at DESC);
             CREATE TABLE IF NOT EXISTS creation_agent_stream_events (
               workspace_scope TEXT NOT NULL,
               run_id TEXT NOT NULL,
               sequence INTEGER NOT NULL CHECK(sequence >= 0),
               event_id TEXT NOT NULL,
               event_type TEXT NOT NULL,
               event_json TEXT NOT NULL,
               created_at TEXT NOT NULL,
               PRIMARY KEY(workspace_scope, run_id, sequence),
               UNIQUE(workspace_scope, event_id),
               FOREIGN KEY(workspace_scope, run_id)
                 REFERENCES creation_writing_runs(workspace_scope, id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS creation_run_checkpoints (
               workspace_scope TEXT NOT NULL,
               run_id TEXT NOT NULL,
               checkpoint_id TEXT NOT NULL,
               sequence INTEGER NOT NULL CHECK(sequence >= -1),
               document_revision INTEGER NOT NULL CHECK(document_revision > 0),
               input_hash TEXT NOT NULL,
               candidate_hash TEXT,
               checkpoint_json TEXT NOT NULL,
               candidate_document_json TEXT,
               created_at TEXT NOT NULL,
               PRIMARY KEY(workspace_scope, run_id, checkpoint_id),
               FOREIGN KEY(workspace_scope, run_id)
                 REFERENCES creation_writing_runs(workspace_scope, id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_creation_run_checkpoint_latest
               ON creation_run_checkpoints(workspace_scope, run_id, created_at DESC);
             CREATE TABLE IF NOT EXISTS creation_run_usage_events (
               workspace_scope TEXT NOT NULL,
               run_id TEXT NOT NULL,
               request_id TEXT NOT NULL,
               trace_id TEXT NOT NULL,
               operation TEXT NOT NULL CHECK(operation IN ('creation.generate', 'creation.edit', 'creation.grounding.verify', 'creation.brand.evaluate')),
               provider TEXT NOT NULL,
               model TEXT NOT NULL,
               state TEXT NOT NULL CHECK(state IN ('started', 'succeeded', 'failed', 'cancelled')),
               prompt_tokens INTEGER NOT NULL DEFAULT 0 CHECK(prompt_tokens >= 0),
               completion_tokens INTEGER NOT NULL DEFAULT 0 CHECK(completion_tokens >= 0),
               total_tokens INTEGER NOT NULL DEFAULT 0 CHECK(total_tokens >= 0),
               estimated_cost_usd REAL,
               duration_ms INTEGER NOT NULL DEFAULT 0 CHECK(duration_ms >= 0),
               error TEXT,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               PRIMARY KEY(workspace_scope, run_id, request_id),
               FOREIGN KEY(workspace_scope, run_id)
                 REFERENCES creation_writing_runs(workspace_scope, id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS creation_brand_profiles (
               workspace_scope TEXT NOT NULL,
               id TEXT NOT NULL,
               revision INTEGER NOT NULL CHECK(revision > 0),
               status TEXT NOT NULL CHECK(status IN ('draft', 'active', 'archived')),
               profile_json TEXT NOT NULL,
               content_hash TEXT NOT NULL,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               PRIMARY KEY(workspace_scope, id),
               FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS creation_brand_profile_revisions (
               workspace_scope TEXT NOT NULL,
               profile_id TEXT NOT NULL,
               revision INTEGER NOT NULL CHECK(revision > 0),
               status TEXT NOT NULL CHECK(status IN ('draft', 'active', 'archived')),
               profile_json TEXT NOT NULL,
               content_hash TEXT NOT NULL,
               created_at TEXT NOT NULL,
               PRIMARY KEY(workspace_scope, profile_id, revision),
               FOREIGN KEY(workspace_scope, profile_id)
                 REFERENCES creation_brand_profiles(workspace_scope, id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS creation_brand_evaluations (
               workspace_scope TEXT NOT NULL,
               id TEXT NOT NULL,
               profile_id TEXT NOT NULL,
               profile_revision INTEGER NOT NULL CHECK(profile_revision > 0),
               document_id TEXT NOT NULL,
               document_revision INTEGER NOT NULL CHECK(document_revision > 0),
               result_json TEXT NOT NULL,
               created_at TEXT NOT NULL,
               PRIMARY KEY(workspace_scope, id),
               FOREIGN KEY(workspace_scope, profile_id)
                 REFERENCES creation_brand_profiles(workspace_scope, id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS creation_document_brand_bindings (
               workspace_scope TEXT NOT NULL,
               document_id TEXT NOT NULL,
               document_revision INTEGER NOT NULL CHECK(document_revision > 0),
               profile_id TEXT NOT NULL,
               profile_revision INTEGER NOT NULL CHECK(profile_revision > 0),
               bound_at TEXT NOT NULL,
               PRIMARY KEY(workspace_scope, document_id),
               FOREIGN KEY(workspace_scope, profile_id)
                 REFERENCES creation_brand_profiles(workspace_scope, id) ON DELETE CASCADE
             );",
        )
        .map_err(|error| format!("无法初始化创作运行时：{error}"))?;
    compact_legacy_creation_runtime_rows(connection)?;
    Ok(())
}

/// Upgrade existing v0.3 rows lazily and with a one-run-at-a-time memory
/// profile. IDs are collected first, while large JSON bodies are read,
/// compacted, and released individually.
fn compact_legacy_creation_runtime_rows(connection: &Connection) -> Result<(), String> {
    let run_ids = {
        let mut statement = connection
            .prepare(
                "SELECT workspace_scope, id FROM creation_writing_runs
                 WHERE base_document_json LIKE '%\"canonicalMarkdown\"%'
                    OR candidate_document_json LIKE '%\"canonicalMarkdown\"%'",
            )
            .map_err(|error| format!("无法准备旧 Creation run 压缩：{error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| format!("无法读取旧 Creation run：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析旧 Creation run：{error}"))?;
        rows
    };
    for (workspace_scope, run_id) in run_ids {
        let (base_json, candidate_json) = connection
            .query_row(
                "SELECT base_document_json, candidate_document_json
                 FROM creation_writing_runs WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, run_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .map_err(|error| format!("无法读取旧 Creation 文稿：{error}"))?;
        let base_value: Value = serde_json::from_str(&base_json)
            .map_err(|error| format!("旧 Creation 基础文稿已损坏：{error}"))?;
        let compact_base = json_text(&compact_document_value(&base_value)?, "基础文稿 manifest")?;
        let compact_candidate = candidate_json
            .map(|text| {
                let value: Value = serde_json::from_str(&text)
                    .map_err(|error| format!("旧 Creation 候选文稿已损坏：{error}"))?;
                json_text(&compact_document_value(&value)?, "候选文稿 manifest")
            })
            .transpose()?;
        connection
            .execute(
                "UPDATE creation_writing_runs
                 SET base_document_json=?3, candidate_document_json=?4
                 WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, run_id, compact_base, compact_candidate],
            )
            .map_err(|error| format!("无法压缩旧 Creation run：{error}"))?;
    }

    let checkpoint_ids = {
        let mut statement = connection
            .prepare(
                "SELECT workspace_scope, run_id, checkpoint_id FROM creation_run_checkpoints
                 WHERE checkpoint_json LIKE '%\"source\"%'
                    OR checkpoint_json LIKE '%\"outputs\"%'
                    OR checkpoint_json LIKE '%\"canonicalMarkdown\"%'
                    OR candidate_document_json LIKE '%\"canonicalMarkdown\"%'",
            )
            .map_err(|error| format!("无法准备旧 Creation checkpoint 压缩：{error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|error| format!("无法读取旧 Creation checkpoint：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析旧 Creation checkpoint：{error}"))?;
        rows
    };
    for (workspace_scope, run_id, checkpoint_id) in checkpoint_ids {
        let (checkpoint_json, candidate_json) = connection
            .query_row(
                "SELECT checkpoint_json, candidate_document_json
                 FROM creation_run_checkpoints
                 WHERE workspace_scope=?1 AND run_id=?2 AND checkpoint_id=?3",
                params![workspace_scope, run_id, checkpoint_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .map_err(|error| format!("无法读取旧 Creation checkpoint 内容：{error}"))?;
        let checkpoint: Value = serde_json::from_str(&checkpoint_json)
            .map_err(|error| format!("旧 Creation checkpoint 已损坏：{error}"))?;
        let compact_checkpoint =
            json_text(&compact_checkpoint_value(checkpoint), "Creation checkpoint")?;
        let compact_candidate = candidate_json
            .map(|text| {
                let value: Value = serde_json::from_str(&text)
                    .map_err(|error| format!("旧 checkpoint 候选文稿已损坏：{error}"))?;
                json_text(&compact_document_value(&value)?, "checkpoint 候选 manifest")
            })
            .transpose()?;
        connection
            .execute(
                "UPDATE creation_run_checkpoints
                 SET checkpoint_json=?4, candidate_document_json=?5
                 WHERE workspace_scope=?1 AND run_id=?2 AND checkpoint_id=?3",
                params![
                    workspace_scope,
                    run_id,
                    checkpoint_id,
                    compact_checkpoint,
                    compact_candidate
                ],
            )
            .map_err(|error| format!("无法压缩旧 Creation checkpoint：{error}"))?;
    }
    Ok(())
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BeginCreationRunInput {
    run: Value,
    document: CreationDocumentV2,
    capability: String,
    #[serde(default)]
    stream_id: Option<String>,
    #[serde(default)]
    operation_id: Option<String>,
    #[serde(default)]
    creation_mode: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreationCheckpointInput {
    run_id: String,
    checkpoint: Value,
    #[serde(default)]
    candidate_document: Option<CreationDocumentV2>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreationRunUsageInput {
    run_id: String,
    request_id: String,
    trace_id: String,
    operation: String,
    provider: String,
    model: String,
    state: String,
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
    #[serde(default)]
    estimated_cost_usd: Option<f64>,
    #[serde(default)]
    duration_ms: u64,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewCreationCandidateInput {
    run_id: String,
    expected_document_revision: u64,
    expected_input_hash: String,
    candidate_document: CreationDocumentV2,
    #[serde(default)]
    verification_trace_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreationRunUsageSummary {
    requests: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
    estimated_cost_usd: f64,
    duration_ms: u64,
    failed_requests: u64,
    last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreationRunRecord {
    writing_run: Value,
    capability: String,
    stream_id: String,
    operation_id: String,
    creation_mode: String,
    last_sequence: i64,
    version: u64,
    /// A compact document manifest. Canonical Markdown and derived blocks are
    /// intentionally absent; the authoritative body is the CreationDocument
    /// durable asset or a replay of the native stream events.
    base_document: Value,
    candidate_document: Option<Value>,
    latest_checkpoint: Option<Value>,
    /// Kept as a compatibility field for old clients. New native reads never
    /// attach events; callers must request an explicit event page.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    events: Vec<Value>,
    usage: CreationRunUsageSummary,
    created_at: String,
    updated_at: String,
    completed_at: Option<String>,
}

/// Compact acknowledgement for high-frequency WritingRun mutations.
///
/// Stream events, checkpoints, and usage updates can occur many times while a
/// large document is being generated. Returning [`CreationRunRecord`] for each
/// mutation would repeatedly serialize the base document, candidate document,
/// latest checkpoint, and the complete event history. Recovery-oriented reads
/// keep using the full record; mutation callers only need the current run
/// identity, concurrency cursor, and aggregate usage.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreationRunMutationReceipt {
    writing_run: Value,
    capability: String,
    stream_id: String,
    operation_id: String,
    last_sequence: i64,
    version: u64,
    usage: CreationRunUsageSummary,
    created_at: String,
    updated_at: String,
    completed_at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreationRunRecoveryHeader {
    writing_run: Value,
    capability: String,
    stream_id: String,
    operation_id: String,
    last_sequence: i64,
    version: u64,
    latest_checkpoint: Option<Value>,
    usage: CreationRunUsageSummary,
    created_at: String,
    updated_at: String,
    completed_at: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroundingVerificationResult {
    verified: bool,
    required: bool,
    issues: Vec<String>,
    document: CreationDocumentV2,
    content_hash: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreationCandidateReviewReceipt {
    run: CreationRunMutationReceipt,
    grounding: GroundingVerificationResult,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreationStreamEventsPageInput {
    run_id: String,
    #[serde(default)]
    after_sequence: Option<i64>,
    #[serde(default)]
    page_size: Option<u64>,
    #[serde(default)]
    max_bytes: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreationStreamEventsPage {
    run_id: String,
    events: Vec<Value>,
    first_sequence: Option<i64>,
    last_sequence: i64,
    run_last_sequence: i64,
    next_sequence: Option<i64>,
    has_more: bool,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrandProfileUpsertInput {
    profile: Value,
    #[serde(default)]
    expected_revision: Option<u64>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrandEvaluationResult {
    evaluation_id: String,
    profile_id: String,
    profile_revision: u64,
    document_id: String,
    document_revision: u64,
    passed: bool,
    score: u8,
    checks: Vec<Value>,
    evaluated_at: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrandProfileRecord {
    profile: Value,
    content_hash: String,
    revision: u64,
    status: String,
    created_at: String,
    updated_at: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindCreationBrandProfileInput {
    document: CreationDocumentV2,
    #[serde(default)]
    profile_id: Option<String>,
    expected_document_revision: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrandProfileBindingReceipt {
    document: CreationDocumentV2,
    profile: Option<BrandProfileRecord>,
    bound_at: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluateCreationBrandProfileInput {
    document: CreationDocumentV2,
    #[serde(default)]
    profile_id: Option<String>,
}

fn content_hash(value: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(value.as_bytes()))
}

/// Return the durable metadata needed to identify a document without copying
/// its body (or the derived block list) into the run/checkpoint ledger.
///
/// `document` is deliberately accepted by reference so callers that already
/// hold a large Markdown body do not create another full document clone.
fn compact_document_manifest(document: &CreationDocumentV2) -> Value {
    let mut manifest = serde_json::to_value(document).unwrap_or_else(|_| json!({}));
    if let Some(object) = manifest.as_object_mut() {
        object.remove("canonicalMarkdown");
        object.remove("blocks");
        object.insert(
            "kind".to_string(),
            Value::String("creationDocumentManifest".to_string()),
        );
        object.insert(
            "contentHash".to_string(),
            Value::String(content_hash(&document.canonical_markdown)),
        );
        object.insert(
            "canonicalByteLength".to_string(),
            Value::from(document.canonical_markdown.len()),
        );
        object.insert("blockCount".to_string(), Value::from(document.blocks.len()));
        object.insert("assetCount".to_string(), Value::from(document.assets.len()));
        object.insert(
            "sourceRefCount".to_string(),
            Value::from(document.source_refs.len()),
        );
    }
    manifest
}

/// Convert both new compact manifests and legacy full documents to the same
/// compact shape. This makes upgrades lazy: an old run is never returned with
/// its historical body, and the first subsequent mutation overwrites it.
fn compact_document_value(value: &Value) -> Result<Value, String> {
    if value.get("kind").and_then(Value::as_str) == Some("creationDocumentManifest") {
        let mut manifest = value.clone();
        if let Some(object) = manifest.as_object_mut() {
            object.remove("canonicalMarkdown");
            object.remove("blocks");
        }
        return Ok(manifest);
    }
    let document: CreationDocumentV2 = serde_json::from_value(value.clone())
        .map_err(|error| format!("创作文稿 manifest 已损坏：{error}"))?;
    Ok(compact_document_manifest(&document))
}

fn document_manifest_hash(value: &Value) -> Option<String> {
    value
        .get("contentHash")
        .and_then(Value::as_str)
        .filter(|hash| valid_hash(hash))
        .map(str::to_string)
        .or_else(|| {
            value
                .get("canonicalMarkdown")
                .and_then(Value::as_str)
                .map(content_hash)
        })
}

/// Remove aggregate content from a checkpoint before it reaches SQLite. The
/// stream event journal remains the sole body authority; source markdown and
/// candidate bodies are therefore represented only by hashes/asset refs.
fn compact_checkpoint_value(mut checkpoint: Value) -> Value {
    let Some(object) = checkpoint.as_object_mut() else {
        return checkpoint;
    };
    object.remove("events");
    if let Some(stream) = object.get_mut("streamState").and_then(Value::as_object_mut) {
        stream.remove("channels");
        stream.remove("receivedEventIds");
    }
    if let Some(execution) = object.get_mut("execution").and_then(Value::as_object_mut) {
        for key in [
            "source",
            "outputs",
            "protectedBlocks",
            "completedMarkdown",
            "partialBatchMarkdown",
        ] {
            execution.remove(key);
        }
        if let Some(candidate) = execution
            .get_mut("candidate")
            .and_then(Value::as_object_mut)
        {
            for key in [
                "original",
                "revised",
                "candidateDocument",
                "canonicalMarkdown",
                "blocks",
            ] {
                candidate.remove(key);
            }
        }
    }
    checkpoint
}

fn json_text(value: &Value, label: &str) -> Result<String, String> {
    serde_json::to_string(value).map_err(|error| format!("无法序列化{label}：{error}"))
}

fn valid_hash(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .chars()
                .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    })
}

fn valid_runtime_id(value: &str) -> bool {
    is_valid_record_id(value)
}

fn sqlite_integer(value: u64, label: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("{label} 超出 SQLite 整数范围"))
}

fn value_string<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .ok_or_else(|| format!("创作运行缺少 `{key}`"))
}

fn set_value_string(value: &mut Value, key: &str, item: Option<&str>) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    object.insert(
        key.to_string(),
        item.map_or(Value::Null, |item| Value::String(item.to_string())),
    );
}

fn set_value_u64(value: &mut Value, key: &str, item: u64) {
    if let Some(object) = value.as_object_mut() {
        object.insert(key.to_string(), Value::from(item));
    }
}

fn normalized_optional_id(value: Option<String>, fallback: impl FnOnce() -> String) -> String {
    value
        .map(|item| item.trim().to_string())
        .filter(|item| valid_runtime_id(item))
        .unwrap_or_else(fallback)
}

fn active_checkpoint_id(run_id: &str) -> String {
    let readable = format!("checkpoint-active-{run_id}");
    if valid_runtime_id(&readable) {
        readable
    } else {
        format!("checkpoint-active-{:x}", Sha256::digest(run_id.as_bytes()))
    }
}

fn ensure_no_embedded_binary(markdown: &str) -> Result<(), String> {
    let lower = markdown.to_ascii_lowercase();
    if lower.contains("data:image/") && lower.contains(";base64,") {
        return Err(
            "创作正文与 WritingRun 快照不能包含 Base64 图片；请使用耐久素材相对路径".to_string(),
        );
    }
    Ok(())
}

fn ensure_no_embedded_binary_value(value: &Value, path: &str) -> Result<(), String> {
    match value {
        Value::String(text) => {
            let lower = text.to_ascii_lowercase();
            if lower.starts_with("data:") && lower.contains(";base64,") {
                return Err(format!("创作运行时 `{path}` 不能保存 Base64 数据 URL"));
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                ensure_no_embedded_binary_value(item, &format!("{path}[{index}]"))?;
            }
        }
        Value::Object(object) => {
            for (key, item) in object {
                if matches!(
                    key.to_ascii_lowercase().as_str(),
                    "contentbase64" | "database64" | "base64" | "dataurl"
                ) && !item.is_null()
                {
                    return Err(format!("创作运行时 `{path}.{key}` 不能保存 Base64 数据"));
                }
                ensure_no_embedded_binary_value(item, &format!("{path}.{key}"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_begin_input(input: &BeginCreationRunInput) -> Result<(), String> {
    if !CREATION_CAPABILITIES.contains(&input.capability.as_str()) {
        return Err("创作运行 capability 只能是 creation.generate 或 creation.edit".to_string());
    }
    if !input.run.is_object() {
        return Err("WritingRun 必须是 JSON 对象".to_string());
    }
    ensure_no_embedded_binary_value(&input.run, "writingRun")?;
    let run_id = value_string(&input.run, "id")?;
    let document_id = value_string(&input.run, "documentId")?;
    let state = value_string(&input.run, "state")?;
    let input_hash = value_string(&input.run, "inputHash")?;
    let revision = input
        .run
        .get("documentRevision")
        .and_then(Value::as_u64)
        .ok_or_else(|| "WritingRun 缺少有效 documentRevision".to_string())?;
    if !valid_runtime_id(run_id) || !valid_runtime_id(document_id) {
        return Err("WritingRun ID 或文稿 ID 无效".to_string());
    }
    if state != "queued" {
        return Err("新建 WritingRun 的初始状态必须是 queued".to_string());
    }
    if !valid_hash(input_hash) {
        return Err("WritingRun inputHash 必须是 SHA-256".to_string());
    }
    if document_id != input.document.id || revision != input.document.revision {
        return Err("WritingRun 与基础文稿的 ID 或 revision 不一致".to_string());
    }
    ensure_no_embedded_binary(&input.document.canonical_markdown)?;
    Ok(())
}

fn usage_summary(
    connection: &Connection,
    workspace_scope: &str,
    run_id: &str,
) -> Result<CreationRunUsageSummary, String> {
    connection
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(prompt_tokens), 0),
                    COALESCE(SUM(completion_tokens), 0), COALESCE(SUM(total_tokens), 0),
                    COALESCE(SUM(estimated_cost_usd), 0), COALESCE(SUM(duration_ms), 0),
                    COALESCE(SUM(CASE WHEN state='failed' THEN 1 ELSE 0 END), 0)
             FROM creation_run_usage_events
             WHERE workspace_scope=?1 AND run_id=?2",
            params![workspace_scope, run_id],
            |row| {
                Ok(CreationRunUsageSummary {
                    requests: row.get::<_, i64>(0)?.max(0) as u64,
                    prompt_tokens: row.get::<_, i64>(1)?.max(0) as u64,
                    completion_tokens: row.get::<_, i64>(2)?.max(0) as u64,
                    total_tokens: row.get::<_, i64>(3)?.max(0) as u64,
                    estimated_cost_usd: row.get::<_, f64>(4)?,
                    duration_ms: row.get::<_, i64>(5)?.max(0) as u64,
                    failed_requests: row.get::<_, i64>(6)?.max(0) as u64,
                    last_error: None,
                })
            },
        )
        .map_err(|error| format!("无法汇总创作模型用量：{error}"))
        .and_then(|mut summary| {
            summary.last_error = connection
                .query_row(
                    "SELECT error FROM creation_run_usage_events
                     WHERE workspace_scope=?1 AND run_id=?2 AND error IS NOT NULL
                     ORDER BY updated_at DESC LIMIT 1",
                    params![workspace_scope, run_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| format!("无法读取创作模型错误：{error}"))?;
            Ok(summary)
        })
}

fn latest_checkpoint(
    connection: &Connection,
    workspace_scope: &str,
    run_id: &str,
) -> Result<Option<Value>, String> {
    connection
        .query_row(
            "SELECT checkpoint_json FROM creation_run_checkpoints
             WHERE workspace_scope=?1 AND run_id=?2
             ORDER BY created_at DESC, checkpoint_id DESC LIMIT 1",
            params![workspace_scope, run_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("无法读取创作 checkpoint：{error}"))?
        .map(|text| {
            serde_json::from_str(&text)
                .map(compact_checkpoint_value)
                .map_err(|error| format!("创作 checkpoint 已损坏：{error}"))
        })
        .transpose()
}

fn load_run_from_connection(
    connection: &Connection,
    workspace_scope: &str,
    run_id: &str,
) -> Result<CreationRunRecord, String> {
    let stored = connection
        .query_row(
            "SELECT capability, stream_id, operation_id, creation_mode, last_sequence, version,
                    run_json, base_document_json, candidate_document_json,
                    created_at, updated_at, completed_at
             FROM creation_writing_runs WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, run_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, Option<String>>(11)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("无法读取 WritingRun：{error}"))?
        .ok_or_else(|| "未找到 WritingRun".to_string())?;
    let writing_run = serde_json::from_str(&stored.6)
        .map_err(|error| format!("WritingRun 数据已损坏：{error}"))?;
    let base_document: Value = serde_json::from_str(&stored.7)
        .map_err(|error| format!("WritingRun 基础文稿已损坏：{error}"))?;
    let base_document = compact_document_value(&base_document)?;
    let candidate_document = stored
        .8
        .map(|text| {
            let value: Value = serde_json::from_str(&text)
                .map_err(|error| format!("WritingRun 候选文稿已损坏：{error}"))?;
            compact_document_value(&value)
        })
        .transpose()?;
    Ok(CreationRunRecord {
        writing_run,
        capability: stored.0,
        stream_id: stored.1,
        operation_id: stored.2,
        creation_mode: stored.3,
        last_sequence: stored.4,
        version: stored.5.max(1) as u64,
        base_document,
        candidate_document,
        latest_checkpoint: latest_checkpoint(connection, workspace_scope, run_id)?,
        events: Vec::new(),
        usage: usage_summary(connection, workspace_scope, run_id)?,
        created_at: stored.9,
        updated_at: stored.10,
        completed_at: stored.11,
    })
}

fn load_run_mutation_receipt_from_connection(
    connection: &Connection,
    workspace_scope: &str,
    run_id: &str,
) -> Result<CreationRunMutationReceipt, String> {
    let stored = connection
        .query_row(
            "SELECT capability, stream_id, operation_id, last_sequence, version,
                    run_json, created_at, updated_at, completed_at
             FROM creation_writing_runs WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, run_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("无法读取 WritingRun mutation 回执：{error}"))?
        .ok_or_else(|| "未找到 WritingRun".to_string())?;
    let writing_run = serde_json::from_str(&stored.5)
        .map_err(|error| format!("WritingRun 数据已损坏：{error}"))?;
    Ok(CreationRunMutationReceipt {
        writing_run,
        capability: stored.0,
        stream_id: stored.1,
        operation_id: stored.2,
        last_sequence: stored.3,
        version: stored.4.max(1) as u64,
        usage: usage_summary(connection, workspace_scope, run_id)?,
        created_at: stored.6,
        updated_at: stored.7,
        completed_at: stored.8,
    })
}

fn load_run_recovery_header_from_connection(
    connection: &Connection,
    workspace_scope: &str,
    run_id: &str,
) -> Result<CreationRunRecoveryHeader, String> {
    let receipt = load_run_mutation_receipt_from_connection(connection, workspace_scope, run_id)?;
    Ok(CreationRunRecoveryHeader {
        writing_run: receipt.writing_run,
        capability: receipt.capability,
        stream_id: receipt.stream_id,
        operation_id: receipt.operation_id,
        last_sequence: receipt.last_sequence,
        version: receipt.version,
        latest_checkpoint: latest_checkpoint(connection, workspace_scope, run_id)?,
        usage: receipt.usage,
        created_at: receipt.created_at,
        updated_at: receipt.updated_at,
        completed_at: receipt.completed_at,
    })
}

#[tauri::command]
pub fn begin_creation_run(
    database: State<'_, RuntimeDatabase>,
    input: BeginCreationRunInput,
) -> Result<CreationRunRecord, String> {
    validate_begin_input(&input)?;
    let workspace_scope = database.local_workspace_scope()?;
    let run_id = value_string(&input.run, "id")?.to_string();
    let document_id = input.document.id.clone();
    let input_hash = value_string(&input.run, "inputHash")?.to_string();
    let stream_id = normalized_optional_id(input.stream_id, || format!("stream-{run_id}"));
    let operation_id = normalized_optional_id(input.operation_id, || format!("operation-{run_id}"));
    let run_json = json_text(&input.run, "WritingRun")?;
    let base_document_json = json_text(
        &compact_document_manifest(&input.document),
        "WritingRun 基础文稿 manifest",
    )?;
    let document_revision = sqlite_integer(input.document.revision, "WritingRun documentRevision")?;
    let creation_mode = input
        .creation_mode
        .as_deref()
        .unwrap_or("quick")
        .trim();
    if !matches!(creation_mode, "quick" | "professional") {
        return Err(format!(
            "无效的创作模式：{creation_mode}，必须是 'quick' 或 'professional'"
        ));
    }
    let now = Utc::now().to_rfc3339();
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始 WritingRun 事务：{error}"))?;
    if let Some(existing_hash) = transaction
        .query_row(
            "SELECT input_hash FROM creation_writing_runs
             WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, run_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("无法检查 WritingRun 幂等键：{error}"))?
    {
        if existing_hash != input_hash {
            return Err("同一 WritingRun ID 不能绑定不同输入".to_string());
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交 WritingRun 幂等读取：{error}"))?;
        return load_run_from_connection(&connection, &workspace_scope, &run_id);
    }
    transaction
        .execute(
            "INSERT INTO creation_writing_runs
             (workspace_scope, id, document_id, document_revision, capability, state,
              input_hash, output_hash, run_json, base_document_json,
              candidate_document_json, stream_id, operation_id, creation_mode,
              last_sequence, version, created_at, updated_at, completed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6, NULL, ?7, ?8, NULL,
                     ?9, ?10, ?11, -1, 1, ?12, ?12, NULL)",
            params![
                workspace_scope,
                run_id,
                document_id,
                document_revision,
                input.capability,
                input_hash,
                run_json,
                base_document_json,
                stream_id,
                operation_id,
                creation_mode,
                now,
            ],
        )
        .map_err(|error| {
            if error
                .to_string()
                .contains("idx_creation_writing_run_document_lock")
                || error.to_string().contains("UNIQUE constraint failed")
            {
                "该文稿已有未完成 WritingRun；请恢复、取消或审核后再开始新运行".to_string()
            } else {
                format!("无法创建 WritingRun：{error}")
            }
        })?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交 WritingRun：{error}"))?;
    load_run_from_connection(&connection, &workspace_scope, &run_id)
}

#[tauri::command]
pub fn get_creation_run(
    database: State<'_, RuntimeDatabase>,
    run_id: String,
) -> Result<CreationRunRecord, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    load_run_from_connection(&connection, &workspace_scope, run_id.trim())
}

/// Read one bounded page from the append-only event journal.
///
/// The page bounds protect a single SQLite/IPC response only. There is no
/// aggregate run limit: `nextSequence` can be followed until `hasMore=false`.
#[tauri::command]
pub fn read_creation_stream_events_page(
    database: State<'_, RuntimeDatabase>,
    input: CreationStreamEventsPageInput,
) -> Result<CreationStreamEventsPage, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let run_id = input.run_id.trim();
    if !valid_runtime_id(run_id) {
        return Err("WritingRun ID 无效".to_string());
    }
    let after_sequence = input.after_sequence.unwrap_or(-1);
    if after_sequence < -1 {
        return Err("Creation 事件页游标无效".to_string());
    }
    let page_size = usize::try_from(input.page_size.unwrap_or(DEFAULT_STREAM_PAGE_EVENTS as u64))
        .unwrap_or(MAX_STREAM_PAGE_EVENTS)
        .clamp(1, MAX_STREAM_PAGE_EVENTS);
    let max_bytes = usize::try_from(input.max_bytes.unwrap_or(DEFAULT_STREAM_PAGE_BYTES as u64))
        .unwrap_or(MAX_STREAM_PAGE_BYTES)
        .clamp(MAX_STREAM_EVENT_BYTES, MAX_STREAM_PAGE_BYTES);
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let stored_last_sequence = connection
        .query_row(
            "SELECT last_sequence FROM creation_writing_runs
             WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, run_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|error| format!("无法读取 Creation 事件页游标：{error}"))?
        .ok_or_else(|| "未找到 WritingRun".to_string())?;
    if after_sequence > stored_last_sequence {
        return Err("Creation 事件页游标领先于 WritingRun".to_string());
    }
    let query_limit = i64::try_from(page_size).unwrap_or(MAX_STREAM_PAGE_EVENTS as i64);
    let mut statement = connection
        .prepare(
            "SELECT sequence, event_json FROM creation_agent_stream_events
             WHERE workspace_scope=?1 AND run_id=?2 AND sequence>?3
             ORDER BY sequence ASC LIMIT ?4",
        )
        .map_err(|error| format!("无法准备 Creation 事件页查询：{error}"))?;
    let mapped = statement
        .query_map(
            params![workspace_scope, run_id, after_sequence, query_limit],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(|error| format!("无法读取 Creation 事件页：{error}"))?;
    let mut events = Vec::new();
    let mut bytes = 0usize;
    for (expected_sequence, row) in (after_sequence + 1..).zip(mapped) {
        let (sequence, text) =
            row.map_err(|error| format!("无法解析 Creation 事件页记录：{error}"))?;
        if sequence != expected_sequence {
            return Err(format!(
                "Creation 事件页序列不连续：期望 {expected_sequence}，实际 {sequence}"
            ));
        }
        let next_bytes = bytes.saturating_add(text.len());
        if !events.is_empty() && next_bytes > max_bytes {
            break;
        }
        let event: Value =
            serde_json::from_str(&text).map_err(|error| format!("Creation 事件已损坏：{error}"))?;
        events.push(event);
        bytes = next_bytes;
    }
    let first_sequence = events
        .first()
        .and_then(|event| event.get("sequence"))
        .and_then(Value::as_i64);
    let page_last_sequence = events
        .last()
        .and_then(|event| event.get("sequence"))
        .and_then(Value::as_i64)
        .unwrap_or(after_sequence);
    let has_more = page_last_sequence < stored_last_sequence;
    if has_more && events.is_empty() {
        return Err("Creation 事件页未取得游标后的首个事件".to_string());
    }
    Ok(CreationStreamEventsPage {
        run_id: run_id.to_string(),
        events,
        first_sequence,
        last_sequence: page_last_sequence,
        run_last_sequence: stored_last_sequence,
        next_sequence: has_more.then_some(page_last_sequence + 1),
        has_more,
    })
}

fn validate_stream_event(
    event: &Value,
    stream_id: &str,
    operation_id: &str,
    capability: &str,
    expected_sequence: i64,
) -> Result<(String, String), String> {
    ensure_no_embedded_binary_value(event, "streamEvent")?;
    let event_bytes = serde_json::to_vec(event)
        .map_err(|error| format!("无法序列化 Creation Agent Stream 事件：{error}"))?;
    if event_bytes.len() > MAX_STREAM_EVENT_BYTES {
        return Err(format!(
            "单个 Creation Agent Stream 事件超过 {MAX_STREAM_EVENT_BYTES} 字节动态安全分块边界；请继续使用下一 sequence"
        ));
    }
    let event_id = value_string(event, "eventId")?;
    let event_type = value_string(event, "eventType")?;
    if !valid_runtime_id(event_id) || !STREAM_EVENT_TYPES.contains(&event_type) {
        return Err("Creation Agent Stream 事件 ID 或类型无效".to_string());
    }
    let sequence = event
        .get("sequence")
        .and_then(Value::as_i64)
        .ok_or_else(|| "Creation Agent Stream 事件缺少 sequence".to_string())?;
    if sequence != expected_sequence {
        return Err(format!(
            "Creation Agent Stream 序列不连续：期望 {expected_sequence}，收到 {sequence}"
        ));
    }
    if value_string(event, "streamId")? != stream_id
        || value_string(event, "operationId")? != operation_id
        || value_string(event, "capability")? != capability
    {
        return Err("Creation Agent Stream 事件身份与 WritingRun 不一致".to_string());
    }
    if event
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .is_none()
        || !event.get("payload").is_some_and(Value::is_object)
    {
        return Err("Creation Agent Stream 事件时间或 payload 无效".to_string());
    }
    if expected_sequence == 0 && event_type != "streamStarted" {
        return Err("Creation Agent Stream 必须以 streamStarted 开始".to_string());
    }
    if expected_sequence > 0 && event_type == "streamStarted" {
        return Err("Creation Agent Stream 不能重复开始".to_string());
    }
    if matches!(event_type, "contentDelta" | "contentSnapshot") {
        let content = event
            .pointer("/payload/content")
            .and_then(Value::as_str)
            .ok_or_else(|| "内容事件缺少文本 content".to_string())?;
        ensure_no_embedded_binary(content)?;
    }
    Ok((event_id.to_string(), event_type.to_string()))
}

fn update_run_state_for_event(
    run: &mut Value,
    event_type: &str,
    timestamp: &str,
    creation_mode: &str,
) -> String {
    let next_state = match event_type {
        "streamStarted" => "running".to_string(),
        "streamCompleted" => {
            // 快速模式：直接完成，跳过审核
            if creation_mode == "quick" {
                "succeeded".to_string()
            } else {
                "awaitingReview".to_string()
            }
        }
        "streamFailed" => "failed".to_string(),
        "streamCancelled" => "cancelled".to_string(),
        _ => match run.get("state").and_then(Value::as_str) {
            Some("queued") | None => "running".to_string(),
            Some(state) => state.to_string(),
        },
    };
    set_value_string(run, "state", Some(&next_state));
    if matches!(next_state.as_str(), "failed" | "cancelled" | "succeeded") {
        set_value_string(run, "completedAt", Some(timestamp));
    }
    next_state
}

#[tauri::command]
pub fn append_creation_stream_event(
    database: State<'_, RuntimeDatabase>,
    run_id: String,
    event: Value,
) -> Result<CreationRunMutationReceipt, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let run_id = run_id.trim();
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始 Creation Agent Stream 事务：{error}"))?;
    let stored = transaction
        .query_row(
            "SELECT capability, state, stream_id, operation_id, last_sequence, run_json, creation_mode
             FROM creation_writing_runs WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, run_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("无法读取 Creation Agent Stream：{error}"))?
        .ok_or_else(|| "未找到 WritingRun".to_string())?;
    if !ACTIVE_RUN_STATES.contains(&stored.1.as_str()) || stored.1 == "awaitingReview" {
        return Err("终态或待审核 WritingRun 不能继续接收流事件".to_string());
    }
    let expected_sequence = stored.4 + 1;
    let (event_id, event_type) =
        validate_stream_event(&event, &stored.2, &stored.3, &stored.0, expected_sequence)?;
    if event_type == "streamCompleted" {
        let content_events = transaction
            .query_row(
                "SELECT COUNT(*) FROM creation_agent_stream_events
                 WHERE workspace_scope=?1 AND run_id=?2
                   AND event_type IN ('contentDelta', 'contentSnapshot')",
                params![workspace_scope, run_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("无法检查 Creation Agent Stream 输出：{error}"))?;
        if content_events == 0 {
            return Err("Creation Agent Stream 没有真实内容事件，不能标记完成".to_string());
        }
    }
    let timestamp = value_string(&event, "timestamp")?;
    let event_json = json_text(&event, "Creation Agent Stream 事件")?;
    transaction
        .execute(
            "INSERT INTO creation_agent_stream_events
             (workspace_scope, run_id, sequence, event_id, event_type, event_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                workspace_scope,
                run_id,
                expected_sequence,
                event_id,
                event_type,
                event_json,
                timestamp,
            ],
        )
        .map_err(|error| format!("无法保存 Creation Agent Stream 事件：{error}"))?;
    let mut run: Value = serde_json::from_str(&stored.5)
        .map_err(|error| format!("WritingRun 数据已损坏：{error}"))?;
    let creation_mode = &stored.6;
    let next_state = update_run_state_for_event(&mut run, &event_type, timestamp, creation_mode);
    let run_json = json_text(&run, "WritingRun")?;
    let completed_at = matches!(next_state.as_str(), "failed" | "cancelled" | "succeeded")
        .then_some(timestamp);
    transaction
        .execute(
            "UPDATE creation_writing_runs
             SET state=?3, run_json=?4, last_sequence=?5, version=version+1,
                 updated_at=?6, completed_at=COALESCE(?7, completed_at)
             WHERE workspace_scope=?1 AND id=?2",
            params![
                workspace_scope,
                run_id,
                next_state,
                run_json,
                expected_sequence,
                timestamp,
                completed_at,
            ],
        )
        .map_err(|error| format!("无法推进 Creation Agent Stream：{error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交 Creation Agent Stream：{error}"))?;
    load_run_mutation_receipt_from_connection(&connection, &workspace_scope, run_id)
}

#[tauri::command]
pub fn checkpoint_creation_run(
    database: State<'_, RuntimeDatabase>,
    mut input: CreationCheckpointInput,
) -> Result<CreationRunMutationReceipt, String> {
    if !input.checkpoint.is_object() {
        return Err("Creation checkpoint 必须是 JSON 对象".to_string());
    }
    ensure_no_embedded_binary_value(&input.checkpoint, "checkpoint")?;
    let workspace_scope = database.local_workspace_scope()?;
    let run_id = input.run_id.trim();
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始 Creation checkpoint 事务：{error}"))?;
    let stored = transaction
        .query_row(
            "SELECT document_id, document_revision, input_hash, state, last_sequence, creation_mode
             FROM creation_writing_runs WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, run_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("无法读取待 checkpoint WritingRun：{error}"))?
        .ok_or_else(|| "未找到 WritingRun".to_string())?;

    let creation_mode = &stored.5;

    if !ACTIVE_RUN_STATES.contains(&stored.3.as_str()) {
        return Err("终态 WritingRun 不能新增 checkpoint".to_string());
    }
    if input
        .checkpoint
        .pointer("/writingRun/documentRevision")
        .and_then(Value::as_i64)
        .is_some_and(|revision| revision != stored.1)
        || input
            .checkpoint
            .pointer("/writingRun/inputHash")
            .and_then(Value::as_str)
            .is_some_and(|hash| hash != stored.2)
    {
        return Err("Creation checkpoint 的文稿 revision 或输入哈希已过期".to_string());
    }
    let candidate = input
        .candidate_document
        .map(|document| normalize_document(document).map(|result| result.document))
        .transpose()?;

    // 快速模式：跳过候选审核逻辑
    if creation_mode == "quick" {
        if candidate.is_some() {
            log::debug!("快速模式：忽略候选文稿，不进行审核");
        }
        // 快速模式下不处理 candidate_document
    } else {
        // 专业模式：保持原有的候选审核逻辑
        if let Some(document) = &candidate {
            ensure_no_embedded_binary(&document.canonical_markdown)?;
        }
    }

    let checkpoint_id = input
        .checkpoint
        .get("checkpointId")
        .and_then(Value::as_str)
        .filter(|value| valid_runtime_id(value))
        .map(str::to_string)
        .unwrap_or_else(|| active_checkpoint_id(run_id));
    input
        .checkpoint
        .as_object_mut()
        .expect("checkpoint object was validated")
        .insert(
            "checkpointId".to_string(),
            Value::String(checkpoint_id.clone()),
        );
    let candidate_hash = candidate
        .as_ref()
        .map(|document| content_hash(&document.canonical_markdown));
    input.checkpoint = compact_checkpoint_value(input.checkpoint);
    let checkpoint_json = json_text(&input.checkpoint, "Creation checkpoint")?;
    let synchronized_run_json = input
        .checkpoint
        .get("writingRun")
        .filter(|writing_run| writing_run.get("id").is_some())
        .map(|writing_run| {
            if !writing_run.is_object()
                || value_string(writing_run, "id")? != run_id
                || value_string(writing_run, "documentId")? != stored.0
                || writing_run.get("documentRevision").and_then(Value::as_i64) != Some(stored.1)
                || value_string(writing_run, "inputHash")? != stored.2
                || value_string(writing_run, "state")? != stored.3
            {
                return Err(
                    "Creation checkpoint.writingRun 的身份、revision、输入哈希或状态不一致"
                        .to_string(),
                );
            }
            if writing_run
                .get("outputHash")
                .and_then(Value::as_str)
                .is_some_and(|hash| !valid_hash(hash))
            {
                return Err("Creation checkpoint.writingRun outputHash 无效".to_string());
            }
            ensure_no_embedded_binary_value(writing_run, "checkpoint.writingRun")?;
            json_text(writing_run, "checkpoint WritingRun")
        })
        .transpose()?;
    let candidate_document_json = if creation_mode == "quick" {
        // 快速模式：不存储候选文稿
        None
    } else {
        // 专业模式：存储候选文稿
        candidate
            .as_ref()
            .map(|document| json_text(&compact_document_manifest(document), "候选文稿 manifest"))
            .transpose()?
    };
    let now = Utc::now().to_rfc3339();
    transaction
        .execute(
            "INSERT INTO creation_run_checkpoints
             (workspace_scope, run_id, checkpoint_id, sequence, document_revision,
              input_hash, candidate_hash, checkpoint_json, candidate_document_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(workspace_scope, run_id, checkpoint_id) DO UPDATE SET
               sequence=excluded.sequence, candidate_hash=excluded.candidate_hash,
               checkpoint_json=excluded.checkpoint_json,
               candidate_document_json=excluded.candidate_document_json,
               created_at=excluded.created_at",
            params![
                workspace_scope,
                run_id,
                checkpoint_id,
                stored.4,
                stored.1,
                stored.2,
                candidate_hash,
                checkpoint_json,
                candidate_document_json,
                now,
            ],
        )
        .map_err(|error| format!("无法保存 Creation checkpoint：{error}"))?;
    transaction
        .execute(
            "UPDATE creation_writing_runs
             SET candidate_document_json=COALESCE(?3, candidate_document_json),
                 run_json=COALESCE(?4, run_json), version=version+1, updated_at=?5
             WHERE workspace_scope=?1 AND id=?2",
            params![
                workspace_scope,
                run_id,
                candidate_document_json,
                synchronized_run_json,
                now
            ],
        )
        .map_err(|error| format!("无法同步 WritingRun checkpoint：{error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交 Creation checkpoint：{error}"))?;
    load_run_mutation_receipt_from_connection(&connection, &workspace_scope, run_id)
}

#[tauri::command]
pub fn record_creation_run_usage(
    database: State<'_, RuntimeDatabase>,
    input: CreationRunUsageInput,
) -> Result<CreationRunMutationReceipt, String> {
    let run_id = input.run_id.trim();
    if !valid_runtime_id(run_id)
        || !valid_runtime_id(input.request_id.trim())
        || !valid_runtime_id(input.trace_id.trim())
        || !CREATION_CAPABILITIES.contains(&input.operation.as_str())
            && !matches!(
                input.operation.as_str(),
                "creation.grounding.verify" | "creation.brand.evaluate"
            )
        || !["started", "succeeded", "failed", "cancelled"].contains(&input.state.as_str())
        || input.provider.trim().is_empty()
        || input.model.trim().is_empty()
        || input
            .estimated_cost_usd
            .is_some_and(|value| !value.is_finite() || value < 0.0)
    {
        return Err("创作模型用量记录无效".to_string());
    }
    let workspace_scope = database.local_workspace_scope()?;
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始创作模型用量事务：{error}"))?;
    let existing = transaction
        .query_row(
            "SELECT trace_id, operation, provider, model, state
             FROM creation_run_usage_events
             WHERE workspace_scope=?1 AND run_id=?2 AND request_id=?3",
            params![workspace_scope, run_id, input.request_id.trim()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("无法检查创作模型用量幂等键：{error}"))?;
    if let Some(existing) = existing {
        if existing.0 != input.trace_id.trim()
            || existing.1 != input.operation
            || existing.2 != input.provider.trim()
            || existing.3 != input.model.trim()
        {
            return Err("同一创作模型 requestId 不能绑定不同请求身份".to_string());
        }
        if existing.4 != "started" && input.state != existing.4 {
            return Err("终态创作模型用量记录不能回退或改写为另一终态".to_string());
        }
    }
    let now = Utc::now().to_rfc3339();
    let total_tokens = input
        .total_tokens
        .max(input.prompt_tokens.saturating_add(input.completion_tokens));
    let prompt_tokens = sqlite_integer(input.prompt_tokens, "promptTokens")?;
    let completion_tokens = sqlite_integer(input.completion_tokens, "completionTokens")?;
    let total_tokens = sqlite_integer(total_tokens, "totalTokens")?;
    let duration_ms = sqlite_integer(input.duration_ms, "durationMs")?;
    transaction
        .execute(
            "INSERT INTO creation_run_usage_events
             (workspace_scope, run_id, request_id, trace_id, operation, provider, model,
              state, prompt_tokens, completion_tokens, total_tokens, estimated_cost_usd,
              duration_ms, error, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15)
             ON CONFLICT(workspace_scope, run_id, request_id) DO UPDATE SET
               trace_id=excluded.trace_id, state=excluded.state,
               prompt_tokens=excluded.prompt_tokens,
               completion_tokens=excluded.completion_tokens,
               total_tokens=excluded.total_tokens,
               estimated_cost_usd=excluded.estimated_cost_usd,
               duration_ms=excluded.duration_ms, error=excluded.error,
               updated_at=excluded.updated_at",
            params![
                workspace_scope,
                run_id,
                input.request_id.trim(),
                input.trace_id.trim(),
                input.operation,
                input.provider.trim(),
                input.model.trim(),
                input.state,
                prompt_tokens,
                completion_tokens,
                total_tokens,
                input.estimated_cost_usd,
                duration_ms,
                input
                    .error
                    .map(|value| value.chars().take(4_000).collect::<String>()),
                now,
            ],
        )
        .map_err(|error| format!("无法保存创作模型用量：{error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交创作模型用量：{error}"))?;
    load_run_mutation_receipt_from_connection(&connection, &workspace_scope, run_id)
}

fn recover_runs_in_connection(
    connection: &mut Connection,
    workspace_scope: &str,
) -> Result<Vec<String>, String> {
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始 WritingRun 恢复事务：{error}"))?;
    let running = {
        let mut statement = transaction
            .prepare(
                "SELECT id, run_json FROM creation_writing_runs
                 WHERE workspace_scope=?1 AND state='running' ORDER BY updated_at ASC",
            )
            .map_err(|error| format!("无法准备 WritingRun 恢复查询：{error}"))?;
        let mapped = statement
            .query_map([workspace_scope], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| format!("无法读取中断 WritingRun：{error}"))?;
        mapped
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析中断 WritingRun：{error}"))?
    };
    let now = Utc::now().to_rfc3339();
    let mut recovered = Vec::new();
    for (run_id, run_json) in running {
        let mut run: Value = serde_json::from_str(&run_json)
            .map_err(|error| format!("中断 WritingRun 数据已损坏：{error}"))?;
        set_value_string(&mut run, "state", Some("queued"));
        set_value_string(&mut run, "failureReason", None);
        transaction
            .execute(
                "UPDATE creation_writing_runs
                 SET state='queued', run_json=?3, version=version+1, updated_at=?4
                 WHERE workspace_scope=?1 AND id=?2 AND state='running'",
                params![workspace_scope, run_id, json_text(&run, "WritingRun")?, now],
            )
            .map_err(|error| format!("无法恢复 WritingRun：{error}"))?;
        recovered.push(run_id);
    }
    transaction
        .commit()
        .map_err(|error| format!("无法提交 WritingRun 恢复事务：{error}"))?;
    Ok(recovered)
}

pub(crate) fn recover_creation_runs_for_startup(
    database: &RuntimeDatabase,
) -> Result<usize, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    recover_runs_in_connection(&mut connection, &workspace_scope).map(|items| items.len())
}

#[tauri::command]
pub fn recover_creation_runs(
    database: State<'_, RuntimeDatabase>,
) -> Result<Vec<CreationRunRecoveryHeader>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let _ = recover_runs_in_connection(&mut connection, &workspace_scope)?;
    let ids = {
        let mut statement = connection
            .prepare(
                "SELECT id FROM creation_writing_runs
                 WHERE workspace_scope=?1 AND state IN ('queued', 'awaitingReview')
                 ORDER BY updated_at DESC",
            )
            .map_err(|error| format!("无法准备可恢复 WritingRun 查询：{error}"))?;
        let mapped = statement
            .query_map([&workspace_scope], |row| row.get::<_, String>(0))
            .map_err(|error| format!("无法读取可恢复 WritingRun：{error}"))?;
        mapped
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析可恢复 WritingRun：{error}"))?
    };
    ids.into_iter()
        .map(|id| load_run_recovery_header_from_connection(&connection, &workspace_scope, &id))
        .collect()
}

fn source_excerpt_hash(source: &super::model::CreationSourceRef) -> Option<String> {
    source.excerpt.as_deref().map(content_hash)
}

fn grounding_block_issue(
    block: &GroundingLedgerBlock,
    document: &CreationDocumentV2,
    sources: &BTreeMap<&str, &super::model::CreationSourceRef>,
) -> Option<String> {
    if !document
        .blocks
        .iter()
        .any(|candidate| candidate.id == block.id)
    {
        return Some(format!("证据块 `{}` 不再对应当前正文块", block.id));
    }
    if block.verdict != "supported" || block.source_ref_ids.is_empty() || block.evidence.is_empty()
    {
        return Some(format!("证据块 `{}` 尚未获得 supported 核验结论", block.id));
    }
    for source_id in &block.source_ref_ids {
        let Some(source) = sources.get(source_id.as_str()).copied() else {
            return Some(format!(
                "证据块 `{}` 引用了不存在的来源 `{source_id}`",
                block.id
            ));
        };
        if source.kind == "vaultNote"
            && (source.vault_id.as_deref().is_none_or(str::is_empty)
                || source.relative_path.as_deref().is_none_or(str::is_empty)
                || !source.content_hash.as_deref().is_some_and(valid_hash))
        {
            return Some(format!(
                "跨 Vault 来源 `{source_id}` 缺少 vaultId、relativePath 或 contentHash"
            ));
        }
        if let (Some(expected), Some(actual)) =
            (source.excerpt_hash.as_deref(), source_excerpt_hash(source))
        {
            if expected != actual {
                return Some(format!("来源 `{source_id}` 的 excerptHash 已过期"));
            }
        }
    }
    for evidence in &block.evidence {
        let Some(source) = sources.get(evidence.source_ref_id.as_str()).copied() else {
            return Some(format!("证据块 `{}` 的引文来源不存在", block.id));
        };
        let quote = evidence
            .quote
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let excerpt = source
            .excerpt
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if quote.is_empty() || !excerpt.contains(&quote) {
            return Some(format!(
                "证据块 `{}` 的引文无法在来源 `{}` 摘录中复核",
                block.id, evidence.source_ref_id
            ));
        }
    }
    None
}

pub fn reverify_creation_grounding_document(
    document: CreationDocumentV2,
    verification_trace_id: Option<&str>,
) -> Result<GroundingVerificationResult, String> {
    ensure_no_embedded_binary(&document.canonical_markdown)?;
    let mut normalized = normalize_document(document)?.document;
    let hash = content_hash(&normalized.canonical_markdown);
    let required = !normalized.grounding_ledger.blocks.is_empty()
        || !normalized.provenance.source_ids.is_empty();
    let sources = normalized
        .source_refs
        .iter()
        .map(|source| (source.id.as_str(), source))
        .collect::<BTreeMap<_, _>>();
    let mut issues = normalized
        .grounding_ledger
        .blocks
        .iter()
        .filter_map(|block| grounding_block_issue(block, &normalized, &sources))
        .collect::<Vec<_>>();
    if required && normalized.grounding_ledger.blocks.is_empty() {
        issues.push("文稿声明了来源，但没有逐块 grounding 账本".to_string());
    }
    let verified = !required || issues.is_empty();
    normalized.grounding_ledger.status = if required {
        if verified {
            "verified"
        } else {
            "failed"
        }
    } else {
        "unverified"
    }
    .to_string();
    normalized.grounding_ledger.content_hash = required.then(|| hash.clone());
    normalized.grounding_ledger.verified_at =
        (required && verified).then(|| Utc::now().to_rfc3339());
    normalized.grounding_ledger.verification_trace_id = verification_trace_id
        .map(str::trim)
        .filter(|value| valid_runtime_id(value))
        .map(str::to_string);
    normalized.metadata.properties.insert(
        "groundingVerified".to_string(),
        Value::Bool(required && verified),
    );
    normalized.metadata.properties.insert(
        "groundingStatus".to_string(),
        Value::String(normalized.grounding_ledger.status.clone()),
    );
    let validation = validate_document(&normalized);
    normalized.validation_receipt = validation.receipt;
    normalized.readiness = Some(validation.readiness);
    Ok(GroundingVerificationResult {
        verified,
        required,
        issues,
        document: normalized,
        content_hash: hash,
    })
}

#[tauri::command]
pub fn reverify_creation_grounding(
    document: CreationDocumentV2,
    verification_trace_id: Option<String>,
) -> Result<GroundingVerificationResult, String> {
    reverify_creation_grounding_document(document, verification_trace_id.as_deref())
}

#[tauri::command]
pub fn accept_creation_candidate(
    database: State<'_, RuntimeDatabase>,
    input: ReviewCreationCandidateInput,
) -> Result<CreationCandidateReviewReceipt, String> {
    ensure_no_embedded_binary(&input.candidate_document.canonical_markdown)?;
    let workspace_scope = database.local_workspace_scope()?;
    let run_id = input.run_id.trim().to_string();
    let stored = {
        let connection = database
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .query_row(
                "SELECT document_id, document_revision, input_hash, state, run_json,
                        candidate_document_json, version
                 FROM creation_writing_runs WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, run_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法读取待审核 WritingRun：{error}"))?
            .ok_or_else(|| "未找到 WritingRun".to_string())?
    };
    if stored.3 != "awaitingReview" {
        return Err("WritingRun 尚未进入人工审核状态".to_string());
    }
    if stored.1.max(0) as u64 != input.expected_document_revision
        || stored.2 != input.expected_input_hash
    {
        return Err("基础文稿 revision 或输入哈希已变化，候选不能接受".to_string());
    }
    let expected_candidate_revision = input
        .expected_document_revision
        .checked_add(1)
        .ok_or_else(|| "候选文稿 revision 超出范围".to_string())?;
    if input.candidate_document.id != stored.0
        || input.candidate_document.revision != expected_candidate_revision
    {
        return Err("候选文稿必须属于同一文稿并且只推进一个 revision".to_string());
    }
    if let Some(checkpoint_candidate) = stored.5.as_deref() {
        let checkpoint_candidate: Value = serde_json::from_str(checkpoint_candidate)
            .map_err(|error| format!("checkpoint 候选文稿已损坏：{error}"))?;
        let expected_hash = document_manifest_hash(&checkpoint_candidate)
            .ok_or_else(|| "checkpoint 候选文稿缺少正文哈希".to_string())?;
        let received_hash = content_hash(&input.candidate_document.canonical_markdown);
        if expected_hash != received_hash {
            return Err("待接受候选与最近一次耐久 checkpoint 不一致".to_string());
        }
    }
    let grounding = reverify_creation_grounding_document(
        input.candidate_document,
        input.verification_trace_id.as_deref(),
    )?;
    if grounding.required && !grounding.verified {
        return Err(format!(
            "候选逐块 grounding 重新核验失败：{}",
            grounding.issues.join("；")
        ));
    }
    let validation = validate_document(&grounding.document);
    if !validation.valid {
        return Err(format!(
            "候选文稿原生校验失败：{}",
            validation
                .issues
                .iter()
                .filter(|issue| issue.severity == "error")
                .map(|issue| issue.message.as_str())
                .collect::<Vec<_>>()
                .join("；")
        ));
    }
    let output_hash = grounding.content_hash.clone();
    let candidate_manifest_json = json_text(
        &compact_document_manifest(&grounding.document),
        "已核验候选 manifest",
    )?;
    let mut run: Value = serde_json::from_str(&stored.4)
        .map_err(|error| format!("WritingRun 数据已损坏：{error}"))?;
    let now = Utc::now().to_rfc3339();
    set_value_string(&mut run, "state", Some("succeeded"));
    set_value_string(&mut run, "outputHash", Some(&output_hash));
    set_value_string(&mut run, "completedAt", Some(&now));
    set_value_string(&mut run, "failureReason", None);
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始候选接受事务：{error}"))?;
    let changed = transaction
        .execute(
            "UPDATE creation_writing_runs
             SET state='succeeded', output_hash=?4, run_json=?5,
                 candidate_document_json=?6, version=version+1,
                 updated_at=?7, completed_at=?7
             WHERE workspace_scope=?1 AND id=?2 AND state='awaitingReview' AND version=?3",
            params![
                workspace_scope,
                run_id,
                stored.6,
                output_hash,
                json_text(&run, "WritingRun")?,
                candidate_manifest_json,
                now,
            ],
        )
        .map_err(|error| format!("无法接受 WritingRun 候选：{error}"))?;
    if changed != 1 {
        return Err("WritingRun 在审核期间已变化，请重新加载后再接受".to_string());
    }
    let checkpoint = json!({
        "schemaVersion": "1.0",
        "kind": "creationAcceptedCheckpoint",
        "checkpointId": format!("accepted-{}", Uuid::new_v4()),
        "writingRun": run,
        "documentId": grounding.document.id,
        "documentRevision": grounding.document.revision,
        "inputHash": input.expected_input_hash,
        "outputHash": output_hash,
        "groundingStatus": grounding.document.grounding_ledger.status,
        "checkpointedAt": now,
    });
    transaction
        .execute(
            "INSERT INTO creation_run_checkpoints
             (workspace_scope, run_id, checkpoint_id, sequence, document_revision,
              input_hash, candidate_hash, checkpoint_json, candidate_document_json, created_at)
             VALUES (?1, ?2, ?3, -1, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                workspace_scope,
                run_id,
                checkpoint["checkpointId"].as_str().unwrap_or("accepted"),
                sqlite_integer(input.expected_document_revision, "文稿 revision")?,
                input.expected_input_hash,
                grounding.content_hash,
                json_text(&checkpoint, "候选接受 checkpoint")?,
                candidate_manifest_json,
                now,
            ],
        )
        .map_err(|error| format!("无法保存候选接受 checkpoint：{error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交候选接受事务：{error}"))?;

    // 记录创作完成事件
    let _ = crate::metrics::record_activity_event(
        database.inner(),
        "creation",
        None,
        None,
        Some(&run_id),
    );

    let run = load_run_mutation_receipt_from_connection(&connection, &workspace_scope, &run_id)?;
    Ok(CreationCandidateReviewReceipt { run, grounding })
}

#[tauri::command]
pub fn cancel_creation_run(
    database: State<'_, RuntimeDatabase>,
    run_id: String,
    reason: Option<String>,
) -> Result<CreationRunMutationReceipt, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let run_id = run_id.trim();
    let reason = reason
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("用户取消")
        .chars()
        .take(4_000)
        .collect::<String>();
    ensure_no_embedded_binary(&reason)?;
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始取消 WritingRun 事务：{error}"))?;
    let stored = transaction
        .query_row(
            "SELECT state, run_json FROM creation_writing_runs
             WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, run_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|error| format!("无法读取待取消 WritingRun：{error}"))?
        .ok_or_else(|| "未找到 WritingRun".to_string())?;
    if !ACTIVE_RUN_STATES.contains(&stored.0.as_str()) {
        transaction
            .commit()
            .map_err(|error| format!("无法提交 WritingRun 状态读取：{error}"))?;
        return load_run_mutation_receipt_from_connection(&connection, &workspace_scope, run_id);
    }
    let now = Utc::now().to_rfc3339();
    let mut run: Value = serde_json::from_str(&stored.1)
        .map_err(|error| format!("WritingRun 数据已损坏：{error}"))?;
    set_value_string(&mut run, "state", Some("cancelled"));
    set_value_string(&mut run, "completedAt", Some(&now));
    set_value_string(&mut run, "failureReason", Some(&reason));
    transaction
        .execute(
            "UPDATE creation_writing_runs
             SET state='cancelled', run_json=?3, version=version+1,
                 updated_at=?4, completed_at=?4
             WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, run_id, json_text(&run, "WritingRun")?, now],
        )
        .map_err(|error| format!("无法取消 WritingRun：{error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交取消 WritingRun 事务：{error}"))?;
    load_run_mutation_receipt_from_connection(&connection, &workspace_scope, run_id)
}

fn canonical_json(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonical_json).collect()),
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            let mut canonical = Map::new();
            for key in keys {
                canonical.insert(key.clone(), canonical_json(&object[key]));
            }
            Value::Object(canonical)
        }
        _ => value.clone(),
    }
}

fn brand_profile_id(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && value.chars().count() <= 80
        && value.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
}

fn profile_object<'a>(profile: &'a Value, key: &str) -> Result<&'a Map<String, Value>, String> {
    profile
        .get(key)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("BrandProfile 缺少对象 `{key}`"))
}

fn profile_string<'a>(profile: &'a Value, key: &str, maximum: usize) -> Result<&'a str, String> {
    let value = profile
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("BrandProfile 缺少字符串 `{key}`"))?;
    if value.chars().count() > maximum {
        return Err(format!("BrandProfile `{key}` 过长"));
    }
    Ok(value)
}

fn validate_unique_string_array(
    object: &Map<String, Value>,
    key: &str,
    minimum: usize,
    maximum: usize,
    maximum_chars: usize,
) -> Result<(), String> {
    let values = object
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("BrandProfile `{key}` 必须是数组"))?;
    if values.len() < minimum || values.len() > maximum {
        return Err(format!("BrandProfile `{key}` 数量无效"));
    }
    let mut seen = BTreeSet::new();
    for value in values {
        let value = value
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty() && value.chars().count() <= maximum_chars)
            .ok_or_else(|| format!("BrandProfile `{key}` 包含无效字符串"))?;
        if !seen.insert(value.to_string()) {
            return Err(format!("BrandProfile `{key}` 包含重复值"));
        }
    }
    Ok(())
}

fn validate_term_rules(
    object: &Map<String, Value>,
    key: &str,
    maximum: usize,
) -> Result<(), String> {
    let values = object
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("BrandProfile `{key}` 必须是数组"))?;
    if values.len() > maximum {
        return Err(format!("BrandProfile `{key}` 数量无效"));
    }
    for rule in values {
        let rule = rule
            .as_object()
            .ok_or_else(|| format!("BrandProfile `{key}` 规则必须是对象"))?;
        let term = rule
            .get("term")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|term| !term.is_empty() && term.chars().count() <= 120)
            .ok_or_else(|| format!("BrandProfile `{key}` 规则缺少有效 term"))?;
        if rule
            .get("replacement")
            .and_then(Value::as_str)
            .is_some_and(|value| value.chars().count() > 120)
            || rule
                .get("note")
                .and_then(Value::as_str)
                .is_some_and(|value| value.chars().count() > 500)
        {
            return Err(format!("BrandProfile `{key}` 规则过长：{term}"));
        }
    }
    Ok(())
}

fn optional_profile_string(
    object: &Map<String, Value>,
    key: &str,
    maximum: usize,
) -> Result<(), String> {
    if object.get(key).is_none_or(Value::is_null) {
        return Ok(());
    }
    if object
        .get(key)
        .and_then(Value::as_str)
        .is_none_or(|value| value.chars().count() > maximum)
    {
        return Err(format!(
            "BrandProfile `{key}` 必须是最多 {maximum} 字符的字符串或 null"
        ));
    }
    Ok(())
}

fn profile_enum(object: &Map<String, Value>, key: &str, allowed: &[&str]) -> Result<(), String> {
    if !object
        .get(key)
        .and_then(Value::as_str)
        .is_some_and(|value| allowed.contains(&value))
    {
        return Err(format!("BrandProfile `{key}` 枚举值无效"));
    }
    Ok(())
}

fn ensure_profile_keys(
    object: &Map<String, Value>,
    required: &[&str],
    optional: &[&str],
    label: &str,
) -> Result<(), String> {
    if required.iter().any(|key| !object.contains_key(*key)) {
        return Err(format!("BrandProfile `{label}` 缺少必需字段"));
    }
    if let Some(key) = object
        .keys()
        .find(|key| !required.contains(&key.as_str()) && !optional.contains(&key.as_str()))
    {
        return Err(format!("BrandProfile `{label}` 包含未知字段 `{key}`"));
    }
    Ok(())
}

fn normalize_and_validate_brand_profile(mut profile: Value) -> Result<Value, String> {
    if !profile.is_object() || profile.get("schemaVersion").and_then(Value::as_str) != Some("1.0") {
        return Err("BrandProfile 必须是 schemaVersion 1.0 的 JSON 对象".to_string());
    }
    ensure_no_embedded_binary_value(&profile, "brandProfile")?;
    let root = profile
        .as_object()
        .ok_or_else(|| "BrandProfile 必须是 JSON 对象".to_string())?;
    ensure_profile_keys(
        root,
        &[
            "schemaVersion",
            "id",
            "revision",
            "name",
            "status",
            "voice",
            "vocabulary",
            "style",
            "claimsPolicy",
            "purposeDefaults",
            "signature",
            "examples",
            "provenance",
            "createdAt",
            "updatedAt",
        ],
        &["description"],
        "root",
    )?;
    let id = profile_string(&profile, "id", 80)?;
    if !brand_profile_id(id) {
        return Err("BrandProfile ID 必须以小写字母开头且只包含小写字母、数字和连字符".to_string());
    }
    profile_string(&profile, "name", 120)?;
    optional_profile_string(root, "description", 1000)?;
    let revision = profile
        .get("revision")
        .and_then(Value::as_u64)
        .filter(|revision| *revision > 0)
        .ok_or_else(|| "BrandProfile revision 无效".to_string())?;
    let status = profile_string(&profile, "status", 16)?;
    if !matches!(status, "draft" | "active" | "archived") {
        return Err("BrandProfile status 无效".to_string());
    }
    let voice = profile_object(&profile, "voice")?;
    ensure_profile_keys(
        voice,
        &["presetId", "traits", "prohibitedTraits"],
        &[],
        "voice",
    )?;
    optional_profile_string(voice, "presetId", 80)?;
    if voice
        .get("presetId")
        .and_then(Value::as_str)
        .is_some_and(|value| !brand_profile_id(value))
    {
        return Err("BrandProfile voice.presetId 无效".to_string());
    }
    validate_unique_string_array(voice, "traits", 1, 20, 80)?;
    validate_unique_string_array(voice, "prohibitedTraits", 0, 20, 80)?;
    let vocabulary = profile_object(&profile, "vocabulary")?;
    ensure_profile_keys(
        vocabulary,
        &["preferred", "avoided", "requiredTerms"],
        &[],
        "vocabulary",
    )?;
    validate_term_rules(vocabulary, "preferred", 500)?;
    validate_term_rules(vocabulary, "avoided", 500)?;
    validate_unique_string_array(vocabulary, "requiredTerms", 0, 200, 120)?;
    let style = profile_object(&profile, "style")?;
    ensure_profile_keys(
        style,
        &[
            "formality",
            "perspective",
            "sentenceLength",
            "emoji",
            "punctuation",
            "callToAction",
        ],
        &[],
        "style",
    )?;
    profile_enum(style, "formality", &["casual", "balanced", "formal"])?;
    profile_enum(
        style,
        "perspective",
        &["firstPerson", "secondPerson", "thirdPerson", "mixed"],
    )?;
    profile_enum(style, "sentenceLength", &["short", "varied", "long"])?;
    profile_enum(style, "emoji", &["none", "restrained", "allowed"])?;
    profile_enum(
        style,
        "punctuation",
        &["standardCjk", "expressive", "minimal"],
    )?;
    profile_enum(style, "callToAction", &["none", "soft", "direct"])?;
    let claims = profile_object(&profile, "claimsPolicy")?;
    ensure_profile_keys(
        claims,
        &[
            "requireSources",
            "labelInference",
            "forbidFabrication",
            "sensitiveTopics",
        ],
        &[],
        "claimsPolicy",
    )?;
    for key in ["requireSources", "labelInference", "forbidFabrication"] {
        if !claims.get(key).is_some_and(Value::is_boolean) {
            return Err(format!("BrandProfile claimsPolicy 缺少布尔值 `{key}`"));
        }
    }
    if claims.get("forbidFabrication").and_then(Value::as_bool) != Some(true) {
        return Err("BrandProfile 必须禁止编造事实".to_string());
    }
    validate_unique_string_array(claims, "sensitiveTopics", 0, 100, 120)?;
    let purpose_defaults = profile_object(&profile, "purposeDefaults")?;
    if purpose_defaults.len() > 20 {
        return Err("BrandProfile purposeDefaults 数量无效".to_string());
    }
    for (purpose, value) in purpose_defaults {
        if purpose.trim().is_empty() || purpose.chars().count() > 80 {
            return Err("BrandProfile purposeDefaults key 无效".to_string());
        }
        let value = value
            .as_object()
            .ok_or_else(|| "BrandProfile purposeDefaults 项必须是对象".to_string())?;
        ensure_profile_keys(value, &["presetId"], &["notes"], "purposeDefaults item")?;
        let preset_id = value
            .get("presetId")
            .and_then(Value::as_str)
            .filter(|value| brand_profile_id(value))
            .ok_or_else(|| "BrandProfile purposeDefaults.presetId 无效".to_string())?;
        if preset_id.is_empty() {
            return Err("BrandProfile purposeDefaults.presetId 无效".to_string());
        }
        optional_profile_string(value, "notes", 500)?;
    }
    let signature = profile_object(&profile, "signature")?;
    ensure_profile_keys(
        signature,
        &["enabled", "author", "byline", "footer"],
        &[],
        "signature",
    )?;
    if !signature.get("enabled").is_some_and(Value::is_boolean) {
        return Err("BrandProfile signature.enabled 无效".to_string());
    }
    optional_profile_string(signature, "author", 120)?;
    optional_profile_string(signature, "byline", 500)?;
    optional_profile_string(signature, "footer", 2000)?;
    let examples = profile
        .get("examples")
        .and_then(Value::as_array)
        .ok_or_else(|| "BrandProfile examples 必须是数组".to_string())?;
    if examples.len() > 50 {
        return Err("BrandProfile examples 数量无效".to_string());
    }
    for example in examples {
        let example = example
            .as_object()
            .ok_or_else(|| "BrandProfile example 必须是对象".to_string())?;
        ensure_profile_keys(example, &["label", "content", "approved"], &[], "example")?;
        for (key, maximum) in [("label", 120), ("content", 20_000)] {
            let value = example
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty() && value.chars().count() <= maximum)
                .ok_or_else(|| format!("BrandProfile example.{key} 无效"))?;
            if value.is_empty() {
                return Err(format!("BrandProfile example.{key} 无效"));
            }
        }
        if !example.get("approved").is_some_and(Value::is_boolean) {
            return Err("BrandProfile example.approved 无效".to_string());
        }
    }
    let provenance = profile_object(&profile, "provenance")?;
    ensure_profile_keys(
        provenance,
        &["createdBy", "source", "userApproved"],
        &["sourceRef"],
        "provenance",
    )?;
    profile_enum(provenance, "createdBy", &["user", "assistant", "import"])?;
    profile_enum(
        provenance,
        "source",
        &["manual", "conversation", "file", "derivedFromExamples"],
    )?;
    optional_profile_string(provenance, "sourceRef", 2048)?;
    if !provenance
        .get("userApproved")
        .is_some_and(Value::is_boolean)
    {
        return Err("BrandProfile provenance.userApproved 无效".to_string());
    }
    for key in ["createdAt", "updatedAt"] {
        let timestamp = profile_string(&profile, key, 64)?;
        if chrono::DateTime::parse_from_rfc3339(timestamp).is_err() {
            return Err(format!("BrandProfile `{key}` 不是 RFC3339 时间"));
        }
    }
    set_value_u64(&mut profile, "revision", revision);
    Ok(canonical_json(&profile))
}

fn brand_profile_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BrandProfileRecord> {
    let profile_json: String = row.get(0)?;
    let profile = serde_json::from_str(&profile_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(BrandProfileRecord {
        profile,
        content_hash: row.get(1)?,
        revision: row.get::<_, i64>(2)?.max(1) as u64,
        status: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

fn load_brand_profile(
    connection: &Connection,
    workspace_scope: &str,
    profile_id: &str,
) -> Result<BrandProfileRecord, String> {
    connection
        .query_row(
            "SELECT profile_json, content_hash, revision, status, created_at, updated_at
             FROM creation_brand_profiles WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, profile_id],
            brand_profile_record_from_row,
        )
        .optional()
        .map_err(|error| format!("无法读取 BrandProfile：{error}"))?
        .ok_or_else(|| "未找到 BrandProfile".to_string())
}

fn prepare_brand_profile_revision(
    mut profile: Value,
    revision: u64,
    status: &str,
    created_at: &str,
    updated_at: &str,
    approved: bool,
) -> Result<(Value, String), String> {
    set_value_u64(&mut profile, "revision", revision);
    set_value_string(&mut profile, "status", Some(status));
    set_value_string(&mut profile, "createdAt", Some(created_at));
    set_value_string(&mut profile, "updatedAt", Some(updated_at));
    profile
        .pointer_mut("/provenance/userApproved")
        .ok_or_else(|| "BrandProfile 缺少 provenance.userApproved".to_string())?
        .clone_from(&Value::Bool(approved));
    let profile = normalize_and_validate_brand_profile(profile)?;
    let json = json_text(&profile, "BrandProfile")?;
    Ok((profile, content_hash(&json)))
}

#[tauri::command]
pub fn upsert_creation_brand_profile(
    database: State<'_, RuntimeDatabase>,
    input: BrandProfileUpsertInput,
) -> Result<BrandProfileRecord, String> {
    let profile = normalize_and_validate_brand_profile(input.profile)?;
    let profile_id = profile_string(&profile, "id", 80)?.to_string();
    let workspace_scope = database.local_workspace_scope()?;
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始 BrandProfile 事务：{error}"))?;
    let existing = transaction
        .query_row(
            "SELECT revision, created_at FROM creation_brand_profiles
             WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, profile_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|error| format!("无法检查 BrandProfile revision：{error}"))?;
    let now = Utc::now().to_rfc3339();
    let (revision, created_at) = match existing {
        Some((revision, created_at)) => {
            let expected = input
                .expected_revision
                .ok_or_else(|| "更新 BrandProfile 必须提供 expectedRevision".to_string())?;
            if revision.max(1) as u64 != expected {
                return Err("BrandProfile revision 已变化，请重新加载".to_string());
            }
            (
                expected
                    .checked_add(1)
                    .ok_or_else(|| "BrandProfile revision 超出范围".to_string())?,
                created_at,
            )
        }
        None => {
            if input.expected_revision.is_some() {
                return Err("新建 BrandProfile 不能提供 expectedRevision".to_string());
            }
            (1, now.clone())
        }
    };
    let (profile, hash) =
        prepare_brand_profile_revision(profile, revision, "draft", &created_at, &now, false)?;
    let profile_json = json_text(&profile, "BrandProfile")?;
    transaction
        .execute(
            "INSERT INTO creation_brand_profiles
             (workspace_scope, id, revision, status, profile_json, content_hash, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'draft', ?4, ?5, ?6, ?7)
             ON CONFLICT(workspace_scope, id) DO UPDATE SET
               revision=excluded.revision, status='draft', profile_json=excluded.profile_json,
               content_hash=excluded.content_hash, updated_at=excluded.updated_at",
            params![workspace_scope, profile_id, revision as i64, profile_json, hash, created_at, now],
        )
        .map_err(|error| format!("无法保存 BrandProfile：{error}"))?;
    transaction
        .execute(
            "INSERT INTO creation_brand_profile_revisions
             (workspace_scope, profile_id, revision, status, profile_json, content_hash, created_at)
             VALUES (?1, ?2, ?3, 'draft', ?4, ?5, ?6)",
            params![
                workspace_scope,
                profile_id,
                revision as i64,
                profile_json,
                hash,
                now
            ],
        )
        .map_err(|error| format!("无法保存 BrandProfile revision：{error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交 BrandProfile：{error}"))?;
    load_brand_profile(&connection, &workspace_scope, &profile_id)
}

#[tauri::command]
pub fn get_creation_brand_profile(
    database: State<'_, RuntimeDatabase>,
    profile_id: String,
) -> Result<BrandProfileRecord, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    load_brand_profile(&connection, &workspace_scope, profile_id.trim())
}

#[tauri::command]
pub fn list_creation_brand_profiles(
    database: State<'_, RuntimeDatabase>,
    include_archived: Option<bool>,
) -> Result<Vec<BrandProfileRecord>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let sql = if include_archived.unwrap_or(false) {
        "SELECT profile_json, content_hash, revision, status, created_at, updated_at
         FROM creation_brand_profiles WHERE workspace_scope=?1 ORDER BY updated_at DESC"
    } else {
        "SELECT profile_json, content_hash, revision, status, created_at, updated_at
         FROM creation_brand_profiles WHERE workspace_scope=?1 AND status!='archived'
         ORDER BY updated_at DESC"
    };
    let mut statement = connection
        .prepare(sql)
        .map_err(|error| format!("无法准备 BrandProfile 列表：{error}"))?;
    let profiles = statement
        .query_map([workspace_scope], brand_profile_record_from_row)
        .map_err(|error| format!("无法读取 BrandProfile 列表：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法解析 BrandProfile 列表：{error}"))?;
    Ok(profiles)
}

fn transition_brand_profile(
    database: State<'_, RuntimeDatabase>,
    profile_id: String,
    expected_revision: u64,
    status: &str,
) -> Result<BrandProfileRecord, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let profile_id = profile_id.trim();
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始 BrandProfile 状态事务：{error}"))?;
    let (stored_json, revision, created_at) = transaction
        .query_row(
            "SELECT profile_json, revision, created_at FROM creation_brand_profiles
             WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, profile_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("无法读取 BrandProfile：{error}"))?
        .ok_or_else(|| "未找到 BrandProfile".to_string())?;
    if revision.max(1) as u64 != expected_revision {
        return Err("BrandProfile revision 已变化，请重新加载".to_string());
    }
    let profile: Value = serde_json::from_str(&stored_json)
        .map_err(|error| format!("BrandProfile 数据已损坏：{error}"))?;
    let now = Utc::now().to_rfc3339();
    let next_revision = expected_revision
        .checked_add(1)
        .ok_or_else(|| "BrandProfile revision 超出范围".to_string())?;
    let (profile, hash) = prepare_brand_profile_revision(
        profile,
        next_revision,
        status,
        &created_at,
        &now,
        status == "active",
    )?;
    let profile_json = json_text(&profile, "BrandProfile")?;
    transaction
        .execute(
            "UPDATE creation_brand_profiles
             SET revision=?3, status=?4, profile_json=?5, content_hash=?6, updated_at=?7
             WHERE workspace_scope=?1 AND id=?2 AND revision=?8",
            params![
                workspace_scope,
                profile_id,
                next_revision as i64,
                status,
                profile_json,
                hash,
                now,
                expected_revision as i64
            ],
        )
        .map_err(|error| format!("无法更新 BrandProfile 状态：{error}"))?;
    transaction
        .execute(
            "INSERT INTO creation_brand_profile_revisions
             (workspace_scope, profile_id, revision, status, profile_json, content_hash, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                workspace_scope,
                profile_id,
                next_revision as i64,
                status,
                profile_json,
                hash,
                now
            ],
        )
        .map_err(|error| format!("无法保存 BrandProfile 状态 revision：{error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交 BrandProfile 状态：{error}"))?;
    load_brand_profile(&connection, &workspace_scope, profile_id)
}

#[tauri::command]
pub fn approve_creation_brand_profile(
    database: State<'_, RuntimeDatabase>,
    profile_id: String,
    expected_revision: u64,
) -> Result<BrandProfileRecord, String> {
    transition_brand_profile(database, profile_id, expected_revision, "active")
}

#[tauri::command]
pub fn archive_creation_brand_profile(
    database: State<'_, RuntimeDatabase>,
    profile_id: String,
    expected_revision: u64,
) -> Result<BrandProfileRecord, String> {
    transition_brand_profile(database, profile_id, expected_revision, "archived")
}

/// Versioned profiles are never physically deleted; the delete operation is an
/// auditable archive transition so existing bindings and evaluation history remain
/// explainable.
#[tauri::command]
pub fn delete_creation_brand_profile(
    database: State<'_, RuntimeDatabase>,
    profile_id: String,
    expected_revision: u64,
) -> Result<BrandProfileRecord, String> {
    archive_creation_brand_profile(database, profile_id, expected_revision)
}

#[tauri::command]
pub fn bind_creation_brand_profile(
    database: State<'_, RuntimeDatabase>,
    input: BindCreationBrandProfileInput,
) -> Result<BrandProfileBindingReceipt, String> {
    if input.document.revision != input.expected_document_revision {
        return Err("文稿 revision 已变化，不能绑定 BrandProfile".to_string());
    }
    ensure_no_embedded_binary(&input.document.canonical_markdown)?;
    let workspace_scope = database.local_workspace_scope()?;
    let now = Utc::now().to_rfc3339();
    let mut document = normalize_document(input.document)?.document;
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始 BrandProfile 绑定事务：{error}"))?;
    let profile = input
        .profile_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|profile_id| {
            let profile = load_brand_profile(&transaction, &workspace_scope, profile_id)?;
            if profile.status != "active" {
                return Err("只有已批准的 active BrandProfile 可以绑定文稿".to_string());
            }
            Ok(profile)
        })
        .transpose()?;
    document.metadata.brand_profile_id = profile.as_ref().map(|profile| {
        profile
            .profile
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    });
    if let Some(profile) = &profile {
        transaction
            .execute(
                "INSERT INTO creation_document_brand_bindings
                 (workspace_scope, document_id, document_revision, profile_id, profile_revision, bound_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(workspace_scope, document_id) DO UPDATE SET
                   document_revision=excluded.document_revision, profile_id=excluded.profile_id,
                   profile_revision=excluded.profile_revision, bound_at=excluded.bound_at",
                params![
                    workspace_scope,
                    document.id,
                    sqlite_integer(document.revision, "文稿 revision")?,
                    document.metadata.brand_profile_id,
                    sqlite_integer(profile.revision, "BrandProfile revision")?,
                    now
                ],
            )
            .map_err(|error| format!("无法保存 BrandProfile 绑定：{error}"))?;
    } else {
        transaction
            .execute(
                "DELETE FROM creation_document_brand_bindings
                 WHERE workspace_scope=?1 AND document_id=?2",
                params![workspace_scope, document.id],
            )
            .map_err(|error| format!("无法解除 BrandProfile 绑定：{error}"))?;
    }
    transaction
        .commit()
        .map_err(|error| format!("无法提交 BrandProfile 绑定：{error}"))?;
    Ok(BrandProfileBindingReceipt {
        document,
        profile,
        bound_at: now,
    })
}

fn profile_terms<'a>(profile: &'a Value, list: &str) -> Vec<&'a str> {
    profile
        .pointer(&format!("/vocabulary/{list}"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|rule| rule.get("term").and_then(Value::as_str))
        .collect()
}

fn required_profile_terms(profile: &Value) -> Vec<&str> {
    profile
        .pointer("/vocabulary/requiredTerms")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn evaluate_brand_document(
    profile: &BrandProfileRecord,
    document: &CreationDocumentV2,
) -> BrandEvaluationResult {
    let mut checks = Vec::new();
    let content = document.canonical_markdown.to_lowercase();
    let avoided = profile_terms(&profile.profile, "avoided")
        .into_iter()
        .filter(|term| content.contains(&term.to_lowercase()))
        .collect::<Vec<_>>();
    checks.push(json!({
        "id": "brand.vocabulary.avoided",
        "status": if avoided.is_empty() { "pass" } else { "fail" },
        "deterministic": true,
        "detail": if avoided.is_empty() { "未发现禁用词".to_string() } else { format!("发现禁用词：{}", avoided.join("、")) },
    }));
    let missing = required_profile_terms(&profile.profile)
        .into_iter()
        .filter(|term| !content.contains(&term.to_lowercase()))
        .collect::<Vec<_>>();
    checks.push(json!({
        "id": "brand.vocabulary.required",
        "status": if missing.is_empty() { "pass" } else { "fail" },
        "deterministic": true,
        "detail": if missing.is_empty() { "必需术语已覆盖".to_string() } else { format!("缺少必需术语：{}", missing.join("、")) },
    }));
    let requires_sources = profile
        .profile
        .pointer("/claimsPolicy/requireSources")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let grounding_ok = !requires_sources || document.grounding_ledger.status == "verified";
    checks.push(json!({
        "id": "brand.claims.sources",
        "status": if grounding_ok { "pass" } else { "fail" },
        "deterministic": true,
        "detail": if grounding_ok { "来源策略已满足" } else { "品牌策略要求来源，但文稿 grounding 尚未 verified" },
    }));
    let signature_enabled = profile
        .profile
        .pointer("/signature/enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let expected_author = profile
        .profile
        .pointer("/signature/author")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let signature_ok = !signature_enabled
        || expected_author.is_none_or(|author| {
            document
                .metadata
                .author
                .as_deref()
                .is_some_and(|value| value.trim() == author)
        });
    checks.push(json!({
        "id": "brand.signature.author",
        "status": if signature_ok { "pass" } else { "fail" },
        "deterministic": true,
        "detail": if signature_ok { "署名策略已满足" } else { "文稿作者与 BrandProfile 署名不一致" },
    }));
    let failures = checks
        .iter()
        .filter(|check| check.get("status").and_then(Value::as_str) == Some("fail"))
        .count();
    let score = (((checks.len() - failures) * 100) / checks.len().max(1)) as u8;
    BrandEvaluationResult {
        evaluation_id: format!("brand-eval-{}", Uuid::new_v4()),
        profile_id: profile
            .profile
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        profile_revision: profile.revision,
        document_id: document.id.clone(),
        document_revision: document.revision,
        passed: failures == 0,
        score,
        checks,
        evaluated_at: Utc::now().to_rfc3339(),
    }
}

#[tauri::command]
pub fn evaluate_creation_brand_profile(
    database: State<'_, RuntimeDatabase>,
    input: EvaluateCreationBrandProfileInput,
) -> Result<BrandEvaluationResult, String> {
    ensure_no_embedded_binary(&input.document.canonical_markdown)?;
    let workspace_scope = database.local_workspace_scope()?;
    let profile_id = input
        .profile_id
        .as_deref()
        .or(input.document.metadata.brand_profile_id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "文稿没有绑定 BrandProfile".to_string())?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let profile = load_brand_profile(&connection, &workspace_scope, profile_id)?;
    if profile.status != "active" {
        return Err("只有已批准的 active BrandProfile 可以评测文稿".to_string());
    }
    let document = normalize_document(input.document)?.document;
    let result = evaluate_brand_document(&profile, &document);
    let result_json = serde_json::to_string(&result)
        .map_err(|error| format!("无法序列化 BrandProfile 评测：{error}"))?;
    connection
        .execute(
            "INSERT INTO creation_brand_evaluations
             (workspace_scope, id, profile_id, profile_revision, document_id,
              document_revision, result_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                workspace_scope,
                result.evaluation_id,
                result.profile_id,
                sqlite_integer(result.profile_revision, "BrandProfile revision")?,
                result.document_id,
                sqlite_integer(result.document_revision, "文稿 revision")?,
                result_json,
                result.evaluated_at
            ],
        )
        .map_err(|error| format!("无法保存 BrandProfile 评测：{error}"))?;
    Ok(result)
}
