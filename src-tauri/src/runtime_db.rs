use crate::obsidian::{
    collect_files_for_runtime_with_cancellation, read_file_limited_for_runtime,
    resolve_vault_for_runtime, OperationEvent, VaultDescriptor,
};
use crate::policy::{ApplicationCommand, PolicyDecision, PolicyOutcome};
use crate::task_runtime::NativeRuntimeTask;
use chrono::Utc;
use regex::Regex;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    collections::HashSet,
    fs,
    io::{ErrorKind, Write},
    path::Path,
    path::PathBuf,
    sync::Mutex,
};
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

const MAX_SNAPSHOT_RECORDS: usize = 10_000;
const MAX_RECORD_BYTES: usize = 2 * 1024 * 1024;
const MAX_INDEXED_NOTE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SEARCH_QUERY_CHARS: usize = 512;
const MAX_INBOUND_RECORD_BYTES: usize = 512 * 1024;
const DEFAULT_LOCAL_WORKSPACE_SCOPE: &str = "local";
const CURRENT_SCHEMA_VERSION: i64 = 21;
const APPLICATION_AUTHORIZATION_VERSION: i64 = 1;

pub struct RuntimeDatabase {
    pub(crate) connection: Mutex<Connection>,
    path: PathBuf,
}

pub(crate) struct ModelUsageRecord<'a> {
    pub(crate) request_id: &'a str,
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedSearchResult {
    vault_id: String,
    relative_path: String,
    title: String,
    excerpt: String,
    modified_at: String,
    score: f64,
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
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        connection
            .execute(
                "INSERT INTO model_usage_events
                 (id, workspace_scope, request_id, operation, provider, model, state,
                  prompt_tokens, completion_tokens, total_tokens, estimated_cost_usd,
                  cost_source, duration_ms, error, created_at, completed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
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
                    Utc::now().to_rfc3339(),
                    if record.state == "started" {
                        None
                    } else {
                        Some(Utc::now().to_rfc3339())
                    },
                ],
            )
            .map_err(|error| format!("无法记录模型 Token 与费用：{error}"))?;
        Ok(())
    }

    pub fn sync_vault_registry(&self, vaults: &[VaultDescriptor]) -> Result<(), String> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("无法开始 Vault 注册事务：{error}"))?;
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
            ("user_skill", snapshot.custom_skills.as_slice()),
            ("schedule", snapshot.schedules.as_slice()),
            (
                "report_subscription",
                snapshot.report_subscriptions.as_slice(),
            ),
            ("report", snapshot.reports.as_slice()),
        ];
        let total = groups.iter().map(|(_, values)| values.len()).sum::<usize>();
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
            custom_skills: list("user_skill")?,
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
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let payload = serde_json::to_string(event)
            .map_err(|error| format!("无法序列化原生操作事件：{error}"))?;
        connection
            .execute(
                "INSERT OR IGNORE INTO operation_events
                 (id, task_id, event_type, state, payload, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    event.id,
                    event.task_id,
                    event.event_type,
                    event.state,
                    payload,
                    event.created_at
                ],
            )
            .map_err(|error| format!("无法写入 SQLite 操作日志：{error}"))?;
        Ok(())
    }

    pub(crate) fn persist_application_command(
        &self,
        workspace_scope: &str,
        command: &ApplicationCommand,
        decision: &PolicyDecision,
        trace_id: &str,
        accepted_at: &str,
    ) -> Result<(Option<String>, bool), String> {
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
                "SELECT task_id FROM application_commands
                 WHERE workspace_scope=?1 AND idempotency_key=?2",
                params![workspace_scope, command.idempotency_key],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(|error| format!("无法检查应用命令幂等键：{error}"))?;
        if let Some(task_id) = duplicate {
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
                 (id, task_id, event_type, state, payload, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    event.id,
                    event.task_id,
                    event.event_type,
                    event.state,
                    event_payload,
                    event.created_at
                ],
            )
            .map_err(|error| format!("无法保存策略决定审计事件：{error}"))?;
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
        let trace_id = current.trace_id.clone();
        let created_at = current.created_at.clone();
        let mut payload = current.payload;
        let object = payload
            .as_object_mut()
            .ok_or_else(|| "原生任务负载不是 JSON 对象".to_string())?;
        object.insert("state".to_string(), Value::String(target_state.to_string()));
        object.insert("progress".to_string(), Value::from(progress));
        object.insert("updatedAt".to_string(), Value::String(now.clone()));
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
                "UPDATE runtime_tasks SET state=?3, payload=?4, updated_at=?5
                 WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, task_id, target_state, serialized, now],
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
            trace_id: trace_id.clone(),
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
        let event_payload = serde_json::to_string(&event)
            .map_err(|error| format!("无法序列化任务状态审计事件：{error}"))?;
        transaction
            .execute(
                "INSERT INTO operation_events
                 (id, task_id, event_type, state, payload, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    event.id,
                    event.task_id,
                    event.event_type,
                    event.state,
                    event_payload,
                    event.created_at
                ],
            )
            .map_err(|error| format!("无法保存任务状态审计事件：{error}"))?;
        let next = NativeRuntimeTask {
            id: task_id.to_string(),
            state: target_state.to_string(),
            title,
            trace_id,
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
            .execute("DELETE FROM note_index WHERE vault_id=?1", [vault_id])
            .map_err(|error| format!("无法清理笔记索引：{error}"))?;

        let mut indexed_notes = 0;
        let mut skipped_notes = 0;
        for path in markdown {
            ensure_index_not_cancelled(is_cancelled)?;
            match index_note_in_transaction(&transaction, vault_id, &root, &path) {
                Ok(true) => indexed_notes += 1,
                Ok(false) | Err(_) => skipped_notes += 1,
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

    pub fn index_note_path_with_cancellation<F>(
        &self,
        vault_id: &str,
        root: &Path,
        path: &Path,
        is_cancelled: &F,
    ) -> Result<(), String>
    where
        F: Fn() -> bool,
    {
        ensure_index_not_cancelled(is_cancelled)?;
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "监听文件越过 Vault 边界".to_string())?
            .to_string_lossy()
            .into_owned();
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        if !path.exists() {
            let transaction = connection
                .unchecked_transaction()
                .map_err(|error| format!("无法开始增量索引事务：{error}"))?;
            ensure_index_not_cancelled(is_cancelled)?;
            transaction
                .execute(
                    "DELETE FROM note_fts WHERE vault_id=?1 AND relative_path=?2",
                    params![vault_id, relative],
                )
                .map_err(|error| format!("无法删除全文索引项：{error}"))?;
            transaction
                .execute(
                    "DELETE FROM note_index WHERE vault_id=?1 AND relative_path=?2",
                    params![vault_id, relative],
                )
                .map_err(|error| format!("无法删除笔记索引项：{error}"))?;
            ensure_index_not_cancelled(is_cancelled)?;
            return transaction
                .commit()
                .map_err(|error| format!("无法提交增量索引事务：{error}"));
        }
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| format!("无法读取监听文件元数据：{error}"))?;
        if metadata.file_type().is_symlink() {
            return Err("拒绝索引 Vault 内的符号链接".to_string());
        }
        let canonical_root = root
            .canonicalize()
            .map_err(|error| format!("无法规范化 Vault 路径：{error}"))?;
        let canonical_path = path
            .canonicalize()
            .map_err(|error| format!("无法规范化监听文件：{error}"))?;
        if !canonical_path.starts_with(&canonical_root) || !canonical_path.is_file() {
            return Err("监听文件越过 Vault 边界或不是普通文件".to_string());
        }
        index_note_in_connection_with_cancellation(
            &connection,
            vault_id,
            &canonical_root,
            &canonical_path,
            is_cancelled,
        )
        .map(|_| ())
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
            run_migrations(&destination)?;
            destination
                .execute_batch(
                    "PRAGMA journal_mode=WAL;
                     PRAGMA synchronous=FULL;
                     PRAGMA foreign_keys=ON;
                     PRAGMA wal_checkpoint(TRUNCATE);",
                )
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
                   trace_id TEXT,
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
                   trace_id TEXT NOT NULL,
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
        let trace_id = task
            .get("traceId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let payload =
            serde_json::to_string(task).map_err(|error| format!("无法序列化原生任务：{error}"))?;
        let old_state = transaction
            .query_row(
                "SELECT state FROM runtime_tasks WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("无法读取原生任务状态：{error}"))?;
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
        if old_state.as_deref() != Some(state) {
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
        .to_string();
    let tag_regex = Regex::new(r"(?:^|\s)#([\p{L}\p{N}_/-]+)").expect("valid tag regex");
    let link_regex = Regex::new(r"\[\[([^\]|#]+)").expect("valid wiki link regex");
    let mut tags = tag_regex
        .captures_iter(content)
        .filter_map(|capture| capture.get(1).map(|value| value.as_str().to_string()))
        .collect::<Vec<_>>();
    let mut links = link_regex
        .captures_iter(content)
        .filter_map(|capture| {
            capture
                .get(1)
                .map(|value| value.as_str().trim().to_string())
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

fn index_note_in_connection_with_cancellation<F>(
    connection: &Connection,
    vault_id: &str,
    root: &Path,
    path: &Path,
    is_cancelled: &F,
) -> Result<bool, String>
where
    F: Fn() -> bool,
{
    ensure_index_not_cancelled(is_cancelled)?;
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| format!("无法开始增量索引事务：{error}"))?;
    let indexed = index_note_in_transaction(&transaction, vault_id, root, path)?;
    ensure_index_not_cancelled(is_cancelled)?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交增量索引事务：{error}"))?;
    Ok(indexed)
}

fn index_note_in_transaction(
    transaction: &Transaction<'_>,
    vault_id: &str,
    root: &Path,
    path: &Path,
) -> Result<bool, String> {
    if path
        .components()
        .any(|component| component.as_os_str().to_string_lossy().starts_with('.'))
    {
        return Ok(false);
    }
    let metadata = fs::metadata(path).map_err(|error| format!("无法读取索引文件：{error}"))?;
    if metadata.len() > MAX_INDEXED_NOTE_BYTES {
        return Ok(false);
    }
    let bytes = read_file_limited_for_runtime(path)?;
    let content =
        String::from_utf8(bytes.clone()).map_err(|_| "索引文件不是有效 UTF-8".to_string())?;
    let relative_path = path
        .strip_prefix(root)
        .map_err(|_| "索引文件越过 Vault 边界".to_string())?
        .to_string_lossy()
        .into_owned();
    let (fallback_title, tags, links) = markdown_metadata(&content);
    let title = if fallback_title == "无标题笔记" {
        path.file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("无标题笔记")
            .to_string()
    } else {
        fallback_title
    };
    let modified_at = metadata
        .modified()
        .ok()
        .map(chrono::DateTime::<Utc>::from)
        .map(|value| value.to_rfc3339())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());
    let content_hash = format!("{:x}", Sha256::digest(&bytes));
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
                relative_path,
                title,
                content_hash,
                modified_at,
                metadata.len(),
                serde_json::to_string(&tags).unwrap_or_else(|_| "[]".to_string()),
                serde_json::to_string(&links).unwrap_or_else(|_| "[]".to_string()),
            ],
        )
        .map_err(|error| format!("无法更新笔记索引：{error}"))?;
    transaction
        .execute(
            "DELETE FROM note_fts WHERE vault_id=?1 AND relative_path=?2",
            params![vault_id, relative_path],
        )
        .map_err(|error| format!("无法刷新全文索引：{error}"))?;
    transaction
        .execute(
            "INSERT INTO note_fts (vault_id, relative_path, title, content)
             VALUES (?1, ?2, ?3, ?4)",
            params![vault_id, relative_path, title, content],
        )
        .map_err(|error| format!("无法写入全文索引：{error}"))?;
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

#[tauri::command]
pub fn indexed_search(
    database: State<'_, RuntimeDatabase>,
    vault_id: Option<String>,
    query: String,
    limit: Option<usize>,
) -> Result<Vec<IndexedSearchResult>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("搜索词不能为空".to_string());
    }
    let match_query = fts_match_query(query)?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let scoped = vault_id.as_deref().filter(|value| *value != "all");
    let sql = if scoped.is_some() {
        "SELECT f.vault_id, f.relative_path, f.title,
                snippet(note_fts, 3, '', '', '…', 24), i.modified_at,
                bm25(note_fts)
         FROM note_fts f
         JOIN note_index i ON i.vault_id=f.vault_id AND i.relative_path=f.relative_path
         WHERE note_fts MATCH ?1 AND f.vault_id=?2
         ORDER BY bm25(note_fts) LIMIT ?3"
    } else {
        "SELECT f.vault_id, f.relative_path, f.title,
                snippet(note_fts, 3, '', '', '…', 24), i.modified_at,
                bm25(note_fts)
         FROM note_fts f
         JOIN note_index i ON i.vault_id=f.vault_id AND i.relative_path=f.relative_path
         WHERE note_fts MATCH ?1
         ORDER BY bm25(note_fts) LIMIT ?2"
    };
    let mut statement = connection
        .prepare(sql)
        .map_err(|error| format!("无法准备全文搜索：{error}"))?;
    let max_results = limit.unwrap_or(50).clamp(1, 200) as i64;
    let map_row = |row: &rusqlite::Row<'_>| {
        Ok(IndexedSearchResult {
            vault_id: row.get(0)?,
            relative_path: row.get(1)?,
            title: row.get(2)?,
            excerpt: row.get(3)?,
            modified_at: row.get(4)?,
            score: row.get(5)?,
        })
    };
    let results = if let Some(vault_id) = scoped {
        statement
            .query_map(params![match_query, vault_id, max_results], map_row)
            .map_err(|error| format!("全文搜索失败：{error}"))?
            .filter_map(Result::ok)
            .collect()
    } else {
        statement
            .query_map(params![match_query, max_results], map_row)
            .map_err(|error| format!("全文搜索失败：{error}"))?
            .filter_map(Result::ok)
            .collect()
    };
    Ok(results)
}
