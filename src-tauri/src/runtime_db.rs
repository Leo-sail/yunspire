use crate::obsidian::{
    collect_files_for_runtime_with_cancellation, read_file_limited_for_runtime,
    resolve_vault_for_runtime, OperationEvent, VaultDescriptor,
};
use crate::policy::{ApplicationCommand, PolicyDecision, PolicyOutcome};
use crate::task_runtime::NativeRuntimeTask;
use chrono::Utc;
use regex::Regex;
use rusqlite::{
    params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{ErrorKind, Write},
    path::Path,
    path::PathBuf,
    sync::Mutex,
};
use tauri::{AppHandle, Manager, State};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

const MAX_SNAPSHOT_RECORDS: usize = 10_000;
const MAX_RECORD_BYTES: usize = 2 * 1024 * 1024;
const MAX_INDEXED_NOTE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SEARCH_QUERY_CHARS: usize = 512;
const MAX_INBOUND_RECORD_BYTES: usize = 512 * 1024;
const DEFAULT_LOCAL_WORKSPACE_SCOPE: &str = "local";
const CURRENT_SCHEMA_VERSION: i64 = 28;
const APPLICATION_AUTHORIZATION_VERSION: i64 = 1;
const VAULT_INDEX_DEBOUNCE_MS: i64 = 300;
const VAULT_INDEX_MAX_ATTEMPTS: i64 = 5;
const VAULT_INDEX_RETRY_BASE_MS: i64 = 1_000;
pub(crate) const VAULT_INDEX_BATCH_SIZE: usize = 32;
const LOCAL_FEATURE_VECTOR_VERSION: i64 = 1;
const LOCAL_FEATURE_VECTOR_DIMENSIONS: usize = 384;
const MAX_LOCAL_VECTOR_CONTENT_CHARS: usize = 250_000;
const MIN_LOCAL_VECTOR_SIMILARITY: f64 = 0.025;
const RRF_K: f64 = 60.0;

pub struct RuntimeDatabase {
    pub(crate) connection: Mutex<Connection>,
    path: PathBuf,
}

pub(crate) struct ModelUsageRecord<'a> {
    pub(crate) request_id: &'a str,
    pub(crate) trace_id: &'a str,
    pub(crate) operation: &'a str,
    pub(crate) provider: &'a str,
    pub(crate) model: &'a str,
    pub(crate) state: &'a str,
    pub(crate) prompt_tokens: u64,
    pub(crate) completion_tokens: u64,
    pub(crate) total_tokens: u64,
    pub(crate) estimated_cost_usd: Option<f64>,
    pub(crate) cost_source: &'a str,
    pub(crate) duration_ms: u64,
    pub(crate) error: Option<&'a str>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSnapshot {
    #[serde(default)]
    tasks: Vec<Value>,
    #[serde(default)]
    messages: Vec<Value>,
    #[serde(default)]
    approvals: Vec<Value>,
    #[serde(default)]
    operation_logs: Vec<Value>,
    #[serde(default)]
    selected_task_id: String,
    #[serde(default)]
    client_state: Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseHealth {
    path: String,
    journal_mode: String,
    integrity: String,
    schema_version: i64,
    task_count: i64,
    approval_count: i64,
    message_count: i64,
    operation_event_count: i64,
    indexed_note_count: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationAuthorizationState {
    status: String,
    authorization_version: i64,
    decided_at: Option<String>,
    updated_at: Option<String>,
}

impl ApplicationAuthorizationState {
    pub(crate) fn is_granted(&self) -> bool {
        self.status == "granted" && self.authorization_version == APPLICATION_AUTHORIZATION_VERSION
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseBackupResult {
    pub(crate) path: String,
    byte_length: u64,
    created_at: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseBackupInfo {
    path: String,
    file_name: String,
    byte_length: u64,
    modified_at: String,
    schema_version: i64,
    integrity: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseRestorePreflight {
    path: String,
    file_name: String,
    byte_length: u64,
    schema_version: i64,
    integrity: String,
    compatible: bool,
    reason: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseRestoreResult {
    pub(crate) restored_from: String,
    safety_backup: String,
    schema_version: i64,
    integrity: String,
    restored_at: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexBuildResult {
    vault_id: String,
    indexed_notes: usize,
    skipped_notes: usize,
    completed_at: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ClaimedVaultIndexChange {
    pub(crate) id: i64,
    pub(crate) vault_id: String,
    pub(crate) canonical_root: PathBuf,
    pub(crate) relative_path: String,
    pub(crate) generation: i64,
    pub(crate) attempt_count: i64,
    pub(crate) trace_id: String,
}

#[derive(Clone, Debug)]
pub(crate) struct AppliedVaultIndexChange {
    pub(crate) vault_id: String,
    pub(crate) relative_path: String,
    pub(crate) change_kind: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct VaultIndexReconcileResult {
    pub(crate) queued_upserts: usize,
    pub(crate) queued_deletes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VaultIndexFailureOutcome {
    pub(crate) updated: bool,
    pub(crate) terminal: bool,
}

struct PreparedNoteIndex {
    relative_path: String,
    title: String,
    content_hash: String,
    modified_at: String,
    byte_length: u64,
    tags_json: String,
    wiki_links_json: String,
    content: String,
    feature_vector: Vec<u8>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedSearchResult {
    vault_id: String,
    relative_path: String,
    title: String,
    excerpt: String,
    modified_at: String,
    score: f64,
    tags: Vec<String>,
    wiki_links: Vec<String>,
    source_kind: String,
    ranking_signals: IndexedSearchSignals,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexedSearchSignals {
    lexical_rank: Option<usize>,
    vector_rank: Option<usize>,
    lexical_rrf: f64,
    vector_rrf: f64,
    vector_similarity: Option<f64>,
    title_path_bonus: f64,
    relation_bonus: f64,
    recency_bonus: f64,
    vector_kind: &'static str,
}

#[derive(Clone)]
struct IndexedSearchCandidate {
    vault_id: String,
    relative_path: String,
    title: String,
    excerpt: String,
    modified_at: String,
    lexical_score: Option<f64>,
    vector_similarity: Option<f64>,
    tags: Vec<String>,
    wiki_links: Vec<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DueRuntimeSchedule {
    pub(crate) id: String,
    pub(crate) schedule_kind: String,
    pub(crate) payload: Value,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskRecovery {
    task_id: String,
    recommendation: String,
    resume_step_id: Option<String>,
    resume_step_index: Option<i64>,
    resume_checkpoint_id: Option<String>,
    evidence: Vec<String>,
    detail: String,
    detected_at: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboundContentRecordInput {
    id: String,
    state: String,
    source_type: String,
    source_ref: String,
    title: String,
    content_hash: String,
    content_characters: usize,
    attachment_count: usize,
    image_count: usize,
    #[serde(default)]
    extraction: Value,
    #[serde(default)]
    analysis: Value,
    #[serde(default)]
    quality: Value,
    #[serde(default)]
    target: Value,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    failure_reason: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboundContentRecordReceipt {
    id: String,
    state: String,
    previous_state: Option<String>,
    duplicate_of: Option<String>,
    updated_at: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedResourceSnapshotInput {
    #[serde(default)]
    custom_skills: Vec<Value>,
    #[serde(default)]
    schedules: Vec<Value>,
    #[serde(default)]
    report_subscriptions: Vec<Value>,
    #[serde(default)]
    reports: Vec<Value>,
    #[serde(default)]
    assistant_profile: Value,
    #[serde(default)]
    optimization_profile: Value,
    #[serde(default)]
    optimization_draft: Value,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedResourceSnapshot {
    initialized: bool,
    custom_skills: Vec<Value>,
    schedules: Vec<Value>,
    report_subscriptions: Vec<Value>,
    reports: Vec<Value>,
    assistant_profile: Value,
    optimization_profile: Value,
    optimization_draft: Value,
}

pub struct LegacyModelProfile {
    pub role: String,
    pub provider: String,
    pub base_url: String,
    pub selected_model: String,
    pub available_models: Value,
    pub api_key_ciphertext: Vec<u8>,
}

pub struct ModelProviderProfile {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub base_url: String,
    pub available_models: Value,
    pub assignments: Value,
    pub defaults: Value,
    pub api_key_ciphertext: Vec<u8>,
}

pub(crate) struct PendingLongTermMemoryEvent {
    pub(crate) id: String,
    pub(crate) payload: Value,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LongTermMemoryRecord {
    id: String,
    event_type: String,
    occurred_at: String,
    actor: String,
    state: String,
    governance_state: String,
    payload: Value,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LongTermMemoryGovernanceInput {
    pub id: String,
    pub action: String,
    #[serde(default)]
    pub replacement_id: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LongTermMemoryMetrics {
    total: i64,
    committed: i64,
    pending: i64,
    failed: i64,
    active: i64,
    corrected: i64,
    expired: i64,
    tombstoned: i64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizationEvidenceEvent {
    id: String,
    event_type: String,
    occurred_at: String,
    actor: String,
    content: String,
    metadata: Value,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizationEvidenceBatch {
    cursor_revision: i64,
    cursor_occurred_at: String,
    cursor_event_id: String,
    next_occurred_at: String,
    next_event_id: String,
    events: Vec<OptimizationEvidenceEvent>,
    has_more: bool,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizationCandidateInput {
    id: String,
    expected_cursor_revision: i64,
    summary: String,
    #[serde(default)]
    rules: Vec<String>,
    #[serde(default)]
    skill_hints: Value,
    #[serde(default)]
    metrics: Value,
    #[serde(default)]
    evidence_count: usize,
    #[serde(default)]
    evidence_cursor_occurred_at: String,
    #[serde(default)]
    evidence_cursor_event_id: String,
    #[serde(default)]
    expires_at: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizationCandidateResult {
    id: String,
    base_version: i64,
    candidate_version: i64,
    state: String,
    summary: String,
    rules: Vec<String>,
    skill_hints: Value,
    metrics: Value,
    evidence_count: usize,
    created_at: String,
    evaluated_at: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizationEvaluationResult {
    candidate_id: String,
    state: String,
    passed: bool,
    checks: Vec<String>,
    evaluated_at: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizationProfileResult {
    version: i64,
    candidate_id: Option<String>,
    guidance: String,
    rules: Vec<String>,
    skill_hints: Value,
    updated_at: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizationVersion {
    version: i64,
    candidate_id: Option<String>,
    state: String,
    guidance: String,
    created_at: String,
    rollback_target: Option<i64>,
}

impl RuntimeDatabase {
    pub fn open(app: &AppHandle) -> Result<Self, String> {
        #[cfg(debug_assertions)]
        let app_data = std::env::var_os("YUNSPIRE_APP_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or(
                app.path()
                    .app_data_dir()
                    .map_err(|error| format!("无法定位应用数据目录：{error}"))?,
            );
        #[cfg(not(debug_assertions))]
        let app_data = app
            .path()
            .app_data_dir()
            .map_err(|error| format!("无法定位应用数据目录：{error}"))?;
        fs::create_dir_all(&app_data).map_err(|error| format!("无法创建应用数据目录：{error}"))?;
        let path = app_data.join("yunspire.sqlite");
        let connection =
            Connection::open(&path).map_err(|error| format!("无法打开 SQLite 数据库：{error}"))?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|error| format!("无法设置 SQLite busy timeout：{error}"))?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=FULL;
                 PRAGMA foreign_keys=ON;
                 PRAGMA temp_store=MEMORY;",
            )
            .map_err(|error| format!("无法配置 SQLite：{error}"))?;
        run_migrations(&connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
            path,
        })
    }

    #[cfg(test)]
    pub(crate) fn open_test(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("无法创建临时 SQLite 目录：{error}"))?;
        }
        let connection = Connection::open(path)
            .map_err(|error| format!("无法打开临时 SQLite 数据库：{error}"))?;
        connection
            .execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")
            .map_err(|error| format!("无法配置临时 SQLite：{error}"))?;
        run_migrations(&connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
            path: path.to_path_buf(),
        })
    }

    pub fn local_workspace_scope(&self) -> Result<String, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .query_row(
                "SELECT workspace_scope FROM (
                   SELECT workspace_scope AS workspace_scope, updated_at FROM workspace_snapshots
                   UNION ALL
                   SELECT workspace_scope AS workspace_scope, updated_at FROM model_providers
                 ) ORDER BY updated_at DESC LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map(|scope| scope.unwrap_or_else(|| DEFAULT_LOCAL_WORKSPACE_SCOPE.to_string()))
            .map_err(|error| format!("无法读取本地工作区作用域：{error}"))
    }

    pub(crate) fn application_authorization(
        &self,
    ) -> Result<ApplicationAuthorizationState, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .query_row(
                "SELECT status, authorization_version, decided_at, updated_at
                 FROM application_authorization WHERE id=1",
                [],
                |row| {
                    Ok(ApplicationAuthorizationState {
                        status: row.get(0)?,
                        authorization_version: row.get(1)?,
                        decided_at: row.get(2)?,
                        updated_at: row.get(3)?,
                    })
                },
            )
            .optional()
            .map(|state| {
                state
                    .filter(|state| {
                        state.authorization_version == APPLICATION_AUTHORIZATION_VERSION
                    })
                    .unwrap_or(ApplicationAuthorizationState {
                        status: "pending".to_string(),
                        authorization_version: APPLICATION_AUTHORIZATION_VERSION,
                        decided_at: None,
                        updated_at: None,
                    })
            })
            .map_err(|error| format!("无法读取云枢统一授权状态：{error}"))
    }

    pub(crate) fn set_application_authorization(
        &self,
        granted: bool,
    ) -> Result<ApplicationAuthorizationState, String> {
        let status = if granted { "granted" } else { "denied" };
        let decided_at = Utc::now().to_rfc3339();
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .execute(
                "INSERT INTO application_authorization
                 (id, status, authorization_version, decided_at, updated_at)
                 VALUES (1, ?1, ?2, ?3, ?3)
                 ON CONFLICT(id) DO UPDATE SET
                   status=excluded.status,
                   authorization_version=excluded.authorization_version,
                   decided_at=excluded.decided_at,
                   updated_at=excluded.updated_at",
                params![status, APPLICATION_AUTHORIZATION_VERSION, decided_at],
            )
            .map_err(|error| format!("无法保存云枢统一授权状态：{error}"))?;
        Ok(ApplicationAuthorizationState {
            status: status.to_string(),
            authorization_version: APPLICATION_AUTHORIZATION_VERSION,
            decided_at: Some(decided_at.clone()),
            updated_at: Some(decided_at),
        })
    }

    pub(crate) fn record_model_usage(&self, record: &ModelUsageRecord<'_>) -> Result<(), String> {
        let workspace_scope = self.local_workspace_scope()?;
        crate::trace::validate_trace_id(record.trace_id)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始模型用量记录事务：{error}"))?;
        let existing_trace = transaction
            .query_row(
                "SELECT trace_id FROM model_usage_events WHERE request_id=?1",
                [record.request_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(|error| format!("无法校验模型请求 Trace：{error}"))?
            .flatten();
        if existing_trace
            .as_deref()
            .is_some_and(|trace_id| trace_id != record.trace_id)
        {
            return Err("同一模型请求不能重新绑定其他 Trace".to_string());
        }
        let now = Utc::now().to_rfc3339();
        transaction
            .execute(
                "INSERT INTO model_usage_events
                 (id, workspace_scope, request_id, trace_id, operation, provider, model, state,
                  prompt_tokens, completion_tokens, total_tokens, estimated_cost_usd,
                  cost_source, duration_ms, error, created_at, completed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
                 ON CONFLICT(request_id) DO UPDATE SET
                   state=excluded.state,
                   prompt_tokens=excluded.prompt_tokens,
                   completion_tokens=excluded.completion_tokens,
                   total_tokens=excluded.total_tokens,
                   estimated_cost_usd=excluded.estimated_cost_usd,
                   cost_source=excluded.cost_source,
                   duration_ms=excluded.duration_ms,
                   error=excluded.error,
                   completed_at=excluded.completed_at",
                params![
                    Uuid::new_v4().to_string(),
                    workspace_scope,
                    record.request_id,
                    record.trace_id,
                    record.operation,
                    record.provider,
                    record.model,
                    record.state,
                    record.prompt_tokens as i64,
                    record.completion_tokens as i64,
                    record.total_tokens as i64,
                    record.estimated_cost_usd,
                    record.cost_source,
                    record.duration_ms as i64,
                    record.error,
                    now,
                    if record.state == "started" {
                        None
                    } else {
                        Some(now.clone())
                    },
                ],
            )
            .map_err(|error| format!("无法记录模型 Token 与费用：{error}"))?;
        crate::trace::record_trace_event_in_connection(
            &transaction,
            &workspace_scope,
            &crate::trace::TraceEventRecord {
                trace_id: record.trace_id,
                entity_kind: "model_request",
                entity_id: record.request_id,
                event_type: "model.request.state",
                state: record.state,
                payload: &serde_json::json!({
                    "operation": record.operation,
                    "provider": record.provider,
                    "model": record.model,
                    "promptTokens": record.prompt_tokens,
                    "completionTokens": record.completion_tokens,
                    "totalTokens": record.total_tokens,
                    "durationMs": record.duration_ms,
                    "error": record.error,
                }),
                created_at: &now,
            },
        )?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交模型用量与 Trace：{error}"))
    }

    pub fn sync_vault_registry(&self, vaults: &[VaultDescriptor]) -> Result<(), String> {
        let current_ids = vaults
            .iter()
            .map(|vault| vault.id.as_str())
            .collect::<HashSet<_>>();
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始 Vault 注册事务：{error}"))?;
        let stale_ids = {
            let mut statement = transaction
                .prepare("SELECT id FROM vault_registry")
                .map_err(|error| format!("无法读取 Vault 注册表：{error}"))?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| format!("无法枚举 Vault 注册表：{error}"))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("无法解析 Vault 注册表：{error}"))?
                .into_iter()
                .filter(|vault_id| !current_ids.contains(vault_id.as_str()))
                .collect::<Vec<_>>()
        };
        for vault_id in stale_ids {
            transaction
                .execute("DELETE FROM note_fts WHERE vault_id=?1", [&vault_id])
                .map_err(|error| format!("无法清理已移除 Vault 的全文索引：{error}"))?;
            transaction
                .execute(
                    "DELETE FROM note_lexical_fts WHERE vault_id=?1",
                    [&vault_id],
                )
                .map_err(|error| format!("无法清理已移除 Vault 的中文词法索引：{error}"))?;
            transaction
                .execute(
                    "DELETE FROM note_feature_vectors WHERE vault_id=?1",
                    [&vault_id],
                )
                .map_err(|error| format!("无法清理已移除 Vault 的本地特征向量：{error}"))?;
            transaction
                .execute("DELETE FROM note_index WHERE vault_id=?1", [&vault_id])
                .map_err(|error| format!("无法清理已移除 Vault 的笔记索引：{error}"))?;
            transaction
                .execute(
                    "DELETE FROM vault_index_changes WHERE vault_id=?1",
                    [&vault_id],
                )
                .map_err(|error| format!("无法清理已移除 Vault 的索引队列：{error}"))?;
            transaction
                .execute("DELETE FROM vault_registry WHERE id=?1", [&vault_id])
                .map_err(|error| format!("无法清理已移除 Vault 的注册记录：{error}"))?;
        }
        for vault in vaults {
            transaction
                .execute(
                    "INSERT INTO vault_registry (
                       id, display_name, canonical_path, note_count, attachment_count,
                       connection_state, is_open, last_indexed_at, last_error
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                     ON CONFLICT(id) DO UPDATE SET
                       display_name=excluded.display_name,
                       canonical_path=excluded.canonical_path,
                       note_count=excluded.note_count,
                       attachment_count=excluded.attachment_count,
                       connection_state=excluded.connection_state,
                       is_open=excluded.is_open,
                       last_indexed_at=excluded.last_indexed_at,
                       last_error=excluded.last_error",
                    params![
                        vault.id,
                        vault.name,
                        vault.path,
                        vault.note_count,
                        vault.attachment_count,
                        vault.connection_state,
                        vault.is_open,
                        vault.last_indexed_at,
                        vault.last_error,
                    ],
                )
                .map_err(|error| format!("无法更新 Vault 注册表：{error}"))?;
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交 Vault 注册事务：{error}"))
    }

    pub(crate) fn stage_long_term_memory_event(
        &self,
        workspace_scope: &str,
        event_id: &str,
        event_type: &str,
        occurred_at: &str,
        payload: &Value,
    ) -> Result<(), String> {
        let serialized = serde_json::to_string(payload)
            .map_err(|error| format!("无法序列化长期记忆投递记录：{error}"))?;
        if serialized.len() > MAX_RECORD_BYTES {
            return Err("长期记忆投递记录超过 2 MB 安全上限".to_string());
        }
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let existing = connection
            .query_row(
                "SELECT workspace_scope, payload, state FROM long_term_memory_events WHERE id=?1",
                [event_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法检查长期记忆投递记录：{error}"))?;
        if let Some((existing_user, existing_payload, state)) = existing {
            if existing_user != workspace_scope || existing_payload != serialized {
                return Err("长期记忆事件 ID 已被其他内容占用".to_string());
            }
            if state != "committed" {
                connection
                    .execute(
                        "UPDATE long_term_memory_events
                         SET state='pending', last_error=NULL, updated_at=?2
                         WHERE id=?1 AND workspace_scope=?3",
                        params![event_id, Utc::now().to_rfc3339(), workspace_scope],
                    )
                    .map_err(|error| format!("无法恢复长期记忆投递：{error}"))?;
            }
            return Ok(());
        }
        let now = Utc::now().to_rfc3339();
        connection
            .execute(
                "INSERT INTO long_term_memory_events
                 (id, workspace_scope, event_type, occurred_at, payload, state, attempt_count, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending', 0, ?6, ?6)",
                params![event_id, workspace_scope, event_type, occurred_at, serialized, now],
            )
            .map_err(|error| format!("无法暂存长期记忆事件：{error}"))?;
        Ok(())
    }

    pub(crate) fn pending_long_term_memory_events(
        &self,
        workspace_scope: &str,
        limit: usize,
    ) -> Result<Vec<PendingLongTermMemoryEvent>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT id, payload FROM long_term_memory_events
                 WHERE workspace_scope=?1 AND state IN ('pending', 'failed')
                 ORDER BY occurred_at, created_at LIMIT ?2",
            )
            .map_err(|error| format!("无法准备长期记忆重放查询：{error}"))?;
        let rows = statement
            .query_map(
                params![workspace_scope, limit.clamp(1, 1000) as i64],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(|error| format!("无法读取待写入长期记忆：{error}"))?;
        Ok(rows
            .filter_map(Result::ok)
            .filter_map(|(id, payload)| {
                serde_json::from_str(&payload)
                    .ok()
                    .map(|payload| PendingLongTermMemoryEvent { id, payload })
            })
            .collect())
    }

    pub(crate) fn commit_long_term_memory_event(
        &self,
        workspace_scope: &str,
        event_id: &str,
        relative_path: &str,
        content_hash: &str,
        committed_at: &str,
    ) -> Result<(), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let changed = connection
            .execute(
                "UPDATE long_term_memory_events
                 SET state='committed', vault_relative_path=?3, content_hash=?4,
                     committed_at=?5, last_error=NULL, updated_at=?5
                 WHERE id=?1 AND workspace_scope=?2",
                params![
                    event_id,
                    workspace_scope,
                    relative_path,
                    content_hash,
                    committed_at
                ],
            )
            .map_err(|error| format!("无法确认长期记忆已写入：{error}"))?;
        if changed == 1 {
            Ok(())
        } else {
            Err("长期记忆投递记录不存在".to_string())
        }
    }

    pub(crate) fn fail_long_term_memory_event(
        &self,
        workspace_scope: &str,
        event_id: &str,
        error: &str,
    ) -> Result<(), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .execute(
                "UPDATE long_term_memory_events
                 SET state='failed', attempt_count=attempt_count+1, last_error=?3, updated_at=?4
                 WHERE id=?1 AND workspace_scope=?2",
                params![
                    event_id,
                    workspace_scope,
                    error.chars().take(1000).collect::<String>(),
                    Utc::now().to_rfc3339()
                ],
            )
            .map_err(|database_error| format!("无法记录长期记忆写入失败：{database_error}"))?;
        Ok(())
    }

    fn query_long_term_memory(
        &self,
        workspace_scope: &str,
        query: &str,
        include_inactive: bool,
        limit: usize,
    ) -> Result<Vec<LongTermMemoryRecord>, String> {
        if query.chars().count() > MAX_SEARCH_QUERY_CHARS {
            return Err("长期记忆查询超过 512 个字符".to_string());
        }
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let now = Utc::now().to_rfc3339();
        let mut statement = connection
            .prepare(
                "SELECT e.id, e.event_type, e.occurred_at, e.state, e.payload,
                        COALESCE(g.status, 'active')
                 FROM long_term_memory_events e
                 LEFT JOIN long_term_memory_governance g
                   ON g.workspace_scope=e.workspace_scope AND g.memory_id=e.id
                 WHERE e.workspace_scope=?1
                   AND (?2=1 OR (
                     COALESCE(g.status, 'active')='active'
                     AND (g.expires_at IS NULL OR g.expires_at>?3)
                   ))
                 ORDER BY e.occurred_at DESC
                 LIMIT 5000",
            )
            .map_err(|error| format!("无法准备长期记忆查询：{error}"))?;
        let rows = statement
            .query_map(
                params![workspace_scope, i64::from(include_inactive), now],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .map_err(|error| format!("无法读取长期记忆：{error}"))?;
        let normalized_query = query.trim().to_lowercase();
        let mut records = Vec::new();
        for row in rows.filter_map(Result::ok) {
            if !normalized_query.is_empty()
                && !format!("{} {} {}", row.0, row.1, row.4)
                    .to_lowercase()
                    .contains(&normalized_query)
            {
                continue;
            }
            let payload = serde_json::from_str::<Value>(&row.4)
                .map_err(|error| format!("长期记忆 {} 的载荷损坏：{error}", row.0))?;
            let actor = payload
                .get("actor")
                .and_then(Value::as_str)
                .unwrap_or("system")
                .to_string();
            records.push(LongTermMemoryRecord {
                id: row.0,
                event_type: row.1,
                occurred_at: row.2,
                actor,
                state: row.3,
                governance_state: row.5,
                payload,
            });
            if records.len() >= limit.clamp(1, 1000) {
                break;
            }
        }
        Ok(records)
    }

    fn govern_long_term_memory(
        &self,
        workspace_scope: &str,
        input: &LongTermMemoryGovernanceInput,
    ) -> Result<(), String> {
        if !valid_runtime_identifier(&input.id, 160) {
            return Err("长期记忆 ID 无效".to_string());
        }
        let status = match input.action.as_str() {
            "activate" => "active",
            "correct" => "corrected",
            "expire" => "expired",
            "tombstone" => "tombstoned",
            "compress" => "compressed",
            _ => return Err("长期记忆治理操作无效".to_string()),
        };
        if input.action == "correct"
            && input
                .replacement_id
                .as_deref()
                .is_none_or(|value| !valid_runtime_identifier(value, 160))
        {
            return Err("纠错操作必须关联有效的替代记忆 ID".to_string());
        }
        let note = input.note.as_deref().unwrap_or("").trim();
        if note.chars().count() > 4000 || contains_sensitive_memory_value(note) {
            return Err("治理备注过长或包含疑似凭据".to_string());
        }
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let exists = connection
            .query_row(
                "SELECT 1 FROM long_term_memory_events WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, input.id],
                |_| Ok(()),
            )
            .optional()
            .map_err(|error| format!("无法检查长期记忆：{error}"))?
            .is_some();
        if !exists {
            return Err("长期记忆记录不存在".to_string());
        }
        let now = Utc::now().to_rfc3339();
        let expires_at = if status == "expired" {
            Some(now.clone())
        } else {
            None
        };
        connection
            .execute(
                "INSERT INTO long_term_memory_governance
                 (workspace_scope, memory_id, status, replacement_id, note, expires_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(workspace_scope, memory_id) DO UPDATE SET
                   status=excluded.status, replacement_id=excluded.replacement_id,
                   note=excluded.note, expires_at=excluded.expires_at, updated_at=excluded.updated_at",
                params![
                    workspace_scope,
                    input.id,
                    status,
                    input.replacement_id,
                    note,
                    expires_at,
                    now
                ],
            )
            .map_err(|error| format!("无法更新长期记忆治理状态：{error}"))?;
        Ok(())
    }

    fn long_term_memory_metrics(
        &self,
        workspace_scope: &str,
    ) -> Result<LongTermMemoryMetrics, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let event_count = |state: Option<&str>| -> Result<i64, String> {
            if let Some(state) = state {
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM long_term_memory_events WHERE workspace_scope=?1 AND state=?2",
                        params![workspace_scope, state],
                        |row| row.get(0),
                    )
                    .map_err(|error| format!("无法统计长期记忆：{error}"))
            } else {
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM long_term_memory_events WHERE workspace_scope=?1",
                        [workspace_scope],
                        |row| row.get(0),
                    )
                    .map_err(|error| format!("无法统计长期记忆：{error}"))
            }
        };
        let governance_count = |status: &str| -> Result<i64, String> {
            connection
                .query_row(
                    "SELECT COUNT(*) FROM long_term_memory_governance WHERE workspace_scope=?1 AND status=?2",
                    params![workspace_scope, status],
                    |row| row.get(0),
                )
                .map_err(|error| format!("无法统计长期记忆治理状态：{error}"))
        };
        let total = event_count(None)?;
        let corrected = governance_count("corrected")?;
        let expired = governance_count("expired")?;
        let tombstoned = governance_count("tombstoned")?;
        let compressed = governance_count("compressed")?;
        Ok(LongTermMemoryMetrics {
            total,
            committed: event_count(Some("committed"))?,
            pending: event_count(Some("pending"))?,
            failed: event_count(Some("failed"))?,
            active: total - corrected - expired - tombstoned - compressed,
            corrected,
            expired,
            tombstoned,
        })
    }

    pub(crate) fn sync_runtime_state(
        &self,
        workspace_scope: &str,
        tasks: &[Value],
        schedules: &[Value],
        report_subscriptions: &[Value],
        scheduler_enabled: bool,
    ) -> Result<(), String> {
        validate_records(tasks, "原生任务")?;
        validate_records(schedules, "原生定时任务")?;
        validate_records(report_subscriptions, "原生报告订阅")?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("无法开始原生运行时同步事务：{error}"))?;
        transaction
            .execute(
                "INSERT INTO runtime_settings (workspace_scope, scheduler_enabled, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(workspace_scope) DO UPDATE SET
                   scheduler_enabled=excluded.scheduler_enabled, updated_at=excluded.updated_at",
                params![
                    workspace_scope,
                    i64::from(scheduler_enabled),
                    Utc::now().to_rfc3339()
                ],
            )
            .map_err(|error| format!("无法保存原生调度开关：{error}"))?;
        sync_runtime_tasks(&transaction, workspace_scope, tasks)?;
        sync_runtime_schedule_group(&transaction, workspace_scope, schedules, "collection")?;
        sync_runtime_schedule_group(
            &transaction,
            workspace_scope,
            report_subscriptions,
            "report",
        )?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交原生运行时同步：{error}"))
    }

    pub(crate) fn sync_managed_resources(
        &self,
        workspace_scope: &str,
        snapshot: &ManagedResourceSnapshotInput,
    ) -> Result<ManagedResourceSnapshot, String> {
        let groups = [
            ("schedule", snapshot.schedules.as_slice()),
            (
                "report_subscription",
                snapshot.report_subscriptions.as_slice(),
            ),
            ("report", snapshot.reports.as_slice()),
        ];
        let total = snapshot.custom_skills.len()
            + groups.iter().map(|(_, values)| values.len()).sum::<usize>();
        if total > MAX_SNAPSHOT_RECORDS {
            return Err("独立资源数量超过安全上限".to_string());
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始独立资源事务：{error}"))?;
        for (resource_type, resources) in groups {
            sync_managed_resource_group(&transaction, workspace_scope, resource_type, resources)?;
        }
        let fixed_resources = [
            (
                "assistant_profile",
                "assistant-profile",
                &snapshot.assistant_profile,
            ),
            (
                "optimization_profile",
                "optimization-profile",
                &snapshot.optimization_profile,
            ),
            (
                "optimization_candidate",
                "optimization-draft",
                &snapshot.optimization_draft,
            ),
        ];
        for (resource_type, id, payload) in fixed_resources {
            if payload.is_object() && !payload.as_object().is_some_and(serde_json::Map::is_empty) {
                upsert_managed_resource(&transaction, workspace_scope, resource_type, id, payload)?;
            } else {
                tombstone_managed_resource(&transaction, workspace_scope, resource_type, id)?;
            }
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交独立资源事务：{error}"))?;
        drop(connection);
        self.load_managed_resources(workspace_scope)
    }

    pub(crate) fn load_managed_resources(
        &self,
        workspace_scope: &str,
    ) -> Result<ManagedResourceSnapshot, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let list = |resource_type: &str| -> Result<Vec<Value>, String> {
            let mut statement = connection
                .prepare(
                    "SELECT payload FROM managed_resources
                     WHERE workspace_scope=?1 AND resource_type=?2 AND state='active'
                     ORDER BY updated_at DESC",
                )
                .map_err(|error| format!("无法准备独立资源查询：{error}"))?;
            let rows = statement
                .query_map(params![workspace_scope, resource_type], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|error| format!("无法读取独立资源：{error}"))?;
            Ok(rows
                .filter_map(Result::ok)
                .filter_map(|payload| serde_json::from_str(&payload).ok())
                .collect())
        };
        let fixed = |resource_type: &str, id: &str| -> Result<Value, String> {
            connection
                .query_row(
                    "SELECT payload FROM managed_resources
                     WHERE workspace_scope=?1 AND resource_type=?2 AND id=?3 AND state='active'",
                    params![workspace_scope, resource_type, id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| format!("无法读取独立配置资源：{error}"))
                .map(|payload| {
                    payload
                        .and_then(|value| serde_json::from_str(&value).ok())
                        .unwrap_or_else(|| Value::Object(serde_json::Map::new()))
                })
        };
        Ok(ManagedResourceSnapshot {
            initialized: connection
                .query_row(
                    "SELECT COUNT(*) FROM managed_resources WHERE workspace_scope=?1",
                    [workspace_scope],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| format!("无法检查独立资源初始化状态：{error}"))?
                > 0,
            custom_skills: crate::skill_lifecycle::list_skills_in_connection(
                &connection,
                workspace_scope,
                false,
            )?
            .into_iter()
            .map(|skill| {
                serde_json::to_value(skill)
                    .map_err(|error| format!("无法序列化 Skill 生命周期状态：{error}"))
            })
            .collect::<Result<Vec<_>, _>>()?,
            schedules: list("schedule")?,
            report_subscriptions: list("report_subscription")?,
            reports: list("report")?,
            assistant_profile: fixed("assistant_profile", "assistant-profile")?,
            optimization_profile: fixed("optimization_profile", "optimization-profile")?,
            optimization_draft: fixed("optimization_candidate", "optimization-draft")?,
        })
    }

    pub(crate) fn claim_due_runtime_schedules(
        &self,
        workspace_scope: &str,
        limit: usize,
    ) -> Result<Vec<DueRuntimeSchedule>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let scheduler_enabled = connection
            .query_row(
                "SELECT scheduler_enabled FROM runtime_settings WHERE workspace_scope=?1",
                [workspace_scope],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| format!("无法读取原生调度开关：{error}"))?
            .unwrap_or(0);
        if scheduler_enabled == 0 {
            return Ok(Vec::new());
        }
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("无法开始调度租约事务：{error}"))?;
        let now = Utc::now().to_rfc3339();
        let mut statement = transaction
            .prepare(
                "SELECT id, schedule_kind, payload
                 FROM runtime_schedules
                 WHERE workspace_scope=?1 AND enabled=1 AND next_run IS NOT NULL AND next_run<=?2
                   AND (lease_expires_at IS NULL OR lease_expires_at<=?2)
                 ORDER BY next_run LIMIT ?3",
            )
            .map_err(|error| format!("无法查询到期原生日程：{error}"))?;
        let selected = statement
            .query_map(
                params![workspace_scope, now, limit.clamp(1, 128) as i64],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(|error| format!("无法读取到期原生日程：{error}"))?
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        drop(statement);
        let lease_owner = Uuid::new_v4().to_string();
        let lease_expires_at = (Utc::now() + chrono::Duration::seconds(90)).to_rfc3339();
        let mut due = Vec::new();
        for (id, schedule_kind, payload) in selected {
            let changed = transaction
                .execute(
                    "UPDATE runtime_schedules
                     SET lease_owner=?3, lease_expires_at=?4, last_claimed_at=?5, updated_at=?5
                     WHERE workspace_scope=?1 AND id=?2 AND (lease_expires_at IS NULL OR lease_expires_at<=?5)",
                    params![workspace_scope, id, lease_owner, lease_expires_at, now],
                )
                .map_err(|error| format!("无法领取到期原生日程：{error}"))?;
            if changed != 1 {
                continue;
            }
            if let Ok(payload) = serde_json::from_str(&payload) {
                due.push(DueRuntimeSchedule {
                    id,
                    schedule_kind,
                    payload,
                });
            }
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交调度租约：{error}"))?;
        Ok(due)
    }

    pub(crate) fn recover_interrupted_runtime_tasks(
        &self,
        workspace_scope: &str,
    ) -> Result<Vec<RuntimeTaskRecovery>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("无法开始任务恢复检查事务：{error}"))?;
        let interrupted = {
            let mut statement = transaction
                .prepare(
                    "SELECT id, payload, updated_at FROM runtime_tasks
                     WHERE workspace_scope=?1 AND state IN ('running', 'awaiting_approval')
                     ORDER BY updated_at",
                )
                .map_err(|error| format!("无法查询中断任务：{error}"))?;
            let rows = statement
                .query_map([workspace_scope], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(|error| format!("无法读取中断任务：{error}"))?
                .filter_map(Result::ok)
                .collect::<Vec<_>>();
            rows
        };
        let detected_at = Utc::now().to_rfc3339();
        for (task_id, payload_json, task_updated_at) in interrupted {
            let payload = serde_json::from_str::<Value>(&payload_json)
                .map_err(|error| format!("中断任务 {task_id} 的快照损坏：{error}"))?;
            let completed_write_events = transaction
                .query_row(
                    "SELECT COUNT(*) FROM operation_events
                     WHERE task_id=?1 AND state IN ('success', 'succeeded')
                       AND (event_type LIKE 'vault.%write%' OR event_type LIKE 'vault.%delete%')",
                    [&task_id],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| format!("无法读取任务写入证据：{error}"))?;
            let committed_content = transaction
                .query_row(
                    "SELECT COUNT(*) FROM inbound_content_records
                     WHERE workspace_scope=?1 AND task_id=?2 AND state='committed'",
                    params![workspace_scope, task_id],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| format!("无法读取任务内容提交证据：{error}"))?;
            let resume_step = transaction
                .query_row(
                    "SELECT step_id, position FROM runtime_task_steps
                     WHERE workspace_scope=?1 AND task_id=?2 AND state NOT IN ('done', 'succeeded')
                     ORDER BY position LIMIT 1",
                    params![workspace_scope, task_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()
                .map_err(|error| format!("无法读取任务恢复步骤：{error}"))?;
            let resume_checkpoint_id = transaction
                .query_row(
                    "SELECT checkpoint_id FROM runtime_task_checkpoints
                     WHERE workspace_scope=?1 AND task_id=?2 AND state IN ('running', 'completed')
                     ORDER BY sequence DESC, updated_at DESC LIMIT 1",
                    params![workspace_scope, task_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| format!("无法读取任务恢复检查点：{error}"))?;
            let attachment_count = payload
                .get("attachmentIds")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            let intent = payload
                .get("intent")
                .and_then(Value::as_str)
                .unwrap_or("general");
            let approval = payload
                .get("approval")
                .and_then(Value::as_str)
                .unwrap_or("none");
            let mut evidence = Vec::new();
            if completed_write_events > 0 {
                evidence.push(format!("{completed_write_events} 条原生 Vault 提交事件"));
            }
            if committed_content > 0 {
                evidence.push(format!("{committed_content} 条已提交内容记录"));
            }
            let recommendation = if !evidence.is_empty() {
                "completed"
            } else if attachment_count > 0 {
                "needs_input"
            } else if intent == "delete" || approval != "none" {
                "manual"
            } else {
                "resume"
            };
            let detail = match recommendation {
                "completed" => "检测到真实副作用已提交，不应重复执行".to_string(),
                "needs_input" => "任务依赖进程内附件，应用重启后需要用户重新提供".to_string(),
                "manual" => "破坏性或外部操作必须重新经过当前用户决策".to_string(),
                _ => "未发现已提交副作用，可从首个未完成步骤重新执行".to_string(),
            };
            transaction
                .execute(
                    "INSERT INTO runtime_task_recoveries
                     (workspace_scope, task_id, interrupted_task_updated_at, recommendation,
                      resume_step_id, resume_step_index, resume_checkpoint_id, evidence_json,
                      detail, state, detected_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', ?10, ?10)
                     ON CONFLICT(workspace_scope, task_id) DO UPDATE SET
                       interrupted_task_updated_at=excluded.interrupted_task_updated_at,
                       recommendation=excluded.recommendation,
                       resume_step_id=excluded.resume_step_id,
                       resume_step_index=excluded.resume_step_index,
                       resume_checkpoint_id=excluded.resume_checkpoint_id,
                       evidence_json=excluded.evidence_json, detail=excluded.detail,
                       state='pending', detected_at=excluded.detected_at, updated_at=excluded.updated_at,
                       resolution=NULL, resolved_at=NULL",
                    params![
                        workspace_scope,
                        task_id,
                        task_updated_at,
                        recommendation,
                        resume_step.as_ref().map(|item| item.0.as_str()),
                        resume_step.as_ref().map(|item| item.1),
                        resume_checkpoint_id,
                        serde_json::to_string(&evidence)
                            .map_err(|error| format!("无法序列化任务恢复证据：{error}"))?,
                        detail,
                        detected_at,
                    ],
                )
                .map_err(|error| format!("无法登记任务恢复建议：{error}"))?;
        }
        let recoveries = {
            let mut statement = transaction
                .prepare(
                    "SELECT task_id, recommendation, resume_step_id, resume_step_index,
                            resume_checkpoint_id, evidence_json, detail, detected_at
                     FROM runtime_task_recoveries
                     WHERE workspace_scope=?1 AND state='pending' ORDER BY detected_at",
                )
                .map_err(|error| format!("无法读取待恢复任务：{error}"))?;
            let rows = statement
                .query_map([workspace_scope], |row| {
                    let evidence_json: String = row.get(5)?;
                    Ok(RuntimeTaskRecovery {
                        task_id: row.get(0)?,
                        recommendation: row.get(1)?,
                        resume_step_id: row.get(2)?,
                        resume_step_index: row.get(3)?,
                        resume_checkpoint_id: row.get(4)?,
                        evidence: serde_json::from_str(&evidence_json).unwrap_or_default(),
                        detail: row.get(6)?,
                        detected_at: row.get(7)?,
                    })
                })
                .map_err(|error| format!("无法枚举待恢复任务：{error}"))?
                .filter_map(Result::ok)
                .collect::<Vec<_>>();
            rows
        };
        transaction
            .commit()
            .map_err(|error| format!("无法提交任务恢复检查：{error}"))?;
        Ok(recoveries)
    }

    pub(crate) fn resolve_runtime_task_recovery(
        &self,
        workspace_scope: &str,
        task_id: &str,
        resolution: &str,
    ) -> Result<(), String> {
        if !matches!(
            resolution,
            "completed" | "resumed" | "needs_input" | "manual" | "failed"
        ) {
            return Err("任务恢复结果无效".to_string());
        }
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .execute(
                "UPDATE runtime_task_recoveries
                 SET state='resolved', resolution=?3, resolved_at=?4, updated_at=?4
                 WHERE workspace_scope=?1 AND task_id=?2",
                params![
                    workspace_scope,
                    task_id,
                    resolution,
                    Utc::now().to_rfc3339()
                ],
            )
            .map_err(|error| format!("无法完成任务恢复登记：{error}"))?;
        Ok(())
    }

    pub(crate) fn upsert_inbound_content_record(
        &self,
        workspace_scope: &str,
        record: &InboundContentRecordInput,
    ) -> Result<InboundContentRecordReceipt, String> {
        validate_inbound_content_record(record)?;
        let extraction_json = serialize_inbound_record_section(&record.extraction, "提取诊断")?;
        let analysis_json = serialize_inbound_record_section(&record.analysis, "模型分析")?;
        let quality_json = serialize_inbound_record_section(&record.quality, "质量门禁")?;
        let target_json = serialize_inbound_record_section(&record.target, "写入目标")?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("无法开始内容处理记录事务：{error}"))?;
        let existing = transaction
            .query_row(
                "SELECT state, content_hash, source_type FROM inbound_content_records
                 WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, record.id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法读取内容处理记录：{error}"))?;
        let duplicate_of = if existing.is_none()
            && matches!(
                record.state.as_str(),
                "extracted" | "analyzing" | "analysis_pending" | "ready_to_write" | "writing"
            ) {
            transaction
                .query_row(
                    "SELECT id FROM inbound_content_records
                     WHERE workspace_scope=?1 AND id<>?2 AND source_type=?3 AND content_hash=?4
                       AND state IN ('ready_to_write', 'writing', 'committed')
                     ORDER BY CASE state WHEN 'committed' THEN 0 WHEN 'writing' THEN 1 ELSE 2 END,
                              updated_at DESC
                     LIMIT 1",
                    params![
                        workspace_scope,
                        record.id,
                        record.source_type,
                        record.content_hash
                    ],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| format!("无法检查跨任务重复内容：{error}"))?
        } else {
            None
        };
        if let Some((previous_state, previous_hash, previous_source_type)) = existing.as_ref() {
            if previous_hash != &record.content_hash || previous_source_type != &record.source_type
            {
                return Err("同一内容记录 ID 不能更换来源类型或正文哈希".to_string());
            }
            if !inbound_content_transition_allowed(previous_state, &record.state) {
                return Err(format!(
                    "内容处理状态不能从 {previous_state} 迁移到 {}",
                    record.state
                ));
            }
        }
        let previous_state = existing.map(|(state, _, _)| state);
        let stored_state = if duplicate_of.is_some() {
            "quality_rejected"
        } else {
            record.state.as_str()
        };
        let now = Utc::now().to_rfc3339();
        let committed_at = (stored_state == "committed").then_some(now.as_str());
        let duplicate_failure = duplicate_of
            .as_deref()
            .map(|id| format!("内容哈希与已有记录 {id} 完全相同，已阻止重复写入"));
        let failure_reason = duplicate_failure
            .as_deref()
            .or(record.failure_reason.as_deref());
        transaction
            .execute(
                "INSERT INTO inbound_content_records
                 (workspace_scope, id, task_id, state, source_type, source_ref, title, content_hash,
                  content_characters, attachment_count, image_count, extraction_json,
                  analysis_json, quality_json, target_json, failure_reason, created_at, updated_at, committed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?17, ?18)
                 ON CONFLICT(workspace_scope, id) DO UPDATE SET
                   task_id=excluded.task_id, state=excluded.state, source_ref=excluded.source_ref,
                   title=excluded.title, content_characters=excluded.content_characters,
                   attachment_count=excluded.attachment_count, image_count=excluded.image_count,
                   extraction_json=excluded.extraction_json, analysis_json=excluded.analysis_json,
                   quality_json=excluded.quality_json, target_json=excluded.target_json,
                   failure_reason=excluded.failure_reason, updated_at=excluded.updated_at,
                   committed_at=COALESCE(excluded.committed_at, inbound_content_records.committed_at)",
                params![
                    workspace_scope,
                    record.id,
                    record.task_id,
                    stored_state,
                    record.source_type,
                    record.source_ref,
                    record.title,
                    record.content_hash,
                    record.content_characters as i64,
                    record.attachment_count as i64,
                    record.image_count as i64,
                    extraction_json,
                    analysis_json,
                    quality_json,
                    target_json,
                    failure_reason,
                    now,
                    committed_at,
                ],
            )
            .map_err(|error| format!("无法保存内容处理记录：{error}"))?;
        if previous_state.as_deref() != Some(stored_state) {
            transaction
                .execute(
                    "INSERT INTO inbound_content_transitions
                     (id, workspace_scope, content_id, from_state, to_state, detail, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        Uuid::new_v4().to_string(),
                        workspace_scope,
                        record.id,
                        previous_state,
                        stored_state,
                        failure_reason
                            .unwrap_or("")
                            .chars()
                            .take(1000)
                            .collect::<String>(),
                        now,
                    ],
                )
                .map_err(|error| format!("无法记录内容处理状态迁移：{error}"))?;
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交内容处理记录：{error}"))?;
        Ok(InboundContentRecordReceipt {
            id: record.id.clone(),
            state: stored_state.to_string(),
            previous_state,
            duplicate_of,
            updated_at: now,
        })
    }

    pub fn should_initialize_default_vaults(&self, workspace_scope: &str) -> Result<bool, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let preference = connection
            .query_row(
                "SELECT defaults_initialized, explicit_vault_id FROM vault_preferences WHERE workspace_scope=?1",
                [workspace_scope],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()
            .map_err(|error| format!("无法读取本地工作区 Vault 初始化状态：{error}"))?;
        if let Some((initialized, explicit_vault_id)) = preference {
            return Ok(initialized == 0 && explicit_vault_id.is_none());
        }

        let legacy_selection = connection
            .query_row(
                "SELECT payload FROM workspace_snapshots WHERE workspace_scope=?1",
                [workspace_scope],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("无法读取本地工作区 Vault 选择：{error}"))?
            .and_then(|payload| serde_json::from_str::<Value>(&payload).ok())
            .and_then(|payload| {
                payload
                    .get("clientState")
                    .and_then(|state| state.get("currentVaultId"))
                    .and_then(Value::as_str)
                    .filter(|vault_id| !vault_id.is_empty() && *vault_id != "all")
                    .map(str::to_string)
            });
        if let Some(vault_id) = legacy_selection {
            connection
                .execute(
                    "INSERT INTO vault_preferences (workspace_scope, defaults_initialized, explicit_vault_id, updated_at)
                     VALUES (?1, 0, ?2, ?3)",
                    params![workspace_scope, vault_id, Utc::now().to_rfc3339()],
                )
                .map_err(|error| format!("无法迁移本地工作区 Vault 选择：{error}"))?;
            return Ok(false);
        }
        Ok(true)
    }

    pub fn mark_default_vaults_initialized(&self, workspace_scope: &str) -> Result<(), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .execute(
                "INSERT INTO vault_preferences (workspace_scope, defaults_initialized, explicit_vault_id, updated_at)
                 VALUES (?1, 1, NULL, ?2)
                 ON CONFLICT(workspace_scope) DO UPDATE SET defaults_initialized=1, updated_at=excluded.updated_at",
                params![workspace_scope, Utc::now().to_rfc3339()],
            )
            .map_err(|error| format!("无法保存本地工作区 Vault 初始化状态：{error}"))?;
        Ok(())
    }

    pub fn save_explicit_vault_selection(
        &self,
        workspace_scope: &str,
        vault_id: Option<&str>,
    ) -> Result<(), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .execute(
                "INSERT INTO vault_preferences (workspace_scope, defaults_initialized, explicit_vault_id, updated_at)
                 VALUES (?1, 0, ?2, ?3)
                 ON CONFLICT(workspace_scope) DO UPDATE SET explicit_vault_id=excluded.explicit_vault_id, updated_at=excluded.updated_at",
                params![workspace_scope, vault_id, Utc::now().to_rfc3339()],
            )
            .map_err(|error| format!("无法保存本地工作区 Vault 选择：{error}"))?;
        Ok(())
    }

    pub fn load_legacy_model_profiles(
        &self,
        workspace_scope: &str,
    ) -> Result<Vec<LegacyModelProfile>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT role, provider, base_url, selected_model, available_models_json, api_key_ciphertext
                 FROM legacy_model_profiles WHERE workspace_scope=?1
                 ORDER BY CASE role WHEN 'chat' THEN 0 WHEN 'analysis' THEN 1 WHEN 'image' THEN 2 ELSE 3 END",
            )
            .map_err(|error| format!("无法准备本地模型配置查询：{error}"))?;
        let rows = statement
            .query_map([workspace_scope], |row| {
                let models: String = row.get(4)?;
                Ok(LegacyModelProfile {
                    role: row.get(0)?,
                    provider: row.get(1)?,
                    base_url: row.get(2)?,
                    selected_model: row.get(3)?,
                    available_models: serde_json::from_str(&models)
                        .unwrap_or_else(|_| Value::Array(Vec::new())),
                    api_key_ciphertext: row.get(5)?,
                })
            })
            .map_err(|error| format!("无法读取本地模型配置：{error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析本地模型配置：{error}"))
    }

    pub fn clear_legacy_model_profiles(&self, workspace_scope: &str) -> Result<(), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .execute(
                "DELETE FROM legacy_model_profiles WHERE workspace_scope=?1",
                [workspace_scope],
            )
            .map_err(|error| format!("无法清除旧版模型配置：{error}"))?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn save_model_provider_record(
        &self,
        workspace_scope: &str,
        id: &str,
        name: &str,
        provider: &str,
        base_url: &str,
        available_models: &Value,
        assignments: &Value,
        defaults: &Value,
        api_key_ciphertext: &[u8],
    ) -> Result<(), String> {
        let available_models_json = serde_json::to_string(available_models)
            .map_err(|error| format!("无法序列化供应商模型列表：{error}"))?;
        let assignments_json = serde_json::to_string(assignments)
            .map_err(|error| format!("无法序列化模型用途：{error}"))?;
        let defaults_json = serde_json::to_string(defaults)
            .map_err(|error| format!("无法序列化默认模型：{error}"))?;
        if available_models_json.len() > MAX_RECORD_BYTES
            || assignments_json.len() > MAX_RECORD_BYTES
            || defaults_json.len() > MAX_RECORD_BYTES
        {
            return Err("供应商模型配置超过 2 MB 安全上限".to_string());
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let now = Utc::now().to_rfc3339();
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始模型供应商事务：{error}"))?;
        for role in ["chat", "analysis", "image"] {
            if defaults
                .get(role)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty())
            {
                transaction
                    .execute(
                        "UPDATE model_providers
                         SET defaults_json=json_remove(defaults_json, ?3), updated_at=?4
                         WHERE workspace_scope=?1 AND id<>?2",
                        params![workspace_scope, id, format!("$.{role}"), now],
                    )
                    .map_err(|error| format!("无法更新默认模型唯一性：{error}"))?;
            }
        }
        transaction
            .execute(
                "INSERT INTO model_providers
                 (workspace_scope, id, name, provider, base_url, available_models_json,
                  assignments_json, defaults_json, api_key_ciphertext, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)
                 ON CONFLICT(workspace_scope, id) DO UPDATE SET
                   name=excluded.name,
                   provider=excluded.provider,
                   base_url=excluded.base_url,
                   available_models_json=excluded.available_models_json,
                   assignments_json=excluded.assignments_json,
                   defaults_json=excluded.defaults_json,
                   api_key_ciphertext=excluded.api_key_ciphertext,
                   updated_at=excluded.updated_at",
                params![
                    workspace_scope,
                    id,
                    name,
                    provider,
                    base_url,
                    available_models_json,
                    assignments_json,
                    defaults_json,
                    api_key_ciphertext,
                    now,
                ],
            )
            .map_err(|error| format!("无法保存模型供应商：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交模型供应商配置：{error}"))
    }

    pub fn load_model_provider(
        &self,
        workspace_scope: &str,
        id: &str,
    ) -> Result<Option<ModelProviderProfile>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .query_row(
                "SELECT id, name, provider, base_url, available_models_json,
                        assignments_json, defaults_json, api_key_ciphertext
                 FROM model_providers WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, id],
                |row| {
                    let available_models: String = row.get(4)?;
                    let assignments: String = row.get(5)?;
                    let defaults: String = row.get(6)?;
                    Ok(ModelProviderProfile {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        provider: row.get(2)?,
                        base_url: row.get(3)?,
                        available_models: serde_json::from_str(&available_models)
                            .unwrap_or_else(|_| Value::Array(Vec::new())),
                        assignments: serde_json::from_str(&assignments)
                            .unwrap_or_else(|_| serde_json::json!({})),
                        defaults: serde_json::from_str(&defaults)
                            .unwrap_or_else(|_| serde_json::json!({})),
                        api_key_ciphertext: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(|error| format!("无法读取模型供应商：{error}"))
    }

    pub fn load_model_providers(
        &self,
        workspace_scope: &str,
    ) -> Result<Vec<ModelProviderProfile>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT id, name, provider, base_url, available_models_json,
                        assignments_json, defaults_json, api_key_ciphertext
                 FROM model_providers WHERE workspace_scope=?1 ORDER BY created_at, name, id",
            )
            .map_err(|error| format!("无法准备模型供应商查询：{error}"))?;
        let rows = statement
            .query_map([workspace_scope], |row| {
                let available_models: String = row.get(4)?;
                let assignments: String = row.get(5)?;
                let defaults: String = row.get(6)?;
                Ok(ModelProviderProfile {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    provider: row.get(2)?,
                    base_url: row.get(3)?,
                    available_models: serde_json::from_str(&available_models)
                        .unwrap_or_else(|_| Value::Array(Vec::new())),
                    assignments: serde_json::from_str(&assignments)
                        .unwrap_or_else(|_| serde_json::json!({})),
                    defaults: serde_json::from_str(&defaults)
                        .unwrap_or_else(|_| serde_json::json!({})),
                    api_key_ciphertext: row.get(7)?,
                })
            })
            .map_err(|error| format!("无法读取模型供应商：{error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析模型供应商：{error}"))
    }

    pub fn delete_model_provider_record(
        &self,
        workspace_scope: &str,
        id: &str,
    ) -> Result<(), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .execute(
                "DELETE FROM model_providers WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, id],
            )
            .map_err(|error| format!("无法删除模型供应商：{error}"))?;
        Ok(())
    }

    /// Reads or creates the per-device key used to encrypt local API keys.
    /// The key never enters SQLite, logs, Obsidian, or the frontend.
    pub fn device_encryption_key(&self) -> Result<[u8; 32], String> {
        let key_path = self.path.with_file_name("yunspire.sqlite.key");
        let read_key = || -> Result<[u8; 32], String> {
            let bytes = fs::read(&key_path)
                .map_err(|error| format!("无法读取云枢本机设备密钥：{error}"))?;
            if bytes.len() != 32 {
                return Err("云枢本机设备密钥长度无效".to_string());
            }
            #[cfg(unix)]
            fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))
                .map_err(|error| format!("无法收紧本机设备密钥权限：{error}"))?;
            let mut key = [0u8; 32];
            key.copy_from_slice(&bytes);
            Ok(key)
        };

        if key_path.exists() {
            return read_key();
        }

        let mut key = [0u8; 32];
        key[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        key[16..].copy_from_slice(Uuid::new_v4().as_bytes());
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&key_path) {
            Ok(mut file) => {
                file.write_all(&key)
                    .map_err(|error| format!("无法写入云枢本机设备密钥：{error}"))?;
                file.sync_all()
                    .map_err(|error| format!("无法持久化云枢本机设备密钥：{error}"))?;
                Ok(key)
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => read_key(),
            Err(error) => Err(format!("无法创建云枢本机设备密钥：{error}")),
        }
    }

    pub fn append_operation_event(&self, event: &OperationEvent) -> Result<(), String> {
        let workspace_scope = self.local_workspace_scope()?;
        let mut event = event.clone();
        let trace_id = event
            .trace_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(crate::trace::new_trace_id);
        crate::trace::validate_trace_id(&trace_id)?;
        event.trace_id = Some(trace_id.clone());
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始操作事件事务：{error}"))?;
        let payload = serde_json::to_string(&event)
            .map_err(|error| format!("无法序列化原生操作事件：{error}"))?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO operation_events
                 (id, task_id, trace_id, event_type, state, payload, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    event.id,
                    event.task_id,
                    trace_id,
                    event.event_type,
                    event.state,
                    payload,
                    event.created_at
                ],
            )
            .map_err(|error| format!("无法写入 SQLite 操作日志：{error}"))?;
        crate::trace::record_trace_event_in_connection(
            &transaction,
            &workspace_scope,
            &crate::trace::TraceEventRecord {
                trace_id: &trace_id,
                entity_kind: "operation_event",
                entity_id: &event.id,
                event_type: &event.event_type,
                state: &event.state,
                payload: &serde_json::json!({
                    "taskId": event.task_id,
                    "vaultId": event.vault_id,
                    "relativePath": event.relative_path,
                    "detail": event.detail,
                }),
                created_at: &event.created_at,
            },
        )?;
        if let Some(vault_id) = event.vault_id.as_deref() {
            crate::trace::record_trace_event_in_connection(
                &transaction,
                &workspace_scope,
                &crate::trace::TraceEventRecord {
                    trace_id: &trace_id,
                    entity_kind: "vault_operation",
                    entity_id: &event.id,
                    event_type: &event.event_type,
                    state: &event.state,
                    payload: &serde_json::json!({
                        "vaultId": vault_id,
                        "relativePath": event.relative_path,
                        "detail": event.detail,
                    }),
                    created_at: &event.created_at,
                },
            )?;
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交操作事件与 Trace：{error}"))
    }

    pub(crate) fn persist_application_command(
        &self,
        workspace_scope: &str,
        command: &ApplicationCommand,
        decision: &PolicyDecision,
        trace_id: &str,
        accepted_at: &str,
    ) -> Result<(Option<String>, bool), String> {
        crate::trace::validate_trace_id(trace_id)?;
        let command_payload = serde_json::to_string(command)
            .map_err(|error| format!("无法序列化应用命令：{error}"))?;
        let decision_payload = serde_json::to_string(decision)
            .map_err(|error| format!("无法序列化策略决定：{error}"))?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始应用命令事务：{error}"))?;
        let duplicate = transaction
            .query_row(
                "SELECT task_id, payload, trace_id FROM application_commands
                 WHERE workspace_scope=?1 AND idempotency_key=?2",
                params![workspace_scope, command.idempotency_key],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法检查应用命令幂等键：{error}"))?;
        if let Some((task_id, stored_payload, stored_trace_id)) = duplicate {
            let stored_command = serde_json::from_str::<ApplicationCommand>(&stored_payload)
                .map_err(|error| format!("无法解析幂等应用命令：{error}"))?;
            if crate::policy::command_authorization_binding(&stored_command)
                != crate::policy::command_authorization_binding(command)
            {
                return Err("应用命令幂等键已经绑定到不同的能力或参数范围".to_string());
            }
            if stored_trace_id != trace_id {
                return Err("应用命令幂等键已经绑定到其他 Trace".to_string());
            }
            transaction
                .commit()
                .map_err(|error| format!("无法完成应用命令幂等查询：{error}"))?;
            return Ok((task_id, true));
        }

        let (command_state, task_state) = match decision.outcome {
            PolicyOutcome::Deny => ("denied", None),
            PolicyOutcome::RequireApproval => ("accepted", Some("awaiting_approval")),
            PolicyOutcome::Allow | PolicyOutcome::AllowWithReducedScope => {
                ("accepted", Some("queued"))
            }
        };
        let task_id = task_state.map(|_| format!("task-{}", Uuid::new_v4()));
        transaction
            .execute(
                "INSERT INTO application_commands
                 (workspace_scope, id, idempotency_key, command_type, operation, state,
                  task_id, trace_id, payload, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
                params![
                    workspace_scope,
                    command.id,
                    command.idempotency_key,
                    command.command_type,
                    command.operation,
                    command_state,
                    task_id,
                    trace_id,
                    command_payload,
                    accepted_at,
                ],
            )
            .map_err(|error| format!("无法保存应用命令：{error}"))?;
        transaction
            .execute(
                "INSERT INTO policy_decisions
                 (id, workspace_scope, command_id, outcome, payload, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    Uuid::new_v4().to_string(),
                    workspace_scope,
                    command.id,
                    match decision.outcome {
                        PolicyOutcome::Allow => "allow",
                        PolicyOutcome::Deny => "deny",
                        PolicyOutcome::RequireApproval => "require_approval",
                        PolicyOutcome::AllowWithReducedScope => "allow_with_reduced_scope",
                    },
                    decision_payload,
                    accepted_at,
                ],
            )
            .map_err(|error| format!("无法保存策略决定：{error}"))?;
        if let (Some(task_id), Some(task_state)) = (task_id.as_ref(), task_state) {
            let title = command
                .parameters
                .get("title")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(&command.intent)
                .chars()
                .take(240)
                .collect::<String>();
            let progress = if task_state == "awaiting_approval" {
                5
            } else {
                0
            };
            let task_payload = serde_json::json!({
                "id": task_id,
                "kind": match command.origin {
                    crate::policy::CommandOrigin::Schedule => "scheduled",
                    crate::policy::CommandOrigin::SystemMaintenance => "maintenance",
                    crate::policy::CommandOrigin::Evolution => "evolution",
                    crate::policy::CommandOrigin::DirectUser | crate::policy::CommandOrigin::Assistant => "interactive",
                },
                "state": task_state,
                "title": title,
                "traceId": trace_id,
                "intent": command.intent,
                "capabilityIds": [command.capability_id.clone()],
                "operation": command.operation,
                "parameters": command.parameters,
                "vaultId": command.vault_id,
                "relativePaths": command.relative_paths,
                "networkTargets": command.network_targets,
                "declaredScope": command.declared_scope,
                "budget": command.budget,
                "idempotencyKey": command.idempotency_key,
                "commandId": command.id,
                "policyDecision": decision,
                "approval": decision.approval_type,
                "progress": progress,
                "steps": [],
                "checkpoints": [],
                "createdAt": accepted_at,
                "updatedAt": accepted_at,
            });
            let serialized = serde_json::to_string(&task_payload)
                .map_err(|error| format!("无法序列化原生任务：{error}"))?;
            transaction
                .execute(
                    "INSERT INTO runtime_tasks
                     (workspace_scope, id, state, title, trace_id, payload, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                    params![
                        workspace_scope,
                        task_id,
                        task_state,
                        title,
                        trace_id,
                        serialized,
                        accepted_at
                    ],
                )
                .map_err(|error| format!("无法创建原生任务：{error}"))?;
            transaction
                .execute(
                    "INSERT INTO runtime_task_attempts
                     (id, workspace_scope, task_id, state, detail, started_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        Uuid::new_v4().to_string(),
                        workspace_scope,
                        task_id,
                        task_state,
                        "由类型化应用命令创建",
                        accepted_at,
                    ],
                )
                .map_err(|error| format!("无法记录原生任务首次尝试：{error}"))?;
        }
        let event = OperationEvent {
            id: Uuid::new_v4().to_string(),
            task_id: task_id.clone(),
            trace_id: Some(trace_id.to_string()),
            event_type: "command.policy_decided".to_string(),
            state: match decision.outcome {
                PolicyOutcome::Deny => "denied",
                PolicyOutcome::RequireApproval => "awaiting_approval",
                PolicyOutcome::Allow | PolicyOutcome::AllowWithReducedScope => "accepted",
            }
            .to_string(),
            created_at: accepted_at.to_string(),
            vault_id: command.vault_id.clone(),
            relative_path: command.relative_paths.first().cloned(),
            detail: format!("{}：{}", command.operation, decision.reason_codes.join(",")),
        };
        let event_payload = serde_json::to_string(&event)
            .map_err(|error| format!("无法序列化策略决定审计事件：{error}"))?;
        transaction
            .execute(
                "INSERT INTO operation_events
                 (id, task_id, trace_id, event_type, state, payload, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    event.id,
                    event.task_id,
                    trace_id,
                    event.event_type,
                    event.state,
                    event_payload,
                    event.created_at
                ],
            )
            .map_err(|error| format!("无法保存策略决定审计事件：{error}"))?;
        crate::trace::record_trace_event_in_connection(
            &transaction,
            workspace_scope,
            &crate::trace::TraceEventRecord {
                trace_id,
                entity_kind: "application_command",
                entity_id: &command.id,
                event_type: "command.policy_decided",
                state: command_state,
                payload: &serde_json::json!({
                    "taskId": task_id,
                    "operation": command.operation,
                    "outcome": decision.outcome,
                }),
                created_at: accepted_at,
            },
        )?;
        if let (Some(task_id), Some(task_state)) = (task_id.as_deref(), task_state) {
            crate::trace::record_trace_event_in_connection(
                &transaction,
                workspace_scope,
                &crate::trace::TraceEventRecord {
                    trace_id,
                    entity_kind: "runtime_task",
                    entity_id: task_id,
                    event_type: "task.created",
                    state: task_state,
                    payload: &serde_json::json!({"commandId": command.id}),
                    created_at: accepted_at,
                },
            )?;
        }
        crate::trace::record_trace_event_in_connection(
            &transaction,
            workspace_scope,
            &crate::trace::TraceEventRecord {
                trace_id,
                entity_kind: "operation_event",
                entity_id: &event.id,
                event_type: &event.event_type,
                state: &event.state,
                payload: &serde_json::json!({
                    "taskId": event.task_id,
                    "vaultId": event.vault_id,
                    "relativePath": event.relative_path,
                    "detail": event.detail,
                }),
                created_at: &event.created_at,
            },
        )?;
        if let Some(vault_id) = event.vault_id.as_deref() {
            crate::trace::record_trace_event_in_connection(
                &transaction,
                workspace_scope,
                &crate::trace::TraceEventRecord {
                    trace_id,
                    entity_kind: "vault_operation",
                    entity_id: &event.id,
                    event_type: &event.event_type,
                    state: &event.state,
                    payload: &serde_json::json!({
                        "vaultId": vault_id,
                        "relativePath": event.relative_path,
                        "detail": event.detail,
                    }),
                    created_at: &event.created_at,
                },
            )?;
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交应用命令事务：{error}"))?;
        Ok((task_id, false))
    }

    pub(crate) fn runtime_task(
        &self,
        workspace_scope: &str,
        task_id: &str,
    ) -> Result<NativeRuntimeTask, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        read_native_runtime_task(&connection, workspace_scope, task_id)
    }

    pub(crate) fn resolve_operation_trace_id(
        &self,
        workspace_scope: &str,
        task_id: Option<&str>,
        supplied_trace_id: Option<&str>,
    ) -> Result<String, String> {
        if let Some(trace_id) = supplied_trace_id.filter(|value| !value.trim().is_empty()) {
            return Ok(crate::trace::validate_trace_id(trace_id)?.to_string());
        }
        if let Some(trace_id) = task_id
            .and_then(|task_id| self.runtime_task(workspace_scope, task_id).ok())
            .and_then(|task| task.trace_id)
            .filter(|value| !value.trim().is_empty())
        {
            crate::trace::validate_trace_id(&trace_id)?;
            return Ok(trace_id);
        }
        Ok(crate::trace::new_trace_id())
    }

    pub(crate) fn ensure_runtime_task_authorized(
        &self,
        workspace_scope: &str,
        task_id: &str,
        capability_ids: &[&str],
        operations: &[&str],
        vault_id: Option<&str>,
        allowed_states: &[&str],
    ) -> Result<NativeRuntimeTask, String> {
        let task = self.runtime_task(workspace_scope, task_id)?;
        if !allowed_states.contains(&task.state.as_str()) {
            return Err(format!("原生任务状态 {} 不允许执行当前操作", task.state));
        }
        let capabilities = task
            .payload
            .get("capabilityIds")
            .and_then(Value::as_array)
            .ok_or_else(|| "原生任务缺少能力范围".to_string())?;
        if !capability_ids.iter().any(|required| {
            capabilities
                .iter()
                .filter_map(Value::as_str)
                .any(|actual| actual == *required)
        }) {
            return Err("原生任务没有当前 Obsidian 操作所需能力".to_string());
        }
        let operation = task
            .payload
            .get("operation")
            .and_then(Value::as_str)
            .ok_or_else(|| "原生任务缺少操作类型".to_string())?;
        if !operations.contains(&operation) {
            return Err(format!("原生任务操作 {operation} 与当前执行器不匹配"));
        }
        if let Some(expected_vault_id) = vault_id {
            let scoped_vault_id = task
                .payload
                .get("vaultId")
                .and_then(Value::as_str)
                .ok_or_else(|| "原生任务缺少 Vault 范围".to_string())?;
            if scoped_vault_id != expected_vault_id && scoped_vault_id != "all" {
                return Err("原生任务 Vault 范围与目标知识库不一致".to_string());
            }
        }
        Ok(task)
    }

    pub(crate) fn list_runtime_tasks(
        &self,
        workspace_scope: &str,
        state: Option<&str>,
        limit: usize,
    ) -> Result<Vec<NativeRuntimeTask>, String> {
        if state.is_some_and(|value| !valid_runtime_task_state(value)) {
            return Err("任务状态筛选无效".to_string());
        }
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let mut tasks = Vec::new();
        let max = limit.clamp(1, 1000) as i64;
        let sql = if state.is_some() {
            "SELECT id, state, title, trace_id, payload, created_at, updated_at
             FROM runtime_tasks WHERE workspace_scope=?1 AND state=?2
             ORDER BY updated_at DESC LIMIT ?3"
        } else {
            "SELECT id, state, title, trace_id, payload, created_at, updated_at
             FROM runtime_tasks WHERE workspace_scope=?1
             ORDER BY updated_at DESC LIMIT ?2"
        };
        let mut statement = connection
            .prepare(sql)
            .map_err(|error| format!("无法准备原生任务列表：{error}"))?;
        if let Some(state) = state {
            let rows = statement
                .query_map(
                    params![workspace_scope, state, max],
                    map_native_runtime_task,
                )
                .map_err(|error| format!("无法读取原生任务列表：{error}"))?;
            tasks.extend(rows.filter_map(Result::ok));
        } else {
            let rows = statement
                .query_map(params![workspace_scope, max], map_native_runtime_task)
                .map_err(|error| format!("无法读取原生任务列表：{error}"))?;
            tasks.extend(rows.filter_map(Result::ok));
        }
        Ok(tasks)
    }

    pub(crate) fn transition_native_runtime_task(
        &self,
        workspace_scope: &str,
        task_id: &str,
        target_state: &str,
        progress: u8,
        detail: &str,
        checkpoint: Option<&Value>,
    ) -> Result<NativeRuntimeTask, String> {
        if !valid_runtime_task_state(target_state) {
            return Err("目标任务状态无效".to_string());
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始任务状态事务：{error}"))?;
        let current = read_native_runtime_task(&transaction, workspace_scope, task_id)?;
        if !crate::task_runtime::valid_task_transition(&current.state, target_state) {
            return Err(format!(
                "不允许任务从 {} 转换为 {target_state}",
                current.state
            ));
        }
        let now = Utc::now().to_rfc3339();
        let current_state = current.state.clone();
        let title = current.title.clone();
        let trace_id = current
            .trace_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(crate::trace::new_trace_id);
        crate::trace::validate_trace_id(&trace_id)?;
        let created_at = current.created_at.clone();
        let mut payload = current.payload;
        let object = payload
            .as_object_mut()
            .ok_or_else(|| "原生任务负载不是 JSON 对象".to_string())?;
        object.insert("state".to_string(), Value::String(target_state.to_string()));
        object.insert("progress".to_string(), Value::from(progress));
        object.insert("updatedAt".to_string(), Value::String(now.clone()));
        object.insert("traceId".to_string(), Value::String(trace_id.clone()));
        if !detail.trim().is_empty() {
            object.insert(
                "result".to_string(),
                Value::String(detail.chars().take(4000).collect()),
            );
        }
        let serialized = serde_json::to_string(&payload)
            .map_err(|error| format!("无法序列化任务状态：{error}"))?;
        transaction
            .execute(
                "UPDATE runtime_tasks SET state=?3, trace_id=?4, payload=?5, updated_at=?6
                 WHERE workspace_scope=?1 AND id=?2",
                params![
                    workspace_scope,
                    task_id,
                    target_state,
                    trace_id,
                    serialized,
                    now
                ],
            )
            .map_err(|error| format!("无法更新任务状态：{error}"))?;
        transaction
            .execute(
                "UPDATE runtime_task_attempts SET finished_at=?3
                 WHERE workspace_scope=?1 AND task_id=?2 AND finished_at IS NULL",
                params![workspace_scope, task_id, now],
            )
            .map_err(|error| format!("无法结束任务尝试：{error}"))?;
        transaction
            .execute(
                "INSERT INTO runtime_task_attempts
                 (id, workspace_scope, task_id, state, detail, started_at, finished_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    Uuid::new_v4().to_string(),
                    workspace_scope,
                    task_id,
                    target_state,
                    detail.chars().take(1000).collect::<String>(),
                    now,
                    if matches!(target_state, "succeeded" | "failed" | "cancelled") {
                        Some(now.clone())
                    } else {
                        None
                    },
                ],
            )
            .map_err(|error| format!("无法记录任务状态转换：{error}"))?;
        transaction
            .execute(
                "INSERT INTO runtime_task_transitions
                 (id, workspace_scope, task_id, from_state, to_state, detail, checkpoint_json, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    Uuid::new_v4().to_string(),
                    workspace_scope,
                    task_id,
                    current_state,
                    target_state,
                    detail.chars().take(2000).collect::<String>(),
                    serde_json::to_string(checkpoint.unwrap_or(&Value::Null))
                        .map_err(|error| format!("无法序列化转换检查点：{error}"))?,
                    now,
                ],
            )
            .map_err(|error| format!("无法保存任务状态转换：{error}"))?;
        if let Some(checkpoint) = checkpoint.filter(|value| value.is_object()) {
            let checkpoint_id = checkpoint
                .get("id")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            let checkpoint_json = serde_json::to_string(checkpoint)
                .map_err(|error| format!("无法序列化任务检查点：{error}"))?;
            let sequence = transaction
                .query_row(
                    "SELECT COALESCE(MAX(sequence), -1) + 1 FROM runtime_task_checkpoints
                     WHERE workspace_scope=?1 AND task_id=?2",
                    params![workspace_scope, task_id],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|error| format!("无法生成任务检查点序号：{error}"))?;
            let checkpoint_state = if matches!(target_state, "failed" | "cancelled") {
                "failed"
            } else if target_state == "succeeded" {
                "completed"
            } else {
                "running"
            };
            transaction
                .execute(
                    "INSERT INTO runtime_task_checkpoints
                     (workspace_scope, task_id, checkpoint_id, sequence, state, payload,
                      payload_hash, created_at, updated_at, completed_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9)
                     ON CONFLICT(workspace_scope, task_id, checkpoint_id) DO UPDATE SET
                       state=excluded.state, payload=excluded.payload,
                       payload_hash=excluded.payload_hash, updated_at=excluded.updated_at,
                       completed_at=excluded.completed_at",
                    params![
                        workspace_scope,
                        task_id,
                        checkpoint_id,
                        sequence,
                        checkpoint_state,
                        checkpoint_json,
                        format!("{:x}", Sha256::digest(checkpoint_json.as_bytes())),
                        now,
                        if checkpoint_state == "completed" {
                            Some(now.clone())
                        } else {
                            None
                        },
                    ],
                )
                .map_err(|error| format!("无法保存任务检查点：{error}"))?;
        }
        let event = OperationEvent {
            id: Uuid::new_v4().to_string(),
            task_id: Some(task_id.to_string()),
            trace_id: Some(trace_id.clone()),
            event_type: "task.state_changed".to_string(),
            state: target_state.to_string(),
            created_at: now.clone(),
            vault_id: payload
                .get("vaultId")
                .and_then(Value::as_str)
                .map(str::to_string),
            relative_path: None,
            detail: detail.chars().take(2000).collect(),
        };
        insert_operation_event_in_transaction(&transaction, &event)
            .map_err(|error| format!("无法保存任务状态审计事件：{error}"))?;
        crate::trace::record_trace_event_in_connection(
            &transaction,
            workspace_scope,
            &crate::trace::TraceEventRecord {
                trace_id: &trace_id,
                entity_kind: "runtime_task",
                entity_id: task_id,
                event_type: "task.state_changed",
                state: target_state,
                payload: &serde_json::json!({
                    "fromState": current_state,
                    "progress": progress,
                    "detail": detail,
                }),
                created_at: &now,
            },
        )?;
        let next = NativeRuntimeTask {
            id: task_id.to_string(),
            state: target_state.to_string(),
            title,
            trace_id: Some(trace_id),
            progress,
            payload,
            created_at,
            updated_at: now,
        };
        transaction
            .commit()
            .map_err(|error| format!("无法提交任务状态事务：{error}"))?;
        Ok(next)
    }

    pub fn ensure_vault_write_allowed(
        &self,
        workspace_scope: &str,
        vault_id: &str,
        relative_path: &str,
    ) -> Result<(), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let client_state = connection
            .query_row(
                "SELECT payload FROM workspace_snapshots WHERE workspace_scope=?1",
                [workspace_scope],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("无法读取 Vault 写入策略：{error}"))?
            .and_then(|value| serde_json::from_str::<Value>(&value).ok())
            .and_then(|snapshot| snapshot.get("clientState").cloned())
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        evaluate_vault_write_policy(&client_state, vault_id, relative_path)
    }

    pub fn list_native_operation_events(
        &self,
        limit: usize,
    ) -> Result<Vec<OperationEvent>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT payload FROM operation_events
                 ORDER BY created_at DESC LIMIT ?1",
            )
            .map_err(|error| format!("无法查询原生操作日志：{error}"))?;
        let rows = statement
            .query_map([limit.clamp(1, 1000) as i64], |row| row.get::<_, String>(0))
            .map_err(|error| format!("无法读取原生操作日志：{error}"))?;
        let mut events = rows
            .filter_map(Result::ok)
            .filter_map(|payload| serde_json::from_str::<OperationEvent>(&payload).ok())
            .collect::<Vec<_>>();
        events.reverse();
        Ok(events)
    }

    pub fn rebuild_index_for_vault_with_cancellation<F>(
        &self,
        vault_id: &str,
        is_cancelled: &F,
    ) -> Result<IndexBuildResult, String>
    where
        F: Fn() -> bool,
    {
        let (_, root) = resolve_vault_for_runtime(vault_id)?;
        let mut markdown = Vec::new();
        let mut attachments = 0;
        collect_files_for_runtime_with_cancellation(
            &root,
            &mut markdown,
            &mut attachments,
            is_cancelled,
        )?;
        ensure_index_not_cancelled(is_cancelled)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始索引事务：{error}"))?;
        transaction
            .execute("DELETE FROM note_fts WHERE vault_id=?1", [vault_id])
            .map_err(|error| format!("无法清理全文索引：{error}"))?;
        transaction
            .execute("DELETE FROM note_lexical_fts WHERE vault_id=?1", [vault_id])
            .map_err(|error| format!("无法清理中文词法索引：{error}"))?;
        transaction
            .execute(
                "DELETE FROM note_feature_vectors WHERE vault_id=?1",
                [vault_id],
            )
            .map_err(|error| format!("无法清理本地特征向量：{error}"))?;
        transaction
            .execute("DELETE FROM note_index WHERE vault_id=?1", [vault_id])
            .map_err(|error| format!("无法清理笔记索引：{error}"))?;

        let mut indexed_notes = 0;
        let mut skipped_notes = 0;
        for path in markdown {
            ensure_index_not_cancelled(is_cancelled)?;
            match prepare_note_index(&root, &path).and_then(|note| {
                note.map(|note| upsert_prepared_note_index(&transaction, vault_id, &note))
                    .transpose()
            }) {
                Ok(Some(())) => indexed_notes += 1,
                Ok(None) | Err(_) => skipped_notes += 1,
            }
        }
        ensure_index_not_cancelled(is_cancelled)?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交索引事务：{error}"))?;
        Ok(IndexBuildResult {
            vault_id: vault_id.to_string(),
            indexed_notes,
            skipped_notes,
            completed_at: Utc::now().to_rfc3339(),
        })
    }

    pub(crate) fn enqueue_vault_index_path(
        &self,
        vault_id: &str,
        root: &Path,
        path: &Path,
    ) -> Result<(), String> {
        self.enqueue_vault_index_path_inner(vault_id, root, path, None)
    }

    pub(crate) fn enqueue_vault_index_path_with_trace(
        &self,
        vault_id: &str,
        root: &Path,
        path: &Path,
        trace_id: &str,
    ) -> Result<(), String> {
        self.enqueue_vault_index_path_inner(vault_id, root, path, Some(trace_id))
    }

    fn enqueue_vault_index_path_inner(
        &self,
        vault_id: &str,
        root: &Path,
        path: &Path,
        inherited_trace_id: Option<&str>,
    ) -> Result<(), String> {
        let relative_path = normalized_index_relative_path(root, path)?;
        validate_index_relative_path(&relative_path)?;
        let canonical_root = canonical_index_root(root)?;
        let change_kind = match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_file() => "upsert",
            Ok(_) => "delete",
            Err(error) if error.kind() == ErrorKind::NotFound => "delete",
            Err(error) => return Err(format!("无法读取索引变更文件：{error}")),
        };
        let root_text = strict_path_text(&canonical_root, "Vault 根目录")?;
        let now = Utc::now();
        let generated_trace_id;
        let (trace_id, replace_existing_trace) = if let Some(trace_id) = inherited_trace_id {
            (crate::trace::validate_trace_id(trace_id)?, true)
        } else {
            generated_trace_id = crate::trace::new_trace_id();
            (generated_trace_id.as_str(), false)
        };
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        ensure_registered_vault_root(&connection, vault_id, &canonical_root)?;
        enqueue_vault_index_change_in_connection(
            &connection,
            vault_id,
            root_text,
            &relative_path,
            change_kind,
            trace_id,
            replace_existing_trace,
            now.timestamp_millis() + VAULT_INDEX_DEBOUNCE_MS,
            &now.to_rfc3339(),
        )?;
        Ok(())
    }

    pub(crate) fn recover_vault_index_changes(&self) -> Result<usize, String> {
        let now = Utc::now();
        let now_text = now.to_rfc3339();
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始 Vault 索引恢复事务：{error}"))?;
        let recovered = transaction
            .execute(
                "UPDATE vault_index_changes
                 SET state='pending', available_at_ms=?1, claimed_at_ms=NULL, updated_at=?2
                 WHERE state='processing' AND attempt_count < ?3",
                params![now.timestamp_millis(), now_text, VAULT_INDEX_MAX_ATTEMPTS,],
            )
            .map_err(|error| format!("无法恢复中断的 Vault 索引任务：{error}"))?;
        let dead_lettered = dead_letter_exhausted_vault_index_changes(
            &transaction,
            "processing",
            "应用退出前索引任务未完成",
            "startup_recovery",
            &now_text,
        )?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交 Vault 索引恢复事务：{error}"))?;
        Ok(recovered + dead_lettered)
    }

    pub(crate) fn claim_vault_index_changes(
        &self,
        limit: usize,
    ) -> Result<Vec<ClaimedVaultIndexChange>, String> {
        let now = Utc::now();
        let now_ms = now.timestamp_millis();
        let now_text = now.to_rfc3339();
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始 Vault 索引认领事务：{error}"))?;
        dead_letter_exhausted_vault_index_changes(
            &transaction,
            "pending",
            "Vault 索引任务超过最大重试次数",
            "claim_sweep",
            &now_text,
        )?;
        let candidates = {
            let mut statement = transaction
                .prepare(
                    "SELECT id, vault_id, canonical_root, relative_path, generation, attempt_count, trace_id
                     FROM vault_index_changes
                     WHERE state='pending' AND attempt_count < ?1 AND available_at_ms <= ?2
                     ORDER BY available_at_ms, id LIMIT ?3",
                )
                .map_err(|error| format!("无法准备 Vault 索引认领查询：{error}"))?;
            let rows = statement
                .query_map(
                    params![VAULT_INDEX_MAX_ATTEMPTS, now_ms, limit.clamp(1, 128) as i64],
                    |row| {
                        Ok(ClaimedVaultIndexChange {
                            id: row.get(0)?,
                            vault_id: row.get(1)?,
                            canonical_root: PathBuf::from(row.get::<_, String>(2)?),
                            relative_path: row.get(3)?,
                            generation: row.get(4)?,
                            attempt_count: row.get::<_, i64>(5)? + 1,
                            trace_id: row.get(6)?,
                        })
                    },
                )
                .map_err(|error| format!("无法读取待处理 Vault 索引任务：{error}"))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("无法解析待处理 Vault 索引任务：{error}"))?
        };
        let mut claimed = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let changed = transaction
                .execute(
                    "UPDATE vault_index_changes
                     SET state='processing', attempt_count=attempt_count+1,
                         claimed_at_ms=?1, updated_at=?2
                     WHERE id=?3 AND generation=?4 AND state='pending'
                       AND attempt_count < ?5 AND available_at_ms <= ?1",
                    params![
                        now_ms,
                        now_text,
                        candidate.id,
                        candidate.generation,
                        VAULT_INDEX_MAX_ATTEMPTS
                    ],
                )
                .map_err(|error| format!("无法认领 Vault 索引任务：{error}"))?;
            if changed == 1 {
                crate::trace::record_trace_event_in_connection(
                    &transaction,
                    DEFAULT_LOCAL_WORKSPACE_SCOPE,
                    &crate::trace::TraceEventRecord {
                        trace_id: &candidate.trace_id,
                        entity_kind: "index_change",
                        entity_id: &format!("{}:{}", candidate.id, candidate.generation),
                        event_type: "index.claimed",
                        state: "processing",
                        payload: &serde_json::json!({
                            "vaultId": candidate.vault_id,
                            "relativePath": candidate.relative_path,
                            "attemptCount": candidate.attempt_count,
                        }),
                        created_at: &now_text,
                    },
                )?;
                claimed.push(candidate);
            }
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交 Vault 索引认领事务：{error}"))?;
        Ok(claimed)
    }

    pub(crate) fn apply_claimed_vault_index_change(
        &self,
        change: &ClaimedVaultIndexChange,
        current_root: &Path,
    ) -> Result<Option<AppliedVaultIndexChange>, String> {
        let canonical_root = canonical_index_root(current_root)?;
        if canonical_root != change.canonical_root {
            return Err("Vault 根目录已变化，拒绝应用旧索引任务".to_string());
        }
        let target = resolve_index_target(&canonical_root, &change.relative_path)?;
        let prepared = match fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.file_type().is_file() => {
                prepare_note_index(&canonical_root, &target)?
            }
            Ok(_) => None,
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => return Err(format!("无法读取待索引笔记：{error}")),
        };
        if prepared
            .as_ref()
            .is_some_and(|note| note.relative_path != change.relative_path)
        {
            return Err("索引任务路径规范化后发生变化".to_string());
        }
        let root_text = strict_path_text(&canonical_root, "Vault 根目录")?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始 Vault 索引提交事务：{error}"))?;
        let owns_change = transaction
            .query_row(
                "SELECT 1 FROM vault_index_changes
                 WHERE id=?1 AND vault_id=?2 AND canonical_root=?3 AND relative_path=?4
                   AND generation=?5 AND state='processing'",
                params![
                    change.id,
                    change.vault_id,
                    root_text,
                    change.relative_path,
                    change.generation
                ],
                |_| Ok(()),
            )
            .optional()
            .map_err(|error| format!("无法校验 Vault 索引任务所有权：{error}"))?
            .is_some();
        if !owns_change {
            return Ok(None);
        }
        ensure_registered_vault_root(&transaction, &change.vault_id, &canonical_root)?;
        let change_kind = if let Some(note) = prepared.as_ref() {
            upsert_prepared_note_index(&transaction, &change.vault_id, note)?;
            "upsert"
        } else {
            delete_note_index_in_transaction(
                &transaction,
                &change.vault_id,
                &change.relative_path,
            )?;
            "delete"
        };
        let event = OperationEvent {
            id: Uuid::new_v4().to_string(),
            task_id: None,
            trace_id: Some(change.trace_id.clone()),
            event_type: "vault.note.index".to_string(),
            state: "success".to_string(),
            created_at: Utc::now().to_rfc3339(),
            vault_id: Some(change.vault_id.clone()),
            relative_path: Some(change.relative_path.clone()),
            detail: format!("Vault 索引队列已完成 {change_kind}"),
        };
        insert_operation_event_in_transaction(&transaction, &event)?;
        crate::trace::record_trace_event_in_connection(
            &transaction,
            DEFAULT_LOCAL_WORKSPACE_SCOPE,
            &crate::trace::TraceEventRecord {
                trace_id: &change.trace_id,
                entity_kind: "index_change",
                entity_id: &format!("{}:{}", change.id, change.generation),
                event_type: "index.completed",
                state: "succeeded",
                payload: &serde_json::json!({
                    "vaultId": change.vault_id,
                    "relativePath": change.relative_path,
                    "changeKind": change_kind,
                }),
                created_at: &event.created_at,
            },
        )?;
        let deleted = transaction
            .execute(
                "DELETE FROM vault_index_changes
                 WHERE id=?1 AND generation=?2 AND state='processing'",
                params![change.id, change.generation],
            )
            .map_err(|error| format!("无法完成 Vault 索引任务：{error}"))?;
        if deleted != 1 {
            return Err("Vault 索引任务在提交期间被新事件替换".to_string());
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交 Vault 索引变更：{error}"))?;
        Ok(Some(AppliedVaultIndexChange {
            vault_id: change.vault_id.clone(),
            relative_path: change.relative_path.clone(),
            change_kind: change_kind.to_string(),
        }))
    }

    pub(crate) fn fail_claimed_vault_index_change(
        &self,
        change: &ClaimedVaultIndexChange,
        error: &str,
    ) -> Result<VaultIndexFailureOutcome, String> {
        let terminal = change.attempt_count >= VAULT_INDEX_MAX_ATTEMPTS;
        let retry_delay = VAULT_INDEX_RETRY_BASE_MS
            .saturating_mul(1_i64 << change.attempt_count.saturating_sub(1).min(6));
        let now = Utc::now();
        let error = error.chars().take(2_000).collect::<String>();
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|db_error| format!("无法开始 Vault 索引失败事务：{db_error}"))?;
        let updated = transaction
            .execute(
                "UPDATE vault_index_changes
                 SET state=?1, available_at_ms=?2, claimed_at_ms=NULL,
                     last_error=?3, updated_at=?4
                 WHERE id=?5 AND generation=?6 AND state='processing'",
                params![
                    if terminal { "dead_letter" } else { "pending" },
                    now.timestamp_millis() + retry_delay,
                    error,
                    now.to_rfc3339(),
                    change.id,
                    change.generation
                ],
            )
            .map_err(|db_error| format!("无法记录 Vault 索引失败：{db_error}"))?
            == 1;
        if updated {
            crate::trace::record_trace_event_in_connection(
                &transaction,
                DEFAULT_LOCAL_WORKSPACE_SCOPE,
                &crate::trace::TraceEventRecord {
                    trace_id: &change.trace_id,
                    entity_kind: "index_change",
                    entity_id: &format!("{}:{}", change.id, change.generation),
                    event_type: if terminal {
                        "index.dead_lettered"
                    } else {
                        "index.retry_scheduled"
                    },
                    state: if terminal { "dead_letter" } else { "pending" },
                    payload: &serde_json::json!({
                        "vaultId": change.vault_id,
                        "relativePath": change.relative_path,
                        "attemptCount": change.attempt_count,
                        "error": error,
                    }),
                    created_at: &now.to_rfc3339(),
                },
            )?;
        }
        if updated && terminal {
            let event = OperationEvent {
                id: Uuid::new_v4().to_string(),
                task_id: None,
                trace_id: Some(change.trace_id.clone()),
                event_type: "vault.note.index".to_string(),
                state: "failed".to_string(),
                created_at: now.to_rfc3339(),
                vault_id: Some(change.vault_id.clone()),
                relative_path: Some(change.relative_path.clone()),
                detail: format!("Vault 索引任务重试耗尽：{error}"),
            };
            insert_operation_event_in_transaction(&transaction, &event)?;
        }
        transaction
            .commit()
            .map_err(|db_error| format!("无法提交 Vault 索引失败状态：{db_error}"))?;
        Ok(VaultIndexFailureOutcome { updated, terminal })
    }

    pub(crate) fn reconcile_vault_index(
        &self,
        vault: &VaultDescriptor,
    ) -> Result<VaultIndexReconcileResult, String> {
        if vault.connection_state != "connected" {
            return Err("只能校准已连接的 Vault".to_string());
        }
        let canonical_root = canonical_index_root(Path::new(&vault.path))?;
        let root_text = strict_path_text(&canonical_root, "Vault 根目录")?;
        let mut markdown = Vec::new();
        let mut attachments = 0;
        collect_files_for_runtime_with_cancellation(
            &canonical_root,
            &mut markdown,
            &mut attachments,
            &|| false,
        )?;
        let mut current_paths = HashSet::with_capacity(markdown.len());
        for path in markdown {
            let relative_path = normalized_index_relative_path(&canonical_root, &path)?;
            validate_index_relative_path(&relative_path)?;
            current_paths.insert(relative_path);
        }

        let now = Utc::now();
        let now_text = now.to_rfc3339();
        let now_ms = now.timestamp_millis();
        let reconcile_trace_id = crate::trace::new_trace_id();
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始 Vault 索引校准事务：{error}"))?;
        ensure_registered_vault_root(&transaction, &vault.id, &canonical_root)?;
        let known_paths = {
            let mut statement = transaction
                .prepare(
                    "SELECT relative_path FROM note_index WHERE vault_id=?1
                     UNION
                     SELECT relative_path FROM vault_index_changes WHERE vault_id=?1",
                )
                .map_err(|error| format!("无法准备 Vault 索引校准查询：{error}"))?;
            let rows = statement
                .query_map([&vault.id], |row| row.get::<_, String>(0))
                .map_err(|error| format!("无法读取 Vault 索引校准基线：{error}"))?;
            rows.collect::<Result<HashSet<_>, _>>()
                .map_err(|error| format!("无法解析 Vault 索引校准基线：{error}"))?
        };
        for relative_path in &current_paths {
            enqueue_vault_index_change_in_connection(
                &transaction,
                &vault.id,
                root_text,
                relative_path,
                "upsert",
                &reconcile_trace_id,
                false,
                now_ms,
                &now_text,
            )?;
        }
        let mut queued_deletes = 0;
        for relative_path in known_paths.difference(&current_paths) {
            let normalized = normalize_queued_relative_path(relative_path)?;
            validate_index_relative_path(&normalized)?;
            enqueue_vault_index_change_in_connection(
                &transaction,
                &vault.id,
                root_text,
                &normalized,
                "delete",
                &reconcile_trace_id,
                false,
                now_ms,
                &now_text,
            )?;
            queued_deletes += 1;
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交 Vault 索引校准事务：{error}"))?;
        Ok(VaultIndexReconcileResult {
            queued_upserts: current_paths.len(),
            queued_deletes,
        })
    }

    fn health(&self, workspace_scope: &str) -> Result<DatabaseHealth, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let journal_mode = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .map_err(|error| format!("无法读取 WAL 状态：{error}"))?;
        let integrity = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(|error| format!("无法执行完整性检查：{error}"))?;
        let schema_version = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| format!("无法读取 schema 版本：{error}"))?;
        let workspace_snapshot = connection
            .query_row(
                "SELECT payload FROM workspace_snapshots WHERE workspace_scope=?1",
                [workspace_scope],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("无法读取本地工作区统计：{error}"))?
            .and_then(|payload| serde_json::from_str::<Value>(&payload).ok());
        let workspace_count = |key: &str| {
            workspace_snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.get(key))
                .and_then(Value::as_array)
                .map(|items| items.len() as i64)
                .unwrap_or(0)
        };
        Ok(DatabaseHealth {
            path: self.path.to_string_lossy().into_owned(),
            journal_mode,
            integrity,
            schema_version,
            task_count: workspace_count("tasks"),
            approval_count: workspace_count("approvals"),
            message_count: workspace_count("messages"),
            operation_event_count: table_count(&connection, "operation_events")?,
            indexed_note_count: table_count(&connection, "note_index")?,
        })
    }

    fn backup(&self) -> Result<DatabaseBackupResult, String> {
        let created_at = Utc::now();
        let backup_dir = self.backup_dir()?;
        fs::create_dir_all(&backup_dir).map_err(|error| format!("无法创建备份目录：{error}"))?;
        let filename = format!("yunspire-{}.sqlite", created_at.format("%Y%m%d-%H%M%S"));
        let backup_path = backup_dir.join(filename);
        let source = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let mut destination = Connection::open(&backup_path)
            .map_err(|error| format!("无法创建 SQLite 备份：{error}"))?;
        let backup = rusqlite::backup::Backup::new(&source, &mut destination)
            .map_err(|error| format!("无法初始化 SQLite 在线备份：{error}"))?;
        if let Err(error) = backup.run_to_completion(64, std::time::Duration::from_millis(10), None)
        {
            drop(backup);
            drop(destination);
            let _ = fs::remove_file(&backup_path);
            return Err(format!("SQLite 在线备份失败：{error}"));
        }
        drop(backup);
        drop(destination);
        #[cfg(unix)]
        fs::set_permissions(&backup_path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("无法限制备份文件权限：{error}"))?;
        let byte_length = fs::metadata(&backup_path)
            .map_err(|error| format!("无法验证备份文件：{error}"))?
            .len();
        Ok(DatabaseBackupResult {
            path: backup_path.to_string_lossy().into_owned(),
            byte_length,
            created_at: created_at.to_rfc3339(),
        })
    }

    pub(crate) fn backup_for_runtime(&self) -> Result<DatabaseBackupResult, String> {
        self.backup()
    }

    fn backup_dir(&self) -> Result<PathBuf, String> {
        self.path
            .parent()
            .map(|parent| parent.join("backups"))
            .ok_or_else(|| "无法定位数据库备份目录".to_string())
    }

    fn validate_backup_path(&self, requested_path: &str) -> Result<PathBuf, String> {
        let backup_dir = self.backup_dir()?;
        fs::create_dir_all(&backup_dir).map_err(|error| format!("无法创建备份目录：{error}"))?;
        let canonical_dir = backup_dir
            .canonicalize()
            .map_err(|error| format!("无法规范化备份目录：{error}"))?;
        let requested = PathBuf::from(requested_path.trim());
        let canonical = requested
            .canonicalize()
            .map_err(|error| format!("无法读取指定备份：{error}"))?;
        if canonical.parent() != Some(canonical_dir.as_path())
            || canonical.extension().and_then(|value| value.to_str()) != Some("sqlite")
            || !canonical.is_file()
        {
            return Err("只能恢复 Yunspire 本地备份目录中的 SQLite 备份".to_string());
        }
        Ok(canonical)
    }

    fn preflight_restore(&self, requested_path: &str) -> Result<DatabaseRestorePreflight, String> {
        let path = self.validate_backup_path(requested_path)?;
        inspect_backup(&path)
    }

    fn list_backups(&self) -> Result<Vec<DatabaseBackupInfo>, String> {
        let backup_dir = self.backup_dir()?;
        fs::create_dir_all(&backup_dir).map_err(|error| format!("无法创建备份目录：{error}"))?;
        let mut backups = fs::read_dir(&backup_dir)
            .map_err(|error| format!("无法读取备份目录：{error}"))?
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.path().extension().and_then(|value| value.to_str()) == Some("sqlite")
                    && entry.file_name().to_string_lossy().starts_with("yunspire-")
            })
            .filter_map(|entry| {
                let preflight = inspect_backup(&entry.path()).ok()?;
                Some(DatabaseBackupInfo {
                    path: preflight.path,
                    file_name: preflight.file_name,
                    byte_length: preflight.byte_length,
                    modified_at: entry
                        .metadata()
                        .ok()?
                        .modified()
                        .ok()
                        .map(chrono::DateTime::<Utc>::from)?
                        .to_rfc3339(),
                    schema_version: preflight.schema_version,
                    integrity: preflight.integrity,
                })
            })
            .collect::<Vec<_>>();
        backups.sort_by(|left, right| right.modified_at.cmp(&left.modified_at));
        Ok(backups)
    }

    fn restore(&self, requested_path: &str) -> Result<DatabaseRestoreResult, String> {
        let preflight = self.preflight_restore(requested_path)?;
        if !preflight.compatible {
            return Err(format!("备份恢复预检未通过：{}", preflight.reason));
        }
        let source_path = PathBuf::from(&preflight.path);
        let backup_dir = self.backup_dir()?;
        let restored_at = Utc::now();
        let safety_path = backup_dir.join(format!(
            "yunspire-before-restore-{}.sqlite",
            restored_at.format("%Y%m%d-%H%M%S-%3f")
        ));
        let mut destination = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let mut safety = Connection::open(&safety_path)
            .map_err(|error| format!("无法创建恢复前安全备份：{error}"))?;
        copy_database(&destination, &mut safety)
            .map_err(|error| format!("无法创建恢复前安全备份：{error}"))?;
        drop(safety);
        #[cfg(unix)]
        fs::set_permissions(&safety_path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("无法限制恢复前备份权限：{error}"))?;

        let restore_result = (|| -> Result<(i64, String), String> {
            let source =
                Connection::open_with_flags(&source_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .map_err(|error| format!("无法以只读方式打开恢复来源：{error}"))?;
            copy_database(&source, &mut destination)?;
            drop(source);
            run_migrations(&destination)?;
            restore_database_runtime_configuration(&destination)
                .map_err(|error| format!("无法恢复 SQLite 运行参数：{error}"))?;
            let integrity = database_integrity(&destination)?;
            if integrity != "ok" {
                return Err(format!("恢复后的数据库完整性检查失败：{integrity}"));
            }
            let schema_version = database_schema_version(&destination)?;
            Ok((schema_version, integrity))
        })();

        let (schema_version, integrity) = match restore_result {
            Ok(result) => result,
            Err(error) => {
                let safety_source =
                    Connection::open_with_flags(&safety_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                        .map_err(|rollback_error| {
                            format!("{error}；同时无法打开恢复前安全备份：{rollback_error}")
                        })?;
                copy_database(&safety_source, &mut destination).map_err(|rollback_error| {
                    format!("{error}；自动回滚也失败：{rollback_error}")
                })?;
                return Err(format!("{error}；已自动回滚到恢复前状态"));
            }
        };
        Ok(DatabaseRestoreResult {
            restored_from: preflight.path,
            safety_backup: safety_path.to_string_lossy().into_owned(),
            schema_version,
            integrity,
            restored_at: restored_at.to_rfc3339(),
        })
    }

    pub(crate) fn restore_for_runtime(
        &self,
        requested_path: &str,
    ) -> Result<DatabaseRestoreResult, String> {
        self.restore(requested_path)
    }

    fn optimization_evidence(
        &self,
        workspace_scope: &str,
        limit: usize,
    ) -> Result<OptimizationEvidenceBatch, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let cursor = connection
            .query_row(
                "SELECT revision, last_occurred_at, last_event_id
                 FROM optimization_cursors WHERE workspace_scope=?1",
                [workspace_scope],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(|error| format!("无法读取后台优化游标：{error}"))?;
        let requested = limit.clamp(1, 500);
        let mut statement = connection
            .prepare(
                "SELECT id, event_type, occurred_at, payload
                 FROM long_term_memory_events e
                 WHERE e.workspace_scope=?1 AND e.state='committed'
                   AND (e.occurred_at>?2 OR (e.occurred_at=?2 AND e.id>?3))
                   AND NOT EXISTS (
                     SELECT 1 FROM long_term_memory_governance g
                     WHERE g.workspace_scope=e.workspace_scope AND g.memory_id=e.id
                       AND (g.status!='active' OR (g.expires_at IS NOT NULL AND g.expires_at<=?4))
                   )
                 ORDER BY e.occurred_at, e.id LIMIT ?5",
            )
            .map_err(|error| format!("无法准备后台优化证据查询：{error}"))?;
        let rows = statement
            .query_map(
                params![
                    workspace_scope,
                    cursor.1,
                    cursor.2,
                    Utc::now().to_rfc3339(),
                    (requested + 1) as i64
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .map_err(|error| format!("无法读取后台优化证据：{error}"))?;
        let mut events = Vec::new();
        let mut next_occurred_at = cursor.1.clone();
        let mut next_event_id = cursor.2.clone();
        let mut has_more = false;
        for row in rows.filter_map(Result::ok) {
            if events.len() >= requested {
                has_more = true;
                break;
            }
            let payload: Value = serde_json::from_str(&row.3)
                .map_err(|error| format!("优化证据 {} 的载荷损坏：{error}", row.0))?;
            let content = payload
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .chars()
                .take(6000)
                .collect::<String>();
            if content.trim().is_empty() {
                next_occurred_at = row.2;
                next_event_id = row.0;
                continue;
            }
            events.push(OptimizationEvidenceEvent {
                id: row.0.clone(),
                event_type: row.1,
                occurred_at: row.2.clone(),
                actor: payload
                    .get("actor")
                    .and_then(Value::as_str)
                    .unwrap_or("system")
                    .to_string(),
                content,
                metadata: payload
                    .get("metadata")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(serde_json::Map::new())),
            });
            next_occurred_at = row.2;
            next_event_id = row.0;
        }
        Ok(OptimizationEvidenceBatch {
            cursor_revision: cursor.0,
            cursor_occurred_at: cursor.1,
            cursor_event_id: cursor.2,
            next_occurred_at,
            next_event_id,
            events,
            has_more,
        })
    }

    fn create_optimization_candidate(
        &self,
        workspace_scope: &str,
        input: OptimizationCandidateInput,
    ) -> Result<OptimizationCandidateResult, String> {
        validate_optimization_candidate_input(&input)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始后台优化候选事务：{error}"))?;
        let (cursor_revision, cursor_occurred_at, cursor_event_id) = transaction
            .query_row(
                "SELECT revision, last_occurred_at, last_event_id
                 FROM optimization_cursors WHERE workspace_scope=?1",
                [workspace_scope],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(|error| format!("无法读取后台优化游标：{error}"))?;
        if cursor_revision != input.expected_cursor_revision {
            return Err("后台优化证据已被另一轮复盘领取，请重新读取增量证据".to_string());
        }
        if input.evidence_cursor_occurred_at < cursor_occurred_at
            || (input.evidence_cursor_occurred_at == cursor_occurred_at
                && input.evidence_cursor_event_id < cursor_event_id)
        {
            return Err("后台优化候选游标不能回退".to_string());
        }
        let base_version = transaction
            .query_row(
                "SELECT version FROM optimization_profiles WHERE workspace_scope=?1",
                [workspace_scope],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("无法读取当前优化版本：{error}"))?;
        let now = Utc::now().to_rfc3339();
        let rules_json = serde_json::to_string(&input.rules)
            .map_err(|error| format!("无法序列化优化规则：{error}"))?;
        let skill_hints_json = serde_json::to_string(&input.skill_hints)
            .map_err(|error| format!("无法序列化 Skill 优化提示：{error}"))?;
        let metrics_json = serde_json::to_string(&input.metrics)
            .map_err(|error| format!("无法序列化优化指标：{error}"))?;
        transaction
            .execute(
                "INSERT INTO optimization_candidates
                 (workspace_scope, id, base_version, candidate_version, state, summary,
                  rules_json, skill_hints_json, metrics_json, evidence_count,
                  evidence_occurred_at, evidence_event_id, created_at, evaluated_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, 'pending_evaluation', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL, ?13)",
                params![
                    workspace_scope,
                    input.id,
                    base_version,
                    base_version + 1,
                    input.summary.trim(),
                    rules_json,
                    skill_hints_json,
                    metrics_json,
                    input.evidence_count as i64,
                    input.evidence_cursor_occurred_at,
                    input.evidence_cursor_event_id,
                    now,
                    input.expires_at,
                ],
            )
            .map_err(|error| format!("无法保存后台优化候选：{error}"))?;
        let advanced = transaction
            .execute(
                "UPDATE optimization_cursors
                 SET revision=revision+1, last_occurred_at=?2, last_event_id=?3, updated_at=?4
                 WHERE workspace_scope=?1 AND revision=?5",
                params![
                    workspace_scope,
                    input.evidence_cursor_occurred_at,
                    input.evidence_cursor_event_id,
                    now,
                    input.expected_cursor_revision,
                ],
            )
            .map_err(|error| format!("无法推进后台优化游标：{error}"))?;
        if advanced != 1 {
            return Err("后台优化游标推进失败，候选没有提交".to_string());
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交后台优化候选：{error}"))?;
        Ok(OptimizationCandidateResult {
            id: input.id,
            base_version,
            candidate_version: base_version + 1,
            state: "pending_evaluation".to_string(),
            summary: input.summary.trim().to_string(),
            rules: input.rules,
            skill_hints: input.skill_hints,
            metrics: input.metrics,
            evidence_count: input.evidence_count,
            created_at: now,
            evaluated_at: None,
        })
    }

    fn evaluate_optimization_candidate(
        &self,
        workspace_scope: &str,
        candidate_id: &str,
    ) -> Result<OptimizationEvaluationResult, String> {
        if !valid_runtime_identifier(candidate_id, 160) {
            return Err("优化候选 ID 无效".to_string());
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始优化候选评估事务：{error}"))?;
        let row = transaction
            .query_row(
                "SELECT base_version, state, summary, rules_json, skill_hints_json,
                        evidence_count, expires_at
                 FROM optimization_candidates WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, candidate_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, Option<String>>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法读取优化候选：{error}"))?
            .ok_or_else(|| "优化候选不存在".to_string())?;
        if row.1 != "pending_evaluation" {
            return Err(format!("优化候选当前状态为 {}，不能重复评估", row.1));
        }
        let current_version = transaction
            .query_row(
                "SELECT version FROM optimization_profiles WHERE workspace_scope=?1",
                [workspace_scope],
                |value| value.get::<_, i64>(0),
            )
            .map_err(|error| format!("无法读取当前优化版本：{error}"))?;
        let mut checks = Vec::new();
        if row.0 != current_version {
            checks.push("候选基线版本已过期".to_string());
        }
        if row.5 < 2 {
            checks.push("证据数量少于 2 条".to_string());
        }
        if row.2.trim().is_empty() || row.2.chars().count() > 32_000 {
            checks.push("候选摘要为空或超过 32000 字".to_string());
        }
        let rules = serde_json::from_str::<Vec<String>>(&row.3)
            .map_err(|error| format!("优化规则载荷损坏：{error}"))?;
        if rules.is_empty() || rules.len() > 12 {
            checks.push("规则数量必须为 1 到 12 条".to_string());
        }
        if rules
            .iter()
            .any(|rule| contains_optimization_forbidden_instruction(rule))
        {
            checks.push("候选规则包含权限、设置或访问控制变更".to_string());
        }
        if serde_json::from_str::<Value>(&row.4)
            .ok()
            .filter(Value::is_object)
            .is_none()
        {
            checks.push("Skill 提示载荷不是 JSON 对象".to_string());
        }
        if let Some(expires_at) = &row.6 {
            if chrono::DateTime::parse_from_rfc3339(expires_at)
                .ok()
                .is_some_and(|value| value.with_timezone(&Utc) <= Utc::now())
            {
                checks.push("候选已过期".to_string());
            }
        }
        let passed = checks.is_empty();
        let state = if passed { "pending_review" } else { "rejected" };
        let evaluated_at = Utc::now().to_rfc3339();
        transaction
            .execute(
                "UPDATE optimization_candidates SET state=?3, evaluated_at=?4
                 WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, candidate_id, state, evaluated_at],
            )
            .map_err(|error| format!("无法更新优化候选评估状态：{error}"))?;
        transaction
            .execute(
                "INSERT INTO optimization_evaluations
                 (id, workspace_scope, candidate_id, state, checks_json, evaluated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    Uuid::new_v4().to_string(),
                    workspace_scope,
                    candidate_id,
                    state,
                    serde_json::to_string(&checks).unwrap_or_else(|_| "[]".to_string()),
                    evaluated_at,
                ],
            )
            .map_err(|error| format!("无法保存优化候选评估：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交优化候选评估：{error}"))?;
        Ok(OptimizationEvaluationResult {
            candidate_id: candidate_id.to_string(),
            state: state.to_string(),
            passed,
            checks,
            evaluated_at,
        })
    }

    fn load_optimization_profile(
        &self,
        workspace_scope: &str,
    ) -> Result<OptimizationProfileResult, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let row = connection
            .query_row(
                "SELECT version, candidate_id, guidance, rules_json, skill_hints_json, updated_at
                 FROM optimization_profiles WHERE workspace_scope=?1",
                [workspace_scope],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                },
            )
            .map_err(|error| format!("无法读取当前优化配置：{error}"))?;
        Ok(OptimizationProfileResult {
            version: row.0,
            candidate_id: row.1,
            guidance: row.2,
            rules: serde_json::from_str(&row.3).unwrap_or_default(),
            skill_hints: serde_json::from_str(&row.4)
                .unwrap_or_else(|_| Value::Object(serde_json::Map::new())),
            updated_at: row.5,
        })
    }

    fn apply_optimization_candidate(
        &self,
        workspace_scope: &str,
        candidate_id: &str,
    ) -> Result<OptimizationProfileResult, String> {
        if !valid_runtime_identifier(candidate_id, 160) {
            return Err("优化候选 ID 无效".to_string());
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始应用优化候选事务：{error}"))?;
        let candidate = transaction
            .query_row(
                "SELECT base_version, candidate_version, state, summary, rules_json, skill_hints_json
                 FROM optimization_candidates WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, candidate_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法读取待应用优化候选：{error}"))?
            .ok_or_else(|| "优化候选不存在".to_string())?;
        if candidate.2 != "pending_review" {
            return Err(format!(
                "优化候选当前状态为 {}，未通过独立评估",
                candidate.2
            ));
        }
        let current_version = transaction
            .query_row(
                "SELECT version FROM optimization_profiles WHERE workspace_scope=?1",
                [workspace_scope],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("无法读取当前优化版本：{error}"))?;
        if candidate.0 != current_version || candidate.1 != current_version + 1 {
            return Err("优化候选基线已变化，需要重新生成和评估".to_string());
        }
        let now = Utc::now().to_rfc3339();
        transaction
            .execute(
                "INSERT INTO optimization_profile_revisions
                 (workspace_scope, version, candidate_id, state, guidance, rules_json, skill_hints_json, created_at, rollback_target)
                 VALUES (?1, ?2, ?3, 'active', ?4, ?5, ?6, ?7, NULL)",
                params![workspace_scope, candidate.1, candidate_id, candidate.3, candidate.4, candidate.5, now],
            )
            .map_err(|error| format!("无法保存优化版本：{error}"))?;
        transaction
            .execute(
                "UPDATE optimization_profiles SET version=?2, candidate_id=?3, guidance=?4,
                 rules_json=?5, skill_hints_json=?6, updated_at=?7 WHERE workspace_scope=?1",
                params![
                    workspace_scope,
                    candidate.1,
                    candidate_id,
                    candidate.3,
                    candidate.4,
                    candidate.5,
                    now
                ],
            )
            .map_err(|error| format!("无法原子应用优化配置：{error}"))?;
        transaction
            .execute(
                "UPDATE optimization_candidates SET state='applied' WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, candidate_id],
            )
            .map_err(|error| format!("无法更新优化候选状态：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交优化配置：{error}"))?;
        drop(connection);
        self.load_optimization_profile(workspace_scope)
    }

    fn rollback_optimization_profile(
        &self,
        workspace_scope: &str,
        target_version: Option<i64>,
    ) -> Result<OptimizationProfileResult, String> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始优化回滚事务：{error}"))?;
        let current_version = transaction
            .query_row(
                "SELECT version FROM optimization_profiles WHERE workspace_scope=?1",
                [workspace_scope],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| format!("无法读取优化回滚版本：{error}"))?;
        let target = target_version.unwrap_or_else(|| current_version.saturating_sub(1));
        if target < 0 || target >= current_version {
            return Err("没有可回滚的上一版优化配置".to_string());
        }
        let revision = transaction
            .query_row(
                "SELECT candidate_id, guidance, rules_json, skill_hints_json
                 FROM optimization_profile_revisions
                 WHERE workspace_scope=?1 AND version=?2",
                params![workspace_scope, target],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("无法读取目标优化版本：{error}"))?
            .ok_or_else(|| "目标优化版本不存在".to_string())?;
        let now = Utc::now().to_rfc3339();
        let new_version = current_version + 1;
        transaction
            .execute(
                "INSERT INTO optimization_profile_revisions
                 (workspace_scope, version, candidate_id, state, guidance, rules_json, skill_hints_json, created_at, rollback_target)
                 VALUES (?1, ?2, ?3, 'rollback', ?4, ?5, ?6, ?7, ?8)",
                params![workspace_scope, new_version, revision.0, revision.1, revision.2, revision.3, now, target],
            )
            .map_err(|error| format!("无法保存回滚版本：{error}"))?;
        transaction
            .execute(
                "UPDATE optimization_profiles SET version=?2, candidate_id=?3, guidance=?4,
                 rules_json=?5, skill_hints_json=?6, updated_at=?7 WHERE workspace_scope=?1",
                params![
                    workspace_scope,
                    new_version,
                    revision.0,
                    revision.1,
                    revision.2,
                    revision.3,
                    now
                ],
            )
            .map_err(|error| format!("无法提交优化回滚：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交优化回滚事务：{error}"))?;
        drop(connection);
        self.load_optimization_profile(workspace_scope)
    }

    fn list_optimization_versions(
        &self,
        workspace_scope: &str,
        limit: usize,
    ) -> Result<Vec<OptimizationVersion>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let mut statement = connection
            .prepare(
                "SELECT version, candidate_id, state, guidance, created_at, rollback_target
                 FROM optimization_profile_revisions WHERE workspace_scope=?1
                 ORDER BY version DESC LIMIT ?2",
            )
            .map_err(|error| format!("无法准备优化版本查询：{error}"))?;
        let rows = statement
            .query_map(
                params![workspace_scope, limit.clamp(1, 100) as i64],
                |row| {
                    Ok(OptimizationVersion {
                        version: row.get(0)?,
                        candidate_id: row.get(1)?,
                        state: row.get(2)?,
                        guidance: row.get(3)?,
                        created_at: row.get(4)?,
                        rollback_target: row.get(5)?,
                    })
                },
            )
            .map_err(|error| format!("无法读取优化版本：{error}"))?;
        Ok(rows.filter_map(Result::ok).collect())
    }
}

fn copy_database(source: &Connection, destination: &mut Connection) -> Result<(), String> {
    let backup = rusqlite::backup::Backup::new(source, destination)
        .map_err(|error| format!("无法初始化 SQLite 复制：{error}"))?;
    backup
        .run_to_completion(64, std::time::Duration::from_millis(10), None)
        .map_err(|error| format!("SQLite 复制失败：{error}"))
}

fn restore_database_runtime_configuration(connection: &Connection) -> Result<(), String> {
    let current_journal_mode = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
        .map_err(|error| format!("无法读取 journal_mode：{error}"))?;
    let was_wal = current_journal_mode.eq_ignore_ascii_case("wal");
    if !was_wal {
        let applied_journal_mode = connection
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0))
            .map_err(|error| format!("无法启用 WAL：{error}"))?;
        if !applied_journal_mode.eq_ignore_ascii_case("wal") {
            return Err(format!(
                "SQLite 未启用 WAL，当前模式为 {applied_journal_mode}"
            ));
        }
    }
    connection
        .execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")
        .map_err(|error| format!("无法配置同步与外键约束：{error}"))?;
    // A rollback-journal database has no WAL frames to flush. Checkpointing immediately after
    // switching that same connection to WAL can return SQLITE_LOCKED.
    if was_wal {
        connection
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .map_err(|error| format!("无法完成 WAL checkpoint：{error}"))?;
    }
    Ok(())
}

fn database_integrity(connection: &Connection) -> Result<String, String> {
    connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|error| format!("无法执行数据库完整性检查：{error}"))
}

fn database_schema_version(connection: &Connection) -> Result<i64, String> {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| format!("无法读取数据库 schema 版本：{error}"))
}

fn inspect_backup(path: &Path) -> Result<DatabaseRestorePreflight, String> {
    let metadata = fs::metadata(path).map_err(|error| format!("无法读取备份元数据：{error}"))?;
    if metadata.len() == 0 {
        return Err("备份文件为空".to_string());
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("无法以只读方式打开备份：{error}"))?;
    let integrity = database_integrity(&connection)?;
    let schema_version = database_schema_version(&connection)?;
    let compatible =
        integrity == "ok" && schema_version > 0 && schema_version <= CURRENT_SCHEMA_VERSION;
    let reason = if integrity != "ok" {
        format!("完整性检查失败：{integrity}")
    } else if schema_version <= 0 {
        "不是可识别的 Yunspire 数据库".to_string()
    } else if schema_version > CURRENT_SCHEMA_VERSION {
        format!("备份 schema {schema_version} 高于当前应用支持的 {CURRENT_SCHEMA_VERSION}")
    } else {
        "预检通过".to_string()
    };
    Ok(DatabaseRestorePreflight {
        path: path.to_string_lossy().into_owned(),
        file_name: path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("yunspire.sqlite")
            .to_string(),
        byte_length: metadata.len(),
        schema_version,
        integrity,
        compatible,
        reason,
    })
}

fn evaluate_vault_write_policy(
    client_state: &Value,
    vault_id: &str,
    relative_path: &str,
) -> Result<(), String> {
    let settings = client_state.get("settings").and_then(Value::as_object);
    let access = settings
        .and_then(|value| value.get("vaultAccess"))
        .and_then(Value::as_object)
        .and_then(|value| value.get(vault_id))
        .and_then(Value::as_str)
        .unwrap_or("readwrite");
    match access {
        "readwrite" => {}
        "readonly" => return Err("当前 Obsidian 知识库仅允许查询，已拒绝写入".to_string()),
        "disabled" => return Err("当前 Obsidian 知识库已设为不接入，已拒绝写入".to_string()),
        _ => return Err("当前 Obsidian 知识库访问策略无效，已拒绝写入".to_string()),
    }

    let write_scope = settings
        .and_then(|value| value.get("vaultWriteScope"))
        .and_then(Value::as_str)
        .unwrap_or("all-writable");
    let current_vault = client_state
        .get("currentVaultId")
        .and_then(Value::as_str)
        .unwrap_or("all");
    match write_scope {
        "all-writable" => Ok(()),
        "readonly" => Err("设置已禁止自动写入 Obsidian".to_string()),
        "current-vault" => {
            if current_vault == vault_id && current_vault != "all" {
                Ok(())
            } else {
                Err("写入目标不属于当前 Obsidian 知识库".to_string())
            }
        }
        "inbox-only" => {
            if current_vault != vault_id || current_vault == "all" {
                return Err("写入目标不属于当前 Obsidian 知识库".to_string());
            }
            if relative_path == "收件箱"
                || relative_path.starts_with("收件箱/")
                || relative_path == "00 收件箱"
                || relative_path.starts_with("00 收件箱/")
            {
                Ok(())
            } else {
                Err("当前策略只允许写入收件箱".to_string())
            }
        }
        _ => Err("知识库写入范围配置无效，已拒绝写入".to_string()),
    }
}

fn valid_runtime_task_state(value: &str) -> bool {
    matches!(
        value,
        "created"
            | "queued"
            | "running"
            | "awaiting_approval"
            | "paused"
            | "succeeded"
            | "failed"
            | "cancelled"
    )
}

fn map_native_runtime_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<NativeRuntimeTask> {
    let payload: String = row.get(4)?;
    let payload = serde_json::from_str::<Value>(&payload)
        .unwrap_or_else(|_| Value::Object(serde_json::Map::new()));
    let progress = payload
        .get("progress")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(100) as u8;
    Ok(NativeRuntimeTask {
        id: row.get(0)?,
        state: row.get(1)?,
        title: row.get(2)?,
        trace_id: row.get(3)?,
        progress,
        payload,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

fn read_native_runtime_task(
    connection: &Connection,
    workspace_scope: &str,
    task_id: &str,
) -> Result<NativeRuntimeTask, String> {
    connection
        .query_row(
            "SELECT id, state, title, trace_id, payload, created_at, updated_at
             FROM runtime_tasks WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, task_id],
            map_native_runtime_task,
        )
        .optional()
        .map_err(|error| format!("无法读取原生任务：{error}"))?
        .ok_or_else(|| "未找到原生任务".to_string())
}

fn add_sqlite_column_if_missing(
    connection: &Connection,
    table: &str,
    column: &str,
    declaration: &str,
) -> Result<(), String> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|error| format!("无法检查 {table}.{column}：{error}"))?;
    let exists = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| format!("无法读取 {table} 字段：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法解析 {table} 字段：{error}"))?
        .iter()
        .any(|name| name == column);
    drop(statement);
    if !exists {
        connection
            .execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} {declaration};"
            ))
            .map_err(|error| format!("无法新增 {table}.{column}：{error}"))?;
    }
    Ok(())
}

fn run_migrations(connection: &Connection) -> Result<(), String> {
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| format!("无法读取 schema 版本：{error}"))?;
    if version < 1 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS workspace_state (
                   key TEXT PRIMARY KEY,
                   value TEXT NOT NULL,
                   updated_at TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS vault_registry (
                   id TEXT PRIMARY KEY,
                   display_name TEXT NOT NULL,
                   canonical_path TEXT NOT NULL UNIQUE,
                   note_count INTEGER NOT NULL DEFAULT 0,
                   attachment_count INTEGER NOT NULL DEFAULT 0,
                   connection_state TEXT NOT NULL,
                   is_open INTEGER NOT NULL DEFAULT 0,
                   last_indexed_at TEXT NOT NULL,
                   last_error TEXT
                 );
                 CREATE TABLE IF NOT EXISTS tasks (
                   id TEXT PRIMARY KEY,
                   state TEXT NOT NULL,
                   trace_id TEXT NOT NULL,
                   payload TEXT NOT NULL,
                   updated_at TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS approvals (
                   id TEXT PRIMARY KEY,
                   task_id TEXT NOT NULL,
                   state TEXT NOT NULL,
                   payload TEXT NOT NULL,
                   updated_at TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS secretary_messages (
                   id TEXT PRIMARY KEY,
                   conversation_id TEXT NOT NULL,
                   payload TEXT NOT NULL,
                   created_at TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS operation_events (
                   id TEXT PRIMARY KEY,
                   task_id TEXT,
                   event_type TEXT NOT NULL,
                   state TEXT NOT NULL,
                   payload TEXT NOT NULL,
                   created_at TEXT NOT NULL
                 );
                 PRAGMA user_version=1;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 1 失败：{error}"))?;
    }
    if version < 2 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS note_index (
                   vault_id TEXT NOT NULL,
                   relative_path TEXT NOT NULL,
                   title TEXT NOT NULL,
                   content_hash TEXT NOT NULL,
                   modified_at TEXT NOT NULL,
                   byte_length INTEGER NOT NULL,
                   tags_json TEXT NOT NULL,
                   wiki_links_json TEXT NOT NULL,
                   PRIMARY KEY (vault_id, relative_path)
                 );
                 CREATE VIRTUAL TABLE IF NOT EXISTS note_fts USING fts5(
                   vault_id UNINDEXED,
                   relative_path UNINDEXED,
                   title,
                   content,
                   tokenize='unicode61'
                 );
                 PRAGMA user_version=2;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 2 失败：{error}"))?;
    }
    if version < 3 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS local_workspace_scopes (
                   id TEXT PRIMARY KEY,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL
                 );
                 INSERT OR IGNORE INTO local_workspace_scopes (id, created_at, updated_at)
                   VALUES ('local', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
                 CREATE TABLE IF NOT EXISTS legacy_api_profiles (
                   workspace_scope TEXT PRIMARY KEY,
                   provider TEXT NOT NULL,
                   base_url TEXT NOT NULL,
                   selected_model TEXT NOT NULL,
                   available_models_json TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 PRAGMA user_version=3;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 3 失败：{error}"))?;
    }
    if version < 4 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS workspace_snapshots (
                   workspace_scope TEXT PRIMARY KEY,
                   payload TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 PRAGMA user_version=4;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 4 失败：{error}"))?;
    }
    if version < 5 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 PRAGMA user_version=5;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 5 失败：{error}"))?;
    }
    if version < 6 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 ALTER TABLE legacy_api_profiles
                   ADD COLUMN api_key_ciphertext BLOB NOT NULL DEFAULT X'';
                 PRAGMA user_version=6;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 6 失败：{error}"))?;
    }
    if version < 7 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS legacy_model_profiles (
                   workspace_scope TEXT NOT NULL,
                   role TEXT NOT NULL CHECK(role IN ('chat', 'analysis', 'image')),
                   provider TEXT NOT NULL,
                   base_url TEXT NOT NULL,
                   selected_model TEXT NOT NULL,
                   available_models_json TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   api_key_ciphertext BLOB NOT NULL DEFAULT X'',
                   PRIMARY KEY(workspace_scope, role),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 INSERT OR IGNORE INTO legacy_model_profiles
                   (workspace_scope, role, provider, base_url, selected_model, available_models_json, updated_at, api_key_ciphertext)
                   SELECT workspace_scope, 'chat', provider, base_url, selected_model, available_models_json, updated_at, api_key_ciphertext
                   FROM legacy_api_profiles;
                 INSERT OR IGNORE INTO legacy_model_profiles
                   (workspace_scope, role, provider, base_url, selected_model, available_models_json, updated_at, api_key_ciphertext)
                   SELECT workspace_scope, 'analysis', provider, base_url, selected_model, available_models_json, updated_at, api_key_ciphertext
                   FROM legacy_api_profiles;
                 PRAGMA user_version=7;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 7 失败：{error}"))?;
    }
    if version < 8 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS model_providers (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   name TEXT NOT NULL,
                   provider TEXT NOT NULL,
                   base_url TEXT NOT NULL,
                   available_models_json TEXT NOT NULL,
                   assignments_json TEXT NOT NULL,
                   defaults_json TEXT NOT NULL,
                   api_key_ciphertext BLOB NOT NULL DEFAULT X'',
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, id),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 PRAGMA user_version=8;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 8 失败：{error}"))?;
    }
    if version < 9 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS vault_preferences (
                   workspace_scope TEXT PRIMARY KEY,
                   defaults_initialized INTEGER NOT NULL DEFAULT 0,
                   explicit_vault_id TEXT,
                   updated_at TEXT NOT NULL,
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 PRAGMA user_version=9;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 9 失败：{error}"))?;
    }
    if version < 10 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS long_term_memory_events (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   event_type TEXT NOT NULL,
                   occurred_at TEXT NOT NULL,
                   payload TEXT NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('pending', 'committed', 'failed')),
                   attempt_count INTEGER NOT NULL DEFAULT 0,
                   vault_relative_path TEXT,
                   content_hash TEXT,
                   last_error TEXT,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   committed_at TEXT,
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_long_term_memory_user_state
                   ON long_term_memory_events(workspace_scope, state, occurred_at);
                 PRAGMA user_version=10;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 10 失败：{error}"))?;
    }
    if version < 11 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS runtime_settings (
                   workspace_scope TEXT PRIMARY KEY,
                   scheduler_enabled INTEGER NOT NULL DEFAULT 1,
                   updated_at TEXT NOT NULL,
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE TABLE IF NOT EXISTS runtime_tasks (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   state TEXT NOT NULL,
                   title TEXT NOT NULL,
                   trace_id TEXT,
                   payload TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, id),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_runtime_tasks_state
                   ON runtime_tasks(workspace_scope, state, updated_at);
                 CREATE TABLE IF NOT EXISTS runtime_task_steps (
                   workspace_scope TEXT NOT NULL,
                   task_id TEXT NOT NULL,
                   step_id TEXT NOT NULL,
                   position INTEGER NOT NULL,
                   state TEXT NOT NULL,
                   detail TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, task_id, step_id),
                   FOREIGN KEY(workspace_scope, task_id) REFERENCES runtime_tasks(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE TABLE IF NOT EXISTS runtime_task_attempts (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   task_id TEXT NOT NULL,
                   state TEXT NOT NULL,
                   detail TEXT NOT NULL,
                   started_at TEXT NOT NULL,
                   finished_at TEXT,
                   FOREIGN KEY(workspace_scope, task_id) REFERENCES runtime_tasks(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE TABLE IF NOT EXISTS runtime_schedules (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   schedule_kind TEXT NOT NULL CHECK(schedule_kind IN ('collection', 'report')),
                   enabled INTEGER NOT NULL,
                   next_run TEXT,
                   payload TEXT NOT NULL,
                   payload_hash TEXT NOT NULL,
                   revision INTEGER NOT NULL,
                   lease_owner TEXT,
                   lease_expires_at TEXT,
                   last_claimed_at TEXT,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, id, schedule_kind),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_runtime_schedules_due
                   ON runtime_schedules(workspace_scope, enabled, next_run, lease_expires_at);
                 CREATE TABLE IF NOT EXISTS runtime_schedule_revisions (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   schedule_id TEXT NOT NULL,
                   schedule_kind TEXT NOT NULL,
                   revision INTEGER NOT NULL,
                   payload TEXT NOT NULL,
                   payload_hash TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   UNIQUE(workspace_scope, schedule_id, schedule_kind, revision),
                   FOREIGN KEY(workspace_scope, schedule_id, schedule_kind)
                     REFERENCES runtime_schedules(workspace_scope, id, schedule_kind) ON DELETE CASCADE
                 );
                 PRAGMA user_version=11;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 11 失败：{error}"))?;
    }
    if version < 12 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS inbound_content_records (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   task_id TEXT,
                   state TEXT NOT NULL CHECK(state IN (
                     'extracted', 'analyzing', 'analysis_pending', 'quality_rejected',
                     'ready_to_write', 'writing', 'committed', 'failed', 'cancelled'
                   )),
                   source_type TEXT NOT NULL,
                   source_ref TEXT NOT NULL,
                   title TEXT NOT NULL,
                   content_hash TEXT NOT NULL,
                   content_characters INTEGER NOT NULL,
                   attachment_count INTEGER NOT NULL,
                   image_count INTEGER NOT NULL,
                   extraction_json TEXT NOT NULL,
                   analysis_json TEXT NOT NULL,
                   quality_json TEXT NOT NULL,
                   target_json TEXT NOT NULL,
                   failure_reason TEXT,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   committed_at TEXT,
                   PRIMARY KEY(workspace_scope, id),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_inbound_content_records_state
                   ON inbound_content_records(workspace_scope, state, updated_at);
                 CREATE TABLE IF NOT EXISTS inbound_content_transitions (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   content_id TEXT NOT NULL,
                   from_state TEXT,
                   to_state TEXT NOT NULL,
                   detail TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   FOREIGN KEY(workspace_scope, content_id)
                     REFERENCES inbound_content_records(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_inbound_content_transitions_record
                   ON inbound_content_transitions(workspace_scope, content_id, created_at);
                 PRAGMA user_version=12;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 12 失败：{error}"))?;
    }
    if version < 13 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 ALTER TABLE runtime_task_steps ADD COLUMN checkpoint_json TEXT NOT NULL DEFAULT '{}';
                 CREATE TABLE IF NOT EXISTS runtime_task_step_revisions (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   task_id TEXT NOT NULL,
                   step_id TEXT NOT NULL,
                   revision INTEGER NOT NULL,
                   position INTEGER NOT NULL,
                   state TEXT NOT NULL,
                   detail TEXT NOT NULL,
                   checkpoint_json TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   UNIQUE(workspace_scope, task_id, step_id, revision),
                   FOREIGN KEY(workspace_scope, task_id) REFERENCES runtime_tasks(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_runtime_task_step_revisions
                   ON runtime_task_step_revisions(workspace_scope, task_id, step_id, revision);
                 CREATE TABLE IF NOT EXISTS runtime_task_checkpoints (
                   workspace_scope TEXT NOT NULL,
                   task_id TEXT NOT NULL,
                   checkpoint_id TEXT NOT NULL,
                   sequence INTEGER NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('pending', 'running', 'completed', 'failed')),
                   payload TEXT NOT NULL,
                   payload_hash TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   completed_at TEXT,
                   PRIMARY KEY(workspace_scope, task_id, checkpoint_id),
                   FOREIGN KEY(workspace_scope, task_id) REFERENCES runtime_tasks(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_runtime_task_checkpoints
                   ON runtime_task_checkpoints(workspace_scope, task_id, sequence, updated_at);
                 CREATE TABLE IF NOT EXISTS runtime_task_recoveries (
                   workspace_scope TEXT NOT NULL,
                   task_id TEXT NOT NULL,
                   interrupted_task_updated_at TEXT NOT NULL,
                   recommendation TEXT NOT NULL CHECK(recommendation IN ('completed', 'resume', 'needs_input', 'manual')),
                   resume_step_id TEXT,
                   resume_step_index INTEGER,
                   resume_checkpoint_id TEXT,
                   evidence_json TEXT NOT NULL,
                   detail TEXT NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('pending', 'resolved')),
                   resolution TEXT,
                   detected_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   resolved_at TEXT,
                   PRIMARY KEY(workspace_scope, task_id),
                   FOREIGN KEY(workspace_scope, task_id) REFERENCES runtime_tasks(workspace_scope, id) ON DELETE CASCADE
                 );
                 PRAGMA user_version=13;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 13 失败：{error}"))?;
    }
    if version < 14 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 DROP INDEX IF EXISTS idx_long_term_memory_user_state;
                 CREATE INDEX IF NOT EXISTS idx_long_term_memory_workspace_state
                   ON long_term_memory_events(workspace_scope, state, occurred_at);
                 PRAGMA user_version=14;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 14 失败：{error}"))?;
    }
    if version < 15 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS application_commands (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   idempotency_key TEXT NOT NULL,
                   command_type TEXT NOT NULL,
                   operation TEXT NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('accepted', 'denied', 'completed', 'failed', 'cancelled')),
                   task_id TEXT,
                   trace_id TEXT,
                   payload TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, id),
                   UNIQUE(workspace_scope, idempotency_key),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_application_commands_task
                   ON application_commands(workspace_scope, task_id, updated_at);
                 CREATE TABLE IF NOT EXISTS policy_decisions (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   command_id TEXT NOT NULL,
                   outcome TEXT NOT NULL CHECK(outcome IN ('allow', 'deny', 'require_approval', 'allow_with_reduced_scope')),
                   payload TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   FOREIGN KEY(workspace_scope, command_id)
                     REFERENCES application_commands(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_policy_decisions_command
                   ON policy_decisions(workspace_scope, command_id, created_at);
                 CREATE TABLE IF NOT EXISTS runtime_task_transitions (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   task_id TEXT NOT NULL,
                   from_state TEXT NOT NULL,
                   to_state TEXT NOT NULL,
                   detail TEXT NOT NULL,
                   checkpoint_json TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   FOREIGN KEY(workspace_scope, task_id)
                     REFERENCES runtime_tasks(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_runtime_task_transitions
                   ON runtime_task_transitions(workspace_scope, task_id, created_at);
                 PRAGMA user_version=15;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 15 失败：{error}"))?;
    }
    if version < 16 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS managed_resources (
                   workspace_scope TEXT NOT NULL,
                   resource_type TEXT NOT NULL CHECK(resource_type IN (
                     'user_skill', 'schedule', 'report_subscription', 'report',
                     'assistant_profile', 'optimization_profile', 'optimization_candidate'
                   )),
                   id TEXT NOT NULL,
                   revision INTEGER NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('active', 'deleted')),
                   payload TEXT NOT NULL,
                   payload_hash TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, resource_type, id),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_managed_resources_type_state
                   ON managed_resources(workspace_scope, resource_type, state, updated_at);
                 CREATE TABLE IF NOT EXISTS managed_resource_revisions (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   resource_type TEXT NOT NULL,
                   resource_id TEXT NOT NULL,
                   revision INTEGER NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('active', 'deleted')),
                   payload TEXT NOT NULL,
                   payload_hash TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   UNIQUE(workspace_scope, resource_type, resource_id, revision),
                   FOREIGN KEY(workspace_scope, resource_type, resource_id)
                     REFERENCES managed_resources(workspace_scope, resource_type, id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_managed_resource_revisions
                   ON managed_resource_revisions(workspace_scope, resource_type, resource_id, revision);
                 PRAGMA user_version=16;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 16 失败：{error}"))?;
    }
    if version < 17 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS long_term_memory_governance (
                   workspace_scope TEXT NOT NULL,
                   memory_id TEXT NOT NULL,
                   status TEXT NOT NULL CHECK(status IN ('active', 'corrected', 'expired', 'tombstoned', 'compressed')),
                   replacement_id TEXT,
                   note TEXT NOT NULL DEFAULT '',
                   expires_at TEXT,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, memory_id),
                   FOREIGN KEY(memory_id) REFERENCES long_term_memory_events(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_long_term_memory_governance_state
                   ON long_term_memory_governance(workspace_scope, status, expires_at, updated_at);
                 PRAGMA user_version=17;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 17 失败：{error}"))?;
    }
    if version < 18 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS external_connectors (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   name TEXT NOT NULL,
                   connector_type TEXT NOT NULL CHECK(connector_type IN ('feishu', 'wechat', 'email_webhook', 'webhook')),
                   endpoint_ciphertext BLOB NOT NULL,
                   secret_ciphertext BLOB NOT NULL,
                   enabled INTEGER NOT NULL DEFAULT 1,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, id),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE TABLE IF NOT EXISTS external_delivery_receipts (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   connector_id TEXT NOT NULL,
                   task_id TEXT NOT NULL,
                   trace_id TEXT,
                   status_code INTEGER NOT NULL,
                   response_hash TEXT NOT NULL,
                   delivered_at TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_external_delivery_task
                   ON external_delivery_receipts(workspace_scope, task_id, delivered_at);
                 PRAGMA user_version=18;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 18 失败：{error}"))?;
    }
    if version < 19 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS optimization_cursors (
                   workspace_scope TEXT PRIMARY KEY,
                   revision INTEGER NOT NULL DEFAULT 0,
                   last_occurred_at TEXT NOT NULL DEFAULT '',
                   last_event_id TEXT NOT NULL DEFAULT '',
                   updated_at TEXT NOT NULL,
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE TABLE IF NOT EXISTS optimization_candidates (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   base_version INTEGER NOT NULL,
                   candidate_version INTEGER NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('pending_evaluation', 'pending_review', 'rejected', 'applied', 'superseded')),
                   summary TEXT NOT NULL,
                   rules_json TEXT NOT NULL,
                   skill_hints_json TEXT NOT NULL,
                   metrics_json TEXT NOT NULL,
                   evidence_count INTEGER NOT NULL,
                   evidence_occurred_at TEXT NOT NULL,
                   evidence_event_id TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   evaluated_at TEXT,
                   expires_at TEXT,
                   PRIMARY KEY(workspace_scope, id),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_optimization_candidates_state
                   ON optimization_candidates(workspace_scope, state, created_at);
                 CREATE TABLE IF NOT EXISTS optimization_evaluations (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   candidate_id TEXT NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('pending_review', 'rejected')),
                   checks_json TEXT NOT NULL,
                   evaluated_at TEXT NOT NULL,
                   FOREIGN KEY(workspace_scope, candidate_id)
                     REFERENCES optimization_candidates(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE TABLE IF NOT EXISTS optimization_profiles (
                   workspace_scope TEXT PRIMARY KEY,
                   version INTEGER NOT NULL,
                   candidate_id TEXT,
                   guidance TEXT NOT NULL,
                   rules_json TEXT NOT NULL,
                   skill_hints_json TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE TABLE IF NOT EXISTS optimization_profile_revisions (
                   workspace_scope TEXT NOT NULL,
                   version INTEGER NOT NULL,
                   candidate_id TEXT,
                   state TEXT NOT NULL CHECK(state IN ('initial', 'active', 'rollback')),
                   guidance TEXT NOT NULL,
                   rules_json TEXT NOT NULL,
                   skill_hints_json TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   rollback_target INTEGER,
                   PRIMARY KEY(workspace_scope, version),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 INSERT OR IGNORE INTO optimization_cursors
                   (workspace_scope, revision, last_occurred_at, last_event_id, updated_at)
                   SELECT id, 0, '', '', CURRENT_TIMESTAMP FROM local_workspace_scopes;
                 INSERT OR IGNORE INTO optimization_profiles
                   (workspace_scope, version, candidate_id, guidance, rules_json, skill_hints_json, updated_at)
                   SELECT id, 0, NULL, '', '[]', '{}', CURRENT_TIMESTAMP FROM local_workspace_scopes;
                 INSERT OR IGNORE INTO optimization_profile_revisions
                   (workspace_scope, version, candidate_id, state, guidance, rules_json, skill_hints_json, created_at, rollback_target)
                   SELECT id, 0, NULL, 'initial', '', '[]', '{}', CURRENT_TIMESTAMP, NULL
                   FROM local_workspace_scopes;
                 PRAGMA user_version=19;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 19 失败：{error}"))?;
    }
    if version < 20 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS model_usage_events (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   request_id TEXT NOT NULL UNIQUE,
                   operation TEXT NOT NULL,
                   provider TEXT NOT NULL,
                   model TEXT NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('started', 'succeeded', 'failed', 'cancelled')),
                   prompt_tokens INTEGER NOT NULL DEFAULT 0,
                   completion_tokens INTEGER NOT NULL DEFAULT 0,
                   total_tokens INTEGER NOT NULL DEFAULT 0,
                   estimated_cost_usd REAL,
                   cost_source TEXT NOT NULL,
                   duration_ms INTEGER NOT NULL DEFAULT 0,
                   error TEXT,
                   created_at TEXT NOT NULL,
                   completed_at TEXT,
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_model_usage_events_time
                   ON model_usage_events(workspace_scope, created_at);
                 PRAGMA user_version=20;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 20 失败：{error}"))?;
    }
    if version < 21 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS application_authorization (
                   id INTEGER PRIMARY KEY CHECK(id=1),
                   status TEXT NOT NULL CHECK(status IN ('granted', 'denied')),
                   authorization_version INTEGER NOT NULL,
                   decided_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL
                 );
                 PRAGMA user_version=21;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 21 失败：{error}"))?;
    }
    if version < 22 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS vault_index_changes (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   vault_id TEXT NOT NULL,
                   canonical_root TEXT NOT NULL,
                   relative_path TEXT NOT NULL,
                   generation INTEGER NOT NULL DEFAULT 1 CHECK(generation > 0),
                   change_kind TEXT NOT NULL CHECK(change_kind IN ('upsert', 'delete')),
                   state TEXT NOT NULL CHECK(state IN ('pending', 'processing', 'failed')),
                   attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
                   available_at_ms INTEGER NOT NULL,
                   claimed_at_ms INTEGER,
                   last_error TEXT,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   UNIQUE(vault_id, relative_path)
                 );
                 CREATE INDEX IF NOT EXISTS idx_vault_index_changes_ready
                   ON vault_index_changes(state, available_at_ms, updated_at);
                 PRAGMA user_version=22;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 22 失败：{error}"))?;
    }
    if version < 23 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE VIRTUAL TABLE IF NOT EXISTS note_lexical_fts USING fts5(
                   vault_id UNINDEXED,
                   relative_path UNINDEXED,
                   title,
                   content,
                   tags,
                   wiki_links,
                   cjk_terms,
                   tokenize='unicode61'
                 );
                 PRAGMA user_version=23;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 23 失败：{error}"))?;
    }
    if version < 24 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS memory_records (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   track TEXT NOT NULL CHECK(track IN ('user_episode', 'user_profile', 'agent_case', 'agent_skill')),
                   title TEXT NOT NULL,
                   content TEXT NOT NULL,
                   user_id TEXT NOT NULL,
                   agent_id TEXT NOT NULL,
                   app_id TEXT NOT NULL,
                   project_id TEXT NOT NULL,
                   session_id TEXT NOT NULL,
                   source_doc_id TEXT NOT NULL,
                   source_relative_path TEXT,
                   source_content_hash TEXT,
                   evidence_json TEXT NOT NULL,
                   confidence REAL NOT NULL CHECK(confidence >= 0 AND confidence <= 1),
                   version INTEGER NOT NULL CHECK(version > 0),
                   supersedes_id TEXT,
                   state TEXT NOT NULL CHECK(state IN ('draft', 'active', 'superseded', 'tombstone')),
                   expires_at TEXT,
                   payload_hash TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, id),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_memory_records_scope_state
                   ON memory_records(workspace_scope, user_id, agent_id, app_id, project_id, session_id, state, updated_at);
                 CREATE INDEX IF NOT EXISTS idx_memory_records_source
                   ON memory_records(workspace_scope, source_doc_id, source_content_hash);
                 CREATE TABLE IF NOT EXISTS memory_record_revisions (
                   id TEXT PRIMARY KEY,
                   workspace_scope TEXT NOT NULL,
                   memory_id TEXT NOT NULL,
                   version INTEGER NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('draft', 'active', 'superseded', 'tombstone')),
                   payload TEXT NOT NULL,
                   payload_hash TEXT NOT NULL,
                   created_at TEXT NOT NULL,
                   UNIQUE(workspace_scope, memory_id, version),
                   FOREIGN KEY(workspace_scope, memory_id)
                     REFERENCES memory_records(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
                   workspace_scope UNINDEXED,
                   memory_id UNINDEXED,
                   title,
                   content,
                   evidence,
                   cjk_terms,
                   tokenize='unicode61'
                 );
                 CREATE TABLE IF NOT EXISTS memory_reflection_jobs (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   idempotency_key TEXT NOT NULL,
                   task_id TEXT,
                   scope_json TEXT NOT NULL,
                   source_doc_ids_json TEXT NOT NULL,
                   source_content_hash TEXT NOT NULL,
                   metrics_json TEXT NOT NULL,
                   state TEXT NOT NULL CHECK(state IN ('queued', 'running', 'awaiting_review', 'completed', 'failed', 'cancelled')),
                   proposal_memory_id TEXT,
                   attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
                   last_error TEXT,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, id),
                   UNIQUE(workspace_scope, idempotency_key),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_memory_reflection_state
                   ON memory_reflection_jobs(workspace_scope, state, updated_at);
                 PRAGMA user_version=24;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 24 失败：{error}"))?;
    }
    if version < 25 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS note_feature_vectors (
                   vault_id TEXT NOT NULL,
                   relative_path TEXT NOT NULL,
                   content_hash TEXT NOT NULL,
                   representation_version INTEGER NOT NULL,
                   dimensions INTEGER NOT NULL,
                   vector_blob BLOB NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY (vault_id, relative_path),
                   FOREIGN KEY (vault_id, relative_path)
                     REFERENCES note_index(vault_id, relative_path) ON DELETE CASCADE
                 );
                 INSERT OR IGNORE INTO vault_index_changes
                   (vault_id, canonical_root, relative_path, generation, change_kind, state,
                    attempt_count, available_at_ms, claimed_at_ms, last_error, created_at, updated_at)
                 SELECT i.vault_id, v.canonical_path, i.relative_path, 1, 'upsert', 'pending',
                        0, 0, NULL, NULL, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
                 FROM note_index i
                 JOIN vault_registry v ON v.id=i.vault_id
                 LEFT JOIN note_feature_vectors n
                   ON n.vault_id=i.vault_id AND n.relative_path=i.relative_path
                 WHERE n.vault_id IS NULL;
                 PRAGMA user_version=25;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 25 失败：{error}"))?;
    }
    if version < 26 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE IF NOT EXISTS assistant_conversations (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   revision INTEGER NOT NULL CHECK(revision >= 0),
                   context_json TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, id),
                   FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
                 );
                 CREATE TABLE IF NOT EXISTS assistant_requests (
                   workspace_scope TEXT NOT NULL,
                   id TEXT NOT NULL,
                   conversation_id TEXT NOT NULL,
                   conversation_revision INTEGER NOT NULL CHECK(conversation_revision >= 0),
                   sequence INTEGER NOT NULL CHECK(sequence > 0),
                   state TEXT NOT NULL CHECK(state IN ('queued', 'running', 'succeeded', 'failed', 'cancelled', 'needs_input')),
                   payload_json TEXT NOT NULL,
                   context_json TEXT,
                   context_hash TEXT,
                   result_json TEXT,
                   has_volatile_attachments INTEGER NOT NULL DEFAULT 0 CHECK(has_volatile_attachments IN (0, 1)),
                   recovery_count INTEGER NOT NULL DEFAULT 0 CHECK(recovery_count >= 0),
                   last_error TEXT,
                   created_at TEXT NOT NULL,
                   started_at TEXT,
                   completed_at TEXT,
                   updated_at TEXT NOT NULL,
                   PRIMARY KEY(workspace_scope, id),
                   UNIQUE(workspace_scope, conversation_id, sequence),
                   FOREIGN KEY(workspace_scope, conversation_id)
                     REFERENCES assistant_conversations(workspace_scope, id) ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS idx_assistant_requests_lane
                   ON assistant_requests(workspace_scope, conversation_id, state, sequence);
                 CREATE INDEX IF NOT EXISTS idx_assistant_requests_recovery
                   ON assistant_requests(workspace_scope, state, updated_at);
                 PRAGMA user_version=26;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 26 失败：{error}"))?;
    }
    if version < 27 {
        let transaction = connection
            .unchecked_transaction()
            .map_err(|error| format!("SQLite migration 27 无法开始事务：{error}"))?;
        crate::trace::migrate_schema(&transaction)?;
        crate::skill_lifecycle::migrate_schema(&transaction)?;
        add_sqlite_column_if_missing(&transaction, "model_usage_events", "trace_id", "TEXT")?;
        add_sqlite_column_if_missing(&transaction, "operation_events", "trace_id", "TEXT")?;
        add_sqlite_column_if_missing(&transaction, "vault_index_changes", "trace_id", "TEXT")?;
        transaction
            .execute_batch(
                "UPDATE runtime_tasks
                    SET trace_id='trace-legacy-task-' || lower(hex(randomblob(16)))
                  WHERE trace_id IS NULL OR trim(trace_id)='';
                 UPDATE model_usage_events
                    SET trace_id='trace-legacy-model-' || lower(hex(randomblob(16)))
                  WHERE trace_id IS NULL OR trim(trace_id)='';
                 UPDATE operation_events
                    SET trace_id=COALESCE(
                      NULLIF(json_extract(payload, '$.traceId'), ''),
                      'trace-legacy-operation-' || id
                    )
                  WHERE trace_id IS NULL OR trim(trace_id)='';
                 UPDATE vault_index_changes
                    SET trace_id='trace-legacy-index-' || id
                  WHERE trace_id IS NULL OR trim(trace_id)='';",
            )
            .map_err(|error| format!("SQLite migration 27 无法扩展 Trace 字段：{error}"))?;
        crate::trace::migrate_legacy_events(&transaction)?;
        crate::skill_lifecycle::migrate_legacy_skills(&transaction)?;
        transaction
            .execute_batch("PRAGMA user_version=27;")
            .map_err(|error| format!("SQLite migration 27 无法更新版本：{error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("SQLite migration 27 失败：{error}"))?;
    }
    if version < 28 {
        connection
            .execute_batch(
                "BEGIN IMMEDIATE;
                 ALTER TABLE vault_index_changes RENAME TO vault_index_changes_v27;
                 CREATE TABLE vault_index_changes (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   vault_id TEXT NOT NULL,
                   canonical_root TEXT NOT NULL,
                   relative_path TEXT NOT NULL,
                   generation INTEGER NOT NULL DEFAULT 1 CHECK(generation > 0),
                   change_kind TEXT NOT NULL CHECK(change_kind IN ('upsert', 'delete')),
                   state TEXT NOT NULL CHECK(state IN ('pending', 'processing', 'dead_letter')),
                   attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
                   available_at_ms INTEGER NOT NULL,
                   claimed_at_ms INTEGER,
                   last_error TEXT,
                   trace_id TEXT,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   UNIQUE(vault_id, relative_path)
                 );
                 INSERT INTO vault_index_changes
                   (id, vault_id, canonical_root, relative_path, generation, change_kind, state,
                    attempt_count, available_at_ms, claimed_at_ms, last_error, trace_id,
                    created_at, updated_at)
                 SELECT id, vault_id, canonical_root, relative_path, generation, change_kind,
                        CASE WHEN state='failed' THEN 'dead_letter' ELSE state END,
                        attempt_count, available_at_ms, claimed_at_ms, last_error, trace_id,
                        created_at, updated_at
                   FROM vault_index_changes_v27;
                 DROP TABLE vault_index_changes_v27;
                 CREATE INDEX idx_vault_index_changes_ready
                   ON vault_index_changes(state, available_at_ms, updated_at);
                 PRAGMA user_version=28;
                 COMMIT;",
            )
            .map_err(|error| format!("SQLite migration 28 失败：{error}"))?;
    }
    Ok(())
}

fn table_count(connection: &Connection, table: &str) -> Result<i64, String> {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    connection
        .query_row(&sql, [], |row| row.get(0))
        .map_err(|error| format!("无法统计 {table}：{error}"))
}

fn valid_runtime_identifier(value: &str, max: usize) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.chars().count() <= max
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
        })
}

fn contains_sensitive_memory_value(value: &str) -> bool {
    let lower = value.to_lowercase();
    lower.contains("authorization:")
        || lower.contains("api_key=")
        || lower.contains("api-key=")
        || lower.contains("password=")
        || lower.contains("cookie:")
        || Regex::new(r"\bsk-[A-Za-z0-9_-]{16,}\b")
            .expect("valid credential pattern")
            .is_match(value)
}

fn contains_optimization_forbidden_instruction(value: &str) -> bool {
    let normalized = value.to_lowercase().replace(char::is_whitespace, "");
    [
        "绕过审批",
        "关闭审批",
        "禁用审批",
        "修改系统提示",
        "覆盖系统指令",
        "扩大权限",
        "授予工具权限",
        "读取密钥",
        "导出密钥",
        "打开设置",
        "修改设置",
        "绕过访问控制",
    ]
    .iter()
    .any(|pattern| normalized.contains(pattern))
}

fn validate_optimization_candidate_input(input: &OptimizationCandidateInput) -> Result<(), String> {
    if !valid_runtime_identifier(&input.id, 160) {
        return Err("优化候选 ID 无效".to_string());
    }
    let summary = input.summary.trim();
    if summary.is_empty()
        || summary.chars().count() > 32_000
        || contains_sensitive_memory_value(summary)
    {
        return Err("优化候选摘要为空、过长或包含疑似凭据".to_string());
    }
    if input.rules.is_empty() || input.rules.len() > 12 {
        return Err("优化候选必须包含 1 到 12 条规则".to_string());
    }
    if input.rules.iter().any(|rule| {
        rule.trim().is_empty()
            || rule.chars().count() > 2000
            || contains_sensitive_memory_value(rule)
    }) {
        return Err("优化规则为空、过长或包含疑似凭据".to_string());
    }
    if !input.skill_hints.is_object() || !input.metrics.is_object() {
        return Err("优化候选的 Skill 提示和指标必须是 JSON 对象".to_string());
    }
    if serde_json::to_vec(&input.skill_hints)
        .map_err(|error| format!("无法校验 Skill 优化提示：{error}"))?
        .len()
        > 128 * 1024
        || serde_json::to_vec(&input.metrics)
            .map_err(|error| format!("无法校验优化指标：{error}"))?
            .len()
            > 64 * 1024
    {
        return Err("优化候选结构化数据超过安全上限".to_string());
    }
    if input.evidence_count < 2
        || chrono::DateTime::parse_from_rfc3339(&input.evidence_cursor_occurred_at).is_err()
        || !valid_runtime_identifier(&input.evidence_cursor_event_id, 160)
    {
        return Err("优化候选缺少足够的增量证据或有效证据游标".to_string());
    }
    if let Some(expires_at) = input.expires_at.as_deref() {
        let expires_at = chrono::DateTime::parse_from_rfc3339(expires_at)
            .map_err(|_| "优化候选过期时间必须是 RFC3339".to_string())?;
        if expires_at.with_timezone(&Utc) <= Utc::now() {
            return Err("优化候选过期时间必须晚于当前时间".to_string());
        }
    }
    Ok(())
}

fn value_string<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("运行记录缺少字段 {key}"))
}

fn validate_records(records: &[Value], label: &str) -> Result<(), String> {
    if records.len() > MAX_SNAPSHOT_RECORDS {
        return Err(format!("{label} 数量超过安全上限"));
    }
    for record in records {
        let serialized =
            serde_json::to_vec(record).map_err(|error| format!("无法序列化 {label}：{error}"))?;
        if serialized.len() > MAX_RECORD_BYTES {
            return Err(format!("单条 {label} 超过 2 MB 安全上限"));
        }
        value_string(record, "id")?;
    }
    Ok(())
}

fn managed_resource_id(payload: &Value, label: &str) -> Result<String, String> {
    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{label}缺少 id"))?;
    if id.chars().count() > 180 || id.chars().any(char::is_control) {
        return Err(format!("{label} id 无效"));
    }
    Ok(id.to_string())
}

fn upsert_managed_resource(
    transaction: &Transaction<'_>,
    workspace_scope: &str,
    resource_type: &str,
    id: &str,
    payload: &Value,
) -> Result<(), String> {
    if !payload.is_object() {
        return Err(format!("{resource_type}/{id} 的资源负载必须是 JSON 对象"));
    }
    let serialized =
        serde_json::to_string(payload).map_err(|error| format!("无法序列化独立资源：{error}"))?;
    if serialized.len() > MAX_RECORD_BYTES {
        return Err(format!("{resource_type}/{id} 超过 2 MB 安全上限"));
    }
    let payload_hash = format!("{:x}", Sha256::digest(serialized.as_bytes()));
    let existing = transaction
        .query_row(
            "SELECT revision, state, payload_hash, created_at FROM managed_resources
             WHERE workspace_scope=?1 AND resource_type=?2 AND id=?3",
            params![workspace_scope, resource_type, id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("无法读取独立资源当前修订：{error}"))?;
    if existing
        .as_ref()
        .is_some_and(|(_, state, hash, _)| state == "active" && hash == &payload_hash)
    {
        return Ok(());
    }
    let revision = existing
        .as_ref()
        .map_or(1, |(revision, _, _, _)| revision + 1);
    let now = Utc::now().to_rfc3339();
    let created_at = existing
        .as_ref()
        .map(|(_, _, _, created_at)| created_at.as_str())
        .unwrap_or(now.as_str());
    transaction
        .execute(
            "INSERT INTO managed_resources
             (workspace_scope, resource_type, id, revision, state, payload, payload_hash, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?6, ?7, ?8)
             ON CONFLICT(workspace_scope, resource_type, id) DO UPDATE SET
               revision=excluded.revision, state='active', payload=excluded.payload,
               payload_hash=excluded.payload_hash, updated_at=excluded.updated_at",
            params![
                workspace_scope,
                resource_type,
                id,
                revision,
                serialized,
                payload_hash,
                created_at,
                now
            ],
        )
        .map_err(|error| format!("无法保存独立资源：{error}"))?;
    transaction
        .execute(
            "INSERT INTO managed_resource_revisions
             (id, workspace_scope, resource_type, resource_id, revision, state, payload, payload_hash, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6, ?7, ?8)",
            params![
                Uuid::new_v4().to_string(),
                workspace_scope,
                resource_type,
                id,
                revision,
                serialized,
                payload_hash,
                now
            ],
        )
        .map_err(|error| format!("无法保存独立资源修订：{error}"))?;
    Ok(())
}

fn tombstone_managed_resource(
    transaction: &Transaction<'_>,
    workspace_scope: &str,
    resource_type: &str,
    id: &str,
) -> Result<(), String> {
    let existing = transaction
        .query_row(
            "SELECT revision, state, payload, payload_hash FROM managed_resources
             WHERE workspace_scope=?1 AND resource_type=?2 AND id=?3",
            params![workspace_scope, resource_type, id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("无法读取待删除独立资源：{error}"))?;
    let Some((revision, state, payload, payload_hash)) = existing else {
        return Ok(());
    };
    if state == "deleted" {
        return Ok(());
    }
    let revision = revision + 1;
    let now = Utc::now().to_rfc3339();
    transaction
        .execute(
            "UPDATE managed_resources SET revision=?4, state='deleted', updated_at=?5
             WHERE workspace_scope=?1 AND resource_type=?2 AND id=?3",
            params![workspace_scope, resource_type, id, revision, now],
        )
        .map_err(|error| format!("无法标记独立资源已删除：{error}"))?;
    transaction
        .execute(
            "INSERT INTO managed_resource_revisions
             (id, workspace_scope, resource_type, resource_id, revision, state, payload, payload_hash, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'deleted', ?6, ?7, ?8)",
            params![
                Uuid::new_v4().to_string(),
                workspace_scope,
                resource_type,
                id,
                revision,
                payload,
                payload_hash,
                now
            ],
        )
        .map_err(|error| format!("无法保存独立资源删除修订：{error}"))?;
    Ok(())
}

fn sync_managed_resource_group(
    transaction: &Transaction<'_>,
    workspace_scope: &str,
    resource_type: &str,
    resources: &[Value],
) -> Result<(), String> {
    let mut incoming_ids = HashSet::new();
    for payload in resources {
        let id = managed_resource_id(payload, resource_type)?;
        if !incoming_ids.insert(id.clone()) {
            return Err(format!("{resource_type} 包含重复 id：{id}"));
        }
        upsert_managed_resource(transaction, workspace_scope, resource_type, &id, payload)?;
    }
    let existing_ids = {
        let mut statement = transaction
            .prepare(
                "SELECT id FROM managed_resources
                 WHERE workspace_scope=?1 AND resource_type=?2 AND state='active'",
            )
            .map_err(|error| format!("无法准备独立资源清理查询：{error}"))?;
        let ids = statement
            .query_map(params![workspace_scope, resource_type], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| format!("无法读取独立资源清理列表：{error}"))?
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        ids
    };
    for id in existing_ids {
        if !incoming_ids.contains(&id) {
            tombstone_managed_resource(transaction, workspace_scope, resource_type, &id)?;
        }
    }
    Ok(())
}

fn validate_inbound_identifier(value: &str, label: &str, max_chars: usize) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max_chars {
        return Err(format!("{label}为空或超过 {max_chars} 个字符"));
    }
    if value.chars().any(|character| character.is_control()) {
        return Err(format!("{label}包含控制字符"));
    }
    Ok(())
}

fn serialize_inbound_record_section(value: &Value, label: &str) -> Result<String, String> {
    if !value.is_object() {
        return Err(format!("内容处理记录的{label}必须是 JSON 对象"));
    }
    let serialized = serde_json::to_string(value)
        .map_err(|error| format!("无法序列化内容处理记录的{label}：{error}"))?;
    if serialized.len() > MAX_INBOUND_RECORD_BYTES / 2 {
        return Err(format!("内容处理记录的{label}超过 256 KB 安全上限"));
    }
    Ok(serialized)
}

fn validate_inbound_content_record(record: &InboundContentRecordInput) -> Result<(), String> {
    validate_inbound_identifier(&record.id, "内容记录 ID", 180)?;
    validate_inbound_identifier(&record.source_type, "来源类型", 32)?;
    validate_inbound_identifier(&record.source_ref, "来源引用", 4096)?;
    validate_inbound_identifier(&record.title, "内容标题", 240)?;
    if !record.content_hash.starts_with("sha256:")
        || record.content_hash.len() != 71
        || !record.content_hash[7..]
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    {
        return Err("正文哈希必须是小写 sha256:<64位十六进制>".to_string());
    }
    if record.content_characters > 4 * 1024 * 1024 {
        return Err("内容处理记录的正文字符数超过 4 MB 安全上限".to_string());
    }
    if record.attachment_count > 100_000 || record.image_count > record.attachment_count + 100_000 {
        return Err("内容处理记录的附件统计超过安全上限".to_string());
    }
    if !matches!(
        record.state.as_str(),
        "extracted"
            | "analyzing"
            | "analysis_pending"
            | "quality_rejected"
            | "ready_to_write"
            | "writing"
            | "committed"
            | "failed"
            | "cancelled"
    ) {
        return Err("内容处理记录状态无效".to_string());
    }
    if let Some(task_id) = record.task_id.as_deref() {
        validate_inbound_identifier(task_id, "内容处理任务 ID", 180)?;
    }
    if let Some(reason) = record.failure_reason.as_deref() {
        if reason.chars().count() > 4000 {
            return Err("内容处理失败原因超过 4000 个字符".to_string());
        }
    }
    let serialized =
        serde_json::to_vec(record).map_err(|error| format!("无法序列化内容处理记录：{error}"))?;
    if serialized.len() > MAX_INBOUND_RECORD_BYTES {
        return Err("单条内容处理记录超过 512 KB 安全上限".to_string());
    }
    Ok(())
}

fn inbound_content_transition_allowed(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    match from {
        "extracted" => matches!(
            to,
            "analyzing" | "analysis_pending" | "quality_rejected" | "failed" | "cancelled"
        ),
        "analyzing" | "analysis_pending" => {
            matches!(
                to,
                "ready_to_write" | "quality_rejected" | "failed" | "cancelled"
            )
        }
        "quality_rejected" => matches!(to, "failed" | "cancelled"),
        "ready_to_write" => matches!(to, "writing" | "failed" | "cancelled"),
        "writing" => matches!(to, "committed" | "failed"),
        "committed" | "failed" | "cancelled" => false,
        _ => false,
    }
}

fn runtime_value_string(value: &Value, key: &str, label: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .filter(|item| item.len() <= 180)
        .map(str::to_string)
        .ok_or_else(|| format!("{label}缺少有效字段 {key}"))
}

fn normalize_runtime_time(value: Option<&str>) -> Option<String> {
    value
        .and_then(|item| chrono::DateTime::parse_from_rfc3339(item).ok())
        .map(|time| time.with_timezone(&Utc).to_rfc3339())
}

fn sync_runtime_tasks(
    transaction: &Transaction<'_>,
    workspace_scope: &str,
    tasks: &[Value],
) -> Result<(), String> {
    for task in tasks {
        let id = runtime_value_string(task, "id", "原生任务")?;
        let state = task
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("created");
        let title = task
            .get("title")
            .or_else(|| task.get("label"))
            .and_then(Value::as_str)
            .unwrap_or("未命名任务")
            .chars()
            .take(240)
            .collect::<String>();
        let incoming_trace_id = task
            .get("traceId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let previous_task = transaction
            .query_row(
                "SELECT state, trace_id FROM runtime_tasks WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()
            .map_err(|error| format!("无法读取原生任务状态：{error}"))?;
        let previous_trace_id = previous_task
            .as_ref()
            .and_then(|(_, trace_id)| trace_id.as_deref())
            .filter(|value| !value.trim().is_empty());
        if incoming_trace_id
            .as_deref()
            .zip(previous_trace_id)
            .is_some_and(|(incoming, previous)| incoming != previous)
        {
            return Err("同一原生任务不能重新绑定其他 Trace".to_string());
        }
        let trace_id = incoming_trace_id
            .or_else(|| previous_trace_id.map(str::to_string))
            .unwrap_or_else(crate::trace::new_trace_id);
        crate::trace::validate_trace_id(&trace_id)?;
        let mut task_payload = task.clone();
        task_payload
            .as_object_mut()
            .ok_or_else(|| "原生任务必须是 JSON 对象".to_string())?
            .insert("traceId".to_string(), Value::String(trace_id.clone()));
        let payload = serde_json::to_string(&task_payload)
            .map_err(|error| format!("无法序列化原生任务：{error}"))?;
        let now = Utc::now().to_rfc3339();
        transaction
            .execute(
                "INSERT INTO runtime_tasks (workspace_scope, id, state, title, trace_id, payload, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
                 ON CONFLICT(workspace_scope, id) DO UPDATE SET
                   state=excluded.state, title=excluded.title, trace_id=excluded.trace_id,
                   payload=excluded.payload, updated_at=excluded.updated_at",
                params![workspace_scope, id, state, title, trace_id, payload, now],
            )
            .map_err(|error| format!("无法保存原生任务：{error}"))?;
        if previous_task.as_ref().map(|(state, _)| state.as_str()) != Some(state) {
            transaction
                .execute(
                    "UPDATE runtime_task_attempts SET finished_at=?3
                     WHERE workspace_scope=?1 AND task_id=?2 AND finished_at IS NULL",
                    params![workspace_scope, id, now],
                )
                .map_err(|error| format!("无法结束原生任务上一次尝试：{error}"))?;
            transaction
                .execute(
                    "INSERT INTO runtime_task_attempts
                     (id, workspace_scope, task_id, state, detail, started_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        Uuid::new_v4().to_string(),
                        workspace_scope,
                        id,
                        state,
                        task.get("result")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .chars()
                            .take(1000)
                            .collect::<String>(),
                        now,
                    ],
                )
                .map_err(|error| format!("无法记录原生任务状态变更：{error}"))?;
            crate::trace::record_trace_event_in_connection(
                transaction,
                workspace_scope,
                &crate::trace::TraceEventRecord {
                    trace_id: &trace_id,
                    entity_kind: "runtime_task",
                    entity_id: &id,
                    event_type: "task.synced",
                    state,
                    payload: &serde_json::json!({
                        "previousState": previous_task.as_ref().map(|(state, _)| state),
                        "source": "runtime-state-sync",
                    }),
                    created_at: &now,
                },
            )?;
        }
        let mut current_step_ids = HashSet::new();
        if let Some(steps) = task.get("steps").and_then(Value::as_array) {
            for (position, step) in steps.iter().enumerate() {
                let step_id = step
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|item| !item.trim().is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{id}:{position}"));
                current_step_ids.insert(step_id.clone());
                let step_state = step
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap_or("created");
                let detail = step.get("detail").and_then(Value::as_str).unwrap_or("");
                let checkpoint_json = serde_json::to_string(
                    step.get("checkpoint")
                        .filter(|checkpoint| checkpoint.is_object())
                        .unwrap_or(&Value::Null),
                )
                .map_err(|error| format!("无法序列化任务步骤检查点：{error}"))?;
                let previous = transaction
                    .query_row(
                        "SELECT state, detail, checkpoint_json FROM runtime_task_steps
                         WHERE workspace_scope=?1 AND task_id=?2 AND step_id=?3",
                        params![workspace_scope, id, step_id],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                            ))
                        },
                    )
                    .optional()
                    .map_err(|error| format!("无法读取任务步骤当前状态：{error}"))?;
                transaction
                    .execute(
                        "INSERT INTO runtime_task_steps
                         (workspace_scope, task_id, step_id, position, state, detail, checkpoint_json, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                         ON CONFLICT(workspace_scope, task_id, step_id) DO UPDATE SET
                           position=excluded.position, state=excluded.state, detail=excluded.detail,
                           checkpoint_json=excluded.checkpoint_json, updated_at=excluded.updated_at",
                        params![
                            workspace_scope,
                            id,
                            step_id,
                            position as i64,
                            step_state,
                            detail.chars().take(4000).collect::<String>(),
                            checkpoint_json,
                            now
                        ],
                    )
                    .map_err(|error| format!("无法保存原生任务步骤：{error}"))?;
                let changed = match previous.as_ref() {
                    Some((old_state, old_detail, old_checkpoint)) => {
                        old_state != step_state
                            || old_detail != detail
                            || old_checkpoint != &checkpoint_json
                    }
                    None => true,
                };
                if changed {
                    let revision = transaction
                        .query_row(
                            "SELECT COALESCE(MAX(revision), 0) + 1 FROM runtime_task_step_revisions
                             WHERE workspace_scope=?1 AND task_id=?2 AND step_id=?3",
                            params![workspace_scope, id, step_id],
                            |row| row.get::<_, i64>(0),
                        )
                        .map_err(|error| format!("无法计算任务步骤修订号：{error}"))?;
                    transaction
                        .execute(
                            "INSERT INTO runtime_task_step_revisions
                             (id, workspace_scope, task_id, step_id, revision, position, state, detail,
                              checkpoint_json, created_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                            params![
                                Uuid::new_v4().to_string(),
                                workspace_scope,
                                id,
                                step_id,
                                revision,
                                position as i64,
                                step_state,
                                detail.chars().take(4000).collect::<String>(),
                                checkpoint_json,
                                now,
                            ],
                        )
                        .map_err(|error| format!("无法记录任务步骤修订：{error}"))?;
                }
            }
        }
        let stale_step_ids = {
            let mut statement = transaction
                .prepare("SELECT step_id FROM runtime_task_steps WHERE workspace_scope=?1 AND task_id=?2")
                .map_err(|error| format!("无法读取任务现有步骤：{error}"))?;
            let rows = statement
                .query_map(params![workspace_scope, id], |row| row.get::<_, String>(0))
                .map_err(|error| format!("无法枚举任务现有步骤：{error}"))?
                .filter_map(Result::ok)
                .filter(|step_id| !current_step_ids.contains(step_id))
                .collect::<Vec<_>>();
            rows
        };
        for step_id in stale_step_ids {
            transaction
                .execute(
                    "DELETE FROM runtime_task_steps WHERE workspace_scope=?1 AND task_id=?2 AND step_id=?3",
                    params![workspace_scope, id, step_id],
                )
                .map_err(|error| format!("无法移除过期任务步骤：{error}"))?;
        }
        sync_runtime_task_checkpoints(transaction, workspace_scope, &id, task)?;
    }
    Ok(())
}

fn sync_runtime_task_checkpoints(
    transaction: &Transaction<'_>,
    workspace_scope: &str,
    task_id: &str,
    task: &Value,
) -> Result<(), String> {
    let checkpoints = task
        .get("checkpoints")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if checkpoints.len() > 512 {
        return Err("单个任务的检查点超过 512 个安全上限".to_string());
    }
    for (sequence, checkpoint) in checkpoints.iter().enumerate() {
        let checkpoint_id = runtime_value_string(checkpoint, "id", "任务检查点")?;
        let state = checkpoint
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        if !matches!(state, "pending" | "running" | "completed" | "failed") {
            return Err("任务检查点状态无效".to_string());
        }
        let payload = serde_json::to_string(checkpoint)
            .map_err(|error| format!("无法序列化任务检查点：{error}"))?;
        if payload.len() > MAX_INBOUND_RECORD_BYTES {
            return Err("单个任务检查点超过 512 KB 安全上限".to_string());
        }
        let payload_hash = format!("{:x}", Sha256::digest(payload.as_bytes()));
        let now = Utc::now().to_rfc3339();
        let completed_at = (state == "completed").then_some(now.as_str());
        transaction
            .execute(
                "INSERT INTO runtime_task_checkpoints
                 (workspace_scope, task_id, checkpoint_id, sequence, state, payload, payload_hash,
                  created_at, updated_at, completed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9)
                 ON CONFLICT(workspace_scope, task_id, checkpoint_id) DO UPDATE SET
                   sequence=excluded.sequence, state=excluded.state, payload=excluded.payload,
                   payload_hash=excluded.payload_hash, updated_at=excluded.updated_at,
                   completed_at=COALESCE(excluded.completed_at, runtime_task_checkpoints.completed_at)",
                params![
                    workspace_scope,
                    task_id,
                    checkpoint_id,
                    sequence as i64,
                    state,
                    payload,
                    payload_hash,
                    now,
                    completed_at,
                ],
            )
            .map_err(|error| format!("无法保存任务检查点：{error}"))?;
    }
    Ok(())
}

fn sync_runtime_schedule_group(
    transaction: &Transaction<'_>,
    workspace_scope: &str,
    schedules: &[Value],
    schedule_kind: &str,
) -> Result<(), String> {
    let current_ids = schedules
        .iter()
        .map(|schedule| runtime_value_string(schedule, "id", "原生日程"))
        .collect::<Result<HashSet<_>, _>>()?;
    let existing = {
        let mut statement = transaction
            .prepare(
                "SELECT id FROM runtime_schedules WHERE workspace_scope=?1 AND schedule_kind=?2",
            )
            .map_err(|error| format!("无法读取已登记原生日程：{error}"))?;
        let ids = statement
            .query_map(params![workspace_scope, schedule_kind], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| format!("无法枚举已登记原生日程：{error}"))?
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        ids
    };
    for id in existing.into_iter().filter(|id| !current_ids.contains(id)) {
        transaction
            .execute(
                "DELETE FROM runtime_schedules WHERE workspace_scope=?1 AND id=?2 AND schedule_kind=?3",
                params![workspace_scope, id, schedule_kind],
            )
            .map_err(|error| format!("无法移除已删除原生日程：{error}"))?;
    }
    for schedule in schedules {
        let id = runtime_value_string(schedule, "id", "原生日程")?;
        let enabled = schedule
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let next_run = normalize_runtime_time(schedule.get("nextRun").and_then(Value::as_str));
        let payload = serde_json::to_string(schedule)
            .map_err(|error| format!("无法序列化原生日程：{error}"))?;
        let payload_hash = format!("{:x}", Sha256::digest(payload.as_bytes()));
        let previous = transaction
            .query_row(
                "SELECT payload_hash, revision FROM runtime_schedules
                 WHERE workspace_scope=?1 AND id=?2 AND schedule_kind=?3",
                params![workspace_scope, id, schedule_kind],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()
            .map_err(|error| format!("无法读取原生日程修订：{error}"))?;
        let payload_changed = previous
            .as_ref()
            .map(|(hash, _)| hash.as_str() != payload_hash.as_str())
            .unwrap_or(true);
        let revision = previous
            .as_ref()
            .map(|(_, value)| if payload_changed { value + 1 } else { *value })
            .unwrap_or(1);
        let now = Utc::now().to_rfc3339();
        transaction
            .execute(
                "INSERT INTO runtime_schedules
                 (workspace_scope, id, schedule_kind, enabled, next_run, payload, payload_hash, revision, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(workspace_scope, id, schedule_kind) DO UPDATE SET
                   enabled=excluded.enabled, next_run=excluded.next_run, payload=excluded.payload,
                   payload_hash=excluded.payload_hash, revision=excluded.revision,
                   lease_owner=NULL, lease_expires_at=NULL, updated_at=excluded.updated_at",
                params![workspace_scope, id, schedule_kind, i64::from(enabled), next_run, payload, payload_hash, revision, now],
            )
            .map_err(|error| format!("无法保存原生日程：{error}"))?;
        if payload_changed {
            transaction
                .execute(
                    "INSERT INTO runtime_schedule_revisions
                     (id, workspace_scope, schedule_id, schedule_kind, revision, payload, payload_hash, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![Uuid::new_v4().to_string(), workspace_scope, id, schedule_kind, revision, payload, payload_hash, now],
                )
                .map_err(|error| format!("无法保存原生日程修订：{error}"))?;
        }
    }
    Ok(())
}

fn read_payloads(
    connection: &Connection,
    sql: &str,
    limit: Option<usize>,
) -> Result<Vec<Value>, String> {
    let mut statement = connection
        .prepare(sql)
        .map_err(|error| format!("无法准备快照查询：{error}"))?;
    let mut payloads = Vec::new();
    if let Some(limit) = limit {
        let rows = statement
            .query_map([limit as i64], |row| row.get::<_, String>(0))
            .map_err(|error| format!("无法读取快照：{error}"))?;
        for payload in rows.filter_map(Result::ok) {
            if let Ok(value) = serde_json::from_str(&payload) {
                payloads.push(value);
            }
        }
    } else {
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| format!("无法读取快照：{error}"))?;
        for payload in rows.filter_map(Result::ok) {
            if let Ok(value) = serde_json::from_str(&payload) {
                payloads.push(value);
            }
        }
    }
    Ok(payloads)
}

fn markdown_metadata(content: &str) -> (String, Vec<String>, Vec<String>) {
    let title = content
        .lines()
        .find_map(|line| line.strip_prefix("# ").map(str::trim))
        .filter(|title| !title.is_empty())
        .unwrap_or("无标题笔记")
        .nfc()
        .collect::<String>();
    let tag_regex = Regex::new(r"(?:^|\s)#([\p{L}\p{N}_/-]+)").expect("valid tag regex");
    let link_regex = Regex::new(r"\[\[([^\]|#]+)").expect("valid wiki link regex");
    let mut tags = tag_regex
        .captures_iter(content)
        .filter_map(|capture| {
            capture
                .get(1)
                .map(|value| value.as_str().nfc().collect::<String>())
        })
        .collect::<Vec<_>>();
    let mut links = link_regex
        .captures_iter(content)
        .filter_map(|capture| {
            capture
                .get(1)
                .map(|value| value.as_str().trim().nfc().collect::<String>())
        })
        .collect::<Vec<_>>();
    tags.sort();
    tags.dedup();
    links.sort();
    links.dedup();
    (title, tags, links)
}

fn fts_match_query(query: &str) -> Result<String, String> {
    if query.chars().count() > MAX_SEARCH_QUERY_CHARS {
        return Err("搜索词超过 512 个字符的安全上限".to_string());
    }
    let terms = query
        .split_whitespace()
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Err("搜索词不能为空".to_string());
    }
    Ok(terms.join(" AND "))
}

fn ensure_index_not_cancelled<F>(is_cancelled: &F) -> Result<(), String>
where
    F: Fn() -> bool,
{
    if is_cancelled() {
        Err("Vault 索引已取消".to_string())
    } else {
        Ok(())
    }
}

fn is_cjk(character: char) -> bool {
    matches!(
        character,
        '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{20000}'..='\u{2FA1F}'
    )
}

fn cjk_lexical_terms(value: &str) -> String {
    let mut terms = Vec::new();
    let mut run = Vec::new();
    let flush = |run: &mut Vec<char>, terms: &mut Vec<String>| {
        if run.is_empty() {
            return;
        }
        terms.extend(run.iter().map(char::to_string));
        terms.extend(run.windows(2).map(|pair| pair.iter().collect::<String>()));
        run.clear();
    };
    for character in value.nfc() {
        if is_cjk(character) {
            run.push(character);
        } else {
            flush(&mut run, &mut terms);
        }
    }
    flush(&mut run, &mut terms);
    terms.sort();
    terms.dedup();
    terms.join(" ")
}

// Local feature vectors are deterministic hashed bags of words and CJK 1-3 grams.
// They are rebuildable search features, not model embeddings.
fn stable_feature_hash(namespace: u8, bytes: impl IntoIterator<Item = u8>) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64 ^ u64::from(namespace);
    for byte in bytes {
        hash = stable_feature_hash_step(hash, byte);
    }
    hash
}

fn stable_feature_hash_step(hash: u64, byte: u8) -> u64 {
    (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
}

fn add_hashed_feature(vector: &mut [f32], hash: u64, weight: f32) {
    let index = (hash as usize) % vector.len();
    let sign = if hash & (1_u64 << 63) == 0 { 1.0 } else { -1.0 };
    vector[index] += weight * sign;
}

fn add_text_feature(vector: &mut [f32], namespace: u8, text: &str, weight: f32) {
    if !text.is_empty() {
        add_hashed_feature(vector, stable_feature_hash(namespace, text.bytes()), weight);
    }
}

fn add_cjk_feature(vector: &mut [f32], namespace: u8, characters: &[char], weight: f32) {
    let mut hash = 0xcbf29ce484222325_u64 ^ u64::from(namespace);
    for character in characters {
        let mut encoded = [0_u8; 4];
        for byte in character.encode_utf8(&mut encoded).bytes() {
            hash = stable_feature_hash_step(hash, byte);
        }
    }
    add_hashed_feature(vector, hash, weight);
}

fn flush_vector_word(vector: &mut [f32], word: &mut String, weight: f32) {
    if !word.is_empty() {
        add_text_feature(vector, b'w', word, weight * 1.4);
        word.clear();
    }
}

fn add_local_vector_text(vector: &mut [f32], value: &str, weight: f32, max_chars: usize) {
    let mut word = String::new();
    let mut previous = None;
    let mut previous_previous = None;
    for character in value.nfc().flat_map(char::to_lowercase).take(max_chars) {
        if is_cjk(character) {
            flush_vector_word(vector, &mut word, weight);
            add_cjk_feature(vector, b'1', &[character], weight * 0.35);
            if let Some(left) = previous {
                add_cjk_feature(vector, b'2', &[left, character], weight);
            }
            if let (Some(left), Some(middle)) = (previous_previous, previous) {
                add_cjk_feature(vector, b'3', &[left, middle, character], weight * 1.25);
            }
            previous_previous = previous;
            previous = Some(character);
        } else {
            previous = None;
            previous_previous = None;
            if character.is_alphanumeric() {
                if word.len() >= 128 {
                    flush_vector_word(vector, &mut word, weight);
                }
                word.push(character);
            } else {
                flush_vector_word(vector, &mut word, weight);
            }
        }
    }
    flush_vector_word(vector, &mut word, weight);
}

fn normalize_local_feature_vector(vector: &mut [f32]) -> bool {
    let norm = vector
        .iter()
        .map(|value| f64::from(*value) * f64::from(*value))
        .sum::<f64>()
        .sqrt();
    if !norm.is_finite() || norm <= f64::EPSILON {
        return false;
    }
    for value in vector {
        *value = (f64::from(*value) / norm) as f32;
    }
    true
}

fn note_local_feature_vector(
    relative_path: &str,
    title: &str,
    content: &str,
    tags: &[String],
    wiki_links: &[String],
) -> Vec<u8> {
    let mut vector = vec![0_f32; LOCAL_FEATURE_VECTOR_DIMENSIONS];
    add_local_vector_text(&mut vector, title, 3.0, usize::MAX);
    add_local_vector_text(&mut vector, relative_path, 1.5, usize::MAX);
    for tag in tags {
        add_local_vector_text(&mut vector, tag, 2.5, usize::MAX);
    }
    for link in wiki_links {
        add_local_vector_text(&mut vector, link, 2.0, usize::MAX);
    }
    add_local_vector_text(&mut vector, content, 1.0, MAX_LOCAL_VECTOR_CONTENT_CHARS);
    normalize_local_feature_vector(&mut vector);
    vector
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect::<Vec<_>>()
}

fn query_local_feature_vector(query: &str) -> Option<Vec<f32>> {
    let mut vector = vec![0_f32; LOCAL_FEATURE_VECTOR_DIMENSIONS];
    add_local_vector_text(&mut vector, query, 1.0, MAX_SEARCH_QUERY_CHARS);
    normalize_local_feature_vector(&mut vector).then_some(vector)
}

fn decode_local_feature_vector(version: i64, dimensions: i64, blob: &[u8]) -> Option<Vec<f32>> {
    if version != LOCAL_FEATURE_VECTOR_VERSION
        || dimensions != LOCAL_FEATURE_VECTOR_DIMENSIONS as i64
        || blob.len() != LOCAL_FEATURE_VECTOR_DIMENSIONS * std::mem::size_of::<f32>()
    {
        return None;
    }
    let vector = blob
        .chunks_exact(std::mem::size_of::<f32>())
        .map(|bytes| f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .collect::<Vec<_>>();
    let norm = vector
        .iter()
        .map(|value| f64::from(*value) * f64::from(*value))
        .sum::<f64>()
        .sqrt();
    (vector.iter().all(|value| value.is_finite()) && (0.99..=1.01).contains(&norm))
        .then_some(vector)
}

fn local_vector_similarity(query: &[f32], candidate: &[f32]) -> Option<f64> {
    if query.len() != candidate.len() {
        return None;
    }
    let similarity = query
        .iter()
        .zip(candidate)
        .map(|(left, right)| f64::from(*left) * f64::from(*right))
        .sum::<f64>();
    similarity
        .is_finite()
        .then_some(similarity.clamp(-1.0, 1.0))
}

fn lexical_fts_match_query(query: &str) -> Result<String, String> {
    if query.chars().count() > MAX_SEARCH_QUERY_CHARS {
        return Err("搜索词超过 512 个字符的安全上限".to_string());
    }
    let normalized = query.trim().nfc().collect::<String>();
    if normalized.is_empty() {
        return Err("搜索词不能为空".to_string());
    }
    let mut groups = Vec::new();
    for raw_term in normalized.split_whitespace() {
        let cjk = raw_term
            .chars()
            .filter(|character| is_cjk(*character))
            .collect::<Vec<_>>();
        if cjk.len() >= 2 {
            let pairs = cjk
                .windows(2)
                .map(|pair| format!("\"{}\"", pair.iter().collect::<String>()))
                .collect::<Vec<_>>();
            groups.push(format!("({})", pairs.join(" AND ")));
        } else {
            groups.push(format!("\"{}\"", raw_term.replace('"', "\"\"")));
        }
    }
    if groups.is_empty() {
        return Err("搜索词不能为空".to_string());
    }
    Ok(groups.join(" AND "))
}

fn strict_path_text<'a>(path: &'a Path, label: &str) -> Result<&'a str, String> {
    path.to_str()
        .ok_or_else(|| format!("{label}不是有效 UTF-8"))
}

fn canonical_index_root(root: &Path) -> Result<PathBuf, String> {
    let canonical = root
        .canonicalize()
        .map_err(|error| format!("无法规范化 Vault 根目录：{error}"))?;
    if !canonical.is_dir() {
        return Err("Vault 根路径不是目录".to_string());
    }
    strict_path_text(&canonical, "Vault 根目录")?;
    Ok(canonical)
}

fn normalize_queued_relative_path(value: &str) -> Result<String, String> {
    if value.is_empty() || value.contains('\0') || value.chars().any(char::is_control) {
        return Err("索引相对路径无效".to_string());
    }
    Ok(value.replace('\\', "/").nfc().collect())
}

fn validate_index_relative_path(value: &str) -> Result<PathBuf, String> {
    let normalized = normalize_queued_relative_path(value)?;
    if normalized != value {
        return Err("索引相对路径尚未规范化".to_string());
    }
    let relative = Path::new(value);
    if relative.as_os_str().is_empty() || relative.is_absolute() {
        return Err("索引路径必须是 Vault 内的相对路径".to_string());
    }
    if relative.components().any(|component| {
        !matches!(component, std::path::Component::Normal(_))
            || component
                .as_os_str()
                .to_str()
                .is_none_or(|value| value.starts_with('.'))
    }) {
        return Err("索引路径包含隐藏目录、无效 UTF-8 或目录跳转".to_string());
    }
    if !relative
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
    {
        return Err("索引队列只接受 Markdown 笔记".to_string());
    }
    Ok(relative.to_path_buf())
}

fn normalized_index_relative_path(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "索引文件越过 Vault 边界".to_string())?;
    let text = strict_path_text(relative, "索引文件路径")?;
    normalize_queued_relative_path(text)
}

fn resolve_index_target(root: &Path, relative_path: &str) -> Result<PathBuf, String> {
    let relative = validate_index_relative_path(relative_path)?;
    let mut target = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err("索引路径包含无效目录组件".to_string());
        };
        let direct = target.join(name);
        match fs::symlink_metadata(&direct) {
            Ok(_) => {
                target = direct;
                continue;
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(format!("无法解析索引目标：{error}")),
        }
        if !target.is_dir() {
            target = direct;
            continue;
        }
        let expected = name
            .to_str()
            .ok_or_else(|| "索引路径组件不是有效 UTF-8".to_string())?;
        let mut normalized_match = None;
        for entry in
            fs::read_dir(&target).map_err(|error| format!("无法读取索引目标目录：{error}"))?
        {
            let entry = entry.map_err(|error| format!("无法读取索引目标目录项：{error}"))?;
            let Some(candidate) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if candidate.nfc().eq(expected.chars()) {
                if normalized_match.is_some() {
                    return Err("Vault 中存在规范化后重名的笔记路径".to_string());
                }
                normalized_match = Some(entry.path());
            }
        }
        target = normalized_match.unwrap_or(direct);
    }
    Ok(target)
}

fn ensure_registered_vault_root(
    connection: &Connection,
    vault_id: &str,
    canonical_root: &Path,
) -> Result<(), String> {
    let registered = connection
        .query_row(
            "SELECT canonical_path FROM vault_registry WHERE id=?1",
            [vault_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("无法校验 Vault 注册路径：{error}"))?
        .ok_or_else(|| "Vault 尚未登记到本地索引注册表".to_string())?;
    if Path::new(&registered) != canonical_root {
        return Err("Vault 注册路径已变化，拒绝应用旧索引任务".to_string());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn enqueue_vault_index_change_in_connection(
    connection: &Connection,
    vault_id: &str,
    canonical_root: &str,
    relative_path: &str,
    change_kind: &str,
    trace_id: &str,
    replace_existing_trace: bool,
    available_at_ms: i64,
    now: &str,
) -> Result<(), String> {
    connection
        .execute(
            "INSERT INTO vault_index_changes
             (vault_id, canonical_root, relative_path, generation, change_kind, state,
              attempt_count, available_at_ms, claimed_at_ms, last_error, trace_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, 1, ?4, 'pending', 0, ?6, NULL, NULL, ?5, ?7, ?7)
             ON CONFLICT(vault_id, relative_path) DO UPDATE SET
               canonical_root=excluded.canonical_root,
               generation=vault_index_changes.generation+1,
               change_kind=excluded.change_kind,
               state='pending', attempt_count=0,
               available_at_ms=excluded.available_at_ms,
               claimed_at_ms=NULL, last_error=NULL,
               trace_id=CASE WHEN ?8 THEN excluded.trace_id
                             ELSE COALESCE(vault_index_changes.trace_id, excluded.trace_id) END,
               updated_at=excluded.updated_at",
            params![
                vault_id,
                canonical_root,
                relative_path,
                change_kind,
                trace_id,
                available_at_ms,
                now,
                replace_existing_trace
            ],
        )
        .map_err(|error| format!("无法持久化 Vault 索引变更：{error}"))?;
    let (change_id, generation, persisted_trace_id) = connection
        .query_row(
            "SELECT id, generation, trace_id FROM vault_index_changes
             WHERE vault_id=?1 AND relative_path=?2",
            params![vault_id, relative_path],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .map_err(|error| format!("无法读取 Vault 索引入队结果：{error}"))?;
    crate::trace::record_trace_event_in_connection(
        connection,
        DEFAULT_LOCAL_WORKSPACE_SCOPE,
        &crate::trace::TraceEventRecord {
            trace_id: &persisted_trace_id,
            entity_kind: "index_change",
            entity_id: &format!("{change_id}:{generation}"),
            event_type: "index.enqueued",
            state: "pending",
            payload: &serde_json::json!({
                "vaultId": vault_id,
                "relativePath": relative_path,
                "changeKind": change_kind,
                "generation": generation,
            }),
            created_at: now,
        },
    )?;
    Ok(())
}

fn dead_letter_exhausted_vault_index_changes(
    transaction: &Transaction<'_>,
    source_state: &str,
    default_error: &str,
    source: &str,
    now: &str,
) -> Result<usize, String> {
    if !matches!(source_state, "pending" | "processing") {
        return Err("Vault 索引死信来源状态无效".to_string());
    }
    let exhausted = {
        let mut statement = transaction
            .prepare(
                "SELECT id, vault_id, relative_path, generation, attempt_count, trace_id, last_error
                 FROM vault_index_changes
                 WHERE state=?1 AND attempt_count >= ?2
                 ORDER BY id",
            )
            .map_err(|error| format!("无法准备 Vault 索引死信查询：{error}"))?;
        let rows = statement
            .query_map(params![source_state, VAULT_INDEX_MAX_ATTEMPTS], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(|error| format!("无法读取 Vault 索引死信任务：{error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析 Vault 索引死信任务：{error}"))?
    };

    let mut dead_lettered = 0;
    for (id, vault_id, relative_path, generation, attempt_count, trace_id, last_error) in exhausted
    {
        let error = last_error
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| default_error.to_string());
        let updated = transaction
            .execute(
                "UPDATE vault_index_changes
                 SET state='dead_letter', claimed_at_ms=NULL, last_error=?1, updated_at=?2
                 WHERE id=?3 AND generation=?4 AND state=?5 AND attempt_count >= ?6",
                params![
                    error,
                    now,
                    id,
                    generation,
                    source_state,
                    VAULT_INDEX_MAX_ATTEMPTS,
                ],
            )
            .map_err(|db_error| format!("无法写入 Vault 索引死信状态：{db_error}"))?;
        if updated != 1 {
            continue;
        }
        crate::trace::record_trace_event_in_connection(
            transaction,
            DEFAULT_LOCAL_WORKSPACE_SCOPE,
            &crate::trace::TraceEventRecord {
                trace_id: &trace_id,
                entity_kind: "index_change",
                entity_id: &format!("{id}:{generation}"),
                event_type: "index.dead_lettered",
                state: "dead_letter",
                payload: &serde_json::json!({
                    "vaultId": vault_id,
                    "relativePath": relative_path,
                    "attemptCount": attempt_count,
                    "error": error,
                    "source": source,
                }),
                created_at: now,
            },
        )?;
        insert_operation_event_in_transaction(
            transaction,
            &OperationEvent {
                id: Uuid::new_v4().to_string(),
                task_id: None,
                trace_id: Some(trace_id),
                event_type: "vault.note.index".to_string(),
                state: "failed".to_string(),
                created_at: now.to_string(),
                vault_id: Some(vault_id),
                relative_path: Some(relative_path),
                detail: format!("Vault 索引任务进入死信：{error}"),
            },
        )?;
        dead_lettered += 1;
    }
    Ok(dead_lettered)
}

fn insert_operation_event_in_transaction(
    transaction: &Transaction<'_>,
    event: &OperationEvent,
) -> Result<(), String> {
    let mut event = event.clone();
    let trace_id = event
        .trace_id
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(crate::trace::new_trace_id);
    crate::trace::validate_trace_id(&trace_id)?;
    event.trace_id = Some(trace_id.clone());
    let payload = serde_json::to_string(&event)
        .map_err(|error| format!("无法序列化 Vault 索引事件：{error}"))?;
    transaction
        .execute(
            "INSERT INTO operation_events
             (id, task_id, trace_id, event_type, state, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                event.id,
                event.task_id,
                trace_id,
                event.event_type,
                event.state,
                payload,
                event.created_at
            ],
        )
        .map_err(|error| format!("无法写入 Vault 索引操作日志：{error}"))?;
    crate::trace::record_trace_event_in_connection(
        transaction,
        DEFAULT_LOCAL_WORKSPACE_SCOPE,
        &crate::trace::TraceEventRecord {
            trace_id: &trace_id,
            entity_kind: "operation_event",
            entity_id: &event.id,
            event_type: &event.event_type,
            state: &event.state,
            payload: &serde_json::json!({
                "taskId": event.task_id,
                "vaultId": event.vault_id,
                "relativePath": event.relative_path,
                "detail": event.detail,
            }),
            created_at: &event.created_at,
        },
    )?;
    if let Some(vault_id) = event.vault_id.as_deref() {
        crate::trace::record_trace_event_in_connection(
            transaction,
            DEFAULT_LOCAL_WORKSPACE_SCOPE,
            &crate::trace::TraceEventRecord {
                trace_id: &trace_id,
                entity_kind: "vault_operation",
                entity_id: &event.id,
                event_type: &event.event_type,
                state: &event.state,
                payload: &serde_json::json!({
                    "vaultId": vault_id,
                    "relativePath": event.relative_path,
                    "detail": event.detail,
                }),
                created_at: &event.created_at,
            },
        )?;
    }
    Ok(())
}

fn prepare_note_index(root: &Path, path: &Path) -> Result<Option<PreparedNoteIndex>, String> {
    let relative_path = normalized_index_relative_path(root, path)?;
    validate_index_relative_path(&relative_path)?;
    let canonical_root = canonical_index_root(root)?;
    let metadata =
        fs::symlink_metadata(path).map_err(|error| format!("无法读取索引文件元数据：{error}"))?;
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_file()
        || metadata.len() > MAX_INDEXED_NOTE_BYTES
    {
        return Ok(None);
    }
    let canonical_path = path
        .canonicalize()
        .map_err(|error| format!("无法规范化索引文件：{error}"))?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err("索引文件越过 Vault 边界".to_string());
    }
    let bytes = read_file_limited_for_runtime(&canonical_path)?;
    let Ok(content) = String::from_utf8(bytes.clone()) else {
        return Ok(None);
    };
    let (fallback_title, tags, links) = markdown_metadata(&content);
    let title = if fallback_title == "无标题笔记" {
        canonical_path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("无标题笔记")
            .nfc()
            .collect()
    } else {
        fallback_title
    };
    let modified_at = metadata
        .modified()
        .ok()
        .map(chrono::DateTime::<Utc>::from)
        .map(|value| value.to_rfc3339())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());
    let feature_vector = note_local_feature_vector(&relative_path, &title, &content, &tags, &links);
    Ok(Some(PreparedNoteIndex {
        relative_path,
        title,
        content_hash: format!("{:x}", Sha256::digest(&bytes)),
        modified_at,
        byte_length: metadata.len(),
        tags_json: serde_json::to_string(&tags).unwrap_or_else(|_| "[]".to_string()),
        wiki_links_json: serde_json::to_string(&links).unwrap_or_else(|_| "[]".to_string()),
        content,
        feature_vector,
    }))
}

fn upsert_prepared_note_index(
    transaction: &Transaction<'_>,
    vault_id: &str,
    note: &PreparedNoteIndex,
) -> Result<(), String> {
    transaction
        .execute(
            "INSERT INTO note_index
             (vault_id, relative_path, title, content_hash, modified_at, byte_length, tags_json, wiki_links_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(vault_id, relative_path) DO UPDATE SET
               title=excluded.title,
               content_hash=excluded.content_hash,
               modified_at=excluded.modified_at,
               byte_length=excluded.byte_length,
               tags_json=excluded.tags_json,
               wiki_links_json=excluded.wiki_links_json",
            params![
                vault_id,
                note.relative_path,
                note.title,
                note.content_hash,
                note.modified_at,
                note.byte_length,
                note.tags_json,
                note.wiki_links_json,
            ],
        )
        .map_err(|error| format!("无法更新笔记索引：{error}"))?;
    transaction
        .execute(
            "DELETE FROM note_fts WHERE vault_id=?1 AND relative_path=?2",
            params![vault_id, note.relative_path],
        )
        .map_err(|error| format!("无法刷新全文索引：{error}"))?;
    transaction
        .execute(
            "INSERT INTO note_fts (vault_id, relative_path, title, content)
             VALUES (?1, ?2, ?3, ?4)",
            params![vault_id, note.relative_path, note.title, note.content],
        )
        .map_err(|error| format!("无法写入全文索引：{error}"))?;
    transaction
        .execute(
            "DELETE FROM note_lexical_fts WHERE vault_id=?1 AND relative_path=?2",
            params![vault_id, note.relative_path],
        )
        .map_err(|error| format!("无法刷新中文词法索引：{error}"))?;
    let searchable = format!(
        "{}\n{}\n{}\n{}\n{}",
        note.relative_path, note.title, note.content, note.tags_json, note.wiki_links_json
    );
    let cjk_terms = cjk_lexical_terms(&searchable);
    transaction
        .execute(
            "INSERT INTO note_lexical_fts
             (vault_id, relative_path, title, content, tags, wiki_links, cjk_terms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                vault_id,
                note.relative_path,
                note.title,
                note.content,
                note.tags_json,
                note.wiki_links_json,
                cjk_terms,
            ],
        )
        .map_err(|error| format!("无法写入中文词法索引：{error}"))?;
    transaction
        .execute(
            "INSERT INTO note_feature_vectors
             (vault_id, relative_path, content_hash, representation_version,
              dimensions, vector_blob, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(vault_id, relative_path) DO UPDATE SET
               content_hash=excluded.content_hash,
               representation_version=excluded.representation_version,
               dimensions=excluded.dimensions,
               vector_blob=excluded.vector_blob,
               updated_at=excluded.updated_at",
            params![
                vault_id,
                note.relative_path,
                note.content_hash,
                LOCAL_FEATURE_VECTOR_VERSION,
                LOCAL_FEATURE_VECTOR_DIMENSIONS as i64,
                note.feature_vector,
                note.modified_at,
            ],
        )
        .map_err(|error| format!("无法写入本地特征向量：{error}"))?;
    Ok(())
}

fn delete_note_index_in_transaction(
    transaction: &Transaction<'_>,
    vault_id: &str,
    relative_path: &str,
) -> Result<(), String> {
    transaction
        .execute(
            "DELETE FROM note_fts WHERE vault_id=?1 AND relative_path=?2",
            params![vault_id, relative_path],
        )
        .map_err(|error| format!("无法删除全文索引项：{error}"))?;
    transaction
        .execute(
            "DELETE FROM note_lexical_fts WHERE vault_id=?1 AND relative_path=?2",
            params![vault_id, relative_path],
        )
        .map_err(|error| format!("无法删除中文词法索引项：{error}"))?;
    transaction
        .execute(
            "DELETE FROM note_feature_vectors WHERE vault_id=?1 AND relative_path=?2",
            params![vault_id, relative_path],
        )
        .map_err(|error| format!("无法删除本地特征向量：{error}"))?;
    transaction
        .execute(
            "DELETE FROM note_index WHERE vault_id=?1 AND relative_path=?2",
            params![vault_id, relative_path],
        )
        .map_err(|error| format!("无法删除笔记索引项：{error}"))?;
    Ok(())
}

#[cfg(test)]
fn index_note_in_connection(
    connection: &Connection,
    vault_id: &str,
    root: &Path,
    path: &Path,
) -> Result<bool, String> {
    let Some(note) = prepare_note_index(root, path)? else {
        return Ok(false);
    };
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| format!("无法开始增量索引事务：{error}"))?;
    upsert_prepared_note_index(&transaction, vault_id, &note)?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交增量索引事务：{error}"))?;
    Ok(true)
}

#[tauri::command]
pub fn save_workspace_snapshot(
    database: State<'_, RuntimeDatabase>,
    snapshot: WorkspaceSnapshot,
) -> Result<(), String> {
    let workspace_scope = database.local_workspace_scope()?;
    validate_records(&snapshot.tasks, "任务")?;
    validate_records(&snapshot.messages, "消息")?;
    validate_records(&snapshot.approvals, "审批")?;
    validate_records(&snapshot.operation_logs, "操作日志")?;
    let client_state_bytes = serde_json::to_vec(&snapshot.client_state)
        .map_err(|error| format!("无法序列化客户端工作区状态：{error}"))?;
    if client_state_bytes.len() > MAX_RECORD_BYTES {
        return Err("客户端工作区状态超过 2 MB 安全上限".to_string());
    }
    let payload = serde_json::to_string(&snapshot)
        .map_err(|error| format!("无法序列化本地工作区：{error}"))?;
    if payload.len() > 32 * 1024 * 1024 {
        return Err("本地工作区快照超过 32 MB 安全上限".to_string());
    }
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    connection
        .execute(
            "INSERT INTO workspace_snapshots (workspace_scope, payload, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(workspace_scope) DO UPDATE SET payload=excluded.payload, updated_at=excluded.updated_at",
            params![workspace_scope, payload, Utc::now().to_rfc3339()],
        )
        .map_err(|error| format!("无法保存本地工作区：{error}"))?;
    Ok(())
}

#[tauri::command]
pub fn sync_runtime_state(
    database: State<'_, RuntimeDatabase>,
    tasks: Vec<Value>,
    schedules: Vec<Value>,
    report_subscriptions: Vec<Value>,
    scheduler_enabled: bool,
) -> Result<(), String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.sync_runtime_state(
        &workspace_scope,
        &tasks,
        &schedules,
        &report_subscriptions,
        scheduler_enabled,
    )
}

#[tauri::command]
pub fn sync_managed_resources(
    database: State<'_, RuntimeDatabase>,
    snapshot: ManagedResourceSnapshotInput,
) -> Result<ManagedResourceSnapshot, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.sync_managed_resources(&workspace_scope, &snapshot)
}

#[tauri::command]
pub fn load_managed_resources(
    database: State<'_, RuntimeDatabase>,
) -> Result<ManagedResourceSnapshot, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.load_managed_resources(&workspace_scope)
}

#[tauri::command]
pub fn recover_interrupted_runtime_tasks(
    database: State<'_, RuntimeDatabase>,
) -> Result<Vec<RuntimeTaskRecovery>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.recover_interrupted_runtime_tasks(&workspace_scope)
}

#[tauri::command]
pub fn resolve_runtime_task_recovery(
    database: State<'_, RuntimeDatabase>,
    task_id: String,
    resolution: String,
) -> Result<(), String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.resolve_runtime_task_recovery(&workspace_scope, task_id.trim(), resolution.trim())
}

#[tauri::command]
pub fn upsert_inbound_content_record(
    database: State<'_, RuntimeDatabase>,
    record: InboundContentRecordInput,
) -> Result<InboundContentRecordReceipt, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.upsert_inbound_content_record(&workspace_scope, &record)
}

#[tauri::command]
pub fn load_workspace_snapshot(
    database: State<'_, RuntimeDatabase>,
) -> Result<Option<WorkspaceSnapshot>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let scoped = connection
        .query_row(
            "SELECT payload FROM workspace_snapshots WHERE workspace_scope=?1",
            [&workspace_scope],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("无法读取本地工作区：{error}"))?;
    if let Some(payload) = scoped {
        return serde_json::from_str::<WorkspaceSnapshot>(&payload)
            .map(Some)
            .map_err(|error| format!("本地工作区快照损坏：{error}"));
    }
    let legacy_claimed = connection
        .query_row(
            "SELECT value FROM workspace_state WHERE key='legacy_workspace_claimed_by'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("无法读取旧工作区归属：{error}"))?;
    if legacy_claimed.is_some() {
        return Ok(None);
    }
    let selected_task_id = connection
        .query_row(
            "SELECT value FROM workspace_state WHERE key='selected_task_id'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("无法读取当前任务：{error}"))?;
    let client_state = connection
        .query_row(
            "SELECT value FROM workspace_state WHERE key='client_state'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("无法读取客户端工作区状态：{error}"))?
        .and_then(|value| serde_json::from_str::<Value>(&value).ok())
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    let has_records: i64 = connection
        .query_row(
            "SELECT
               (SELECT COUNT(*) FROM tasks) +
               (SELECT COUNT(*) FROM approvals) +
               (SELECT COUNT(*) FROM secretary_messages)",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("无法检查工作区快照：{error}"))?;
    if selected_task_id.is_none() && has_records == 0 {
        return Ok(None);
    }
    let legacy = WorkspaceSnapshot {
        tasks: read_payloads(&connection, "SELECT payload FROM tasks ORDER BY updated_at", None)?,
        messages: read_payloads(
            &connection,
            "SELECT payload FROM secretary_messages ORDER BY created_at",
            None,
        )?,
        approvals: read_payloads(
            &connection,
            "SELECT payload FROM approvals ORDER BY updated_at",
            None,
        )?,
        operation_logs: read_payloads(
            &connection,
            "SELECT payload FROM operation_events WHERE event_type='workspace.operation' ORDER BY created_at DESC LIMIT ?1",
            Some(1000),
        )?,
        selected_task_id: selected_task_id.unwrap_or_default(),
        client_state,
    };
    let payload =
        serde_json::to_string(&legacy).map_err(|error| format!("无法迁移旧工作区：{error}"))?;
    connection
        .execute(
            "INSERT INTO workspace_snapshots (workspace_scope, payload, updated_at) VALUES (?1, ?2, ?3)",
            params![workspace_scope, payload, Utc::now().to_rfc3339()],
        )
        .map_err(|error| format!("无法保存迁移后的本地工作区：{error}"))?;
    connection
        .execute(
            "INSERT INTO workspace_state (key, value, updated_at)
             VALUES ('legacy_workspace_claimed_by', ?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
            params![workspace_scope, Utc::now().to_rfc3339()],
        )
        .map_err(|error| format!("无法登记旧工作区归属：{error}"))?;
    Ok(Some(legacy))
}

#[tauri::command]
pub fn load_application_authorization(
    database: State<'_, RuntimeDatabase>,
) -> Result<ApplicationAuthorizationState, String> {
    database.application_authorization()
}

#[tauri::command]
pub fn database_health(database: State<'_, RuntimeDatabase>) -> Result<DatabaseHealth, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.health(&workspace_scope)
}

#[tauri::command]
pub fn backup_local_database(
    database: State<'_, RuntimeDatabase>,
) -> Result<DatabaseBackupResult, String> {
    database.backup()
}

#[tauri::command]
pub fn list_database_backups(
    database: State<'_, RuntimeDatabase>,
) -> Result<Vec<DatabaseBackupInfo>, String> {
    database.list_backups()
}

#[tauri::command]
pub fn preflight_database_restore(
    database: State<'_, RuntimeDatabase>,
    backup_path: String,
) -> Result<DatabaseRestorePreflight, String> {
    database.preflight_restore(&backup_path)
}

#[tauri::command]
pub fn restore_local_database(
    database: State<'_, RuntimeDatabase>,
    backup_path: String,
) -> Result<DatabaseRestoreResult, String> {
    database.restore(&backup_path)
}

#[tauri::command]
pub fn query_long_term_memory(
    database: State<'_, RuntimeDatabase>,
    query: Option<String>,
    include_inactive: Option<bool>,
    limit: Option<usize>,
) -> Result<Vec<LongTermMemoryRecord>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.query_long_term_memory(
        &workspace_scope,
        query.as_deref().unwrap_or(""),
        include_inactive.unwrap_or(false),
        limit.unwrap_or(100),
    )
}

#[tauri::command]
pub fn govern_long_term_memory(
    database: State<'_, RuntimeDatabase>,
    input: LongTermMemoryGovernanceInput,
) -> Result<(), String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.govern_long_term_memory(&workspace_scope, &input)
}

#[tauri::command]
pub fn export_long_term_memory(
    database: State<'_, RuntimeDatabase>,
    include_inactive: Option<bool>,
) -> Result<String, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let records = database.query_long_term_memory(
        &workspace_scope,
        "",
        include_inactive.unwrap_or(true),
        1000,
    )?;
    serde_json::to_string_pretty(&serde_json::json!({
        "format": "yunspire-long-term-memory-v1",
        "exportedAt": Utc::now().to_rfc3339(),
        "records": records,
    }))
    .map_err(|error| format!("无法导出长期记忆：{error}"))
}

#[tauri::command]
pub fn long_term_memory_metrics(
    database: State<'_, RuntimeDatabase>,
) -> Result<LongTermMemoryMetrics, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.long_term_memory_metrics(&workspace_scope)
}

#[tauri::command]
pub fn read_optimization_evidence(
    database: State<'_, RuntimeDatabase>,
    limit: Option<usize>,
) -> Result<OptimizationEvidenceBatch, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.optimization_evidence(&workspace_scope, limit.unwrap_or(240))
}

#[tauri::command]
pub fn create_optimization_candidate(
    database: State<'_, RuntimeDatabase>,
    input: OptimizationCandidateInput,
) -> Result<OptimizationCandidateResult, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.create_optimization_candidate(&workspace_scope, input)
}

#[tauri::command]
pub fn evaluate_optimization_candidate(
    database: State<'_, RuntimeDatabase>,
    candidate_id: String,
) -> Result<OptimizationEvaluationResult, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.evaluate_optimization_candidate(&workspace_scope, candidate_id.trim())
}

#[tauri::command]
pub fn load_optimization_profile(
    database: State<'_, RuntimeDatabase>,
) -> Result<OptimizationProfileResult, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.load_optimization_profile(&workspace_scope)
}

#[tauri::command]
pub fn apply_optimization_candidate(
    database: State<'_, RuntimeDatabase>,
    candidate_id: String,
) -> Result<OptimizationProfileResult, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.apply_optimization_candidate(&workspace_scope, candidate_id.trim())
}

#[tauri::command]
pub fn rollback_optimization_profile(
    database: State<'_, RuntimeDatabase>,
    target_version: Option<i64>,
) -> Result<OptimizationProfileResult, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.rollback_optimization_profile(&workspace_scope, target_version)
}

#[tauri::command]
pub fn list_optimization_versions(
    database: State<'_, RuntimeDatabase>,
    limit: Option<usize>,
) -> Result<Vec<OptimizationVersion>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.list_optimization_versions(&workspace_scope, limit.unwrap_or(30))
}

#[tauri::command]
pub fn poll_due_runtime_schedules(
    database: State<'_, RuntimeDatabase>,
) -> Result<Vec<DueRuntimeSchedule>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    database.claim_due_runtime_schedules(&workspace_scope, 32)
}

fn indexed_search_candidate_signals(
    candidate: &IndexedSearchCandidate,
    normalized_query: &str,
    now: &chrono::DateTime<Utc>,
) -> (f64, f64, f64) {
    let title = candidate.title.nfc().collect::<String>().to_lowercase();
    let path = candidate
        .relative_path
        .nfc()
        .collect::<String>()
        .to_lowercase();
    let tags = candidate.tags.join(" ").to_lowercase();
    let links = candidate.wiki_links.join(" ").to_lowercase();
    let title_path_bonus = if title == normalized_query {
        12.0
    } else if title.contains(normalized_query) {
        8.0
    } else if path.contains(normalized_query) {
        6.0
    } else {
        0.0
    };
    let relation_bonus = if tags.contains(normalized_query) {
        4.0
    } else {
        0.0
    } + if links.contains(normalized_query) {
        3.0
    } else {
        0.0
    };
    let recency_bonus = chrono::DateTime::parse_from_rfc3339(&candidate.modified_at)
        .ok()
        .map(|modified| {
            let age_days = now
                .signed_duration_since(modified.with_timezone(&Utc))
                .num_days()
                .max(0) as f64;
            1.0 / (1.0 + age_days / 30.0)
        })
        .unwrap_or(0.0);
    (title_path_bonus, relation_bonus, recency_bonus)
}

fn load_lexical_search_candidates(
    connection: &Connection,
    scoped: Option<&str>,
    query: &str,
    candidate_limit: i64,
) -> Result<Vec<IndexedSearchCandidate>, String> {
    let lexical_match_query = lexical_fts_match_query(query)?;
    let legacy_match_query = fts_match_query(query)?;
    let lexical_sql = if scoped.is_some() {
        "SELECT f.vault_id, f.relative_path, f.title,
                snippet(note_lexical_fts, 3, '', '', '…', 24), i.modified_at,
                bm25(note_lexical_fts), i.tags_json, i.wiki_links_json
         FROM note_lexical_fts f
         JOIN note_index i ON i.vault_id=f.vault_id AND i.relative_path=f.relative_path
         WHERE note_lexical_fts MATCH ?1 AND f.vault_id=?2
         ORDER BY bm25(note_lexical_fts) LIMIT ?3"
    } else {
        "SELECT f.vault_id, f.relative_path, f.title,
                snippet(note_lexical_fts, 3, '', '', '…', 24), i.modified_at,
                bm25(note_lexical_fts), i.tags_json, i.wiki_links_json
         FROM note_lexical_fts f
         JOIN note_index i ON i.vault_id=f.vault_id AND i.relative_path=f.relative_path
         WHERE note_lexical_fts MATCH ?1
         ORDER BY bm25(note_lexical_fts) LIMIT ?2"
    };
    let mut statement = connection
        .prepare(lexical_sql)
        .map_err(|error| format!("无法准备中文混合搜索：{error}"))?;
    let map_row = |row: &rusqlite::Row<'_>| {
        let tags_json = row.get::<_, String>(6)?;
        let wiki_links_json = row.get::<_, String>(7)?;
        Ok(IndexedSearchCandidate {
            vault_id: row.get(0)?,
            relative_path: row.get(1)?,
            title: row.get(2)?,
            excerpt: row.get(3)?,
            modified_at: row.get(4)?,
            lexical_score: Some(row.get(5)?),
            vector_similarity: None,
            tags: serde_json::from_str(&tags_json).unwrap_or_default(),
            wiki_links: serde_json::from_str(&wiki_links_json).unwrap_or_default(),
        })
    };
    let mut candidates = if let Some(vault_id) = scoped {
        statement
            .query_map(
                params![lexical_match_query, vault_id, candidate_limit],
                map_row,
            )
            .map_err(|error| format!("中文混合搜索失败：{error}"))?
            .filter_map(Result::ok)
            .collect::<Vec<_>>()
    } else {
        statement
            .query_map(params![lexical_match_query, candidate_limit], map_row)
            .map_err(|error| format!("中文混合搜索失败：{error}"))?
            .filter_map(Result::ok)
            .collect::<Vec<_>>()
    };
    drop(statement);

    // Existing installations may search before the startup reindex has populated the new
    // lexical table. Keep the original FTS index as a temporary, read-only fallback.
    if candidates.is_empty() {
        let legacy_sql = if scoped.is_some() {
            "SELECT f.vault_id, f.relative_path, f.title,
                    snippet(note_fts, 3, '', '', '…', 24), i.modified_at,
                    bm25(note_fts), i.tags_json, i.wiki_links_json
             FROM note_fts f
             JOIN note_index i ON i.vault_id=f.vault_id AND i.relative_path=f.relative_path
             WHERE note_fts MATCH ?1 AND f.vault_id=?2
             ORDER BY bm25(note_fts) LIMIT ?3"
        } else {
            "SELECT f.vault_id, f.relative_path, f.title,
                    snippet(note_fts, 3, '', '', '…', 24), i.modified_at,
                    bm25(note_fts), i.tags_json, i.wiki_links_json
             FROM note_fts f
             JOIN note_index i ON i.vault_id=f.vault_id AND i.relative_path=f.relative_path
             WHERE note_fts MATCH ?1
             ORDER BY bm25(note_fts) LIMIT ?2"
        };
        let mut legacy = connection
            .prepare(legacy_sql)
            .map_err(|error| format!("无法准备兼容全文搜索：{error}"))?;
        candidates = if let Some(vault_id) = scoped {
            legacy
                .query_map(
                    params![legacy_match_query, vault_id, candidate_limit],
                    map_row,
                )
                .map_err(|error| format!("兼容全文搜索失败：{error}"))?
                .filter_map(Result::ok)
                .collect()
        } else {
            legacy
                .query_map(params![legacy_match_query, candidate_limit], map_row)
                .map_err(|error| format!("兼容全文搜索失败：{error}"))?
                .filter_map(Result::ok)
                .collect()
        };
    }
    Ok(candidates)
}

fn load_vector_search_candidates(
    connection: &Connection,
    scoped: Option<&str>,
    query: &str,
) -> Result<Vec<IndexedSearchCandidate>, String> {
    let Some(query_vector) = query_local_feature_vector(query) else {
        return Ok(Vec::new());
    };
    let vector_sql = if scoped.is_some() {
        "SELECT i.vault_id, i.relative_path, i.title,
                COALESCE((
                  SELECT substr(f.content, 1, 320) FROM note_fts f
                  WHERE f.vault_id=i.vault_id AND f.relative_path=i.relative_path LIMIT 1
                ), ''),
                i.modified_at, i.tags_json, i.wiki_links_json,
                v.representation_version, v.dimensions, v.vector_blob
         FROM note_feature_vectors v
         JOIN note_index i
           ON i.vault_id=v.vault_id AND i.relative_path=v.relative_path
          AND i.content_hash=v.content_hash
         WHERE v.vault_id=?1"
    } else {
        "SELECT i.vault_id, i.relative_path, i.title,
                COALESCE((
                  SELECT substr(f.content, 1, 320) FROM note_fts f
                  WHERE f.vault_id=i.vault_id AND f.relative_path=i.relative_path LIMIT 1
                ), ''),
                i.modified_at, i.tags_json, i.wiki_links_json,
                v.representation_version, v.dimensions, v.vector_blob
         FROM note_feature_vectors v
         JOIN note_index i
           ON i.vault_id=v.vault_id AND i.relative_path=v.relative_path
          AND i.content_hash=v.content_hash"
    };
    let mut statement = connection
        .prepare(vector_sql)
        .map_err(|error| format!("无法准备本地特征向量搜索：{error}"))?;
    let map_row = |row: &rusqlite::Row<'_>| {
        let vault_id = row.get::<_, String>(0)?;
        let relative_path = row.get::<_, String>(1)?;
        let title = row.get::<_, String>(2)?;
        let excerpt = row.get::<_, String>(3)?;
        let modified_at = row.get::<_, String>(4)?;
        let tags_json = row.get::<_, String>(5)?;
        let wiki_links_json = row.get::<_, String>(6)?;
        let version = row.get::<_, i64>(7)?;
        let dimensions = row.get::<_, i64>(8)?;
        let blob = row.get::<_, Vec<u8>>(9)?;
        let similarity = decode_local_feature_vector(version, dimensions, &blob)
            .and_then(|candidate| local_vector_similarity(&query_vector, &candidate));
        Ok(similarity
            .filter(|score| *score >= MIN_LOCAL_VECTOR_SIMILARITY)
            .map(|score| IndexedSearchCandidate {
                vault_id,
                relative_path,
                title,
                excerpt,
                modified_at,
                lexical_score: None,
                vector_similarity: Some(score),
                tags: serde_json::from_str(&tags_json).unwrap_or_default(),
                wiki_links: serde_json::from_str(&wiki_links_json).unwrap_or_default(),
            }))
    };
    let candidates = if let Some(vault_id) = scoped {
        statement
            .query_map([vault_id], map_row)
            .map_err(|error| format!("本地特征向量搜索失败：{error}"))?
            .filter_map(Result::ok)
            .flatten()
            .collect()
    } else {
        statement
            .query_map([], map_row)
            .map_err(|error| format!("本地特征向量搜索失败：{error}"))?
            .filter_map(Result::ok)
            .flatten()
            .collect()
    };
    Ok(candidates)
}

fn indexed_search_in_connection(
    connection: &Connection,
    vault_id: Option<&str>,
    query: &str,
    max_results: usize,
) -> Result<Vec<IndexedSearchResult>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("搜索词不能为空".to_string());
    }
    if query.chars().count() > MAX_SEARCH_QUERY_CHARS {
        return Err("搜索词超过 512 个字符的安全上限".to_string());
    }
    let scoped = vault_id.filter(|value| *value != "all");
    let candidate_limit = (max_results * 5).min(1_000);
    let normalized_query = query.nfc().collect::<String>().to_lowercase();
    let now = Utc::now();
    let mut lexical_candidates =
        load_lexical_search_candidates(connection, scoped, query, candidate_limit as i64)?;
    lexical_candidates.sort_by(|left, right| {
        let left_signals = indexed_search_candidate_signals(left, &normalized_query, &now);
        let right_signals = indexed_search_candidate_signals(right, &normalized_query, &now);
        let left_score = -left.lexical_score.unwrap_or_default()
            + left_signals.0
            + left_signals.1
            + left_signals.2;
        let right_score = -right.lexical_score.unwrap_or_default()
            + right_signals.0
            + right_signals.1
            + right_signals.2;
        right_score
            .partial_cmp(&left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    lexical_candidates.truncate(candidate_limit);

    let mut vector_candidates = match load_vector_search_candidates(connection, scoped, query) {
        Ok(candidates) => candidates,
        Err(error) => {
            log::warn!("本地特征向量不可用，继续使用 FTS：{error}");
            Vec::new()
        }
    };
    vector_candidates.sort_by(|left, right| {
        right
            .vector_similarity
            .unwrap_or_default()
            .partial_cmp(&left.vector_similarity.unwrap_or_default())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let left_signals = indexed_search_candidate_signals(left, &normalized_query, &now);
                let right_signals =
                    indexed_search_candidate_signals(right, &normalized_query, &now);
                (right_signals.0 + right_signals.1 + right_signals.2)
                    .partial_cmp(&(left_signals.0 + left_signals.1 + left_signals.2))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    vector_candidates.truncate(candidate_limit);

    let lexical_ranks = lexical_candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            (
                (candidate.vault_id.clone(), candidate.relative_path.clone()),
                index + 1,
            )
        })
        .collect::<HashMap<_, _>>();
    let vector_ranks = vector_candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            (
                (candidate.vault_id.clone(), candidate.relative_path.clone()),
                index + 1,
            )
        })
        .collect::<HashMap<_, _>>();
    let mut fused = HashMap::new();
    for candidate in lexical_candidates {
        fused.insert(
            (candidate.vault_id.clone(), candidate.relative_path.clone()),
            candidate,
        );
    }
    for candidate in vector_candidates {
        let key = (candidate.vault_id.clone(), candidate.relative_path.clone());
        fused
            .entry(key)
            .and_modify(|existing: &mut IndexedSearchCandidate| {
                existing.vector_similarity = candidate.vector_similarity;
                if existing.excerpt.is_empty() {
                    existing.excerpt.clone_from(&candidate.excerpt);
                }
            })
            .or_insert(candidate);
    }

    let mut results = fused
        .into_iter()
        .map(|(key, candidate)| {
            let lexical_rank = lexical_ranks.get(&key).copied();
            let vector_rank = vector_ranks.get(&key).copied();
            let lexical_rrf = lexical_rank
                .map(|rank| 1.0 / (RRF_K + rank as f64))
                .unwrap_or(0.0);
            let vector_rrf = vector_rank
                .map(|rank| 1.0 / (RRF_K + rank as f64))
                .unwrap_or(0.0);
            let (title_path_bonus, relation_bonus, recency_bonus) =
                indexed_search_candidate_signals(&candidate, &normalized_query, &now);
            IndexedSearchResult {
                vault_id: candidate.vault_id,
                relative_path: candidate.relative_path,
                title: candidate.title,
                excerpt: candidate.excerpt,
                modified_at: candidate.modified_at,
                score: lexical_rrf + vector_rrf,
                tags: candidate.tags,
                wiki_links: candidate.wiki_links,
                source_kind: "obsidian_markdown".to_string(),
                ranking_signals: IndexedSearchSignals {
                    lexical_rank,
                    vector_rank,
                    lexical_rrf,
                    vector_rrf,
                    vector_similarity: candidate.vector_similarity,
                    title_path_bonus,
                    relation_bonus,
                    recency_bonus,
                    vector_kind: "local_feature_hash_v1",
                },
            }
        })
        .collect::<Vec<_>>();
    results.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let left_signals = &left.ranking_signals;
                let right_signals = &right.ranking_signals;
                (right_signals.title_path_bonus
                    + right_signals.relation_bonus
                    + right_signals.recency_bonus)
                    .partial_cmp(
                        &(left_signals.title_path_bonus
                            + left_signals.relation_bonus
                            + left_signals.recency_bonus),
                    )
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| right.modified_at.cmp(&left.modified_at))
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    results.truncate(max_results.clamp(1, 200));
    Ok(results)
}

#[tauri::command]
pub fn indexed_search(
    database: State<'_, RuntimeDatabase>,
    vault_id: Option<String>,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<IndexedSearchResult>, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    indexed_search_in_connection(
        &connection,
        vault_id.as_deref(),
        &query,
        limit.unwrap_or(50).clamp(1, 200),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{CommandBudget, CommandOrigin};
    use serde_json::json;

    fn test_database(path: &Path) -> RuntimeDatabase {
        let connection = Connection::open(path).expect("open temporary sqlite");
        connection
            .execute_batch("PRAGMA foreign_keys=ON;")
            .expect("enable foreign keys");
        run_migrations(&connection).expect("run migrations");
        RuntimeDatabase {
            connection: Mutex::new(connection),
            path: path.to_path_buf(),
        }
    }

    fn test_vault(id: &str, root: &Path) -> VaultDescriptor {
        let canonical = root.canonicalize().expect("canonicalize test vault");
        VaultDescriptor {
            id: id.to_string(),
            name: id.to_string(),
            path: canonical
                .to_str()
                .expect("test vault path is utf8")
                .to_string(),
            note_count: 0,
            attachment_count: 0,
            connection_state: "connected".to_string(),
            is_open: false,
            last_indexed_at: Utc::now().to_rfc3339(),
            last_error: None,
        }
    }

    fn register_test_vault(database: &RuntimeDatabase, id: &str, root: &Path) -> VaultDescriptor {
        let vault = test_vault(id, root);
        database
            .sync_vault_registry(std::slice::from_ref(&vault))
            .expect("register test vault");
        vault
    }

    fn force_vault_index_queue_due(database: &RuntimeDatabase) {
        database
            .connection
            .lock()
            .expect("lock test database")
            .execute(
                "UPDATE vault_index_changes SET available_at_ms=0 WHERE state='pending'",
                [],
            )
            .expect("make queue due");
    }

    fn snapshot(skills: Vec<Value>) -> ManagedResourceSnapshotInput {
        ManagedResourceSnapshotInput {
            custom_skills: skills,
            schedules: Vec::new(),
            report_subscriptions: Vec::new(),
            reports: Vec::new(),
            assistant_profile: json!({"name": "AI助手"}),
            optimization_profile: json!({}),
            optimization_draft: json!({}),
        }
    }

    #[test]
    fn unicode_metadata_and_paths_are_normalized_without_replacement_characters() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let root = directory.path();
        let filename = "知识-Cafe\u{301}-🧭.md";
        let path = root.join(filename);
        fs::write(
            &path,
            "# 知识 Cafe\u{301} 🧭\n\n#标签 [[关联 Cafe\u{301} 🧩]]",
        )
        .expect("write unicode note");
        let connection = Connection::open_in_memory().expect("open sqlite");
        run_migrations(&connection).expect("run migrations");
        assert!(
            index_note_in_connection(&connection, "vault-unicode", root, &path)
                .expect("index unicode note")
        );
        let (relative_path, title, links): (String, String, String) = connection
            .query_row(
                "SELECT relative_path, title, wiki_links_json FROM note_index WHERE vault_id='vault-unicode'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("read unicode index");
        assert_eq!(relative_path, "知识-Café-🧭.md");
        assert_eq!(title, "知识 Café 🧭");
        assert!(links.contains("关联 Café 🧩"));
        assert!(!format!("{relative_path}{title}{links}").contains('\u{fffd}'));
        assert_eq!(
            normalize_queued_relative_path("目录\\Cafe\u{301}\\笔记.md")
                .expect("normalize windows separators"),
            "目录/Café/笔记.md"
        );
    }

    #[test]
    fn latest_migrations_are_incremental_and_preserve_version_21_data() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let path = directory.path().join("runtime.sqlite");
        let database = test_database(&path);
        let connection = database.connection.lock().expect("lock sqlite");
        connection
            .execute_batch(
                "PRAGMA foreign_keys=OFF;
                 DROP TABLE assistant_requests;
                 DROP TABLE assistant_conversations;
                 DROP TABLE note_feature_vectors;
                 DROP TABLE memory_reflection_jobs;
                 DROP TABLE memory_record_revisions;
                 DROP TABLE memory_fts;
                 DROP TABLE memory_records;
                 DROP TABLE note_lexical_fts;
                 DROP TABLE vault_index_changes;
                 DROP TABLE skill_lifecycle_audit;
                 DROP TABLE skill_approvals;
                 DROP TABLE skill_evaluations;
                 DROP TABLE skill_versions;
                 DROP TABLE skill_registry;
                 DROP TABLE runtime_trace_events;
                 DROP TABLE runtime_trace_bindings;
                 DROP TABLE runtime_traces;
                 CREATE TABLE migration_sentinel (value TEXT NOT NULL);
                 INSERT INTO migration_sentinel (value) VALUES ('keep-me');
                 PRAGMA user_version=21;
                 PRAGMA foreign_keys=ON;",
            )
            .expect("prepare version 21 database");
        run_migrations(&connection).expect("migrate to latest version");
        let version = connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .expect("read schema version");
        let sentinel = connection
            .query_row("SELECT value FROM migration_sentinel", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("read sentinel");
        let new_tables = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type='table' AND name IN (
                   'vault_index_changes', 'note_lexical_fts', 'memory_records',
                   'memory_record_revisions', 'memory_fts', 'memory_reflection_jobs',
                   'note_feature_vectors', 'assistant_conversations', 'assistant_requests'
                 )",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("read Memory V2 and lexical search schema");
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
        assert_eq!(sentinel, "keep-me");
        assert_eq!(new_tables, 9);
    }

    #[test]
    fn version_27_upgrade_converts_failed_index_rows_to_dead_letter() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let connection = Connection::open(directory.path().join("runtime.sqlite"))
            .expect("open sqlite database");
        connection
            .execute_batch(
                "CREATE TABLE vault_index_changes (
                   id INTEGER PRIMARY KEY AUTOINCREMENT,
                   vault_id TEXT NOT NULL,
                   canonical_root TEXT NOT NULL,
                   relative_path TEXT NOT NULL,
                   generation INTEGER NOT NULL DEFAULT 1 CHECK(generation > 0),
                   change_kind TEXT NOT NULL CHECK(change_kind IN ('upsert', 'delete')),
                   state TEXT NOT NULL CHECK(state IN ('pending', 'processing', 'failed')),
                   attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
                   available_at_ms INTEGER NOT NULL,
                   claimed_at_ms INTEGER,
                   last_error TEXT,
                   trace_id TEXT,
                   created_at TEXT NOT NULL,
                   updated_at TEXT NOT NULL,
                   UNIQUE(vault_id, relative_path)
                 );
                 CREATE INDEX idx_vault_index_changes_ready
                   ON vault_index_changes(state, available_at_ms, updated_at);
                 INSERT INTO vault_index_changes
                   (vault_id, canonical_root, relative_path, generation, change_kind, state,
                    attempt_count, available_at_ms, claimed_at_ms, last_error, trace_id,
                    created_at, updated_at)
                 VALUES
                   ('vault-migration', '/tmp/vault', 'dead.md', 1, 'upsert', 'failed',
                    5, 0, NULL, 'exhausted', 'trace-index-migration',
                    '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
                 PRAGMA user_version=27;",
            )
            .expect("prepare version 27 database");

        run_migrations(&connection).expect("migrate version 27 database");
        let (version, state, trace_id) = connection
            .query_row(
                "SELECT (SELECT user_version FROM pragma_user_version), state, trace_id
                 FROM vault_index_changes WHERE vault_id='vault-migration'",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .expect("read migrated dead-letter row");
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
        assert_eq!(state, "dead_letter");
        assert_eq!(trace_id, "trace-index-migration");
    }

    #[test]
    fn vault_index_queue_coalesces_rapid_file_changes() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let root = directory.path().join("vault");
        fs::create_dir(&root).expect("create vault");
        let note = root.join("变化.md");
        let database = test_database(&directory.path().join("runtime.sqlite"));
        register_test_vault(&database, "vault-coalesce", &root);

        fs::write(&note, "# 第一版").expect("write first note");
        database
            .enqueue_vault_index_path("vault-coalesce", &root, &note)
            .expect("enqueue create");
        fs::write(&note, "# 第二版").expect("write second note");
        database
            .enqueue_vault_index_path("vault-coalesce", &root, &note)
            .expect("enqueue modify");
        fs::remove_file(&note).expect("delete note");
        database
            .enqueue_vault_index_path("vault-coalesce", &root, &note)
            .expect("enqueue delete");
        fs::write(&note, "# 最终版").expect("recreate note");
        database
            .enqueue_vault_index_path("vault-coalesce", &root, &note)
            .expect("enqueue recreate");

        let row = database
            .connection
            .lock()
            .expect("lock database")
            .query_row(
                "SELECT generation, change_kind, state, attempt_count
                 FROM vault_index_changes WHERE vault_id='vault-coalesce'",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .expect("read coalesced queue row");
        assert_eq!(row, (4, "upsert".to_string(), "pending".to_string(), 0));
    }

    #[test]
    fn vault_index_queue_preserves_known_trace_across_watcher_events() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let root = directory.path().join("vault");
        fs::create_dir(&root).expect("create vault");
        let note = root.join("追踪.md");
        fs::write(&note, "# Trace").expect("write note");
        let database = test_database(&directory.path().join("runtime.sqlite"));
        register_test_vault(&database, "vault-trace", &root);

        database
            .enqueue_vault_index_path("vault-trace", &root, &note)
            .expect("enqueue watcher fallback");
        let fallback_trace = database
            .connection
            .lock()
            .expect("lock database")
            .query_row(
                "SELECT trace_id FROM vault_index_changes WHERE vault_id='vault-trace'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("read fallback trace");

        database
            .enqueue_vault_index_path_with_trace("vault-trace", &root, &note, "trace-known-a")
            .expect("known trace replaces fallback");
        database
            .enqueue_vault_index_path("vault-trace", &root, &note)
            .expect("watcher preserves known trace");
        database
            .enqueue_vault_index_path_with_trace("vault-trace", &root, &note, "trace-known-b")
            .expect("new known trace replaces old known trace");
        database
            .enqueue_vault_index_path("vault-trace", &root, &note)
            .expect("later watcher preserves newest known trace");

        let (trace_id, generation) = database
            .connection
            .lock()
            .expect("lock database")
            .query_row(
                "SELECT trace_id, generation FROM vault_index_changes WHERE vault_id='vault-trace'",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .expect("read persisted trace");
        assert_ne!(fallback_trace, "trace-known-a");
        assert_eq!(trace_id, "trace-known-b");
        assert_eq!(generation, 5);
    }

    #[test]
    fn repeated_watcher_events_keep_the_first_fallback_trace() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let root = directory.path().join("vault");
        fs::create_dir(&root).expect("create vault");
        let note = root.join("外部变化.md");
        fs::write(&note, "# First").expect("write note");
        let database = test_database(&directory.path().join("runtime.sqlite"));
        register_test_vault(&database, "vault-fallback-trace", &root);
        database
            .enqueue_vault_index_path("vault-fallback-trace", &root, &note)
            .expect("enqueue first watcher event");
        let first_trace = database
            .connection
            .lock()
            .expect("lock database")
            .query_row(
                "SELECT trace_id FROM vault_index_changes WHERE vault_id='vault-fallback-trace'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("read first trace");
        database
            .enqueue_vault_index_path("vault-fallback-trace", &root, &note)
            .expect("enqueue second watcher event");
        let second_trace = database
            .connection
            .lock()
            .expect("lock database")
            .query_row(
                "SELECT trace_id FROM vault_index_changes WHERE vault_id='vault-fallback-trace'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("read second trace");
        assert_eq!(second_trace, first_trace);
    }

    #[test]
    fn newer_generation_supersedes_a_claimed_change() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let root = directory.path().join("vault");
        fs::create_dir(&root).expect("create vault");
        let note = root.join("并发.md");
        fs::write(&note, "# 第一版").expect("write note");
        let database = test_database(&directory.path().join("runtime.sqlite"));
        register_test_vault(&database, "vault-generation", &root);
        database
            .enqueue_vault_index_path("vault-generation", &root, &note)
            .expect("enqueue first generation");
        force_vault_index_queue_due(&database);
        let claimed = database
            .claim_vault_index_changes(1)
            .expect("claim first generation")
            .pop()
            .expect("claimed row");

        fs::write(&note, "# 第二版").expect("update note");
        database
            .enqueue_vault_index_path("vault-generation", &root, &note)
            .expect("enqueue second generation");
        assert!(database
            .apply_claimed_vault_index_change(&claimed, &root)
            .expect("old apply returns cleanly")
            .is_none());
        let failure = database
            .fail_claimed_vault_index_change(&claimed, "旧任务失败")
            .expect("old failure returns cleanly");
        assert!(!failure.updated);

        let row = database
            .connection
            .lock()
            .expect("lock database")
            .query_row(
                "SELECT generation, state, attempt_count FROM vault_index_changes",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .expect("read current generation");
        assert_eq!(row, (2, "pending".to_string(), 0));
    }

    #[test]
    fn interrupted_vault_index_claim_is_recovered_after_reopen() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let root = directory.path().join("vault");
        fs::create_dir(&root).expect("create vault");
        let note = root.join("恢复.md");
        fs::write(&note, "# 恢复").expect("write note");
        let database_path = directory.path().join("runtime.sqlite");
        {
            let database = test_database(&database_path);
            register_test_vault(&database, "vault-recovery", &root);
            database
                .enqueue_vault_index_path("vault-recovery", &root, &note)
                .expect("enqueue note");
            force_vault_index_queue_due(&database);
            assert_eq!(
                database
                    .claim_vault_index_changes(1)
                    .expect("claim note")
                    .len(),
                1
            );
        }

        let reopened = RuntimeDatabase::open_test(&database_path).expect("reopen database");
        assert_eq!(
            reopened
                .recover_vault_index_changes()
                .expect("recover queue"),
            1
        );
        let state = reopened
            .connection
            .lock()
            .expect("lock database")
            .query_row(
                "SELECT state, attempt_count FROM vault_index_changes",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .expect("read recovered row");
        assert_eq!(state, ("pending".to_string(), 1));
    }

    #[test]
    fn exhausted_interrupted_index_claim_is_dead_lettered_with_trace_and_audit() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let root = directory.path().join("vault");
        fs::create_dir(&root).expect("create vault");
        let note = root.join("恢复死信.md");
        fs::write(&note, "# 恢复死信").expect("write note");
        let database_path = directory.path().join("runtime.sqlite");
        {
            let database = test_database(&database_path);
            register_test_vault(&database, "vault-recovery-dead-letter", &root);
            database
                .enqueue_vault_index_path("vault-recovery-dead-letter", &root, &note)
                .expect("enqueue note");
            database
                .connection
                .lock()
                .expect("lock database")
                .execute(
                    "UPDATE vault_index_changes
                     SET state='processing', attempt_count=?1, claimed_at_ms=1",
                    [VAULT_INDEX_MAX_ATTEMPTS],
                )
                .expect("simulate exhausted interrupted claim");
        }

        let reopened = RuntimeDatabase::open_test(&database_path).expect("reopen database");
        assert_eq!(
            reopened
                .recover_vault_index_changes()
                .expect("recover exhausted queue"),
            1
        );
        let connection = reopened.connection.lock().expect("lock reopened database");
        let (state, last_error) = connection
            .query_row(
                "SELECT state, last_error FROM vault_index_changes",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .expect("read recovered dead-letter row");
        let dead_letter_traces = connection
            .query_row(
                "SELECT COUNT(*) FROM runtime_trace_events
                 WHERE entity_kind='index_change' AND event_type='index.dead_lettered'
                   AND state='dead_letter'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count dead-letter traces");
        let failure_events = connection
            .query_row(
                "SELECT COUNT(*) FROM operation_events
                 WHERE event_type='vault.note.index' AND state='failed'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count dead-letter operation events");
        assert_eq!(state, "dead_letter");
        assert_eq!(last_error, "应用退出前索引任务未完成");
        assert_eq!(dead_letter_traces, 1);
        assert_eq!(failure_events, 1);
    }

    #[test]
    fn claim_sweep_dead_letters_exhausted_pending_rows_with_trace_and_audit() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let root = directory.path().join("vault");
        fs::create_dir(&root).expect("create vault");
        let note = root.join("认领死信.md");
        fs::write(&note, "# 认领死信").expect("write note");
        let database = test_database(&directory.path().join("runtime.sqlite"));
        register_test_vault(&database, "vault-claim-dead-letter", &root);
        database
            .enqueue_vault_index_path("vault-claim-dead-letter", &root, &note)
            .expect("enqueue note");
        database
            .connection
            .lock()
            .expect("lock database")
            .execute(
                "UPDATE vault_index_changes SET attempt_count=?1, available_at_ms=0",
                [VAULT_INDEX_MAX_ATTEMPTS],
            )
            .expect("simulate exhausted pending row");

        assert!(database
            .claim_vault_index_changes(1)
            .expect("sweep exhausted pending row")
            .is_empty());
        let connection = database.connection.lock().expect("lock database");
        let (state, last_error) = connection
            .query_row(
                "SELECT state, last_error FROM vault_index_changes",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .expect("read claim-swept dead-letter row");
        let dead_letter_traces = connection
            .query_row(
                "SELECT COUNT(*) FROM runtime_trace_events
                 WHERE entity_kind='index_change' AND event_type='index.dead_lettered'
                   AND state='dead_letter'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count dead-letter traces");
        let failure_events = connection
            .query_row(
                "SELECT COUNT(*) FROM operation_events
                 WHERE event_type='vault.note.index' AND state='failed'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count dead-letter operation events");
        assert_eq!(state, "dead_letter");
        assert_eq!(last_error, "Vault 索引任务超过最大重试次数");
        assert_eq!(dead_letter_traces, 1);
        assert_eq!(failure_events, 1);
    }

    #[test]
    fn vault_index_retry_limit_transitions_to_dead_letter() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let root = directory.path().join("vault");
        fs::create_dir(&root).expect("create vault");
        let note = root.join("失败.md");
        fs::write(&note, "# 失败").expect("write note");
        let database = test_database(&directory.path().join("runtime.sqlite"));
        register_test_vault(&database, "vault-retry", &root);
        database
            .enqueue_vault_index_path("vault-retry", &root, &note)
            .expect("enqueue note");

        for attempt in 1..=VAULT_INDEX_MAX_ATTEMPTS {
            force_vault_index_queue_due(&database);
            let claimed = database
                .claim_vault_index_changes(1)
                .expect("claim retry")
                .pop()
                .expect("claimed retry");
            assert_eq!(claimed.attempt_count, attempt);
            let outcome = database
                .fail_claimed_vault_index_change(&claimed, "测试失败")
                .expect("record retry failure");
            assert!(outcome.updated);
            assert_eq!(outcome.terminal, attempt == VAULT_INDEX_MAX_ATTEMPTS);
        }
        let connection = database.connection.lock().expect("lock database");
        let state = connection
            .query_row("SELECT state FROM vault_index_changes", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("read failed state");
        let failed_events = connection
            .query_row(
                "SELECT COUNT(*) FROM operation_events
                 WHERE event_type='vault.note.index' AND state='failed'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count failure events");
        let dead_letter_traces = connection
            .query_row(
                "SELECT COUNT(*) FROM runtime_trace_events
                 WHERE entity_kind='index_change' AND event_type='index.dead_lettered'
                   AND state='dead_letter'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count dead-letter traces");
        assert_eq!(state, "dead_letter");
        assert_eq!(failed_events, 1);
        assert_eq!(dead_letter_traces, 1);
    }

    #[test]
    fn vault_index_apply_is_atomic_with_fts_audit_and_queue_completion() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let root = directory.path().join("vault");
        fs::create_dir(&root).expect("create vault");
        let note = root.join("事务.md");
        fs::write(&note, "# 事务\n\n必须一起提交").expect("write note");
        let database = test_database(&directory.path().join("runtime.sqlite"));
        register_test_vault(&database, "vault-atomic", &root);
        database
            .enqueue_vault_index_path("vault-atomic", &root, &note)
            .expect("enqueue note");
        force_vault_index_queue_due(&database);
        let claimed = database
            .claim_vault_index_changes(1)
            .expect("claim note")
            .pop()
            .expect("claimed note");
        database
            .connection
            .lock()
            .expect("lock database")
            .execute_batch(
                "CREATE TRIGGER reject_vault_index_audit
                 BEFORE INSERT ON operation_events
                 WHEN NEW.event_type='vault.note.index'
                 BEGIN SELECT RAISE(ABORT, 'reject audit'); END;",
            )
            .expect("install rollback trigger");
        assert!(database
            .apply_claimed_vault_index_change(&claimed, &root)
            .is_err());
        let connection = database.connection.lock().expect("lock database");
        let note_count = connection
            .query_row("SELECT COUNT(*) FROM note_index", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count note index");
        let fts_count = connection
            .query_row("SELECT COUNT(*) FROM note_fts", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count fts");
        let lexical_count = connection
            .query_row("SELECT COUNT(*) FROM note_lexical_fts", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count lexical fts");
        let vector_count = connection
            .query_row("SELECT COUNT(*) FROM note_feature_vectors", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count feature vectors");
        let queue_state = connection
            .query_row("SELECT state FROM vault_index_changes", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("read queue state");
        assert_eq!(note_count, 0);
        assert_eq!(fts_count, 0);
        assert_eq!(lexical_count, 0);
        assert_eq!(vector_count, 0);
        assert_eq!(queue_state, "processing");
    }

    #[test]
    fn vault_index_queue_upsert_completes_with_searchable_fts_and_success_audit() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let root = directory.path().join("vault");
        fs::create_dir(&root).expect("create vault");
        let note = root.join("可搜索.md");
        fs::write(&note, "# 可搜索\n\n独立检索词 yunspirequeue").expect("write note");
        let database = test_database(&directory.path().join("runtime.sqlite"));
        register_test_vault(&database, "vault-upsert", &root);
        database
            .enqueue_vault_index_path("vault-upsert", &root, &note)
            .expect("enqueue note");
        force_vault_index_queue_due(&database);
        let claimed = database
            .claim_vault_index_changes(1)
            .expect("claim note")
            .pop()
            .expect("claimed note");
        let applied = database
            .apply_claimed_vault_index_change(&claimed, &root)
            .expect("apply note")
            .expect("owned queue generation");
        assert_eq!(applied.vault_id, "vault-upsert");
        assert_eq!(applied.relative_path, "可搜索.md");
        assert_eq!(applied.change_kind, "upsert");

        let connection = database.connection.lock().expect("lock database");
        let queue_count = connection
            .query_row("SELECT COUNT(*) FROM vault_index_changes", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count queue");
        let fts_count = connection
            .query_row(
                "SELECT COUNT(*) FROM note_fts
                 WHERE note_fts MATCH ?1 AND vault_id=?2",
                params!["\"yunspirequeue\"", "vault-upsert"],
                |row| row.get::<_, i64>(0),
            )
            .expect("search fts");
        let vector_count = connection
            .query_row(
                "SELECT COUNT(*) FROM note_feature_vectors
                 WHERE vault_id=?1 AND relative_path=?2",
                params!["vault-upsert", "可搜索.md"],
                |row| row.get::<_, i64>(0),
            )
            .expect("count feature vector");
        let payload = connection
            .query_row(
                "SELECT payload FROM operation_events
                 WHERE event_type='vault.note.index' AND state='success'
                 ORDER BY rowid DESC LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("read success audit");
        let event: OperationEvent =
            serde_json::from_str(&payload).expect("parse success audit payload");
        assert_eq!(queue_count, 0);
        assert_eq!(fts_count, 1);
        assert_eq!(vector_count, 1);
        assert_eq!(event.vault_id.as_deref(), Some("vault-upsert"));
        assert_eq!(event.relative_path.as_deref(), Some("可搜索.md"));
    }

    #[test]
    fn chinese_lexical_index_matches_subphrases_tags_and_wiki_links() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let root = directory.path().join("vault");
        fs::create_dir(&root).expect("create vault");
        let note = root.join("知识系统.md");
        fs::write(
            &note,
            "# 个人知识系统\n\n关联 [[知识图谱]] 与 #知识管理，支持中文局部检索。",
        )
        .expect("write note");
        let database = test_database(&directory.path().join("runtime.sqlite"));
        register_test_vault(&database, "vault-chinese", &root);
        database
            .enqueue_vault_index_path("vault-chinese", &root, &note)
            .expect("enqueue note");
        force_vault_index_queue_due(&database);
        let claimed = database
            .claim_vault_index_changes(1)
            .expect("claim note")
            .pop()
            .expect("claimed note");
        database
            .apply_claimed_vault_index_change(&claimed, &root)
            .expect("apply note")
            .expect("owned queue generation");

        let query = lexical_fts_match_query("知识图谱").expect("build chinese query");
        let connection = database.connection.lock().expect("lock database");
        let matched = connection
            .query_row(
                "SELECT COUNT(*) FROM note_lexical_fts
                 WHERE note_lexical_fts MATCH ?1 AND vault_id=?2",
                params![query, "vault-chinese"],
                |row| row.get::<_, i64>(0),
            )
            .expect("search chinese lexical index");
        let (tags, links) = connection
            .query_row(
                "SELECT tags_json, wiki_links_json FROM note_index
                 WHERE vault_id=?1 AND relative_path=?2",
                params!["vault-chinese", "知识系统.md"],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .expect("read relation metadata");
        assert_eq!(matched, 1);
        assert!(tags.contains("知识管理"));
        assert!(links.contains("知识图谱"));
    }

    #[test]
    fn local_vector_adds_chinese_feature_hit_and_rrf_is_explainable() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let root = directory.path().join("vault");
        fs::create_dir(&root).expect("create vault");
        let related = root.join("智能方法.md");
        let unrelated = root.join("厨房记录.md");
        fs::write(
            &related,
            "# 智能方法\n\n机器智能用于预测，学习方法用于归纳规律。#算法",
        )
        .expect("write related note");
        fs::write(&unrelated, "# 厨房记录\n\n记录烘焙温度和食材比例。")
            .expect("write unrelated note");
        let connection = Connection::open_in_memory().expect("open sqlite");
        run_migrations(&connection).expect("run migrations");
        assert!(
            index_note_in_connection(&connection, "vault-vector", &root, &related)
                .expect("index related note")
        );
        assert!(
            index_note_in_connection(&connection, "vault-vector", &root, &unrelated)
                .expect("index unrelated note")
        );
        let related_vector = connection
            .query_row(
                "SELECT representation_version, dimensions, vector_blob
                 FROM note_feature_vectors
                 WHERE vault_id=?1 AND relative_path=?2",
                params!["vault-vector", "智能方法.md"],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )
            .expect("read related vector");
        let related_similarity = local_vector_similarity(
            &query_local_feature_vector("机器学习").expect("query vector"),
            &decode_local_feature_vector(related_vector.0, related_vector.1, &related_vector.2)
                .expect("decode related vector"),
        )
        .expect("calculate related similarity");
        assert!(
            related_similarity >= MIN_LOCAL_VECTOR_SIMILARITY,
            "related similarity {related_similarity}"
        );

        let vector_only =
            indexed_search_in_connection(&connection, Some("vault-vector"), "机器学习", 10)
                .expect("run vector search");
        let related_result = vector_only
            .iter()
            .find(|result| result.relative_path == "智能方法.md")
            .expect("find vector-only related result");
        assert_eq!(related_result.ranking_signals.lexical_rank, None);
        assert!(related_result.ranking_signals.vector_rank.is_some());
        assert!(related_result
            .ranking_signals
            .vector_similarity
            .is_some_and(|score| score >= MIN_LOCAL_VECTOR_SIMILARITY));
        assert!((related_result.score - related_result.ranking_signals.vector_rrf).abs() < 1e-12);

        let fused = indexed_search_in_connection(&connection, Some("vault-vector"), "机器智能", 10)
            .expect("run fused search");
        let fused_result = fused
            .iter()
            .find(|result| result.relative_path == "智能方法.md")
            .expect("find fused result");
        let lexical_rank = fused_result
            .ranking_signals
            .lexical_rank
            .expect("lexical rank");
        let vector_rank = fused_result
            .ranking_signals
            .vector_rank
            .expect("vector rank");
        assert!(
            (fused_result.ranking_signals.lexical_rrf - 1.0 / (RRF_K + lexical_rank as f64)).abs()
                < 1e-12
        );
        assert!(
            (fused_result.ranking_signals.vector_rrf - 1.0 / (RRF_K + vector_rank as f64)).abs()
                < 1e-12
        );
        assert!(
            (fused_result.score
                - fused_result.ranking_signals.lexical_rrf
                - fused_result.ranking_signals.vector_rrf)
                .abs()
                < 1e-12
        );
    }

    #[test]
    fn empty_missing_or_corrupt_vectors_keep_fts_available() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let root = directory.path().join("vault");
        fs::create_dir(&root).expect("create vault");
        let note = root.join("回退.md");
        fs::write(&note, "# 回退\n\nlexicalfallbacktoken").expect("write note");
        let connection = Connection::open_in_memory().expect("open sqlite");
        run_migrations(&connection).expect("run migrations");
        assert!(
            indexed_search_in_connection(&connection, None, "空索引", 10)
                .expect("search empty index")
                .is_empty()
        );
        assert!(
            index_note_in_connection(&connection, "vault-fallback", &root, &note)
                .expect("index fallback note")
        );
        connection
            .execute(
                "UPDATE note_feature_vectors SET vector_blob=X'00'
                 WHERE vault_id=?1 AND relative_path=?2",
                params!["vault-fallback", "回退.md"],
            )
            .expect("corrupt feature vector");
        let corrupted = indexed_search_in_connection(
            &connection,
            Some("vault-fallback"),
            "lexicalfallbacktoken",
            10,
        )
        .expect("search with corrupt vector");
        assert_eq!(corrupted.len(), 1);
        assert!(corrupted[0].ranking_signals.lexical_rank.is_some());
        assert_eq!(corrupted[0].ranking_signals.vector_rank, None);

        connection
            .execute(
                "DELETE FROM note_feature_vectors WHERE vault_id=?1 AND relative_path=?2",
                params!["vault-fallback", "回退.md"],
            )
            .expect("remove feature vector");
        let missing = indexed_search_in_connection(
            &connection,
            Some("vault-fallback"),
            "lexicalfallbacktoken",
            10,
        )
        .expect("search with missing vector");
        assert_eq!(missing.len(), 1);
        assert!(missing[0].ranking_signals.lexical_rank.is_some());
        assert_eq!(missing[0].ranking_signals.vector_rank, None);
    }

    #[test]
    fn feature_vectors_persist_after_database_reopen() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let root = directory.path().join("vault");
        fs::create_dir(&root).expect("create vault");
        let note = root.join("持久向量.md");
        fs::write(&note, "# 持久向量\n\n本地特征表示会写入 SQLite。").expect("write note");
        let database_path = directory.path().join("runtime.sqlite");
        {
            let database = RuntimeDatabase::open_test(&database_path).expect("open database");
            let connection = database.connection.lock().expect("lock database");
            assert!(
                index_note_in_connection(&connection, "vault-persist", &root, &note)
                    .expect("index note")
            );
        }
        let reopened = RuntimeDatabase::open_test(&database_path).expect("reopen database");
        let connection = reopened.connection.lock().expect("lock reopened database");
        let stored = connection
            .query_row(
                "SELECT representation_version, dimensions, length(vector_blob)
                 FROM note_feature_vectors
                 WHERE vault_id=?1 AND relative_path=?2",
                params!["vault-persist", "持久向量.md"],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .expect("read persisted vector");
        assert_eq!(
            stored,
            (
                LOCAL_FEATURE_VECTOR_VERSION,
                LOCAL_FEATURE_VECTOR_DIMENSIONS as i64,
                (LOCAL_FEATURE_VECTOR_DIMENSIONS * std::mem::size_of::<f32>()) as i64
            )
        );
        assert!(
            !indexed_search_in_connection(&connection, Some("vault-persist"), "本地特征", 10,)
                .expect("search reopened database")
                .is_empty()
        );
    }

    #[test]
    fn version_24_upgrade_queues_and_rebuilds_missing_vectors() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let root = directory.path().join("vault");
        fs::create_dir(&root).expect("create vault");
        let note = root.join("升级.md");
        fs::write(&note, "# 升级\n\n旧索引升级后重建本地特征向量。").expect("write note");
        let database_path = directory.path().join("runtime.sqlite");
        {
            let database = RuntimeDatabase::open_test(&database_path).expect("open database");
            register_test_vault(&database, "vault-upgrade", &root);
            let connection = database.connection.lock().expect("lock database");
            assert!(
                index_note_in_connection(&connection, "vault-upgrade", &root, &note)
                    .expect("index old note")
            );
            connection
                .execute_batch(
                    "DROP TABLE note_feature_vectors;
                     DELETE FROM vault_index_changes;
                     PRAGMA user_version=24;",
                )
                .expect("simulate version 24 database");
        }

        let reopened = RuntimeDatabase::open_test(&database_path).expect("upgrade database");
        let (version, queued) = {
            let connection = reopened.connection.lock().expect("lock upgraded database");
            let version = connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("read upgraded version");
            let queued = connection
                .query_row(
                    "SELECT COUNT(*) FROM vault_index_changes
                     WHERE vault_id=?1 AND relative_path=?2 AND state='pending'",
                    params!["vault-upgrade", "升级.md"],
                    |row| row.get::<_, i64>(0),
                )
                .expect("count queued rebuild");
            (version, queued)
        };
        assert_eq!((version, queued), (CURRENT_SCHEMA_VERSION, 1));
        let claimed = reopened
            .claim_vault_index_changes(1)
            .expect("claim upgrade rebuild")
            .pop()
            .expect("queued upgrade rebuild");
        reopened
            .apply_claimed_vault_index_change(&claimed, &root)
            .expect("apply upgrade rebuild")
            .expect("owned upgrade generation");
        let vector_count = reopened
            .connection
            .lock()
            .expect("lock rebuilt database")
            .query_row(
                "SELECT COUNT(*) FROM note_feature_vectors
                 WHERE vault_id=?1 AND relative_path=?2",
                params!["vault-upgrade", "升级.md"],
                |row| row.get::<_, i64>(0),
            )
            .expect("count rebuilt vector");
        assert_eq!(vector_count, 1);
    }

    #[test]
    fn vault_index_queue_delete_removes_note_and_fts_together() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let root = directory.path().join("vault");
        fs::create_dir(&root).expect("create vault");
        let note = root.join("待删除.md");
        fs::write(&note, "# 待删除\n\ndeletequeue").expect("write note");
        let database = test_database(&directory.path().join("runtime.sqlite"));
        register_test_vault(&database, "vault-delete", &root);

        database
            .enqueue_vault_index_path("vault-delete", &root, &note)
            .expect("enqueue initial note");
        force_vault_index_queue_due(&database);
        let initial = database
            .claim_vault_index_changes(1)
            .expect("claim initial note")
            .pop()
            .expect("claimed initial note");
        database
            .apply_claimed_vault_index_change(&initial, &root)
            .expect("apply initial note")
            .expect("owned initial generation");

        fs::remove_file(&note).expect("delete note");
        database
            .enqueue_vault_index_path("vault-delete", &root, &note)
            .expect("enqueue deletion");
        force_vault_index_queue_due(&database);
        let deletion = database
            .claim_vault_index_changes(1)
            .expect("claim deletion")
            .pop()
            .expect("claimed deletion");
        let applied = database
            .apply_claimed_vault_index_change(&deletion, &root)
            .expect("apply deletion")
            .expect("owned deletion generation");
        assert_eq!(applied.vault_id, "vault-delete");
        assert_eq!(applied.relative_path, "待删除.md");
        assert_eq!(applied.change_kind, "delete");

        let connection = database.connection.lock().expect("lock database");
        let note_count = connection
            .query_row(
                "SELECT COUNT(*) FROM note_index WHERE vault_id=?1 AND relative_path=?2",
                params!["vault-delete", "待删除.md"],
                |row| row.get::<_, i64>(0),
            )
            .expect("count note index");
        let fts_count = connection
            .query_row(
                "SELECT COUNT(*) FROM note_fts WHERE vault_id=?1 AND relative_path=?2",
                params!["vault-delete", "待删除.md"],
                |row| row.get::<_, i64>(0),
            )
            .expect("count fts index");
        let lexical_count = connection
            .query_row(
                "SELECT COUNT(*) FROM note_lexical_fts WHERE vault_id=?1 AND relative_path=?2",
                params!["vault-delete", "待删除.md"],
                |row| row.get::<_, i64>(0),
            )
            .expect("count lexical index");
        let vector_count = connection
            .query_row(
                "SELECT COUNT(*) FROM note_feature_vectors WHERE vault_id=?1 AND relative_path=?2",
                params!["vault-delete", "待删除.md"],
                |row| row.get::<_, i64>(0),
            )
            .expect("count feature vector");
        let queue_count = connection
            .query_row("SELECT COUNT(*) FROM vault_index_changes", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count queue");
        assert_eq!(
            (
                note_count,
                fts_count,
                lexical_count,
                vector_count,
                queue_count
            ),
            (0, 0, 0, 0, 0)
        );
    }

    #[test]
    fn vault_reconciliation_queues_new_and_deleted_notes() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let root = directory.path().join("vault");
        fs::create_dir(&root).expect("create vault");
        let deleted = root.join("旧笔记.md");
        let created = root.join("新笔记.md");
        fs::write(&deleted, "# 旧笔记").expect("write old note");
        let database = test_database(&directory.path().join("runtime.sqlite"));
        let vault = register_test_vault(&database, "vault-reconcile", &root);
        {
            let connection = database.connection.lock().expect("lock database");
            assert!(
                index_note_in_connection(&connection, "vault-reconcile", &root, &deleted)
                    .expect("index old note")
            );
        }
        fs::remove_file(&deleted).expect("delete old note");
        fs::write(&created, "# 新笔记").expect("write new note");
        let result = database
            .reconcile_vault_index(&vault)
            .expect("reconcile vault");
        assert_eq!(result.queued_upserts, 1);
        assert_eq!(result.queued_deletes, 1);
        let connection = database.connection.lock().expect("lock database");
        let changes = connection
            .prepare(
                "SELECT relative_path, change_kind FROM vault_index_changes
                 ORDER BY relative_path",
            )
            .expect("prepare queue query")
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("query queue")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect queue");
        assert_eq!(
            changes,
            vec![
                ("新笔记.md".to_string(), "upsert".to_string()),
                ("旧笔记.md".to_string(), "delete".to_string())
            ]
        );
    }

    #[test]
    fn claimed_change_is_rejected_after_vault_root_changes() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let first_root = directory.path().join("first");
        let second_root = directory.path().join("second");
        fs::create_dir(&first_root).expect("create first vault");
        fs::create_dir(&second_root).expect("create second vault");
        let note = first_root.join("路径.md");
        fs::write(&note, "# 原路径").expect("write note");
        let database = test_database(&directory.path().join("runtime.sqlite"));
        register_test_vault(&database, "vault-root", &first_root);
        database
            .enqueue_vault_index_path("vault-root", &first_root, &note)
            .expect("enqueue note");
        force_vault_index_queue_due(&database);
        let claimed = database
            .claim_vault_index_changes(1)
            .expect("claim note")
            .pop()
            .expect("claimed note");
        register_test_vault(&database, "vault-root", &second_root);
        let error = database
            .apply_claimed_vault_index_change(&claimed, &second_root)
            .expect_err("old root must be rejected");
        assert!(error.contains("根目录已变化"));
        let indexed = database
            .connection
            .lock()
            .expect("lock database")
            .query_row("SELECT COUNT(*) FROM note_index", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("count index");
        assert_eq!(indexed, 0);
    }

    #[test]
    fn managed_resources_version_delete_and_reopen_without_legacy_skill_bypass() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let path = directory.path().join("runtime.sqlite");
        {
            let database = test_database(&path);
            let mut first_snapshot =
                snapshot(vec![json!({"id": "skill-1", "name": "旧入口不得写入"})]);
            first_snapshot.schedules = vec![json!({"id": "schedule-1", "name": "第一版"})];
            let first = database
                .sync_managed_resources(DEFAULT_LOCAL_WORKSPACE_SCOPE, &first_snapshot)
                .expect("save first revision");
            assert!(first.custom_skills.is_empty());
            assert_eq!(first.schedules.len(), 1);
            let mut second_snapshot = snapshot(Vec::new());
            second_snapshot.schedules = vec![json!({"id": "schedule-1", "name": "第二版"})];
            database
                .sync_managed_resources(DEFAULT_LOCAL_WORKSPACE_SCOPE, &second_snapshot)
                .expect("save second revision");
            let connection = database.connection.lock().expect("lock sqlite");
            let legacy_skills: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM managed_resources
                     WHERE workspace_scope=?1 AND resource_type='user_skill'",
                    [DEFAULT_LOCAL_WORKSPACE_SCOPE],
                    |row| row.get(0),
                )
                .expect("count legacy skills");
            assert_eq!(legacy_skills, 0);
            let revisions: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM managed_resource_revisions
                     WHERE workspace_scope=?1 AND resource_type='schedule' AND resource_id='schedule-1'",
                    [DEFAULT_LOCAL_WORKSPACE_SCOPE],
                    |row| row.get(0),
                )
                .expect("count revisions");
            assert_eq!(revisions, 2);
        }
        {
            let database = test_database(&path);
            let restored = database
                .load_managed_resources(DEFAULT_LOCAL_WORKSPACE_SCOPE)
                .expect("reload managed resources");
            assert!(restored.custom_skills.is_empty());
            assert_eq!(restored.schedules[0]["name"], "第二版");
            let deleted = database
                .sync_managed_resources(DEFAULT_LOCAL_WORKSPACE_SCOPE, &snapshot(Vec::new()))
                .expect("sync empty resource group");
            assert!(deleted.schedules.is_empty());
            let connection = database.connection.lock().expect("lock sqlite");
            let state: String = connection
                .query_row(
                    "SELECT state FROM managed_resources
                     WHERE workspace_scope=?1 AND resource_type='schedule' AND id='schedule-1'",
                    [DEFAULT_LOCAL_WORKSPACE_SCOPE],
                    |row| row.get(0),
                )
                .expect("read tombstone");
            assert_eq!(state, "deleted");
            let revisions: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM managed_resource_revisions
                     WHERE workspace_scope=?1 AND resource_type='schedule' AND resource_id='schedule-1'",
                    [DEFAULT_LOCAL_WORKSPACE_SCOPE],
                    |row| row.get(0),
                )
                .expect("count revisions after delete");
            assert_eq!(revisions, 3);
        }
    }

    fn inbound_record(id: &str, state: &str, hash: &str) -> InboundContentRecordInput {
        InboundContentRecordInput {
            id: id.to_string(),
            state: state.to_string(),
            source_type: "file".to_string(),
            source_ref: format!("本地文件/{id}.md"),
            title: format!("内容 {id}"),
            content_hash: hash.to_string(),
            content_characters: 12,
            attachment_count: 0,
            image_count: 0,
            extraction: json!({"warnings": []}),
            analysis: json!({"summaryCharacters": 8}),
            quality: json!({"status": "passed"}),
            target: json!({"vaultId": "vault-test"}),
            task_id: Some(format!("task-{id}")),
            failure_reason: None,
        }
    }

    #[test]
    fn inbound_content_hash_deduplicates_across_task_ids() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let database = test_database(&directory.path().join("runtime.sqlite"));
        let hash = format!("sha256:{}", "a".repeat(64));
        let first = inbound_record("capture-first", "extracted", &hash);
        database
            .upsert_inbound_content_record(DEFAULT_LOCAL_WORKSPACE_SCOPE, &first)
            .expect("save first extraction");
        for state in ["analyzing", "ready_to_write", "writing", "committed"] {
            let mut update = inbound_record("capture-first", state, &hash);
            update.task_id = first.task_id.clone();
            database
                .upsert_inbound_content_record(DEFAULT_LOCAL_WORKSPACE_SCOPE, &update)
                .expect("advance first capture");
        }

        let duplicate = inbound_record("capture-second", "extracted", &hash);
        let receipt = database
            .upsert_inbound_content_record(DEFAULT_LOCAL_WORKSPACE_SCOPE, &duplicate)
            .expect("record duplicate extraction");
        assert_eq!(receipt.state, "quality_rejected");
        assert_eq!(receipt.duplicate_of.as_deref(), Some("capture-first"));
        let connection = database.connection.lock().expect("lock sqlite");
        let (state, failure): (String, String) = connection
            .query_row(
                "SELECT state, failure_reason FROM inbound_content_records
                 WHERE workspace_scope=?1 AND id='capture-second'",
                [DEFAULT_LOCAL_WORKSPACE_SCOPE],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("read duplicate ledger entry");
        assert_eq!(state, "quality_rejected");
        assert!(failure.contains("capture-first"));
    }

    #[test]
    fn application_command_idempotency_does_not_duplicate_task_or_audit_event() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let database = test_database(&directory.path().join("runtime.sqlite"));
        let command = ApplicationCommand {
            id: "command-idempotent".to_string(),
            command_type: "assistant.operation".to_string(),
            origin: CommandOrigin::Assistant,
            intent: "delete".to_string(),
            capability_id: "system:delete".to_string(),
            operation: "delete".to_string(),
            parameters: json!({"relative_path": "资料/笔记.md"}),
            vault_id: Some("vault-test".to_string()),
            relative_paths: vec!["资料/笔记.md".to_string()],
            network_targets: Vec::new(),
            declared_scope: vec!["capability:system:delete".to_string()],
            budget: CommandBudget {
                max_steps: 8,
                max_runtime_seconds: 300,
                max_tool_calls: 16,
                max_tokens: Some(100_000),
                max_cost: None,
            },
            idempotency_key: "delete-idempotency-key".to_string(),
            trace_id: Some("trace-idempotent".to_string()),
            model_decision_receipt: Some("receipt-idempotent".to_string()),
        };
        let decision = crate::policy::evaluate(&command);
        let first = database
            .persist_application_command(
                DEFAULT_LOCAL_WORKSPACE_SCOPE,
                &command,
                &decision,
                "trace-idempotent",
                "2026-07-21T00:00:00Z",
            )
            .expect("persist first application command");
        let second = database
            .persist_application_command(
                DEFAULT_LOCAL_WORKSPACE_SCOPE,
                &command,
                &decision,
                "trace-idempotent",
                "2026-07-21T00:00:01Z",
            )
            .expect("persist duplicate application command");
        assert!(!first.1);
        assert!(second.1);
        assert_eq!(first.0, second.0);
        let mut substituted = command.clone();
        substituted.vault_id = Some("vault-substituted".to_string());
        substituted.relative_paths = vec!["其他库/替换.md".to_string()];
        let substituted_decision = crate::policy::evaluate(&substituted);
        let error = database
            .persist_application_command(
                DEFAULT_LOCAL_WORKSPACE_SCOPE,
                &substituted,
                &substituted_decision,
                "trace-substituted",
                "2026-07-21T00:00:02Z",
            )
            .expect_err("idempotency key must not authorize substituted scope");
        assert!(error.contains("不同的能力或参数范围"));
        let connection = database.connection.lock().expect("lock sqlite");
        for (table, expected) in [
            ("application_commands", 1_i64),
            ("policy_decisions", 1_i64),
            ("runtime_tasks", 1_i64),
            ("operation_events", 1_i64),
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("count idempotent records");
            assert_eq!(count, expected, "unexpected count in {table}");
        }
    }

    #[test]
    fn task_transition_and_audit_event_commit_or_rollback_together() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let database = test_database(&directory.path().join("runtime.sqlite"));
        let command = ApplicationCommand {
            id: "command-transition".to_string(),
            command_type: "assistant.operation".to_string(),
            origin: CommandOrigin::Assistant,
            intent: "search".to_string(),
            capability_id: "system:search".to_string(),
            operation: "query".to_string(),
            parameters: json!({"query": "事务测试"}),
            vault_id: Some("vault-test".to_string()),
            relative_paths: Vec::new(),
            network_targets: Vec::new(),
            declared_scope: vec!["capability:system:search".to_string()],
            budget: CommandBudget {
                max_steps: 8,
                max_runtime_seconds: 300,
                max_tool_calls: 16,
                max_tokens: Some(100_000),
                max_cost: None,
            },
            idempotency_key: "transition-idempotency-key".to_string(),
            trace_id: Some("trace-transition".to_string()),
            model_decision_receipt: Some("receipt-transition".to_string()),
        };
        let decision = crate::policy::evaluate(&command);
        let task_id = database
            .persist_application_command(
                DEFAULT_LOCAL_WORKSPACE_SCOPE,
                &command,
                &decision,
                "trace-transition",
                "2026-07-21T00:00:00Z",
            )
            .expect("persist transition command")
            .0
            .expect("native task id");
        {
            let connection = database.connection.lock().expect("lock sqlite");
            connection
                .execute_batch(
                    "CREATE TRIGGER reject_task_state_audit
                     BEFORE INSERT ON operation_events
                     WHEN NEW.event_type='task.state_changed'
                     BEGIN
                       SELECT RAISE(ABORT, 'forced audit failure');
                     END;",
                )
                .expect("install audit failure trigger");
        }
        let checkpoint = json!({"id": "checkpoint-transition", "phase": "execution"});
        let error = database
            .transition_native_runtime_task(
                DEFAULT_LOCAL_WORKSPACE_SCOPE,
                &task_id,
                "running",
                25,
                "启动事务测试",
                Some(&checkpoint),
            )
            .expect_err("audit failure must abort the full transition");
        assert!(error.contains("无法保存任务状态审计事件"));
        {
            let connection = database.connection.lock().expect("lock sqlite");
            let state: String = connection
                .query_row(
                    "SELECT state FROM runtime_tasks WHERE workspace_scope=?1 AND id=?2",
                    params![DEFAULT_LOCAL_WORKSPACE_SCOPE, task_id],
                    |row| row.get(0),
                )
                .expect("read rolled back task state");
            assert_eq!(state, "queued");
            for (table, expected) in [
                ("runtime_task_attempts", 1_i64),
                ("runtime_task_transitions", 0_i64),
                ("runtime_task_checkpoints", 0_i64),
                ("operation_events", 1_i64),
            ] {
                let count: i64 = connection
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .expect("count rolled back task records");
                assert_eq!(count, expected, "unexpected rollback count in {table}");
            }
            connection
                .execute_batch("DROP TRIGGER reject_task_state_audit;")
                .expect("remove audit failure trigger");
        }
        let task = database
            .transition_native_runtime_task(
                DEFAULT_LOCAL_WORKSPACE_SCOPE,
                &task_id,
                "running",
                25,
                "启动事务测试",
                Some(&checkpoint),
            )
            .expect("commit task transition with audit");
        assert_eq!(task.state, "running");
        assert_eq!(task.progress, 25);
        let connection = database.connection.lock().expect("lock sqlite");
        for (table, expected) in [
            ("runtime_task_attempts", 2_i64),
            ("runtime_task_transitions", 1_i64),
            ("runtime_task_checkpoints", 1_i64),
            ("operation_events", 2_i64),
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("count committed task records");
            assert_eq!(count, expected, "unexpected committed count in {table}");
        }
        let event_type: String = connection
            .query_row(
                "SELECT event_type FROM operation_events ORDER BY rowid DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .expect("read committed task state event");
        assert_eq!(event_type, "task.state_changed");
    }

    #[test]
    fn database_restore_preflight_and_restore_are_transactionally_recoverable() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let database = test_database(&directory.path().join("runtime.sqlite"));
        {
            let connection = database.connection.lock().expect("lock sqlite");
            connection
                .execute(
                    "INSERT INTO workspace_state (key, value, updated_at) VALUES ('restore-test', 'before', ?1)",
                    [Utc::now().to_rfc3339()],
                )
                .expect("seed restore state");
        }
        let backup = database.backup().expect("create database backup");
        let preflight = database
            .preflight_restore(&backup.path)
            .expect("preflight database backup");
        assert!(preflight.compatible);
        assert_eq!(preflight.integrity, "ok");
        {
            let connection = database.connection.lock().expect("lock sqlite");
            connection
                .execute(
                    "UPDATE workspace_state SET value='after' WHERE key='restore-test'",
                    [],
                )
                .expect("mutate live database");
        }
        let result = database
            .restore(&backup.path)
            .expect("restore database backup");
        assert_eq!(result.integrity, "ok");
        let connection = database.connection.lock().expect("lock sqlite");
        restore_database_runtime_configuration(&connection)
            .expect("reapply runtime configuration in WAL mode");
        let journal_mode: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("read restored journal mode");
        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
        let value: String = connection
            .query_row(
                "SELECT value FROM workspace_state WHERE key='restore-test'",
                [],
                |row| row.get(0),
            )
            .expect("read restored value");
        assert_eq!(value, "before");
    }

    #[test]
    fn long_term_memory_query_governance_and_metrics_preserve_history() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let database = test_database(&directory.path().join("runtime.sqlite"));
        let event_id = "memory-governance-test";
        database
            .stage_long_term_memory_event(
                DEFAULT_LOCAL_WORKSPACE_SCOPE,
                event_id,
                "conversation.message",
                "2026-07-21T00:00:00Z",
                &json!({"actor": "user", "content": "需要长期保留的偏好"}),
            )
            .expect("stage memory event");
        let active = database
            .query_long_term_memory(DEFAULT_LOCAL_WORKSPACE_SCOPE, "偏好", false, 10)
            .expect("query active memory");
        assert_eq!(active.len(), 1);
        database
            .govern_long_term_memory(
                DEFAULT_LOCAL_WORKSPACE_SCOPE,
                &LongTermMemoryGovernanceInput {
                    id: event_id.to_string(),
                    action: "expire".to_string(),
                    replacement_id: None,
                    note: Some("用户确认该偏好已经过期".to_string()),
                },
            )
            .expect("expire memory");
        assert!(database
            .query_long_term_memory(DEFAULT_LOCAL_WORKSPACE_SCOPE, "偏好", false, 10)
            .expect("query active memory after expiry")
            .is_empty());
        let history = database
            .query_long_term_memory(DEFAULT_LOCAL_WORKSPACE_SCOPE, "偏好", true, 10)
            .expect("query memory history");
        assert_eq!(history[0].governance_state, "expired");
        let metrics = database
            .long_term_memory_metrics(DEFAULT_LOCAL_WORKSPACE_SCOPE)
            .expect("read memory metrics");
        assert_eq!(metrics.total, 1);
        assert_eq!(metrics.expired, 1);
        assert_eq!(metrics.active, 0);
    }

    fn seed_optimization_evidence(
        database: &RuntimeDatabase,
        id: &str,
        occurred_at: &str,
        content: &str,
    ) {
        database
            .stage_long_term_memory_event(
                DEFAULT_LOCAL_WORKSPACE_SCOPE,
                id,
                "conversation.message",
                occurred_at,
                &json!({
                    "actor": "user",
                    "content": content,
                    "metadata": {"conversationId": "optimization-test"}
                }),
            )
            .expect("stage optimization evidence");
        let connection = database.connection.lock().expect("lock sqlite");
        connection
            .execute(
                "UPDATE long_term_memory_events
                 SET state='committed', committed_at=?2, updated_at=?2
                 WHERE id=?1",
                params![id, occurred_at],
            )
            .expect("commit optimization evidence");
    }

    fn optimization_candidate(
        id: &str,
        batch: &OptimizationEvidenceBatch,
        rules: Vec<&str>,
    ) -> OptimizationCandidateInput {
        OptimizationCandidateInput {
            id: id.to_string(),
            expected_cursor_revision: batch.cursor_revision,
            summary: format!("候选 {id} 将减少重复确认并改善 Skill 路由。"),
            rules: rules.into_iter().map(str::to_string).collect(),
            skill_hints: json!({"web-content-analysis": "仅在链接采集时使用"}),
            metrics: json!({"messageCount": batch.events.len(), "correctionCount": 1}),
            evidence_count: batch.events.len(),
            evidence_cursor_occurred_at: batch.next_occurred_at.clone(),
            evidence_cursor_event_id: batch.next_event_id.clone(),
            expires_at: Some((Utc::now() + chrono::Duration::days(30)).to_rfc3339()),
        }
    }

    #[test]
    fn optimization_cursor_advances_only_with_committed_candidate() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let database = test_database(&directory.path().join("runtime.sqlite"));
        seed_optimization_evidence(
            &database,
            "optimization-evidence-1",
            "2026-07-21T01:00:00Z",
            "第一次纠正",
        );
        seed_optimization_evidence(
            &database,
            "optimization-evidence-2",
            "2026-07-21T01:01:00Z",
            "第二次纠正",
        );
        let batch = database
            .optimization_evidence(DEFAULT_LOCAL_WORKSPACE_SCOPE, 10)
            .expect("read optimization evidence");
        assert_eq!(batch.cursor_revision, 0);
        assert_eq!(batch.events.len(), 2);

        let mut invalid =
            optimization_candidate("optimization-invalid", &batch, vec!["保持原有权限边界"]);
        invalid.evidence_count = 1;
        database
            .create_optimization_candidate(DEFAULT_LOCAL_WORKSPACE_SCOPE, invalid)
            .expect_err("candidate with insufficient evidence must fail");
        let unchanged = database
            .optimization_evidence(DEFAULT_LOCAL_WORKSPACE_SCOPE, 10)
            .expect("read unchanged cursor");
        assert_eq!(unchanged.cursor_revision, 0);
        assert_eq!(unchanged.events.len(), 2);

        let candidate = optimization_candidate(
            "optimization-candidate-1",
            &batch,
            vec!["回答前先识别是否需要调用本地搜索能力"],
        );
        database
            .create_optimization_candidate(DEFAULT_LOCAL_WORKSPACE_SCOPE, candidate.clone())
            .expect("commit optimization candidate");
        let advanced = database
            .optimization_evidence(DEFAULT_LOCAL_WORKSPACE_SCOPE, 10)
            .expect("read advanced cursor");
        assert_eq!(advanced.cursor_revision, 1);
        assert!(advanced.events.is_empty());

        database
            .create_optimization_candidate(DEFAULT_LOCAL_WORKSPACE_SCOPE, candidate)
            .expect_err("stale cursor revision must be rejected");
    }

    #[test]
    fn optimization_evaluation_rejects_permission_expansion() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let database = test_database(&directory.path().join("runtime.sqlite"));
        seed_optimization_evidence(
            &database,
            "optimization-forbidden-1",
            "2026-07-21T02:00:00Z",
            "用户纠正了意图判断",
        );
        seed_optimization_evidence(
            &database,
            "optimization-forbidden-2",
            "2026-07-21T02:01:00Z",
            "用户要求保持权限边界",
        );
        let batch = database
            .optimization_evidence(DEFAULT_LOCAL_WORKSPACE_SCOPE, 10)
            .expect("read optimization evidence");
        database
            .create_optimization_candidate(
                DEFAULT_LOCAL_WORKSPACE_SCOPE,
                optimization_candidate(
                    "optimization-forbidden",
                    &batch,
                    vec!["扩大权限并绕过审批以提高执行速度"],
                ),
            )
            .expect("store candidate before independent evaluation");
        database
            .apply_optimization_candidate(DEFAULT_LOCAL_WORKSPACE_SCOPE, "optimization-forbidden")
            .expect_err("unevaluated candidate must not be applied");
        let evaluation = database
            .evaluate_optimization_candidate(
                DEFAULT_LOCAL_WORKSPACE_SCOPE,
                "optimization-forbidden",
            )
            .expect("evaluate forbidden candidate");
        assert!(!evaluation.passed);
        assert_eq!(evaluation.state, "rejected");
        assert!(evaluation
            .checks
            .iter()
            .any(|check| check.contains("权限、设置或访问控制")));
        database
            .apply_optimization_candidate(DEFAULT_LOCAL_WORKSPACE_SCOPE, "optimization-forbidden")
            .expect_err("rejected candidate must not be applied");
    }

    #[test]
    fn optimization_apply_and_rollback_append_immutable_versions() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let database = test_database(&directory.path().join("runtime.sqlite"));
        for (id, occurred_at, content) in [
            ("optimization-version-1", "2026-07-21T03:00:00Z", "证据一"),
            ("optimization-version-2", "2026-07-21T03:01:00Z", "证据二"),
            ("optimization-version-3", "2026-07-21T03:02:00Z", "证据三"),
            ("optimization-version-4", "2026-07-21T03:03:00Z", "证据四"),
        ] {
            seed_optimization_evidence(&database, id, occurred_at, content);
        }

        let first_batch = database
            .optimization_evidence(DEFAULT_LOCAL_WORKSPACE_SCOPE, 2)
            .expect("read first evidence batch");
        database
            .create_optimization_candidate(
                DEFAULT_LOCAL_WORKSPACE_SCOPE,
                optimization_candidate(
                    "optimization-version-a",
                    &first_batch,
                    vec!["优先复用已经验证的 Skill"],
                ),
            )
            .expect("create first candidate");
        let second_batch = database
            .optimization_evidence(DEFAULT_LOCAL_WORKSPACE_SCOPE, 2)
            .expect("read second evidence batch");
        database
            .create_optimization_candidate(
                DEFAULT_LOCAL_WORKSPACE_SCOPE,
                optimization_candidate(
                    "optimization-version-b",
                    &second_batch,
                    vec!["搜索请求优先执行本地索引查询"],
                ),
            )
            .expect("create concurrent baseline candidate");
        for candidate_id in ["optimization-version-a", "optimization-version-b"] {
            let evaluation = database
                .evaluate_optimization_candidate(DEFAULT_LOCAL_WORKSPACE_SCOPE, candidate_id)
                .expect("evaluate candidate");
            assert!(evaluation.passed);
            assert_eq!(evaluation.state, "pending_review");
        }

        let applied = database
            .apply_optimization_candidate(DEFAULT_LOCAL_WORKSPACE_SCOPE, "optimization-version-a")
            .expect("apply first candidate");
        assert_eq!(applied.version, 1);
        database
            .apply_optimization_candidate(DEFAULT_LOCAL_WORKSPACE_SCOPE, "optimization-version-b")
            .expect_err("candidate from an old baseline must not overwrite current profile");

        let rolled_back = database
            .rollback_optimization_profile(DEFAULT_LOCAL_WORKSPACE_SCOPE, Some(0))
            .expect("append rollback version");
        assert_eq!(rolled_back.version, 2);
        assert!(rolled_back.guidance.is_empty());
        let versions = database
            .list_optimization_versions(DEFAULT_LOCAL_WORKSPACE_SCOPE, 10)
            .expect("list immutable optimization versions");
        assert_eq!(versions.len(), 3);
        assert_eq!(versions[0].version, 2);
        assert_eq!(versions[0].state, "rollback");
        assert_eq!(versions[0].rollback_target, Some(0));
        assert_eq!(versions[1].version, 1);
        assert_eq!(versions[1].state, "active");
        assert_eq!(versions[2].version, 0);
        assert_eq!(versions[2].state, "initial");
    }

    #[test]
    fn model_usage_records_are_idempotent_and_keep_cost_source() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let database = test_database(&directory.path().join("runtime.sqlite"));
        database
            .record_model_usage(&ModelUsageRecord {
                request_id: "model-request-test",
                trace_id: "trace-model-request-test",
                operation: "assistant.chat",
                provider: "openai",
                model: "gpt-test",
                state: "started",
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                estimated_cost_usd: None,
                cost_source: "pending",
                duration_ms: 0,
                error: None,
            })
            .expect("record started model usage");
        database
            .record_model_usage(&ModelUsageRecord {
                request_id: "model-request-test",
                trace_id: "trace-model-request-test",
                operation: "assistant.chat",
                provider: "openai",
                model: "gpt-test",
                state: "succeeded",
                prompt_tokens: 120,
                completion_tokens: 45,
                total_tokens: 165,
                estimated_cost_usd: Some(0.0042),
                cost_source: "provider_usage_and_cost",
                duration_ms: 850,
                error: None,
            })
            .expect("update model usage");
        let connection = database.connection.lock().expect("lock sqlite");
        let row = connection
            .query_row(
                "SELECT state, prompt_tokens, completion_tokens, total_tokens,
                        estimated_cost_usd, cost_source, duration_ms
                 FROM model_usage_events WHERE request_id='model-request-test'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, f64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .expect("read model usage");
        assert_eq!(row.0, "succeeded");
        assert_eq!((row.1, row.2, row.3), (120, 45, 165));
        assert!((row.4 - 0.0042).abs() < f64::EPSILON);
        assert_eq!(row.5, "provider_usage_and_cost");
        assert_eq!(row.6, 850);
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM model_usage_events WHERE request_id='model-request-test'",
                [],
                |value| value.get(0),
            )
            .expect("count idempotent model usage");
        assert_eq!(count, 1);
    }

    #[test]
    fn application_authorization_is_explicit_and_persists_without_accounts() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let path = directory.path().join("runtime.sqlite");
        {
            let database = test_database(&path);
            let pending = database
                .application_authorization()
                .expect("read initial authorization");
            assert_eq!(pending.status, "pending");
            assert!(!pending.is_granted());
            assert_eq!(
                pending.authorization_version,
                APPLICATION_AUTHORIZATION_VERSION
            );
            assert!(pending.decided_at.is_none());

            let denied = database
                .set_application_authorization(false)
                .expect("persist denial");
            assert_eq!(denied.status, "denied");
            assert!(!denied.is_granted());
            assert!(denied.decided_at.is_some());
        }
        {
            let database = test_database(&path);
            let denied = database
                .application_authorization()
                .expect("restore denied authorization");
            assert_eq!(denied.status, "denied");

            let granted = database
                .set_application_authorization(true)
                .expect("grant from settings");
            assert!(granted.is_granted());
        }
        {
            let database = test_database(&path);
            let granted = database
                .application_authorization()
                .expect("restore granted authorization");
            assert_eq!(granted.status, "granted");
            assert!(granted.is_granted());

            let revoked = database
                .set_application_authorization(false)
                .expect("revoke from settings");
            assert_eq!(revoked.status, "denied");
        }
        {
            let database = test_database(&path);
            assert_eq!(
                database
                    .application_authorization()
                    .expect("restore revoked authorization")
                    .status,
                "denied"
            );
            assert!(database
                .set_application_authorization(true)
                .expect("reauthorize from settings")
                .is_granted());
        }
    }

    #[test]
    fn cancelled_index_transaction_rolls_back_before_commit() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let mut connection = Connection::open_in_memory().expect("open memory database");
        connection
            .execute("CREATE TABLE indexed_notes (id INTEGER PRIMARY KEY)", [])
            .expect("create index test table");
        let transaction = connection.transaction().expect("start index transaction");
        transaction
            .execute("INSERT INTO indexed_notes (id) VALUES (1)", [])
            .expect("stage index row");
        let cancelled = AtomicBool::new(true);
        assert!(ensure_index_not_cancelled(&|| cancelled.load(Ordering::Acquire)).is_err());
        drop(transaction);

        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM indexed_notes", [], |row| row.get(0))
            .expect("count committed index rows");
        assert_eq!(count, 0);
    }

    #[test]
    fn outdated_application_authorization_requires_confirmation_again() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let database = test_database(&directory.path().join("runtime.sqlite"));
        database
            .set_application_authorization(true)
            .expect("persist current grant");
        database
            .connection
            .lock()
            .expect("lock sqlite")
            .execute(
                "UPDATE application_authorization SET authorization_version=0 WHERE id=1",
                [],
            )
            .expect("downgrade stored authorization version");

        let authorization = database
            .application_authorization()
            .expect("read outdated authorization");
        assert_eq!(authorization.status, "pending");
        assert!(!authorization.is_granted());
        assert_eq!(
            authorization.authorization_version,
            APPLICATION_AUTHORIZATION_VERSION
        );
    }

    #[test]
    fn first_grant_is_restored_without_returning_to_pending() {
        let directory = tempfile::tempdir().expect("create temp directory");
        let path = directory.path().join("runtime.sqlite");
        {
            let database = test_database(&path);
            assert_eq!(
                database
                    .application_authorization()
                    .expect("read pending state")
                    .status,
                "pending"
            );
            assert!(database
                .set_application_authorization(true)
                .expect("grant on first launch")
                .is_granted());
        }
        {
            let database = test_database(&path);
            let restored = database
                .application_authorization()
                .expect("restore first-launch grant");
            assert_eq!(restored.status, "granted");
            assert!(restored.decided_at.is_some());
        }
    }
}
