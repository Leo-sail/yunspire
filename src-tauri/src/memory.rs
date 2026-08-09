use crate::{
    execution_ticket::ExecutionTicketState,
    obsidian::OperationContext,
    prompt::{prompt_text, render_prompt_template},
    runtime_db::{
        persist_runtime_effect_mutation_result, read_runtime_effect_mutation_result,
        record_optimization_runtime_handler_completion, runtime_effect_mutation_key,
        validate_optimization_runtime_handler, OptimizationProfileResult, RuntimeDatabase,
        RuntimeEffectMutationKey,
    },
};
use chrono::{Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::HashSet, time::Instant};
use tauri::State;
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

const MAX_MEMORY_CONTENT_BYTES: usize = 512 * 1024;
const MAX_MEMORY_EVIDENCE: usize = 64;
const MAX_MEMORY_QUERY_CHARS: usize = 512;
const ASSISTANT_MEMORY_CONTEXT_HEADER_PROMPT: &str =
    include_str!("../../prompts/runtime/memory/context-header.txt");
const ASSISTANT_USER_MEMORY_SECTION_PROMPT_TEMPLATE: &str =
    include_str!("../../prompts/runtime/memory/user-section.template.txt");
const ASSISTANT_AGENT_MEMORY_SECTION_PROMPT_TEMPLATE: &str =
    include_str!("../../prompts/runtime/memory/agent-section.template.txt");
const ASSISTANT_USER_EPISODE_CONTENT_PROMPT_TEMPLATE: &str =
    include_str!("../../prompts/runtime/memory/user-episode-content.template.txt");
const ASSISTANT_USER_EPISODE_TITLE_PROMPT_TEMPLATE: &str =
    include_str!("../../prompts/runtime/memory/user-episode-title.template.txt");
const ASSISTANT_AGENT_CASE_CONTENT_PROMPT_TEMPLATE: &str =
    include_str!("../../prompts/runtime/memory/agent-case-content.template.txt");
const ASSISTANT_AGENT_CASE_TITLE_PROMPT_TEMPLATE: &str =
    include_str!("../../prompts/runtime/memory/agent-case-title.template.txt");
const ASSISTANT_RESPONSE_STYLE_CONTENT_PROMPT_TEMPLATE: &str =
    include_str!("../../prompts/runtime/memory/response-style-content.template.txt");
const ASSISTANT_RESPONSE_STYLE_TITLE_PROMPT: &str =
    include_str!("../../prompts/runtime/memory/response-style-title.txt");
const ASSISTANT_MEMORY_ITEM_PROMPT_TEMPLATE: &str =
    include_str!("../../prompts/runtime/memory/context-item.template.txt");

fn default_user_id() -> String {
    "local".to_string()
}

fn default_agent_id() -> String {
    "yunspire-assistant".to_string()
}

fn default_app_id() -> String {
    "yunspire".to_string()
}

fn default_global_scope() -> String {
    "global".to_string()
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MemoryScope {
    #[serde(default = "default_user_id")]
    pub user_id: String,
    #[serde(default = "default_agent_id")]
    pub agent_id: String,
    #[serde(default = "default_app_id")]
    pub app_id: String,
    #[serde(default = "default_global_scope")]
    pub project_id: String,
    #[serde(default = "default_global_scope")]
    pub session_id: String,
}

impl Default for MemoryScope {
    fn default() -> Self {
        Self {
            user_id: default_user_id(),
            agent_id: default_agent_id(),
            app_id: default_app_id(),
            project_id: default_global_scope(),
            session_id: default_global_scope(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryEvidence {
    pub source_id: String,
    #[serde(default)]
    pub excerpt: String,
    #[serde(default)]
    pub content_hash: Option<String>,
    #[serde(default)]
    pub relative_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryRecordInput {
    #[serde(default)]
    pub id: Option<String>,
    pub track: String,
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub scope: MemoryScope,
    pub source_doc_id: String,
    #[serde(default)]
    pub source_relative_path: Option<String>,
    #[serde(default)]
    pub source_content_hash: Option<String>,
    pub evidence: Vec<MemoryEvidence>,
    pub confidence: f64,
    #[serde(default)]
    pub supersedes_id: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default = "default_memory_state")]
    pub state: String,
}

fn default_memory_state() -> String {
    "active".to_string()
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryRecord {
    pub id: String,
    pub track: String,
    pub title: String,
    pub content: String,
    pub scope: MemoryScope,
    pub source_doc_id: String,
    pub source_relative_path: Option<String>,
    pub source_content_hash: Option<String>,
    pub evidence: Vec<MemoryEvidence>,
    pub confidence: f64,
    pub version: i64,
    pub supersedes_id: Option<String>,
    pub state: String,
    pub expires_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySearchRequest {
    pub query: String,
    #[serde(default)]
    pub tracks: Vec<String>,
    #[serde(default)]
    pub scope: MemoryScope,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryListRequest {
    #[serde(default)]
    pub tracks: Vec<String>,
    #[serde(default)]
    pub scope: MemoryScope,
    #[serde(default)]
    pub include_all_contexts: bool,
    #[serde(default)]
    pub include_inactive: bool,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySearchResult {
    #[serde(flatten)]
    pub record: MemoryRecord,
    pub score: f64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReflectionJobInput {
    pub idempotency_key: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub scope: MemoryScope,
    pub source_doc_ids: Vec<String>,
    pub source_content_hash: String,
    #[serde(default)]
    pub metrics: Value,
    #[serde(default)]
    pub source_effect_ids: Vec<String>,
    #[serde(default)]
    pub source_snapshot: Option<Value>,
    #[serde(default)]
    pub source_snapshot_hash: Option<String>,
}

// `sourceContentHash` identifies the caller's source documents; the separate
// `sourceSnapshotHash` covers this canonical JSON envelope and its material.
// Keeping both hashes makes replay provenance explicit without pretending that
// metadata and the original source bytes are interchangeable.

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReflectionJob {
    pub id: String,
    pub idempotency_key: String,
    pub task_id: Option<String>,
    pub scope: MemoryScope,
    pub source_doc_ids: Vec<String>,
    pub source_content_hash: String,
    pub source_snapshot: Value,
    pub source_snapshot_hash: String,
    pub metrics: Value,
    pub state: String,
    pub proposal_memory_id: Option<String>,
    pub optimization_candidate_id: Option<String>,
    pub attempt_count: i64,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub claimed_by: Option<String>,
    pub lease_expires_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReflectionJobListRequest {
    #[serde(default)]
    pub states: Vec<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReflectionJobClaimInput {
    pub worker_id: String,
    #[serde(default)]
    pub lease_seconds: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReflectionJobClaim {
    pub job: ReflectionJob,
    pub claim_token: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryBackendStatus {
    pub active_backend: &'static str,
    pub canonical_source: &'static str,
}

#[derive(Clone, Debug)]
pub(crate) struct AssistantTurnMemoryCapture {
    pub(crate) request_id: String,
    pub(crate) conversation_id: String,
    pub(crate) user_message_id: Option<String>,
    pub(crate) user_message: String,
    pub(crate) assistant_message_id: Option<String>,
    pub(crate) assistant_reply: String,
    pub(crate) intent: Option<String>,
    pub(crate) action: Option<String>,
    pub(crate) state: String,
    pub(crate) error: Option<String>,
    pub(crate) explicit_user_style: Option<String>,
    pub(crate) scope: MemoryScope,
}

fn normalized_required(value: &str, label: &str, max_chars: usize) -> Result<String, String> {
    let normalized = value.trim().nfc().collect::<String>();
    if normalized.is_empty()
        || normalized.chars().count() > max_chars
        || normalized.chars().any(char::is_control)
    {
        return Err(format!("{label}无效"));
    }
    Ok(normalized)
}

fn normalized_memory_content(value: &str) -> Result<String, String> {
    let normalized = value.trim().nfc().collect::<String>();
    if normalized.is_empty()
        || normalized.chars().count() > MAX_MEMORY_CONTENT_BYTES
        || normalized
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err("记忆内容无效".to_string());
    }
    Ok(normalized)
}

fn normalized_optional(
    value: Option<&str>,
    label: &str,
    max_chars: usize,
) -> Result<Option<String>, String> {
    value
        .map(|value| normalized_required(value, label, max_chars))
        .transpose()
}

fn validate_track(track: &str) -> Result<String, String> {
    let track = normalized_required(track, "记忆轨道", 32)?;
    if !matches!(
        track.as_str(),
        "user_episode" | "user_profile" | "agent_case" | "agent_skill"
    ) {
        return Err(
            "记忆轨道必须是 user_episode、user_profile、agent_case 或 agent_skill".to_string(),
        );
    }
    Ok(track)
}

fn validate_scope(scope: &MemoryScope) -> Result<MemoryScope, String> {
    Ok(MemoryScope {
        user_id: normalized_required(&scope.user_id, "userId", 160)?,
        agent_id: normalized_required(&scope.agent_id, "agentId", 160)?,
        app_id: normalized_required(&scope.app_id, "appId", 160)?,
        project_id: normalized_required(&scope.project_id, "projectId", 160)?,
        session_id: normalized_required(&scope.session_id, "sessionId", 160)?,
    })
}

fn looks_sensitive(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    [
        "authorization: bearer ",
        "api_key=",
        "apikey=",
        "password=",
        "set-cookie:",
        "cookie:",
        "-----begin private key-----",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn validate_hash(value: Option<&str>) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim().to_ascii_lowercase();
    let digest = value.strip_prefix("sha256:").unwrap_or(&value);
    if digest.len() != 64
        || !digest
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err("content hash 必须是 SHA-256".to_string());
    }
    Ok(Some(format!("sha256:{digest}")))
}

fn validate_memory_input(input: &MemoryRecordInput) -> Result<MemoryRecordInput, String> {
    let content = normalized_memory_content(&input.content)?;
    if content.len() > MAX_MEMORY_CONTENT_BYTES {
        return Err("记忆内容超过 512 KB 安全上限".to_string());
    }
    if looks_sensitive(&content) {
        return Err("记忆内容疑似包含凭据，已拒绝保存".to_string());
    }
    if input.evidence.is_empty() || input.evidence.len() > MAX_MEMORY_EVIDENCE {
        return Err("记忆必须包含 1 到 64 条证据".to_string());
    }
    if !(0.0..=1.0).contains(&input.confidence) || !input.confidence.is_finite() {
        return Err("记忆置信度必须在 0 到 1 之间".to_string());
    }
    if !matches!(input.state.as_str(), "active" | "draft") {
        return Err("新记忆状态只能是 active 或 draft".to_string());
    }
    let evidence = input
        .evidence
        .iter()
        .map(|item| {
            let excerpt = item.excerpt.trim().nfc().take(4_000).collect::<String>();
            if looks_sensitive(&excerpt) {
                return Err("记忆证据疑似包含凭据，已拒绝保存".to_string());
            }
            Ok(MemoryEvidence {
                source_id: normalized_required(&item.source_id, "证据 sourceId", 200)?,
                excerpt,
                content_hash: validate_hash(item.content_hash.as_deref())?,
                relative_path: normalized_optional(
                    item.relative_path.as_deref(),
                    "证据 relativePath",
                    2_048,
                )?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(MemoryRecordInput {
        id: normalized_optional(input.id.as_deref(), "记忆 ID", 160)?,
        track: validate_track(&input.track)?,
        title: normalized_required(&input.title, "记忆标题", 240)?,
        content,
        scope: validate_scope(&input.scope)?,
        source_doc_id: normalized_required(&input.source_doc_id, "sourceDocId", 200)?,
        source_relative_path: normalized_optional(
            input.source_relative_path.as_deref(),
            "sourceRelativePath",
            2_048,
        )?,
        source_content_hash: validate_hash(input.source_content_hash.as_deref())?,
        evidence,
        confidence: input.confidence,
        supersedes_id: normalized_optional(input.supersedes_id.as_deref(), "supersedesId", 160)?,
        expires_at: normalized_optional(input.expires_at.as_deref(), "expiresAt", 80)?,
        state: input.state.clone(),
    })
}

fn memory_payload_hash(input: &MemoryRecordInput) -> Result<String, String> {
    let serialized =
        serde_json::to_vec(input).map_err(|error| format!("无法序列化记忆记录：{error}"))?;
    Ok(format!("{:x}", Sha256::digest(serialized)))
}

fn parse_json<T: serde::de::DeserializeOwned>(value: &str, label: &str) -> rusqlite::Result<T> {
    serde_json::from_str(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            value.len(),
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{label}: {error}"),
            )),
        )
    })
}

fn map_memory_record(row: &Row<'_>) -> rusqlite::Result<MemoryRecord> {
    let evidence_json = row.get::<_, String>(12)?;
    Ok(MemoryRecord {
        id: row.get(0)?,
        track: row.get(1)?,
        title: row.get(2)?,
        content: row.get(3)?,
        scope: MemoryScope {
            user_id: row.get(4)?,
            agent_id: row.get(5)?,
            app_id: row.get(6)?,
            project_id: row.get(7)?,
            session_id: row.get(8)?,
        },
        source_doc_id: row.get(9)?,
        source_relative_path: row.get(10)?,
        source_content_hash: row.get(11)?,
        evidence: parse_json(&evidence_json, "无法解析记忆证据")?,
        confidence: row.get(13)?,
        version: row.get(14)?,
        supersedes_id: row.get(15)?,
        state: row.get(16)?,
        expires_at: row.get(17)?,
        created_at: row.get(18)?,
        updated_at: row.get(19)?,
    })
}

const MEMORY_RECORD_COLUMNS: &str =
    "id, track, title, content, user_id, agent_id, app_id, project_id, session_id, \
     source_doc_id, source_relative_path, source_content_hash, evidence_json, confidence, \
     version, supersedes_id, state, expires_at, created_at, updated_at";

const MEMORY_RECORD_JOIN_COLUMNS: &str =
    "r.id, r.track, r.title, r.content, r.user_id, r.agent_id, r.app_id, r.project_id, r.session_id, \
     r.source_doc_id, r.source_relative_path, r.source_content_hash, r.evidence_json, r.confidence, \
     r.version, r.supersedes_id, r.state, r.expires_at, r.created_at, r.updated_at";

fn read_memory_record(
    connection: &rusqlite::Connection,
    workspace_scope: &str,
    record_id: &str,
) -> Result<MemoryRecord, String> {
    connection
        .query_row(
            &format!(
                "SELECT {MEMORY_RECORD_COLUMNS} FROM memory_records \
                 WHERE workspace_scope=?1 AND id=?2"
            ),
            params![workspace_scope, record_id],
            map_memory_record,
        )
        .optional()
        .map_err(|error| format!("无法读取记忆记录：{error}"))?
        .ok_or_else(|| "记忆记录不存在".to_string())
}

fn refresh_memory_fts(
    connection: &rusqlite::Connection,
    workspace_scope: &str,
    record: &MemoryRecord,
) -> Result<(), String> {
    connection
        .execute(
            "DELETE FROM memory_fts WHERE workspace_scope=?1 AND memory_id=?2",
            params![workspace_scope, record.id],
        )
        .map_err(|error| format!("无法刷新记忆全文索引：{error}"))?;
    if record.state != "active" {
        return Ok(());
    }
    let evidence = record
        .evidence
        .iter()
        .map(|item| item.excerpt.as_str())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let cjk_terms = memory_cjk_terms(&format!(
        "{}\n{}\n{}",
        record.title, record.content, evidence
    ));
    connection
        .execute(
            "INSERT INTO memory_fts (workspace_scope, memory_id, title, content, evidence, cjk_terms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                workspace_scope,
                record.id,
                record.title,
                record.content,
                evidence,
                cjk_terms,
            ],
        )
        .map_err(|error| format!("无法写入记忆全文索引：{error}"))?;
    Ok(())
}

fn insert_memory_revision(
    connection: &rusqlite::Connection,
    workspace_scope: &str,
    record: &MemoryRecord,
    payload_hash: &str,
) -> Result<(), String> {
    let payload =
        serde_json::to_string(record).map_err(|error| format!("无法序列化记忆修订：{error}"))?;
    connection
        .execute(
            "INSERT INTO memory_record_revisions \
             (id, workspace_scope, memory_id, version, state, payload, payload_hash, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                Uuid::new_v4().to_string(),
                workspace_scope,
                record.id,
                record.version,
                record.state,
                payload,
                payload_hash,
                record.updated_at,
            ],
        )
        .map_err(|error| format!("无法保存记忆修订：{error}"))?;
    Ok(())
}

fn upsert_memory_in_transaction(
    transaction: &Transaction<'_>,
    workspace_scope: &str,
    input: &MemoryRecordInput,
) -> Result<MemoryRecord, String> {
    let input = validate_memory_input(input)?;
    let record_id = input
        .id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let payload_hash = memory_payload_hash(&input)?;
    let existing = transaction
        .query_row(
            "SELECT version, payload_hash, state, created_at FROM memory_records \
             WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, record_id],
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
        .map_err(|error| format!("无法读取当前记忆版本：{error}"))?;
    if existing
        .as_ref()
        .is_some_and(|(_, hash, state, _)| hash == &payload_hash && state == &input.state)
    {
        return read_memory_record(transaction, workspace_scope, &record_id);
    }
    if existing
        .as_ref()
        .is_some_and(|(_, _, state, _)| matches!(state.as_str(), "superseded" | "tombstone"))
    {
        return Err("已替代或已墓碑的记忆不能重新激活，请使用新的记忆 ID".to_string());
    }
    if let Some(supersedes_id) = input.supersedes_id.as_deref() {
        if supersedes_id == record_id {
            return Err("记忆不能替代自身".to_string());
        }
        let superseded = read_memory_record(transaction, workspace_scope, supersedes_id)
            .map_err(|_| "要替代的记忆不存在或已经失效".to_string())?;
        if !matches!(superseded.state.as_str(), "active" | "draft") {
            return Err("要替代的记忆不存在或已经失效".to_string());
        }
        change_memory_state_in_transaction(
            transaction,
            workspace_scope,
            supersedes_id,
            "superseded",
            &format!("superseded_by:{record_id}"),
        )?;
    }
    let now = Utc::now().to_rfc3339();
    let version = existing
        .as_ref()
        .map_or(1, |(version, _, _, _)| version + 1);
    let created_at = existing
        .as_ref()
        .map(|(_, _, _, created_at)| created_at.as_str())
        .unwrap_or(now.as_str());
    let evidence_json = serde_json::to_string(&input.evidence)
        .map_err(|error| format!("无法序列化记忆证据：{error}"))?;
    transaction
        .execute(
            "INSERT INTO memory_records \
             (workspace_scope, id, track, title, content, user_id, agent_id, app_id, project_id, session_id, \
              source_doc_id, source_relative_path, source_content_hash, evidence_json, confidence, version, \
              supersedes_id, state, expires_at, payload_hash, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22) \
             ON CONFLICT(workspace_scope, id) DO UPDATE SET \
               track=excluded.track, title=excluded.title, content=excluded.content, user_id=excluded.user_id, \
               agent_id=excluded.agent_id, app_id=excluded.app_id, project_id=excluded.project_id, \
               session_id=excluded.session_id, source_doc_id=excluded.source_doc_id, \
               source_relative_path=excluded.source_relative_path, source_content_hash=excluded.source_content_hash, \
               evidence_json=excluded.evidence_json, confidence=excluded.confidence, version=excluded.version, \
               supersedes_id=excluded.supersedes_id, state=excluded.state, expires_at=excluded.expires_at, \
               payload_hash=excluded.payload_hash, updated_at=excluded.updated_at",
            params![
                workspace_scope,
                record_id,
                input.track,
                input.title,
                input.content,
                input.scope.user_id,
                input.scope.agent_id,
                input.scope.app_id,
                input.scope.project_id,
                input.scope.session_id,
                input.source_doc_id,
                input.source_relative_path,
                input.source_content_hash,
                evidence_json,
                input.confidence,
                version,
                input.supersedes_id,
                input.state,
                input.expires_at,
                payload_hash,
                created_at,
                now,
            ],
        )
        .map_err(|error| format!("无法保存记忆记录：{error}"))?;
    let record = read_memory_record(transaction, workspace_scope, &record_id)?;
    refresh_memory_fts(transaction, workspace_scope, &record)?;
    insert_memory_revision(transaction, workspace_scope, &record, &payload_hash)?;
    Ok(record)
}

fn assistant_memory_text(value: &str, max_chars: usize) -> String {
    value.trim().nfc().take(max_chars).collect::<String>()
}

fn assistant_memory_id(kind: &str, stable_key: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(stable_key.as_bytes()));
    format!("{kind}-{}", &digest[..32])
}

fn assistant_memory_evidence(source_id: &str, excerpt: &str, content_hash: &str) -> MemoryEvidence {
    MemoryEvidence {
        source_id: assistant_memory_text(source_id, 200),
        excerpt: assistant_memory_text(excerpt, 4_000),
        content_hash: Some(content_hash.to_string()),
        relative_path: None,
    }
}

pub(crate) fn persist_assistant_turn_memories(
    transaction: &Transaction<'_>,
    workspace_scope: &str,
    capture: &AssistantTurnMemoryCapture,
) -> Result<Vec<MemoryRecord>, String> {
    let request_id = normalized_required(&capture.request_id, "AI助手请求 ID", 160)?;
    let conversation_id = normalized_required(&capture.conversation_id, "AI助手对话 ID", 200)?;
    let scope = validate_scope(&capture.scope)?;
    let user_message = assistant_memory_text(&capture.user_message, 12_000);
    let assistant_reply = assistant_memory_text(&capture.assistant_reply, 12_000);
    let error = capture
        .error
        .as_deref()
        .map(|value| assistant_memory_text(value, 2_000))
        .filter(|value| !value.is_empty());
    let intent = capture
        .intent
        .as_deref()
        .map(|value| assistant_memory_text(value, 120))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "general".to_string());
    let action = capture
        .action
        .as_deref()
        .map(|value| assistant_memory_text(value, 120))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "chat".to_string());
    let state = assistant_memory_text(&capture.state, 40);
    let source_material = format!(
        "request={request_id}\nconversation={conversation_id}\nuser={user_message}\nassistant={assistant_reply}\nintent={intent}\naction={action}\nstate={state}\nerror={}",
        error.as_deref().unwrap_or("")
    );
    let source_hash = format!("sha256:{:x}", Sha256::digest(source_material.as_bytes()));
    let user_source_id = capture
        .user_message_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&request_id);
    let assistant_source_id = capture
        .assistant_message_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&request_id);
    let mut records = Vec::new();

    if !user_message.is_empty() && !looks_sensitive(&user_message) {
        let outcome = if assistant_reply.is_empty() {
            error.as_deref().unwrap_or(&state)
        } else {
            &assistant_reply
        };
        let outcome_summary = assistant_memory_text(outcome, 4_000);
        let content = render_prompt_template(
            ASSISTANT_USER_EPISODE_CONTENT_PROMPT_TEMPLATE,
            &[
                ("user_message", &user_message),
                ("state", &state),
                ("outcome", &outcome_summary),
            ],
        )?;
        if !looks_sensitive(&content) {
            let title_summary = assistant_memory_text(&user_message.replace(['\n', '\r'], " "), 80);
            records.push(upsert_memory_in_transaction(
                transaction,
                workspace_scope,
                &MemoryRecordInput {
                    id: Some(assistant_memory_id("user-episode", &request_id)),
                    track: "user_episode".to_string(),
                    title: render_prompt_template(
                        ASSISTANT_USER_EPISODE_TITLE_PROMPT_TEMPLATE,
                        &[("summary", &title_summary)],
                    )?,
                    content,
                    scope: scope.clone(),
                    source_doc_id: request_id.clone(),
                    source_relative_path: None,
                    source_content_hash: Some(source_hash.clone()),
                    evidence: vec![assistant_memory_evidence(
                        user_source_id,
                        &user_message,
                        &source_hash,
                    )],
                    confidence: 1.0,
                    supersedes_id: None,
                    expires_at: None,
                    state: "draft".to_string(),
                },
            )?);
        }
    }

    let case_outcome = if assistant_reply.is_empty() {
        error.as_deref().unwrap_or(&state)
    } else {
        &assistant_reply
    };
    let case_outcome_summary = assistant_memory_text(case_outcome, 6_000);
    let case_content = render_prompt_template(
        ASSISTANT_AGENT_CASE_CONTENT_PROMPT_TEMPLATE,
        &[
            ("intent", &intent),
            ("action", &action),
            ("state", &state),
            ("outcome", &case_outcome_summary),
        ],
    )?;
    if !case_outcome.is_empty() && !looks_sensitive(&case_content) {
        records.push(upsert_memory_in_transaction(
            transaction,
            workspace_scope,
            &MemoryRecordInput {
                id: Some(assistant_memory_id("agent-case", &request_id)),
                track: "agent_case".to_string(),
                title: render_prompt_template(
                    ASSISTANT_AGENT_CASE_TITLE_PROMPT_TEMPLATE,
                    &[("intent", &intent), ("action", &action)],
                )?,
                content: case_content,
                scope: scope.clone(),
                source_doc_id: request_id.clone(),
                source_relative_path: None,
                source_content_hash: Some(source_hash.clone()),
                evidence: vec![assistant_memory_evidence(
                    assistant_source_id,
                    case_outcome,
                    &source_hash,
                )],
                confidence: 1.0,
                supersedes_id: None,
                expires_at: None,
                state: "draft".to_string(),
            },
        )?);
    }

    if let Some(style) = capture
        .explicit_user_style
        .as_deref()
        .map(|value| assistant_memory_text(value, 240))
        .filter(|value| !value.is_empty() && !looks_sensitive(value))
    {
        let profile_content = render_prompt_template(
            ASSISTANT_RESPONSE_STYLE_CONTENT_PROMPT_TEMPLATE,
            &[("style", &style)],
        )?;
        records.push(upsert_memory_in_transaction(
            transaction,
            workspace_scope,
            &MemoryRecordInput {
                id: Some("user-profile-response-style".to_string()),
                track: "user_profile".to_string(),
                title: prompt_text(ASSISTANT_RESPONSE_STYLE_TITLE_PROMPT).to_string(),
                content: profile_content,
                scope,
                source_doc_id: request_id.clone(),
                source_relative_path: None,
                source_content_hash: Some(source_hash.clone()),
                evidence: vec![assistant_memory_evidence(
                    user_source_id,
                    &user_message,
                    &source_hash,
                )],
                confidence: 1.0,
                supersedes_id: None,
                expires_at: None,
                state: "active".to_string(),
            },
        )?);
    }

    Ok(records)
}

fn change_memory_state_in_transaction(
    transaction: &Transaction<'_>,
    workspace_scope: &str,
    record_id: &str,
    target_state: &str,
    reason: &str,
) -> Result<MemoryRecord, String> {
    if !matches!(target_state, "active" | "superseded" | "tombstone") {
        return Err("不支持的记忆目标状态".to_string());
    }
    let now = Utc::now().to_rfc3339();
    let changed = transaction
        .execute(
            "UPDATE memory_records SET state=?3, version=version+1, updated_at=?4 \
             WHERE workspace_scope=?1 AND id=?2 AND state<>?3",
            params![workspace_scope, record_id, target_state, now],
        )
        .map_err(|error| format!("无法更新记忆状态：{error}"))?;
    if changed == 0 {
        let existing = read_memory_record(transaction, workspace_scope, record_id)?;
        if existing.state != target_state {
            return Err("记忆状态不能按请求更新".to_string());
        }
        return Ok(existing);
    }
    let record = read_memory_record(transaction, workspace_scope, record_id)?;
    refresh_memory_fts(transaction, workspace_scope, &record)?;
    let revision_hash = format!(
        "{:x}",
        Sha256::digest(format!("{}:{}:{reason}", record.id, record.version).as_bytes())
    );
    insert_memory_revision(transaction, workspace_scope, &record, &revision_hash)?;
    Ok(record)
}

fn memory_is_cjk(character: char) -> bool {
    matches!(
        character,
        '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{20000}'..='\u{2FA1F}'
    )
}

fn memory_cjk_terms(value: &str) -> String {
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
        if memory_is_cjk(character) {
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

fn memory_match_query(query: &str) -> Result<String, String> {
    if query.chars().count() > MAX_MEMORY_QUERY_CHARS {
        return Err("记忆搜索词超过 512 个字符的安全上限".to_string());
    }
    let normalized = query.trim().nfc().collect::<String>();
    if normalized.is_empty() {
        return Err("记忆搜索词不能为空".to_string());
    }
    let mut groups = Vec::new();
    for raw_term in normalized.split_whitespace() {
        let cjk = raw_term
            .chars()
            .filter(|character| memory_is_cjk(*character))
            .collect::<Vec<_>>();
        if cjk.len() >= 2 {
            groups.push(
                cjk.windows(2)
                    .map(|pair| format!("\"{}\"", pair.iter().collect::<String>()))
                    .collect::<Vec<_>>()
                    .join(" AND "),
            );
        } else {
            groups.push(format!("\"{}\"", raw_term.replace('"', "\"\"")));
        }
    }
    Ok(groups.join(" AND "))
}

fn list_memory_records_with_connection(
    connection: &rusqlite::Connection,
    workspace_scope: &str,
    request: &MemoryListRequest,
) -> Result<Vec<MemoryRecord>, String> {
    let scope = validate_scope(&request.scope)?;
    let tracks = request
        .tracks
        .iter()
        .map(|track| validate_track(track))
        .collect::<Result<HashSet<_>, _>>()?;
    let limit = request.limit.unwrap_or(200).clamp(1, 5_000);
    let fetch_limit = if tracks.is_empty() {
        limit
    } else {
        limit.saturating_mul(4).min(20_000)
    };
    let mut statement = connection
        .prepare(&format!(
            "SELECT {MEMORY_RECORD_COLUMNS} FROM memory_records \
             WHERE workspace_scope=?1 \
               AND user_id=?2 AND agent_id=?3 AND app_id=?4 \
               AND (?5=1 OR (project_id=?6 AND session_id=?7)) \
               AND (?8=1 OR (state='active' AND (expires_at IS NULL OR expires_at>?9))) \
             ORDER BY updated_at DESC, id ASC LIMIT ?10"
        ))
        .map_err(|error| format!("无法准备结构化记忆列表：{error}"))?;
    let rows = statement
        .query_map(
            params![
                workspace_scope,
                scope.user_id,
                scope.agent_id,
                scope.app_id,
                i64::from(request.include_all_contexts),
                scope.project_id,
                scope.session_id,
                i64::from(request.include_inactive),
                Utc::now().to_rfc3339(),
                fetch_limit as i64,
            ],
            map_memory_record,
        )
        .map_err(|error| format!("无法读取结构化记忆列表：{error}"))?;
    Ok(rows
        .filter_map(Result::ok)
        .filter(|record| tracks.is_empty() || tracks.contains(&record.track))
        .take(limit)
        .collect())
}

fn assistant_memory_match_query(query: &str) -> Result<String, String> {
    let normalized = query
        .trim()
        .nfc()
        .take(MAX_MEMORY_QUERY_CHARS)
        .collect::<String>();
    if normalized.is_empty() {
        return Err("记忆搜索词不能为空".to_string());
    }
    let mut terms = Vec::new();
    let mut cjk_run = Vec::new();
    let flush_cjk = |run: &mut Vec<char>, terms: &mut Vec<String>| {
        if run.len() == 1 {
            terms.push(run[0].to_string());
        } else {
            terms.extend(run.windows(2).map(|pair| pair.iter().collect::<String>()));
        }
        run.clear();
    };
    let mut ascii = String::new();
    let flush_ascii = |word: &mut String, terms: &mut Vec<String>| {
        let normalized = word.trim().to_lowercase();
        if normalized.chars().count() >= 2 {
            terms.push(normalized);
        }
        word.clear();
    };
    for character in normalized.chars() {
        if memory_is_cjk(character) {
            flush_ascii(&mut ascii, &mut terms);
            cjk_run.push(character);
        } else {
            flush_cjk(&mut cjk_run, &mut terms);
            if character.is_alphanumeric() || matches!(character, '-' | '_') {
                ascii.push(character);
            } else {
                flush_ascii(&mut ascii, &mut terms);
            }
        }
    }
    flush_cjk(&mut cjk_run, &mut terms);
    flush_ascii(&mut ascii, &mut terms);
    terms.sort();
    terms.dedup();
    terms.truncate(48);
    if terms.is_empty() {
        return memory_match_query(&normalized);
    }
    Ok(terms
        .into_iter()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" OR "))
}

fn search_memory_with_connection(
    connection: &rusqlite::Connection,
    workspace_scope: &str,
    request: &MemorySearchRequest,
    match_query: String,
) -> Result<Vec<MemorySearchResult>, String> {
    let scope = validate_scope(&request.scope)?;
    let tracks = request
        .tracks
        .iter()
        .map(|track| validate_track(track))
        .collect::<Result<HashSet<_>, _>>()?;
    let normalized_query = request
        .query
        .trim()
        .nfc()
        .collect::<String>()
        .to_lowercase();
    let limit = request.limit.unwrap_or(24).clamp(1, 100);
    let mut statement = connection
        .prepare(&format!(
            "SELECT {MEMORY_RECORD_JOIN_COLUMNS}, bm25(memory_fts) \
             FROM memory_fts f JOIN memory_records r \
               ON r.workspace_scope=f.workspace_scope AND r.id=f.memory_id \
             WHERE memory_fts MATCH ?1 AND r.workspace_scope=?2 AND r.state='active' \
               AND r.user_id=?3 AND r.agent_id=?4 AND r.app_id=?5 \
               AND r.project_id=?6 AND r.session_id=?7 \
               AND (r.expires_at IS NULL OR r.expires_at>?8) \
             ORDER BY bm25(memory_fts) LIMIT ?9"
        ))
        .map_err(|error| format!("无法准备记忆检索：{error}"))?;
    let rows = statement
        .query_map(
            params![
                match_query,
                workspace_scope,
                scope.user_id,
                scope.agent_id,
                scope.app_id,
                scope.project_id,
                scope.session_id,
                Utc::now().to_rfc3339(),
                (limit * 5) as i64,
            ],
            |row| {
                let record = map_memory_record(row)?;
                let lexical_score = -row.get::<_, f64>(20)?;
                Ok((record, lexical_score))
            },
        )
        .map_err(|error| format!("记忆检索失败：{error}"))?;
    let mut results = rows
        .filter_map(Result::ok)
        .filter(|(record, _)| tracks.is_empty() || tracks.contains(&record.track))
        .map(|(record, lexical_score)| {
            let title = record.title.to_lowercase();
            let content = record.content.to_lowercase();
            let exact_bonus = if title == normalized_query {
                8.0
            } else if title.contains(&normalized_query) {
                5.0
            } else if content.contains(&normalized_query) {
                2.0
            } else {
                0.0
            };
            MemorySearchResult {
                score: lexical_score + exact_bonus + record.confidence,
                record,
            }
        })
        .collect::<Vec<_>>();
    results.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right.record.updated_at.cmp(&left.record.updated_at))
    });
    results.truncate(limit);
    Ok(results)
}

pub(crate) fn assistant_memory_context_in_connection(
    connection: &rusqlite::Connection,
    workspace_scope: &str,
    query: &str,
    scope: &MemoryScope,
) -> Result<Option<String>, String> {
    let query = query
        .trim()
        .chars()
        .take(MAX_MEMORY_QUERY_CHARS)
        .collect::<String>();
    if query.is_empty() {
        return Ok(None);
    }
    let request = MemorySearchRequest {
        query: query.clone(),
        tracks: Vec::new(),
        scope: scope.clone(),
        limit: Some(10),
    };
    let results = search_memory_with_connection(
        connection,
        workspace_scope,
        &request,
        assistant_memory_match_query(&query)?,
    )?;
    if results.is_empty() {
        return Ok(None);
    }
    let mut user_memories = Vec::new();
    let mut agent_memories = Vec::new();
    for result in results {
        let content = result
            .record
            .content
            .trim()
            .chars()
            .take(1_200)
            .collect::<String>();
        let line = render_prompt_template(
            ASSISTANT_MEMORY_ITEM_PROMPT_TEMPLATE,
            &[
                ("track", &result.record.track),
                ("title", &result.record.title),
                ("content", &content),
            ],
        )?;
        match result.record.track.as_str() {
            "user_episode" | "user_profile" => user_memories.push(line),
            "agent_case" | "agent_skill" => agent_memories.push(line),
            _ => {}
        }
    }
    let mut sections = vec![prompt_text(ASSISTANT_MEMORY_CONTEXT_HEADER_PROMPT).to_string()];
    if !user_memories.is_empty() {
        sections.push(render_prompt_template(
            ASSISTANT_USER_MEMORY_SECTION_PROMPT_TEMPLATE,
            &[("memories", &user_memories.join("\n"))],
        )?);
    }
    if !agent_memories.is_empty() {
        sections.push(render_prompt_template(
            ASSISTANT_AGENT_MEMORY_SECTION_PROMPT_TEMPLATE,
            &[("memories", &agent_memories.join("\n"))],
        )?);
    }
    Ok(Some(sections.join("\n\n")))
}

fn reflection_snapshot_value(
    scope_json: &str,
    task_id: Option<&str>,
    source_doc_ids_json: &str,
    source_content_hash: &str,
    metrics_json: &str,
    source_effect_ids: &[String],
    source_material: Option<&Value>,
) -> Result<Value, String> {
    let scope: Value = serde_json::from_str(scope_json)
        .map_err(|error| format!("无法解析反思快照作用域：{error}"))?;
    let source_doc_ids: Value = serde_json::from_str(source_doc_ids_json)
        .map_err(|error| format!("无法解析反思快照来源：{error}"))?;
    let metrics: Value = serde_json::from_str(metrics_json)
        .map_err(|error| format!("无法解析反思快照指标：{error}"))?;
    let material = source_material.cloned().unwrap_or(Value::Null);
    let material_json = serde_json::to_string(&material)
        .map_err(|error| format!("无法序列化反思实际来源：{error}"))?;
    if material_json.len() > 512 * 1024 {
        return Err("反思实际来源超过 512 KB 安全上限".to_string());
    }
    if looks_sensitive(&material_json) {
        return Err("反思实际来源疑似包含凭据，已拒绝保存".to_string());
    }
    Ok(serde_json::json!({
        "version": 1,
        "replayable": source_material.is_some(),
        "scope": scope,
        "taskId": task_id,
        "sourceDocIds": source_doc_ids,
        "sourceContentHash": source_content_hash,
        "metrics": metrics,
        "sourceEffectIds": source_effect_ids,
        "material": material,
    }))
}

fn reflection_snapshot_hash(snapshot: &Value) -> Result<String, String> {
    let bytes =
        serde_json::to_vec(snapshot).map_err(|error| format!("无法序列化反思来源快照：{error}"))?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

/// Creates the durable reflection runtime state used by claim/lease and backfills
/// snapshots for jobs created before the runtime table was introduced.
pub(crate) fn migrate_reflection_schema(connection: &Connection) -> Result<(), String> {
    let base_table_exists = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='memory_reflection_jobs'",
            [],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| format!("无法检查反思基础表：{error}"))?
        .is_some();
    // Some persisted records predate the memory migration and are intentionally
    // allowed to finish the remaining schema migrations.
    if !base_table_exists {
        return Ok(());
    }
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS memory_reflection_job_runtime (
               workspace_scope TEXT NOT NULL,
               job_id TEXT NOT NULL,
               source_snapshot_json TEXT NOT NULL,
               source_snapshot_hash TEXT NOT NULL,
               claimed_by TEXT,
               claim_token TEXT,
               claimed_at_ms INTEGER,
               lease_expires_at_ms INTEGER,
               PRIMARY KEY(workspace_scope, job_id),
               FOREIGN KEY(workspace_scope, job_id)
                 REFERENCES memory_reflection_jobs(workspace_scope, id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_memory_reflection_runtime_claim
               ON memory_reflection_job_runtime(workspace_scope, lease_expires_at_ms, job_id);
             CREATE TRIGGER IF NOT EXISTS memory_reflection_snapshot_immutable_update
               BEFORE UPDATE OF source_snapshot_json, source_snapshot_hash
               ON memory_reflection_job_runtime
               WHEN NEW.source_snapshot_json <> OLD.source_snapshot_json
                 OR NEW.source_snapshot_hash <> OLD.source_snapshot_hash
               BEGIN SELECT RAISE(ABORT, 'reflection source snapshots are immutable'); END;",
        )
        .map_err(|error| format!("无法创建反思运行时表：{error}"))?;

    let mut statement = connection
        .prepare(
            "SELECT j.workspace_scope, j.id, j.task_id, j.scope_json,
                    j.source_doc_ids_json, j.source_content_hash, j.metrics_json
             FROM memory_reflection_jobs j
             LEFT JOIN memory_reflection_job_runtime r
               ON r.workspace_scope=j.workspace_scope AND r.job_id=j.id
             WHERE r.job_id IS NULL",
        )
        .map_err(|error| format!("无法读取待回填反思快照：{error}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })
        .map_err(|error| format!("无法枚举待回填反思快照：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法读取待回填反思快照：{error}"))?;
    drop(statement);
    for (
        workspace_scope,
        job_id,
        task_id,
        scope_json,
        source_doc_ids_json,
        source_content_hash,
        metrics_json,
    ) in rows
    {
        let snapshot = reflection_snapshot_value(
            &scope_json,
            task_id.as_deref(),
            &source_doc_ids_json,
            &source_content_hash,
            &metrics_json,
            &[],
            None,
        )?;
        let snapshot_json = serde_json::to_string(&snapshot)
            .map_err(|error| format!("无法序列化待回填反思快照：{error}"))?;
        let snapshot_hash = reflection_snapshot_hash(&snapshot)?;
        connection
            .execute(
                "INSERT OR IGNORE INTO memory_reflection_job_runtime
                 (workspace_scope, job_id, source_snapshot_json, source_snapshot_hash)
                 VALUES (?1, ?2, ?3, ?4)",
                params![workspace_scope, job_id, snapshot_json, snapshot_hash],
            )
            .map_err(|error| format!("无法回填反思来源快照：{error}"))?;
    }
    Ok(())
}

pub(crate) fn migrate_reflection_optimization_schema(
    connection: &Connection,
) -> Result<(), String> {
    for table in [
        "memory_reflection_jobs",
        "memory_records",
        "optimization_candidates",
    ] {
        let exists = connection
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |_| Ok(()),
            )
            .optional()
            .map_err(|error| format!("无法检查反思优化关联依赖表 {table}：{error}"))?
            .is_some();
        if !exists {
            return Ok(());
        }
    }
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS memory_reflection_optimization_candidates (
               workspace_scope TEXT NOT NULL,
               reflection_job_id TEXT NOT NULL,
               candidate_id TEXT NOT NULL,
               proposal_memory_id TEXT NOT NULL,
               state TEXT NOT NULL CHECK(state IN ('bound', 'applied', 'superseded')),
               bound_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               PRIMARY KEY(workspace_scope, reflection_job_id, candidate_id),
               UNIQUE(workspace_scope, candidate_id),
               FOREIGN KEY(workspace_scope, reflection_job_id)
                 REFERENCES memory_reflection_jobs(workspace_scope, id) ON DELETE RESTRICT,
               FOREIGN KEY(workspace_scope, proposal_memory_id)
                 REFERENCES memory_records(workspace_scope, id) ON DELETE RESTRICT,
               FOREIGN KEY(workspace_scope, candidate_id)
                 REFERENCES optimization_candidates(workspace_scope, id) ON DELETE RESTRICT
             );
             CREATE UNIQUE INDEX IF NOT EXISTS idx_memory_reflection_optimization_active
               ON memory_reflection_optimization_candidates(workspace_scope, reflection_job_id)
               WHERE state='bound';
             CREATE INDEX IF NOT EXISTS idx_memory_reflection_optimization_candidate
               ON memory_reflection_optimization_candidates(workspace_scope, candidate_id, state);",
        )
        .map_err(|error| format!("无法创建反思优化候选关联表：{error}"))
}

fn map_reflection_job(row: &Row<'_>) -> rusqlite::Result<ReflectionJob> {
    let scope_json = row.get::<_, String>(3)?;
    let source_doc_ids_json = row.get::<_, String>(4)?;
    let metrics_json = row.get::<_, String>(6)?;
    let fallback_snapshot = reflection_snapshot_value(
        &scope_json,
        row.get::<_, Option<String>>(2)?.as_deref(),
        &source_doc_ids_json,
        &row.get::<_, String>(5)?,
        &metrics_json,
        &[],
        None,
    )
    .map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            3,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, error)),
        )
    })?;
    let source_snapshot_json = row.get::<_, Option<String>>(13)?;
    let source_snapshot = source_snapshot_json
        .as_deref()
        .map(|value| parse_json(value, "无法解析反思来源快照"))
        .transpose()?
        .unwrap_or(fallback_snapshot);
    let source_snapshot_hash = row
        .get::<_, Option<String>>(14)?
        .unwrap_or_else(|| reflection_snapshot_hash(&source_snapshot).unwrap_or_default());
    Ok(ReflectionJob {
        id: row.get(0)?,
        idempotency_key: row.get(1)?,
        task_id: row.get(2)?,
        scope: parse_json(&scope_json, "无法解析反思作用域")?,
        source_doc_ids: parse_json(&source_doc_ids_json, "无法解析反思来源")?,
        source_content_hash: row.get(5)?,
        source_snapshot,
        source_snapshot_hash,
        metrics: parse_json(&metrics_json, "无法解析反思指标")?,
        state: row.get(7)?,
        proposal_memory_id: row.get(8)?,
        optimization_candidate_id: row.get(17)?,
        attempt_count: row.get(9)?,
        last_error: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
        claimed_by: row.get(15)?,
        lease_expires_at_ms: row.get(16)?,
    })
}

const REFLECTION_JOB_COLUMNS: &str =
    "j.id, j.idempotency_key, j.task_id, j.scope_json, j.source_doc_ids_json, j.source_content_hash, \
     j.metrics_json, j.state, j.proposal_memory_id, j.attempt_count, j.last_error, j.created_at, j.updated_at, \
     r.source_snapshot_json, r.source_snapshot_hash, r.claimed_by, r.lease_expires_at_ms, \
     (SELECT b.candidate_id FROM memory_reflection_optimization_candidates b \
      WHERE b.workspace_scope=j.workspace_scope AND b.reflection_job_id=j.id \
        AND b.state IN ('bound', 'applied') \
      ORDER BY CASE b.state WHEN 'bound' THEN 0 ELSE 1 END, b.updated_at DESC LIMIT 1)";

fn read_reflection_job(
    connection: &rusqlite::Connection,
    workspace_scope: &str,
    job_id: &str,
) -> Result<ReflectionJob, String> {
    let job = connection
        .query_row(
            &format!(
                "SELECT {REFLECTION_JOB_COLUMNS} FROM memory_reflection_jobs j
                 LEFT JOIN memory_reflection_job_runtime r
                   ON r.workspace_scope=j.workspace_scope AND r.job_id=j.id
                 WHERE j.workspace_scope=?1 AND j.id=?2"
            ),
            params![workspace_scope, job_id],
            map_reflection_job,
        )
        .optional()
        .map_err(|error| format!("无法读取反思任务：{error}"))?
        .ok_or_else(|| "反思任务不存在".to_string())?;
    let computed_hash = reflection_snapshot_hash(&job.source_snapshot)?;
    if computed_hash != job.source_snapshot_hash {
        return Err("反思来源快照哈希校验失败，已拒绝继续".to_string());
    }
    Ok(job)
}

fn normalized_reflection_claim_token(value: &str) -> Result<String, String> {
    normalized_required(value, "反思领取令牌", 160)
}

fn reflection_lease_seconds(value: Option<i64>) -> Result<i64, String> {
    let seconds = value.unwrap_or(90);
    if !(5..=15 * 60).contains(&seconds) {
        return Err("反思 lease 必须在 5 到 900 秒之间".to_string());
    }
    Ok(seconds)
}

fn reflection_lease_expiry_ms(lease_seconds: i64) -> i64 {
    (Utc::now() + Duration::seconds(lease_seconds)).timestamp_millis()
}

fn require_live_reflection_claim(
    connection: &Connection,
    workspace_scope: &str,
    job_id: &str,
    claim_token: &str,
    now_ms: i64,
) -> Result<(), String> {
    let valid = connection
        .query_row(
            "SELECT 1
             FROM memory_reflection_jobs j
             JOIN memory_reflection_job_runtime r
               ON r.workspace_scope=j.workspace_scope AND r.job_id=j.id
             WHERE j.workspace_scope=?1 AND j.id=?2 AND j.state='running'
               AND r.claim_token=?3 AND r.lease_expires_at_ms>?4",
            params![workspace_scope, job_id, claim_token, now_ms],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| format!("无法验证反思领取令牌：{error}"))?
        .is_some();
    if valid {
        Ok(())
    } else {
        Err("反思领取令牌无效、已过期或任务已不再运行".to_string())
    }
}

fn clear_reflection_lease(
    connection: &Connection,
    workspace_scope: &str,
    job_id: &str,
) -> Result<(), String> {
    connection
        .execute(
            "UPDATE memory_reflection_job_runtime
             SET claimed_by=NULL, claim_token=NULL, claimed_at_ms=NULL, lease_expires_at_ms=NULL
             WHERE workspace_scope=?1 AND job_id=?2",
            params![workspace_scope, job_id],
        )
        .map_err(|error| format!("无法清理反思领取 lease：{error}"))?;
    Ok(())
}

fn normalized_optimization_candidate_id(value: &str) -> Result<String, String> {
    normalized_required(value, "优化候选 ID", 160)
}

fn reflection_source_effect_ids(job: &ReflectionJob) -> Result<Vec<String>, String> {
    let effect_ids = job
        .source_snapshot
        .get("sourceEffectIds")
        .and_then(Value::as_array)
        .ok_or_else(|| "反思来源快照缺少 sourceEffectIds".to_string())?;
    let mut unique = HashSet::new();
    effect_ids
        .iter()
        .map(|value| {
            let value = value
                .as_str()
                .ok_or_else(|| "反思来源效果 ID 无效".to_string())?;
            let effect_id = normalized_required(value, "反思效果 ID", 160)?;
            if !unique.insert(effect_id.clone()) {
                return Err("反思来源效果 ID 不能重复".to_string());
            }
            Ok(effect_id)
        })
        .collect()
}

fn reflection_optimization_binding(
    connection: &Connection,
    workspace_scope: &str,
    reflection_job_id: &str,
    candidate_id: &str,
) -> Result<Option<(String, String)>, String> {
    connection
        .query_row(
            "SELECT proposal_memory_id, state
             FROM memory_reflection_optimization_candidates
             WHERE workspace_scope=?1 AND reflection_job_id=?2 AND candidate_id=?3",
            params![workspace_scope, reflection_job_id, candidate_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|error| format!("无法读取反思优化候选关联：{error}"))
}

fn bind_reflection_optimization_candidate(
    connection: &Connection,
    workspace_scope: &str,
    reflection_job_id: &str,
    candidate_id: &str,
    proposal_memory_id: &str,
) -> Result<(), String> {
    let candidate_state = connection
        .query_row(
            "SELECT state FROM optimization_candidates WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, candidate_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("无法读取待绑定优化候选：{error}"))?
        .ok_or_else(|| "待绑定优化候选不存在".to_string())?;
    if candidate_state != "pending_review" {
        return Err("只有通过独立评估、等待审阅的优化候选可以绑定反思任务".to_string());
    }
    if let Some((existing_proposal_id, state)) = reflection_optimization_binding(
        connection,
        workspace_scope,
        reflection_job_id,
        candidate_id,
    )? {
        if existing_proposal_id == proposal_memory_id && state == "bound" {
            return Ok(());
        }
        return Err("优化候选已经绑定到不同的反思提案或已完成".to_string());
    }
    let active_binding = connection
        .query_row(
            "SELECT candidate_id FROM memory_reflection_optimization_candidates
             WHERE workspace_scope=?1 AND reflection_job_id=?2 AND state='bound'",
            params![workspace_scope, reflection_job_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("无法检查反思任务的候选绑定：{error}"))?;
    if active_binding.is_some() {
        return Err("反思任务已经绑定到其他等待审阅的优化候选".to_string());
    }
    let now = Utc::now().to_rfc3339();
    connection
        .execute(
            "INSERT INTO memory_reflection_optimization_candidates
             (workspace_scope, reflection_job_id, candidate_id, proposal_memory_id,
              state, bound_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'bound', ?5, ?5)",
            params![
                workspace_scope,
                reflection_job_id,
                candidate_id,
                proposal_memory_id,
                now,
            ],
        )
        .map_err(|error| format!("无法绑定反思任务和优化候选：{error}"))?;
    Ok(())
}

fn supersede_reflection_optimization_binding(
    connection: &Connection,
    workspace_scope: &str,
    reflection_job_id: &str,
) -> Result<Vec<String>, String> {
    let bindings = {
        let mut statement = connection
            .prepare(
                "SELECT candidate_id FROM memory_reflection_optimization_candidates
                 WHERE workspace_scope=?1 AND reflection_job_id=?2 AND state='bound'",
            )
            .map_err(|error| format!("无法读取待撤销优化候选绑定：{error}"))?;
        let rows = statement
            .query_map(params![workspace_scope, reflection_job_id], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| format!("无法查询待撤销优化候选绑定：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法读取待撤销优化候选绑定：{error}"))?;
        rows
    };
    if bindings.is_empty() {
        return Ok(bindings);
    }
    let now = Utc::now().to_rfc3339();
    connection
        .execute(
            "UPDATE memory_reflection_optimization_candidates
             SET state='superseded', updated_at=?3
             WHERE workspace_scope=?1 AND reflection_job_id=?2 AND state='bound'",
            params![workspace_scope, reflection_job_id, now],
        )
        .map_err(|error| format!("无法撤销反思优化候选绑定：{error}"))?;
    for candidate_id in &bindings {
        connection
            .execute(
                "UPDATE optimization_candidates SET state='superseded'
                 WHERE workspace_scope=?1 AND id=?2 AND state='pending_review'",
                params![workspace_scope, candidate_id],
            )
            .map_err(|error| format!("无法撤销绑定的优化候选：{error}"))?;
    }
    Ok(bindings)
}

impl RuntimeDatabase {
    fn upsert_memory(&self, input: &MemoryRecordInput) -> Result<MemoryRecord, String> {
        let workspace_scope = self.local_workspace_scope()?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始记忆写入事务：{error}"))?;
        let record = upsert_memory_in_transaction(&transaction, &workspace_scope, input)?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交记忆写入事务：{error}"))?;
        Ok(record)
    }

    fn tombstone_memory(&self, record_id: &str, reason: &str) -> Result<MemoryRecord, String> {
        let workspace_scope = self.local_workspace_scope()?;
        let record_id = normalized_required(record_id, "记忆 ID", 160)?;
        let reason = normalized_required(reason, "墓碑原因", 1_000)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始记忆墓碑事务：{error}"))?;
        let record = change_memory_state_in_transaction(
            &transaction,
            &workspace_scope,
            &record_id,
            "tombstone",
            &reason,
        )?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交记忆墓碑事务：{error}"))?;
        Ok(record)
    }

    fn search_memory(
        &self,
        request: &MemorySearchRequest,
    ) -> Result<Vec<MemorySearchResult>, String> {
        let workspace_scope = self.local_workspace_scope()?;
        let match_query = memory_match_query(&request.query)?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        search_memory_with_connection(&connection, &workspace_scope, request, match_query)
    }

    fn list_memories(&self, request: &MemoryListRequest) -> Result<Vec<MemoryRecord>, String> {
        let workspace_scope = self.local_workspace_scope()?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        list_memory_records_with_connection(&connection, &workspace_scope, request)
    }

    fn begin_reflection(&self, input: &ReflectionJobInput) -> Result<ReflectionJob, String> {
        let workspace_scope = self.local_workspace_scope()?;
        let idempotency_key = normalized_required(&input.idempotency_key, "反思幂等键", 200)?;
        let task_id = normalized_optional(input.task_id.as_deref(), "反思任务 ID", 160)?;
        let scope = validate_scope(&input.scope)?;
        if input.source_doc_ids.is_empty() || input.source_doc_ids.len() > 256 {
            return Err("反思任务必须包含 1 到 256 个来源文档 ID".to_string());
        }
        let source_doc_ids = input
            .source_doc_ids
            .iter()
            .map(|value| normalized_required(value, "反思来源 ID", 200))
            .collect::<Result<Vec<_>, _>>()?;
        let source_content_hash = validate_hash(Some(&input.source_content_hash))?
            .ok_or_else(|| "反思来源哈希不能为空".to_string())?;
        let source_effect_ids = input
            .source_effect_ids
            .iter()
            .map(|value| normalized_required(value, "反思效果 ID", 160))
            .collect::<Result<Vec<_>, _>>()?;
        let mut unique_effect_ids = HashSet::new();
        if source_effect_ids
            .iter()
            .any(|effect_id| !unique_effect_ids.insert(effect_id.clone()))
        {
            return Err("反思来源效果 ID 不能重复".to_string());
        }
        let scope_json = serde_json::to_string(&scope)
            .map_err(|error| format!("无法序列化反思作用域：{error}"))?;
        let sources_json = serde_json::to_string(&source_doc_ids)
            .map_err(|error| format!("无法序列化反思来源：{error}"))?;
        let metrics_json = serde_json::to_string(&input.metrics)
            .map_err(|error| format!("无法序列化反思指标：{error}"))?;
        if metrics_json.len() > 128 * 1024 {
            return Err("反思指标超过 128 KB 安全上限".to_string());
        }
        if input
            .source_snapshot
            .as_ref()
            .is_some_and(|snapshot| !snapshot.is_object())
        {
            return Err("反思 sourceSnapshot 必须是 JSON 对象".to_string());
        }
        let mut source_snapshot = reflection_snapshot_value(
            &scope_json,
            task_id.as_deref(),
            &sources_json,
            &source_content_hash,
            &metrics_json,
            &source_effect_ids,
            input.source_snapshot.as_ref(),
        )?;
        let mut source_snapshot_json = serde_json::to_string(&source_snapshot)
            .map_err(|error| format!("无法序列化反思来源快照：{error}"))?;
        if source_snapshot_json.len() > 512 * 1024 {
            return Err("反思来源快照超过 512 KB 安全上限".to_string());
        }
        let mut source_snapshot_hash = reflection_snapshot_hash(&source_snapshot)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始反思任务事务：{error}"))?;
        if !source_effect_ids.is_empty() {
            let effect_evidence =
                crate::skill_lifecycle::reflection_effect_snapshots_in_connection(
                    &transaction,
                    &workspace_scope,
                    &source_effect_ids,
                )?;
            if let Some(object) = source_snapshot.as_object_mut() {
                object.insert("effectEvidence".to_string(), Value::Array(effect_evidence));
            }
            source_snapshot_json = serde_json::to_string(&source_snapshot)
                .map_err(|error| format!("无法序列化反思 Skill 效果证据：{error}"))?;
            if source_snapshot_json.len() > 512 * 1024 {
                return Err("反思来源快照超过 512 KB 安全上限".to_string());
            }
            source_snapshot_hash = reflection_snapshot_hash(&source_snapshot)?;
        }
        if let Some(expected_hash) = validate_hash(input.source_snapshot_hash.as_deref())? {
            if expected_hash != source_snapshot_hash {
                return Err("反思 sourceSnapshotHash 与规范化快照不匹配".to_string());
            }
        }
        let existing_id = transaction
            .query_row(
                "SELECT id FROM memory_reflection_jobs \
                 WHERE workspace_scope=?1 AND idempotency_key=?2",
                params![workspace_scope, idempotency_key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("无法检查反思幂等键：{error}"))?;
        if let Some(existing_id) = existing_id.as_deref() {
            let existing = read_reflection_job(&transaction, &workspace_scope, existing_id)?;
            if existing.task_id != task_id
                || existing.scope != scope
                || existing.source_doc_ids != source_doc_ids
                || existing.source_content_hash != source_content_hash
                || existing.metrics != input.metrics
                || existing.source_snapshot_hash != source_snapshot_hash
            {
                return Err("反思幂等键已经绑定到不同的任务、作用域或来源证据".to_string());
            }
        }
        let job_id = existing_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let now = Utc::now().to_rfc3339();
        transaction
            .execute(
                "INSERT INTO memory_reflection_jobs \
                 (workspace_scope, id, idempotency_key, task_id, scope_json, source_doc_ids_json, \
                  source_content_hash, metrics_json, state, proposal_memory_id, attempt_count, \
                  last_error, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'queued', NULL, 0, NULL, ?9, ?9) \
                 ON CONFLICT(workspace_scope, idempotency_key) DO UPDATE SET \
                   state=CASE WHEN memory_reflection_jobs.state='failed' THEN 'queued' \
                              ELSE memory_reflection_jobs.state END, \
                   last_error=CASE WHEN memory_reflection_jobs.state IN ('queued', 'failed') \
                                   THEN NULL ELSE memory_reflection_jobs.last_error END, \
                   updated_at=excluded.updated_at",
                params![
                    workspace_scope,
                    job_id,
                    idempotency_key,
                    task_id,
                    scope_json,
                    sources_json,
                    source_content_hash,
                    metrics_json,
                    now,
                ],
            )
            .map_err(|error| format!("无法保存反思任务：{error}"))?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO memory_reflection_job_runtime
                 (workspace_scope, job_id, source_snapshot_json, source_snapshot_hash)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    workspace_scope,
                    job_id,
                    source_snapshot_json,
                    source_snapshot_hash
                ],
            )
            .map_err(|error| format!("无法保存反思运行时状态：{error}"))?;
        let job = read_reflection_job(&transaction, &workspace_scope, &job_id)?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交反思任务：{error}"))?;
        Ok(job)
    }

    fn get_reflection(&self, job_id: &str) -> Result<ReflectionJob, String> {
        let workspace_scope = self.local_workspace_scope()?;
        let job_id = normalized_required(job_id, "反思任务 ID", 160)?;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        read_reflection_job(&connection, &workspace_scope, &job_id)
    }

    fn list_reflections(
        &self,
        request: &ReflectionJobListRequest,
    ) -> Result<Vec<ReflectionJob>, String> {
        let workspace_scope = self.local_workspace_scope()?;
        let states = request
            .states
            .iter()
            .map(|state| normalized_required(state, "反思任务状态", 32))
            .collect::<Result<Vec<_>, _>>()?;
        if states.iter().any(|state| {
            !matches!(
                state.as_str(),
                "queued" | "running" | "awaiting_review" | "completed" | "failed" | "cancelled"
            )
        }) {
            return Err("反思任务状态无效".to_string());
        }
        let limit = request.limit.unwrap_or(100).clamp(1, 500) as i64;
        let connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let mut sql = format!(
            "SELECT {REFLECTION_JOB_COLUMNS} FROM memory_reflection_jobs j
             LEFT JOIN memory_reflection_job_runtime r
               ON r.workspace_scope=j.workspace_scope AND r.job_id=j.id
             WHERE j.workspace_scope=?1"
        );
        if !states.is_empty() {
            let placeholders = (0..states.len())
                .map(|index| format!("?{}", index + 2))
                .collect::<Vec<_>>()
                .join(", ");
            sql.push_str(&format!(" AND j.state IN ({placeholders})"));
        }
        sql.push_str(" ORDER BY j.updated_at DESC, j.id DESC");
        let mut statement = connection
            .prepare(&sql)
            .map_err(|error| format!("无法准备反思任务列表查询：{error}"))?;
        let mut values = vec![rusqlite::types::Value::Text(workspace_scope)];
        values.extend(states.into_iter().map(rusqlite::types::Value::Text));
        let rows = statement
            .query_map(rusqlite::params_from_iter(values), map_reflection_job)
            .map_err(|error| format!("无法查询反思任务列表：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法读取反思任务列表：{error}"))?;
        Ok(rows.into_iter().take(limit as usize).collect())
    }

    fn claim_reflection(
        &self,
        input: &ReflectionJobClaimInput,
    ) -> Result<Option<ReflectionJobClaim>, String> {
        let workspace_scope = self.local_workspace_scope()?;
        let worker_id = normalized_required(&input.worker_id, "反思 worker ID", 160)?;
        let lease_seconds = reflection_lease_seconds(input.lease_seconds)?;
        let now_ms = Utc::now().timestamp_millis();
        let lease_expires_at_ms = reflection_lease_expiry_ms(lease_seconds);
        let claim_token = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始反思领取事务：{error}"))?;
        let job_id = transaction
            .query_row(
                "SELECT j.id
                 FROM memory_reflection_jobs j
                 JOIN memory_reflection_job_runtime r
                   ON r.workspace_scope=j.workspace_scope AND r.job_id=j.id
                 WHERE j.workspace_scope=?1
                   AND json_extract(r.source_snapshot_json, '$.replayable')=1
                   AND (j.state='queued'
                     OR (j.state='running' AND COALESCE(r.lease_expires_at_ms, 0)<=?2))
                 ORDER BY CASE j.state WHEN 'queued' THEN 0 ELSE 1 END,
                          j.updated_at ASC, j.id ASC
                 LIMIT 1",
                params![workspace_scope, now_ms],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("无法选择待领取反思任务：{error}"))?;
        let Some(job_id) = job_id else {
            transaction
                .commit()
                .map_err(|error| format!("无法提交空反思领取事务：{error}"))?;
            return Ok(None);
        };
        let changed = transaction
            .execute(
                "UPDATE memory_reflection_jobs
                 SET state='running', attempt_count=attempt_count+1, last_error=NULL, updated_at=?3
                 WHERE workspace_scope=?1 AND id=?2
                   AND state IN ('queued', 'running')",
                params![workspace_scope, job_id, now],
            )
            .map_err(|error| format!("无法标记反思任务为运行中：{error}"))?;
        if changed != 1 {
            return Err("反思任务领取冲突，请重试".to_string());
        }
        let changed = transaction
            .execute(
                "UPDATE memory_reflection_job_runtime
                 SET claimed_by=?3, claim_token=?4, claimed_at_ms=?5, lease_expires_at_ms=?6
                 WHERE workspace_scope=?1 AND job_id=?2
                   AND (claim_token IS NULL OR lease_expires_at_ms IS NULL OR lease_expires_at_ms<=?5)",
                params![workspace_scope, job_id, worker_id, claim_token, now_ms, lease_expires_at_ms],
            )
            .map_err(|error| format!("无法保存反思领取 lease：{error}"))?;
        if changed != 1 {
            return Err("反思任务领取 lease 冲突，请重试".to_string());
        }
        let job = read_reflection_job(&transaction, &workspace_scope, &job_id)?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交反思领取事务：{error}"))?;
        Ok(Some(ReflectionJobClaim { job, claim_token }))
    }

    fn renew_reflection_lease(
        &self,
        job_id: &str,
        claim_token: &str,
        lease_seconds: Option<i64>,
    ) -> Result<ReflectionJob, String> {
        let workspace_scope = self.local_workspace_scope()?;
        let job_id = normalized_required(job_id, "反思任务 ID", 160)?;
        let claim_token = normalized_reflection_claim_token(claim_token)?;
        let lease_seconds = reflection_lease_seconds(lease_seconds)?;
        let now_ms = Utc::now().timestamp_millis();
        let lease_expires_at_ms = reflection_lease_expiry_ms(lease_seconds);
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始反思 lease 续期事务：{error}"))?;
        require_live_reflection_claim(
            &transaction,
            &workspace_scope,
            &job_id,
            &claim_token,
            now_ms,
        )?;
        let changed = transaction
            .execute(
                "UPDATE memory_reflection_job_runtime
                 SET lease_expires_at_ms=?4
                 WHERE workspace_scope=?1 AND job_id=?2 AND claim_token=?3
                   AND lease_expires_at_ms>?5",
                params![
                    workspace_scope,
                    job_id,
                    claim_token,
                    lease_expires_at_ms,
                    now_ms
                ],
            )
            .map_err(|error| format!("无法续期反思 lease：{error}"))?;
        if changed != 1 {
            return Err("反思 lease 已失效，无法续期".to_string());
        }
        let job = read_reflection_job(&transaction, &workspace_scope, &job_id)?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交反思 lease 续期：{error}"))?;
        Ok(job)
    }

    fn complete_reflection(
        &self,
        job_id: &str,
        claim_token: &str,
        proposal: &MemoryRecordInput,
        candidate_id: Option<&str>,
    ) -> Result<ReflectionJob, String> {
        let workspace_scope = self.local_workspace_scope()?;
        let job_id = normalized_required(job_id, "反思任务 ID", 160)?;
        let claim_token = normalized_reflection_claim_token(claim_token)?;
        let candidate_id = candidate_id
            .map(normalized_optimization_candidate_id)
            .transpose()?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始反思完成事务：{error}"))?;
        let job = read_reflection_job(&transaction, &workspace_scope, &job_id)?;
        if job.state == "awaiting_review" || job.state == "completed" {
            if let Some(candidate_id) = candidate_id.as_deref() {
                let proposal_memory_id = job
                    .proposal_memory_id
                    .as_deref()
                    .ok_or_else(|| "反思任务缺少建议记忆".to_string())?;
                let binding = reflection_optimization_binding(
                    &transaction,
                    &workspace_scope,
                    &job_id,
                    candidate_id,
                )?
                .ok_or_else(|| "反思任务没有绑定该优化候选".to_string())?;
                if binding.0 != proposal_memory_id {
                    return Err("反思任务和优化候选的提案绑定不一致".to_string());
                }
            }
            return Ok(job);
        }
        if job.state != "running" {
            return Err("只有运行中的反思任务可以提交建议".to_string());
        }
        require_live_reflection_claim(
            &transaction,
            &workspace_scope,
            &job_id,
            &claim_token,
            Utc::now().timestamp_millis(),
        )?;
        let mut proposal = proposal.clone();
        proposal.state = "draft".to_string();
        proposal.scope = job.scope.clone();
        proposal.source_doc_id = job.id.clone();
        proposal.source_content_hash = Some(job.source_content_hash.clone());
        let record = upsert_memory_in_transaction(&transaction, &workspace_scope, &proposal)?;
        if let Some(candidate_id) = candidate_id.as_deref() {
            bind_reflection_optimization_candidate(
                &transaction,
                &workspace_scope,
                &job_id,
                candidate_id,
                &record.id,
            )?;
        }
        let now = Utc::now().to_rfc3339();
        transaction
            .execute(
                "UPDATE memory_reflection_jobs \
                 SET state='awaiting_review', proposal_memory_id=?3, last_error=NULL, updated_at=?4 \
                 WHERE workspace_scope=?1 AND id=?2 AND state='running'",
                params![workspace_scope, job_id, record.id, now],
            )
            .map_err(|error| format!("无法完成反思任务：{error}"))?;
        clear_reflection_lease(&transaction, &workspace_scope, &job_id)?;
        let job = read_reflection_job(&transaction, &workspace_scope, &job_id)?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交反思建议：{error}"))?;
        Ok(job)
    }

    fn review_reflection(&self, job_id: &str, decision: &str) -> Result<ReflectionJob, String> {
        let workspace_scope = self.local_workspace_scope()?;
        let job_id = normalized_required(job_id, "反思任务 ID", 160)?;
        if !matches!(decision, "approve" | "reject" | "revise") {
            return Err("反思审阅决策必须是 approve、reject 或 revise".to_string());
        }
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始反思审阅事务：{error}"))?;
        let job = read_reflection_job(&transaction, &workspace_scope, &job_id)?;
        if job.state == "completed" {
            return Ok(job);
        }
        if job.state != "awaiting_review" {
            return Err("反思任务当前不处于等待审阅状态".to_string());
        }
        let has_bound_candidate = transaction
            .query_row(
                "SELECT 1 FROM memory_reflection_optimization_candidates
                 WHERE workspace_scope=?1 AND reflection_job_id=?2 AND state='bound'",
                params![workspace_scope, job_id],
                |_| Ok(()),
            )
            .optional()
            .map_err(|error| format!("无法检查反思优化候选绑定：{error}"))?
            .is_some();
        if decision == "approve" && has_bound_candidate {
            return Err("已绑定优化候选的反思任务必须使用原子审批命令".to_string());
        }
        let proposal_id = job
            .proposal_memory_id
            .as_deref()
            .ok_or_else(|| "反思任务缺少建议记忆".to_string())?;
        let (memory_state, job_state) = match decision {
            "approve" => ("active", "completed"),
            "reject" => ("tombstone", "completed"),
            "revise" => ("tombstone", "queued"),
            _ => unreachable!(),
        };
        change_memory_state_in_transaction(
            &transaction,
            &workspace_scope,
            proposal_id,
            memory_state,
            decision,
        )?;
        let mut correction_reference_id = proposal_id.to_string();
        if matches!(decision, "reject" | "revise") && has_bound_candidate {
            let superseded =
                supersede_reflection_optimization_binding(&transaction, &workspace_scope, &job_id)?;
            if let Some(candidate_id) = superseded.first() {
                correction_reference_id.clone_from(candidate_id);
            }
        }
        if matches!(decision, "reject" | "revise") {
            let note = if decision == "reject" {
                "用户拒绝反思建议"
            } else {
                "用户要求重做反思建议"
            };
            for effect_id in reflection_source_effect_ids(&job)? {
                crate::skill_lifecycle::record_skill_execution_feedback_link_in_connection(
                    &transaction,
                    &workspace_scope,
                    &effect_id,
                    "correction",
                    &correction_reference_id,
                    note,
                )?;
            }
        }
        let now = Utc::now().to_rfc3339();
        transaction
            .execute(
                "UPDATE memory_reflection_jobs \
                 SET state=?3, proposal_memory_id=CASE WHEN ?3='queued' THEN NULL ELSE proposal_memory_id END, \
                     updated_at=?4 WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, job_id, job_state, now],
            )
            .map_err(|error| format!("无法保存反思审阅结果：{error}"))?;
        if job_state == "queued" {
            clear_reflection_lease(&transaction, &workspace_scope, &job_id)?;
        }
        let job = read_reflection_job(&transaction, &workspace_scope, &job_id)?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交反思审阅事务：{error}"))?;
        Ok(job)
    }

    fn approve_reflection_optimization_candidate(
        &self,
        reflection_job_id: &str,
        candidate_id: &str,
        mutation_key: Option<&RuntimeEffectMutationKey>,
    ) -> Result<OptimizationProfileResult, String> {
        let workspace_scope = self.local_workspace_scope()?;
        let reflection_job_id = normalized_required(reflection_job_id, "反思任务 ID", 160)?;
        let candidate_id = normalized_optimization_candidate_id(candidate_id)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始反思优化原子审批事务：{error}"))?;
        if let Some(key) = mutation_key {
            if let Some(result) =
                read_runtime_effect_mutation_result(&transaction, &workspace_scope, key)?
            {
                transaction
                    .commit()
                    .map_err(|error| format!("无法完成反思优化审批幂等重放：{error}"))?;
                return Ok(result);
            }
        }
        let job = read_reflection_job(&transaction, &workspace_scope, &reflection_job_id)?;
        let proposal_memory_id = job
            .proposal_memory_id
            .as_deref()
            .ok_or_else(|| "反思任务缺少建议记忆".to_string())?;
        let binding = reflection_optimization_binding(
            &transaction,
            &workspace_scope,
            &reflection_job_id,
            &candidate_id,
        )?
        .ok_or_else(|| "反思任务没有绑定该优化候选，已拒绝错配审批".to_string())?;
        if binding.0 != proposal_memory_id {
            return Err("反思任务、优化候选与建议记忆绑定不一致".to_string());
        }
        let proposal = read_memory_record(&transaction, &workspace_scope, proposal_memory_id)?;
        if proposal.track != "agent_skill" {
            return Err("优化候选只能激活经过审阅的 agent_skill 建议记忆".to_string());
        }
        let candidate_state = transaction
            .query_row(
                "SELECT state FROM optimization_candidates WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, candidate_id],
                |row| row.get::<_, String>(0),
            )
            .map_err(|error| format!("无法读取原子审批优化候选：{error}"))?;
        let idempotent = job.state == "completed"
            && binding.1 == "applied"
            && proposal.state == "active"
            && candidate_state == "applied";
        if !idempotent {
            if job.state != "awaiting_review"
                || binding.1 != "bound"
                || proposal.state != "draft"
                || candidate_state != "pending_review"
            {
                return Err("反思任务、候选、建议记忆或绑定状态不允许原子审批".to_string());
            }
            change_memory_state_in_transaction(
                &transaction,
                &workspace_scope,
                proposal_memory_id,
                "active",
                "approve_optimization_candidate",
            )?;
        }

        let profile = crate::runtime_db::apply_evaluated_optimization_candidate_in_connection(
            &transaction,
            &workspace_scope,
            &candidate_id,
        )?;
        if !idempotent {
            let now = Utc::now().to_rfc3339();
            let changed = transaction
                .execute(
                    "UPDATE memory_reflection_jobs
                     SET state='completed', last_error=NULL, updated_at=?3
                     WHERE workspace_scope=?1 AND id=?2 AND state='awaiting_review'",
                    params![workspace_scope, reflection_job_id, now],
                )
                .map_err(|error| format!("无法完成原子审批反思任务：{error}"))?;
            if changed != 1 {
                return Err("反思任务状态已变化，原子审批没有提交".to_string());
            }
            let changed = transaction
                .execute(
                    "UPDATE memory_reflection_optimization_candidates
                     SET state='applied', updated_at=?4
                     WHERE workspace_scope=?1 AND reflection_job_id=?2 AND candidate_id=?3
                       AND state='bound'",
                    params![workspace_scope, reflection_job_id, candidate_id, now],
                )
                .map_err(|error| format!("无法完成反思优化候选关联：{error}"))?;
            if changed != 1 {
                return Err("反思优化候选绑定已变化，原子审批没有提交".to_string());
            }
        }
        for effect_id in reflection_source_effect_ids(&job)? {
            crate::skill_lifecycle::record_skill_execution_feedback_link_in_connection(
                &transaction,
                &workspace_scope,
                &effect_id,
                "acceptance",
                &candidate_id,
                "用户批准反思优化候选",
            )?;
        }
        if let Some(key) = mutation_key {
            persist_runtime_effect_mutation_result(&transaction, &workspace_scope, key, &profile)?;
        }
        transaction
            .commit()
            .map_err(|error| format!("无法提交反思优化原子审批：{error}"))?;
        Ok(profile)
    }

    fn fail_reflection(
        &self,
        job_id: &str,
        claim_token: &str,
        error: &str,
    ) -> Result<ReflectionJob, String> {
        let workspace_scope = self.local_workspace_scope()?;
        let job_id = normalized_required(job_id, "反思任务 ID", 160)?;
        let claim_token = normalized_reflection_claim_token(claim_token)?;
        let error = normalized_required(error, "反思失败原因", 2_000)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|database_error| format!("无法开始反思失败事务：{database_error}"))?;
        let existing = read_reflection_job(&transaction, &workspace_scope, &job_id)?;
        if existing.state == "failed" {
            return Ok(existing);
        }
        if existing.state != "running" {
            return Err("反思任务当前不能标记失败".to_string());
        }
        require_live_reflection_claim(
            &transaction,
            &workspace_scope,
            &job_id,
            &claim_token,
            Utc::now().timestamp_millis(),
        )?;
        let changed = transaction
            .execute(
                "UPDATE memory_reflection_jobs SET state='failed', last_error=?3, updated_at=?4 \
                 WHERE workspace_scope=?1 AND id=?2 AND state='running'",
                params![workspace_scope, job_id, error, Utc::now().to_rfc3339()],
            )
            .map_err(|database_error| format!("无法记录反思任务失败：{database_error}"))?;
        if changed != 1 {
            return Err("反思任务状态已变化，无法标记失败".to_string());
        }
        clear_reflection_lease(&transaction, &workspace_scope, &job_id)?;
        let job = read_reflection_job(&transaction, &workspace_scope, &job_id)?;
        transaction
            .commit()
            .map_err(|database_error| format!("无法提交反思失败事务：{database_error}"))?;
        Ok(job)
    }

    fn cancel_reflection(&self, job_id: &str, reason: &str) -> Result<ReflectionJob, String> {
        let workspace_scope = self.local_workspace_scope()?;
        let job_id = normalized_required(job_id, "反思任务 ID", 160)?;
        let reason = normalized_required(reason, "反思取消原因", 2_000)?;
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| format!("无法开始反思取消事务：{error}"))?;
        let job = read_reflection_job(&transaction, &workspace_scope, &job_id)?;
        if job.state == "cancelled" {
            return Ok(job);
        }
        if job.state == "completed" {
            return Err("已完成的反思任务不能取消".to_string());
        }
        if let Some(proposal_id) = job.proposal_memory_id.as_deref() {
            let proposal = read_memory_record(&transaction, &workspace_scope, proposal_id)?;
            if proposal.state == "draft" {
                change_memory_state_in_transaction(
                    &transaction,
                    &workspace_scope,
                    proposal_id,
                    "tombstone",
                    &format!("reflection_cancelled:{reason}"),
                )?;
            }
        }
        transaction
            .execute(
                "UPDATE memory_reflection_jobs
                 SET state='cancelled', last_error=?3, updated_at=?4
                 WHERE workspace_scope=?1 AND id=?2
                   AND state IN ('queued', 'running', 'awaiting_review', 'failed')",
                params![workspace_scope, job_id, reason, Utc::now().to_rfc3339()],
            )
            .map_err(|error| format!("无法取消反思任务：{error}"))?;
        clear_reflection_lease(&transaction, &workspace_scope, &job_id)?;
        let job = read_reflection_job(&transaction, &workspace_scope, &job_id)?;
        transaction
            .commit()
            .map_err(|error| format!("无法提交反思取消事务：{error}"))?;
        Ok(job)
    }
}

pub(crate) fn recover_reflection_jobs(database: &RuntimeDatabase) -> Result<usize, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| format!("无法开始反思恢复事务：{error}"))?;
    let recovered = transaction
        .execute(
            "UPDATE memory_reflection_jobs SET state='queued', \
             last_error=COALESCE(last_error, '应用退出前反思任务未完成'), updated_at=?2 \
             WHERE workspace_scope=?1 AND state='running'",
            params![workspace_scope, Utc::now().to_rfc3339()],
        )
        .map_err(|error| format!("无法恢复中断的反思任务：{error}"))?;
    transaction
        .execute(
            "UPDATE memory_reflection_job_runtime
             SET claimed_by=NULL, claim_token=NULL, claimed_at_ms=NULL, lease_expires_at_ms=NULL
             WHERE workspace_scope=?1",
            params![workspace_scope],
        )
        .map_err(|error| format!("无法清理反思恢复 lease：{error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交反思恢复事务：{error}"))?;
    Ok(recovered)
}

#[tauri::command]
pub fn upsert_memory_record(
    database: State<'_, RuntimeDatabase>,
    input: MemoryRecordInput,
) -> Result<MemoryRecord, String> {
    database.upsert_memory(&input)
}

#[tauri::command]
pub fn tombstone_memory_record(
    database: State<'_, RuntimeDatabase>,
    record_id: String,
    reason: String,
) -> Result<MemoryRecord, String> {
    database.tombstone_memory(&record_id, &reason)
}

#[tauri::command]
pub fn search_memory_records(
    database: State<'_, RuntimeDatabase>,
    request: MemorySearchRequest,
) -> Result<Vec<MemorySearchResult>, String> {
    database.search_memory(&request)
}

#[tauri::command]
pub fn list_memory_records(
    database: State<'_, RuntimeDatabase>,
    request: MemoryListRequest,
) -> Result<Vec<MemoryRecord>, String> {
    database.list_memories(&request)
}

#[tauri::command]
pub fn begin_memory_reflection(
    database: State<'_, RuntimeDatabase>,
    input: ReflectionJobInput,
) -> Result<ReflectionJob, String> {
    database.begin_reflection(&input)
}

#[tauri::command]
pub fn get_memory_reflection(
    database: State<'_, RuntimeDatabase>,
    job_id: String,
) -> Result<ReflectionJob, String> {
    database.get_reflection(&job_id)
}

#[tauri::command]
pub fn list_memory_reflections(
    database: State<'_, RuntimeDatabase>,
    request: ReflectionJobListRequest,
) -> Result<Vec<ReflectionJob>, String> {
    database.list_reflections(&request)
}

#[tauri::command]
pub fn claim_memory_reflection(
    database: State<'_, RuntimeDatabase>,
    input: ReflectionJobClaimInput,
) -> Result<Option<ReflectionJobClaim>, String> {
    database.claim_reflection(&input)
}

#[tauri::command]
pub fn renew_memory_reflection_lease(
    database: State<'_, RuntimeDatabase>,
    job_id: String,
    claim_token: String,
    lease_seconds: Option<i64>,
) -> Result<ReflectionJob, String> {
    database.renew_reflection_lease(&job_id, &claim_token, lease_seconds)
}

#[tauri::command]
pub fn complete_memory_reflection(
    database: State<'_, RuntimeDatabase>,
    job_id: String,
    claim_token: String,
    proposal: MemoryRecordInput,
    candidate_id: Option<String>,
) -> Result<ReflectionJob, String> {
    database.complete_reflection(&job_id, &claim_token, &proposal, candidate_id.as_deref())
}

#[tauri::command]
pub fn review_memory_reflection(
    database: State<'_, RuntimeDatabase>,
    job_id: String,
    decision: String,
) -> Result<ReflectionJob, String> {
    database.review_reflection(&job_id, &decision)
}

#[tauri::command]
pub fn approve_reflection_optimization_candidate(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    reflection_job_id: String,
    candidate_id: String,
    operation_context: OperationContext,
) -> Result<OptimizationProfileResult, String> {
    let handler_started = Instant::now();
    let workspace_scope = database.local_workspace_scope()?;
    let authorization = validate_optimization_runtime_handler(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        &operation_context,
    )?;
    let reflection_job_id = reflection_job_id.trim();
    let candidate_id = candidate_id.trim();
    let mutation_key = runtime_effect_mutation_key(
        &authorization,
        "optimization.approve_reflection_candidate",
        &serde_json::json!({
            "reflectionJobId": reflection_job_id,
            "candidateId": candidate_id,
        }),
    )?;
    let profile = database.approve_reflection_optimization_candidate(
        reflection_job_id,
        candidate_id,
        Some(&mutation_key),
    )?;
    record_optimization_runtime_handler_completion(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        &operation_context,
        &mutation_key,
        handler_started,
    )?;
    Ok(profile)
}

#[tauri::command]
pub fn fail_memory_reflection(
    database: State<'_, RuntimeDatabase>,
    job_id: String,
    claim_token: String,
    error: String,
) -> Result<ReflectionJob, String> {
    database.fail_reflection(&job_id, &claim_token, &error)
}

#[tauri::command]
pub fn cancel_memory_reflection(
    database: State<'_, RuntimeDatabase>,
    job_id: String,
    reason: String,
) -> Result<ReflectionJob, String> {
    database.cancel_reflection(&job_id, &reason)
}

#[tauri::command]
pub fn memory_backend_status() -> MemoryBackendStatus {
    MemoryBackendStatus {
        active_backend: "sqlite",
        canonical_source: "obsidian-markdown",
    }
}
