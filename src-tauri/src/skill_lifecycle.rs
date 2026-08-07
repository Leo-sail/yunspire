use crate::{
    execution_ticket::{ExecutionTicketState, TrustedHandlerUsage},
    model_provider::{self, ApprovedSkillModelInput, ModelRequestState, ModelUsageSummary},
    obsidian::OperationContext,
    runtime_db::RuntimeDatabase,
    trace::{self, TraceEventRecord},
};
use chrono::Utc;
use futures_util::StreamExt;
use reqwest::{redirect::Policy, Client, Url};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    time::{Duration, Instant},
};
use tauri::{AppHandle, State};
use uuid::Uuid;

const EVALUATOR_VERSION: &str = "yunspire-deterministic-v1";
const MAX_SKILL_PAYLOAD_BYTES: usize = 512 * 1024;
const MAX_SKILL_EXECUTION_INPUT_BYTES: usize = 512 * 1024;
const MAX_REMOTE_SKILL_BYTES: usize = 512 * 1024;
const REMOTE_SKILL_TIMEOUT_SECONDS: u64 = 20;
const GITHUB_SKILL_SOURCE_KIND: &str = "github_skill_markdown";
const ALLOWED_CAPABILITIES: [&str; 4] = ["vault_read", "vault_write", "network", "shell"];

pub(crate) fn migrate_schema(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS skill_registry (
               workspace_scope TEXT NOT NULL,
               id TEXT NOT NULL,
               current_version INTEGER NOT NULL,
               state TEXT NOT NULL CHECK(state IN (
                 'draft', 'candidate', 'rejected', 'enabled', 'disabled', 'retired'
               )),
               name TEXT NOT NULL,
               description TEXT NOT NULL,
               payload_hash TEXT NOT NULL,
               trace_id TEXT NOT NULL,
               replacement_skill_id TEXT,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               PRIMARY KEY(workspace_scope, id),
               FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_skill_registry_state
               ON skill_registry(workspace_scope, state, updated_at);
             CREATE TABLE IF NOT EXISTS skill_versions (
               workspace_scope TEXT NOT NULL,
               skill_id TEXT NOT NULL,
               version INTEGER NOT NULL,
               payload_json TEXT NOT NULL,
               payload_hash TEXT NOT NULL,
               supersedes_version INTEGER,
               rollback_of_version INTEGER,
               created_at TEXT NOT NULL,
               PRIMARY KEY(workspace_scope, skill_id, version),
               FOREIGN KEY(workspace_scope, skill_id)
                 REFERENCES skill_registry(workspace_scope, id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS skill_evaluations (
               id TEXT PRIMARY KEY,
               workspace_scope TEXT NOT NULL,
               skill_id TEXT NOT NULL,
               version INTEGER NOT NULL,
               evaluator_version TEXT NOT NULL,
               passed INTEGER NOT NULL CHECK(passed IN (0, 1)),
               checks_json TEXT NOT NULL,
               payload_hash TEXT NOT NULL,
               evaluated_at TEXT NOT NULL,
               UNIQUE(workspace_scope, skill_id, version, evaluator_version),
               FOREIGN KEY(workspace_scope, skill_id, version)
                 REFERENCES skill_versions(workspace_scope, skill_id, version) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS skill_approvals (
               id TEXT PRIMARY KEY,
               workspace_scope TEXT NOT NULL,
               skill_id TEXT NOT NULL,
               version INTEGER NOT NULL,
               decision TEXT NOT NULL CHECK(decision IN ('approved', 'rejected')),
               actor TEXT NOT NULL CHECK(actor='user'),
               note TEXT NOT NULL,
               decided_at TEXT NOT NULL,
               FOREIGN KEY(workspace_scope, skill_id, version)
                 REFERENCES skill_versions(workspace_scope, skill_id, version) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_skill_approvals_latest
               ON skill_approvals(workspace_scope, skill_id, version, decided_at, id);
             CREATE TABLE IF NOT EXISTS skill_lifecycle_audit (
               id TEXT PRIMARY KEY,
               workspace_scope TEXT NOT NULL,
               skill_id TEXT NOT NULL,
               version INTEGER NOT NULL,
               trace_id TEXT NOT NULL,
               event_type TEXT NOT NULL,
               from_state TEXT,
               to_state TEXT NOT NULL,
               detail TEXT NOT NULL,
               created_at TEXT NOT NULL,
               FOREIGN KEY(workspace_scope, skill_id, version)
                 REFERENCES skill_versions(workspace_scope, skill_id, version) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_skill_lifecycle_audit_skill
               ON skill_lifecycle_audit(workspace_scope, skill_id, created_at, id);",
        )
        .map_err(|error| format!("无法创建 Skill 生命周期表：{error}"))
}

pub(crate) fn migrate_effect_schema(connection: &Connection) -> Result<(), String> {
    let skill_versions_exist = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='skill_versions'",
            [],
            |_| Ok(()),
        )
        .optional()
        .map_err(|error| format!("无法检查 Skill 版本表：{error}"))?
        .is_some();
    if !skill_versions_exist {
        return Ok(());
    }
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS skill_execution_effects (
               id TEXT PRIMARY KEY,
               execution_id TEXT NOT NULL,
               workspace_scope TEXT NOT NULL,
               skill_id TEXT NOT NULL,
               skill_version INTEGER NOT NULL CHECK(skill_version > 0),
               request_id TEXT NOT NULL,
               task_id TEXT,
               trace_id TEXT NOT NULL,
               input_hash TEXT NOT NULL,
               output_hash TEXT,
               outcome TEXT NOT NULL CHECK(outcome IN ('started', 'succeeded', 'failed', 'cancelled')),
               started_at TEXT NOT NULL,
               completed_at TEXT,
               warnings_json TEXT NOT NULL,
               error TEXT,
               created_at TEXT NOT NULL,
               UNIQUE(workspace_scope, execution_id, outcome),
               UNIQUE(workspace_scope, id),
               FOREIGN KEY(workspace_scope, skill_id, skill_version)
                 REFERENCES skill_versions(workspace_scope, skill_id, version) ON DELETE RESTRICT
             );
             CREATE INDEX IF NOT EXISTS idx_skill_execution_effects_lookup
               ON skill_execution_effects(
                 workspace_scope, skill_id, skill_version, created_at DESC, id DESC
               );
             CREATE INDEX IF NOT EXISTS idx_skill_execution_effects_request
               ON skill_execution_effects(workspace_scope, request_id, execution_id, created_at);
             CREATE TABLE IF NOT EXISTS skill_execution_effect_feedback (
               id TEXT PRIMARY KEY,
               workspace_scope TEXT NOT NULL,
               effect_id TEXT NOT NULL,
               relation_kind TEXT NOT NULL CHECK(relation_kind IN ('correction', 'acceptance')),
               reference_id TEXT NOT NULL,
               note TEXT NOT NULL,
               created_at TEXT NOT NULL,
               UNIQUE(workspace_scope, effect_id, relation_kind, reference_id),
               FOREIGN KEY(workspace_scope, effect_id)
                 REFERENCES skill_execution_effects(workspace_scope, id) ON DELETE RESTRICT
             );
             CREATE INDEX IF NOT EXISTS idx_skill_execution_effect_feedback_effect
               ON skill_execution_effect_feedback(workspace_scope, effect_id, created_at, id);
             CREATE TRIGGER IF NOT EXISTS skill_execution_effects_immutable_update
               BEFORE UPDATE ON skill_execution_effects
               BEGIN SELECT RAISE(ABORT, 'skill execution effects are immutable'); END;
             CREATE TRIGGER IF NOT EXISTS skill_execution_effects_immutable_delete
               BEFORE DELETE ON skill_execution_effects
               BEGIN SELECT RAISE(ABORT, 'skill execution effects are immutable'); END;
             CREATE TRIGGER IF NOT EXISTS skill_execution_feedback_immutable_update
               BEFORE UPDATE ON skill_execution_effect_feedback
               BEGIN SELECT RAISE(ABORT, 'skill execution feedback is immutable'); END;
             CREATE TRIGGER IF NOT EXISTS skill_execution_feedback_immutable_delete
               BEFORE DELETE ON skill_execution_effect_feedback
               BEGIN SELECT RAISE(ABORT, 'skill execution feedback is immutable'); END;",
        )
        .map_err(|error| format!("无法创建 Skill 执行效果表：{error}"))
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillDraftInput {
    id: String,
    #[serde(default)]
    expected_version: Option<i64>,
    name: String,
    #[serde(default)]
    description: String,
    instructions: String,
    #[serde(default)]
    input_schema: String,
    #[serde(default)]
    output_schema: String,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    trace_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillInstallInput {
    source_url: String,
    #[serde(default)]
    user_confirmed: bool,
    #[serde(default)]
    trace_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SkillSourceEvidence {
    source_url: String,
    source_hash: String,
    source_kind: String,
    source_revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NormalizedGithubSkillUrl {
    fetch_url: String,
    source_revision: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ImportedSkillManifest {
    name: String,
    description: String,
    instructions: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillRecord {
    id: String,
    name: String,
    description: String,
    instructions: String,
    input_schema: String,
    output_schema: String,
    capabilities: Vec<String>,
    source_url: Option<String>,
    source_hash: Option<String>,
    source_kind: Option<String>,
    source_revision: Option<String>,
    status: String,
    version: i64,
    payload_hash: String,
    evaluation_passed: bool,
    approval_state: Option<String>,
    routing_eligible: bool,
    replacement_skill_id: Option<String>,
    rollback_of_version: Option<i64>,
    trace_id: String,
    created_at: String,
    updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillEvaluationCheck {
    code: String,
    passed: bool,
    detail: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillEvaluationResult {
    skill_id: String,
    version: i64,
    evaluator_version: String,
    passed: bool,
    checks: Vec<SkillEvaluationCheck>,
    evaluated_at: String,
    skill: SkillRecord,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillVersionInput {
    skill_id: String,
    #[serde(default)]
    expected_version: Option<i64>,
    #[serde(default)]
    trace_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillApprovalInput {
    skill_id: String,
    expected_version: i64,
    approved: bool,
    #[serde(default)]
    note: String,
    #[serde(default)]
    trace_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillActivationAction {
    Enable,
    Disable,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillActivationInput {
    skill_id: String,
    expected_version: i64,
    action: SkillActivationAction,
    #[serde(default)]
    trace_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillRetirementInput {
    skill_id: String,
    expected_version: i64,
    #[serde(default)]
    replacement_skill_id: Option<String>,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    trace_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillRollbackInput {
    skill_id: String,
    expected_version: i64,
    target_version: i64,
    #[serde(default)]
    trace_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillVersionRecord {
    skill_id: String,
    version: i64,
    payload: Value,
    payload_hash: String,
    supersedes_version: Option<i64>,
    rollback_of_version: Option<i64>,
    created_at: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillExecutionInput {
    skill_id: String,
    expected_version: i64,
    expected_payload_hash: String,
    input: Value,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    trace_id: Option<String>,
    #[serde(default)]
    operation_context: Option<OperationContext>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillExecutionEffectFeedback {
    id: String,
    relation_kind: String,
    reference_id: String,
    note: String,
    created_at: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillExecutionEffect {
    id: String,
    execution_id: String,
    skill_id: String,
    skill_version: i64,
    request_id: String,
    task_id: Option<String>,
    trace_id: String,
    input_hash: String,
    output_hash: Option<String>,
    outcome: String,
    started_at: String,
    completed_at: Option<String>,
    warnings: Vec<String>,
    error: Option<String>,
    created_at: String,
    feedback: Vec<SkillExecutionEffectFeedback>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillExecutionEffectQuery {
    #[serde(default)]
    skill_id: Option<String>,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    trace_id: Option<String>,
    #[serde(default)]
    outcomes: Vec<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillExecutionEffectFeedbackInput {
    effect_id: String,
    relation_kind: String,
    reference_id: String,
    #[serde(default)]
    note: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SkillExecutionIdentity {
    id: String,
    name: String,
    version: i64,
    payload_hash: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SkillExecutionTrace {
    trace_id: String,
    request_id: String,
    started_at: String,
    completed_at: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SkillExecutionModel {
    provider: String,
    model: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillExecutionResult {
    output_text: String,
    output_data: Value,
    warnings: Vec<String>,
    skill: SkillExecutionIdentity,
    trace: SkillExecutionTrace,
    model: SkillExecutionModel,
    usage: ModelUsageSummary,
}

#[derive(Clone, Debug)]
struct SkillExecutionSnapshot {
    id: String,
    name: String,
    version: i64,
    payload_hash: String,
    instructions: String,
    input_schema: String,
    output_schema: String,
    declared_capabilities: Vec<String>,
}

#[derive(Clone, Debug)]
struct SkillExecutionEffectContext {
    execution_id: String,
    request_id: String,
    task_id: Option<String>,
    trace_id: String,
    input_hash: String,
    started_at: String,
}

#[derive(Clone)]
struct CurrentSkill {
    id: String,
    version: i64,
    state: String,
    payload_hash: String,
    trace_id: String,
    created_at: String,
}

fn valid_skill_id(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
        && value.chars().count() <= 64
        && characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
}

fn normalized_schema(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(String::new());
    }
    let parsed = serde_json::from_str::<Value>(value)
        .map_err(|_| "Skill Schema 必须是有效 JSON".to_string())?;
    if !parsed.is_object() {
        return Err("Skill Schema 必须是 JSON 对象".to_string());
    }
    serde_json::to_string_pretty(&parsed)
        .map_err(|error| format!("无法规范化 Skill Schema：{error}"))
}

fn normalize_capabilities(capabilities: &[String]) -> Result<Vec<String>, String> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for capability in capabilities {
        let capability = capability.trim();
        if !ALLOWED_CAPABILITIES.contains(&capability) {
            return Err(format!("Skill 声明了不支持的能力：{capability}"));
        }
        if seen.insert(capability.to_string()) {
            normalized.push(capability.to_string());
        }
    }
    normalized.sort();
    Ok(normalized)
}

fn valid_github_path_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value != "."
        && value != ".."
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
}

fn normalize_github_skill_url(source_url: &str) -> Result<NormalizedGithubSkillUrl, String> {
    let source_url = source_url.trim();
    if source_url.is_empty()
        || source_url.len() > 1_400
        || source_url
            .chars()
            .any(|character| character.is_control() || character == '\\')
        || source_url.contains('%')
        || source_url
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
    {
        return Err("第三方 Skill URL 为空、过长或包含不允许的转义字符".to_string());
    }
    let url = Url::parse(source_url).map_err(|_| "第三方 Skill URL 无效".to_string())?;
    if url.scheme() != "https" {
        return Err("第三方 Skill 只允许使用 HTTPS GitHub URL".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("第三方 Skill URL 不允许包含凭据".to_string());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("第三方 Skill URL 不允许包含查询参数或 fragment".to_string());
    }
    if url.port().is_some() {
        return Err("第三方 Skill URL 不允许指定端口".to_string());
    }
    let host = url
        .host_str()
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| "第三方 Skill URL 缺少主机".to_string())?;
    if !matches!(host.as_str(), "github.com" | "raw.githubusercontent.com") {
        return Err(
            "第三方 Skill 只允许从 github.com 或 raw.githubusercontent.com 导入".to_string(),
        );
    }
    let segments = url
        .path_segments()
        .ok_or_else(|| "第三方 Skill URL 路径无效".to_string())?
        .map(str::to_string)
        .collect::<Vec<_>>();
    if segments
        .iter()
        .any(|segment| !valid_github_path_segment(segment))
    {
        return Err("第三方 Skill URL 包含不安全的路径片段".to_string());
    }

    let (fetch_segments, source_revision) = if host == "github.com" {
        if segments.len() < 5 || segments.get(2).map(String::as_str) != Some("blob") {
            return Err(
                "github.com Skill URL 必须使用 /owner/repo/blob/revision/.../SKILL.md 格式"
                    .to_string(),
            );
        }
        let mut fetch_segments = vec![segments[0].clone(), segments[1].clone()];
        fetch_segments.extend(segments[3..].iter().cloned());
        (fetch_segments, segments[3].clone())
    } else {
        if segments.len() < 4 {
            return Err(
                "raw.githubusercontent.com Skill URL 缺少仓库、版本或 SKILL.md 路径".to_string(),
            );
        }
        let source_revision = if segments.get(2).map(String::as_str) == Some("refs") {
            if segments.len() < 6
                || !matches!(segments.get(3).map(String::as_str), Some("heads" | "tags"))
            {
                return Err(
                    "GitHub refs URL 必须包含 refs/heads/name 或 refs/tags/name".to_string()
                );
            }
            format!("refs/{}/{}", segments[3], segments[4])
        } else {
            segments[2].clone()
        };
        (segments, source_revision)
    };
    if fetch_segments.last().map(String::as_str) != Some("SKILL.md") {
        return Err("第三方 Skill URL 必须明确指向名为 SKILL.md 的文件".to_string());
    }
    let fetch_url = format!(
        "https://raw.githubusercontent.com/{}",
        fetch_segments.join("/")
    );
    Url::parse(&fetch_url).map_err(|_| "无法规范化 GitHub Skill URL".to_string())?;
    Ok(NormalizedGithubSkillUrl {
        fetch_url,
        source_revision,
    })
}

fn parse_manifest_scalar(value: &str, label: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("SKILL.md front matter 缺少 {label}"));
    }
    if value.starts_with('"') {
        return serde_json::from_str::<String>(value)
            .map_err(|_| format!("SKILL.md {label} 双引号字符串无效"));
    }
    if value.starts_with('\'') {
        if value.len() < 2 || !value.ends_with('\'') {
            return Err(format!("SKILL.md {label} 单引号字符串无效"));
        }
        return Ok(value[1..value.len() - 1].replace("''", "'"));
    }
    if matches!(
        value.chars().next(),
        Some('[' | ']' | '{' | '}' | '&' | '*' | '!' | '|' | '>')
    ) {
        return Err(format!("SKILL.md {label} 必须是简单声明式文本"));
    }
    Ok(value.to_string())
}

fn parse_manifest_block(lines: &[&str], folded: bool, label: &str) -> Result<String, String> {
    let indent = lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.bytes().take_while(|byte| *byte == b' ').count())
        .min()
        .ok_or_else(|| format!("SKILL.md {label} 块为空"))?;
    if indent == 0 || lines.iter().any(|line| line.starts_with('\t')) {
        return Err(format!("SKILL.md {label} 块缩进无效"));
    }
    let values = lines
        .iter()
        .map(|line| {
            if line.trim().is_empty() {
                Ok(String::new())
            } else if line.bytes().take_while(|byte| *byte == b' ').count() < indent {
                Err(format!("SKILL.md {label} 块缩进不一致"))
            } else {
                Ok(line[indent..].trim_end().to_string())
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let value = if folded {
        let mut output = String::new();
        let mut previous_blank = false;
        for value in values {
            if value.is_empty() {
                if !output.is_empty() && !output.ends_with('\n') {
                    output.push('\n');
                }
                previous_blank = true;
            } else {
                if !output.is_empty() && !previous_blank {
                    output.push(' ');
                }
                output.push_str(&value);
                previous_blank = false;
            }
        }
        output
    } else {
        values.join("\n")
    };
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(format!("SKILL.md {label} 块为空"));
    }
    Ok(value)
}

fn parse_imported_skill_manifest(content: &str) -> Result<ImportedSkillManifest, String> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let lines = normalized.split('\n').collect::<Vec<_>>();
    if lines.first().map(|line| line.trim()) != Some("---") {
        return Err("SKILL.md 必须以 YAML front matter 开始".to_string());
    }
    let closing = lines
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, line)| (line.trim() == "---").then_some(index))
        .ok_or_else(|| "SKILL.md front matter 未闭合".to_string())?;
    let mut name = None;
    let mut description = None;
    let mut index = 1;
    while index < closing {
        let line = lines[index];
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || line.starts_with([' ', '\t']) {
            index += 1;
            continue;
        }
        let Some((key, raw_value)) = line.split_once(':') else {
            return Err("SKILL.md front matter 包含无效字段".to_string());
        };
        let key = key.trim();
        let raw_value = raw_value.trim();
        index += 1;
        if !matches!(key, "name" | "description") {
            continue;
        }
        let value = if matches!(raw_value, "|" | "|-" | "|+" | ">" | ">-" | ">+") {
            let block_start = index;
            while index < closing
                && (lines[index].trim().is_empty() || lines[index].starts_with([' ', '\t']))
            {
                index += 1;
            }
            parse_manifest_block(&lines[block_start..index], raw_value.starts_with('>'), key)?
        } else {
            parse_manifest_scalar(raw_value, key)?
        };
        let target = if key == "name" {
            &mut name
        } else {
            &mut description
        };
        if target.replace(value).is_some() {
            return Err(format!("SKILL.md front matter 重复声明 {key}"));
        }
    }
    let name = name.ok_or_else(|| "SKILL.md front matter 缺少 name".to_string())?;
    let description =
        description.ok_or_else(|| "SKILL.md front matter 缺少 description".to_string())?;
    let instructions = lines[closing + 1..].join("\n").trim().to_string();
    if instructions.is_empty() {
        return Err("SKILL.md 正文 instructions 为空".to_string());
    }
    for (label, value) in [
        ("name", name.as_str()),
        ("description", description.as_str()),
        ("instructions", instructions.as_str()),
    ] {
        if value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
        {
            return Err(format!("SKILL.md {label} 包含不允许的控制字符"));
        }
    }
    Ok(ImportedSkillManifest {
        name,
        description,
        instructions,
    })
}

async fn download_github_skill_manifest(
    normalized: &NormalizedGithubSkillUrl,
) -> Result<Vec<u8>, String> {
    let client = Client::builder()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(REMOTE_SKILL_TIMEOUT_SECONDS))
        .user_agent("Yunspire/third-party-skill-import")
        .build()
        .map_err(|error| format!("无法创建第三方 Skill 下载客户端：{error}"))?;
    let response = client
        .get(&normalized.fetch_url)
        .header(reqwest::header::ACCEPT, "text/plain")
        .send()
        .await
        .map_err(|error| format!("无法下载第三方 SKILL.md：{error}"))?;
    if response.status().is_redirection() {
        return Err("第三方 SKILL.md 返回了重定向，已拒绝导入".to_string());
    }
    if !response.status().is_success() {
        return Err(format!(
            "第三方 SKILL.md 下载失败，HTTP 状态为 {}",
            response.status()
        ));
    }
    if response.url().as_str() != normalized.fetch_url {
        return Err("第三方 SKILL.md 响应 URL 与已批准来源不一致".to_string());
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_REMOTE_SKILL_BYTES as u64)
    {
        return Err("第三方 SKILL.md 超过 512 KB 安全上限".to_string());
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("读取第三方 SKILL.md 失败：{error}"))?;
        if body
            .len()
            .checked_add(chunk.len())
            .is_none_or(|length| length > MAX_REMOTE_SKILL_BYTES)
        {
            return Err("第三方 SKILL.md 超过 512 KB 流式安全上限".to_string());
        }
        body.extend_from_slice(&chunk);
    }
    if body.is_empty() {
        return Err("第三方 SKILL.md 内容为空".to_string());
    }
    Ok(body)
}

fn draft_payload(input: &SkillDraftInput) -> Result<Value, String> {
    draft_payload_with_source(input, None)
}

fn draft_payload_with_source(
    input: &SkillDraftInput,
    source: Option<&SkillSourceEvidence>,
) -> Result<Value, String> {
    let id = input.id.trim();
    if !valid_skill_id(id) {
        return Err("Skill ID 必须以小写字母开头且只包含小写字母、数字和连字符".to_string());
    }
    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 96 {
        return Err("Skill 名称为空或超过 96 个字符".to_string());
    }
    let description = input.description.trim();
    if description.chars().count() > 1_000 {
        return Err("Skill 用途说明超过 1000 个字符".to_string());
    }
    let instructions = input.instructions.trim();
    if instructions.is_empty() || instructions.chars().count() > 32_000 {
        return Err("Skill 指令为空或超过 32000 个字符".to_string());
    }
    let mut payload = serde_json::json!({
        "id": id,
        "name": name,
        "description": description,
        "instructions": instructions,
        "inputSchema": input.input_schema.trim(),
        "outputSchema": input.output_schema.trim(),
        "capabilities": normalize_capabilities(&input.capabilities)?,
    });
    if let (Some(source), Some(payload)) = (source, payload.as_object_mut()) {
        payload.insert(
            "sourceUrl".to_string(),
            Value::String(source.source_url.clone()),
        );
        payload.insert(
            "sourceHash".to_string(),
            Value::String(source.source_hash.clone()),
        );
        payload.insert(
            "sourceKind".to_string(),
            Value::String(source.source_kind.clone()),
        );
        payload.insert(
            "sourceRevision".to_string(),
            Value::String(source.source_revision.clone()),
        );
    }
    if serde_json::to_vec(&payload)
        .map_err(|error| format!("无法序列化 Skill：{error}"))?
        .len()
        > MAX_SKILL_PAYLOAD_BYTES
    {
        return Err("Skill 定义超过 512 KB 安全上限".to_string());
    }
    Ok(payload)
}

fn payload_hash(payload: &Value) -> Result<String, String> {
    let bytes = serde_json::to_vec(payload).map_err(|error| format!("无法哈希 Skill：{error}"))?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn value_hash(value: &Value) -> Result<String, String> {
    let bytes =
        serde_json::to_vec(value).map_err(|error| format!("无法哈希 Skill 执行数据：{error}"))?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn validate_effect_hash(value: &str, label: &str) -> Result<String, String> {
    let value = value.trim().to_ascii_lowercase();
    let digest = value
        .strip_prefix("sha256:")
        .ok_or_else(|| format!("{label} 必须是 SHA-256"))?;
    if digest.len() != 64
        || !digest
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err(format!("{label} 必须是 SHA-256"));
    }
    Ok(format!("sha256:{digest}"))
}

fn normalize_effect_warnings(warnings: &[String]) -> Result<Vec<String>, String> {
    if warnings.len() > 64 {
        return Err("Skill 执行 warnings 不能超过 64 条".to_string());
    }
    warnings
        .iter()
        .map(|warning| {
            let warning = warning.trim().to_string();
            if warning.is_empty() || warning.chars().count() > 2_000 {
                return Err("Skill 执行 warning 无效".to_string());
            }
            Ok(warning)
        })
        .collect()
}

fn map_skill_execution_effect_base(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<SkillExecutionEffect> {
    let warnings_json = row.get::<_, String>(12)?;
    Ok(SkillExecutionEffect {
        id: row.get(0)?,
        execution_id: row.get(1)?,
        skill_id: row.get(2)?,
        skill_version: row.get(3)?,
        request_id: row.get(4)?,
        task_id: row.get(5)?,
        trace_id: row.get(6)?,
        input_hash: row.get(7)?,
        output_hash: row.get(8)?,
        outcome: row.get(9)?,
        started_at: row.get(10)?,
        completed_at: row.get(11)?,
        warnings: serde_json::from_str(&warnings_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                warnings_json.len(),
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("无法解析 Skill 执行 warnings：{error}"),
                )),
            )
        })?,
        error: row.get(13)?,
        created_at: row.get(14)?,
        feedback: Vec::new(),
    })
}

fn read_skill_execution_effect(
    connection: &Connection,
    workspace_scope: &str,
    effect_id: &str,
) -> Result<SkillExecutionEffect, String> {
    let mut effect = connection
        .query_row(
            "SELECT id, execution_id, skill_id, skill_version, request_id, task_id, trace_id,
                    input_hash, output_hash, outcome, started_at, completed_at, warnings_json,
                    error, created_at
             FROM skill_execution_effects
             WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, effect_id],
            map_skill_execution_effect_base,
        )
        .optional()
        .map_err(|error| format!("无法读取 Skill 执行效果：{error}"))?
        .ok_or_else(|| "Skill 执行效果不存在".to_string())?;
    let mut feedback_statement = connection
        .prepare(
            "SELECT id, relation_kind, reference_id, note, created_at
             FROM skill_execution_effect_feedback
             WHERE workspace_scope=?1 AND effect_id=?2
             ORDER BY created_at ASC, id ASC",
        )
        .map_err(|error| format!("无法准备 Skill 执行反馈查询：{error}"))?;
    effect.feedback = feedback_statement
        .query_map(params![workspace_scope, effect_id], |row| {
            Ok(SkillExecutionEffectFeedback {
                id: row.get(0)?,
                relation_kind: row.get(1)?,
                reference_id: row.get(2)?,
                note: row.get(3)?,
                created_at: row.get(4)?,
            })
        })
        .map_err(|error| format!("无法查询 Skill 执行反馈：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法读取 Skill 执行反馈：{error}"))?;
    Ok(effect)
}

#[allow(clippy::too_many_arguments)]
fn record_skill_execution_effect_in_connection(
    connection: &Connection,
    workspace_scope: &str,
    execution_id: &str,
    skill_id: &str,
    skill_version: i64,
    request_id: &str,
    task_id: Option<&str>,
    trace_id: &str,
    input_hash: &str,
    output_hash: Option<&str>,
    outcome: &str,
    started_at: &str,
    completed_at: Option<&str>,
    warnings: &[String],
    error: Option<&str>,
) -> Result<SkillExecutionEffect, String> {
    let execution_id = execution_id.trim();
    if execution_id.is_empty() || execution_id.chars().count() > 160 {
        return Err("Skill 执行 effect executionId 无效".to_string());
    }
    let skill_id = skill_id.trim();
    if !valid_skill_id(skill_id) {
        return Err("Skill 执行 effect skillId 无效".to_string());
    }
    if skill_version <= 0 {
        return Err("Skill 执行 effect skillVersion 无效".to_string());
    }
    let request_id = request_id.trim();
    if request_id.is_empty() || request_id.chars().count() > 160 {
        return Err("Skill 执行 effect requestId 无效".to_string());
    }
    let task_id = task_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let trace_id = checked_trace_id(Some(trace_id), None)?;
    let input_hash = validate_effect_hash(input_hash, "Skill 执行 inputHash")?;
    let output_hash = output_hash
        .map(|value| validate_effect_hash(value, "Skill 执行 outputHash"))
        .transpose()?;
    if !matches!(outcome, "started" | "succeeded" | "failed" | "cancelled") {
        return Err("Skill 执行 effect outcome 无效".to_string());
    }
    if outcome == "succeeded" && output_hash.is_none() {
        return Err("成功的 Skill 执行必须包含 outputHash".to_string());
    }
    let warnings = normalize_effect_warnings(warnings)?;
    let warnings_json = serde_json::to_string(&warnings)
        .map_err(|error| format!("无法序列化 Skill 执行 warnings：{error}"))?;
    if warnings_json.len() > 128 * 1024 {
        return Err("Skill 执行 warnings 超过 128 KB 安全上限".to_string());
    }
    let error = error
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(4_000).collect::<String>());
    let completed_at = completed_at.map(str::to_string);
    let created_at = Utc::now().to_rfc3339();
    let effect_id = Uuid::new_v4().to_string();
    connection
        .execute(
            "INSERT OR IGNORE INTO skill_execution_effects
             (id, execution_id, workspace_scope, skill_id, skill_version, request_id, task_id,
              trace_id, input_hash, output_hash, outcome, started_at, completed_at,
              warnings_json, error, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                effect_id,
                execution_id,
                workspace_scope,
                skill_id,
                skill_version,
                request_id,
                task_id,
                trace_id,
                input_hash,
                output_hash,
                outcome,
                started_at,
                completed_at,
                warnings_json,
                error,
                created_at,
            ],
        )
        .map_err(|error| format!("无法保存 Skill 执行效果：{error}"))?;
    let effect_id = connection
        .query_row(
            "SELECT id FROM skill_execution_effects
             WHERE workspace_scope=?1 AND execution_id=?2 AND outcome=?3",
            params![workspace_scope, execution_id, outcome],
            |row| row.get::<_, String>(0),
        )
        .map_err(|error| format!("无法确认 Skill 执行效果：{error}"))?;
    read_skill_execution_effect(connection, workspace_scope, &effect_id)
}

pub(crate) fn reflection_effect_snapshots_in_connection(
    connection: &Connection,
    workspace_scope: &str,
    effect_ids: &[String],
) -> Result<Vec<Value>, String> {
    effect_ids
        .iter()
        .map(|effect_id| {
            let effect = read_skill_execution_effect(connection, workspace_scope, effect_id)?;
            if !matches!(
                effect.outcome.as_str(),
                "succeeded" | "failed" | "cancelled"
            ) {
                return Err("反思来源只能引用已结束的 Skill 执行效果".to_string());
            }
            Ok(serde_json::json!({
                "id": effect.id,
                "executionId": effect.execution_id,
                "skillId": effect.skill_id,
                "skillVersion": effect.skill_version,
                "requestId": effect.request_id,
                "taskId": effect.task_id,
                "traceId": effect.trace_id,
                "inputHash": effect.input_hash,
                "outputHash": effect.output_hash,
                "outcome": effect.outcome,
                "startedAt": effect.started_at,
                "completedAt": effect.completed_at,
                "warnings": effect.warnings,
                "error": effect.error,
                "feedback": effect.feedback,
            }))
        })
        .collect()
}

fn list_skill_execution_effects_in_connection(
    connection: &Connection,
    workspace_scope: &str,
    query: &SkillExecutionEffectQuery,
) -> Result<Vec<SkillExecutionEffect>, String> {
    let skill_id = query
        .skill_id
        .as_deref()
        .map(|value| {
            if !valid_skill_id(value.trim()) {
                Err("Skill 执行查询 skillId 无效".to_string())
            } else {
                Ok(value.trim().to_string())
            }
        })
        .transpose()?;
    let request_id = query
        .request_id
        .as_deref()
        .map(|value| normalized_effect_query_string(value, "requestId"))
        .transpose()?;
    let task_id = query
        .task_id
        .as_deref()
        .map(|value| normalized_effect_query_string(value, "taskId"))
        .transpose()?;
    let trace_id = query
        .trace_id
        .as_deref()
        .map(|value| normalized_effect_query_string(value, "traceId"))
        .transpose()?;
    let outcomes = query
        .outcomes
        .iter()
        .map(|outcome| {
            if !matches!(
                outcome.as_str(),
                "started" | "succeeded" | "failed" | "cancelled"
            ) {
                Err("Skill 执行查询 outcome 无效".to_string())
            } else {
                Ok(outcome.clone())
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let mut sql = "SELECT id, execution_id, skill_id, skill_version, request_id, task_id, trace_id,
                          input_hash, output_hash, outcome, started_at, completed_at, warnings_json,
                          error, created_at
                   FROM skill_execution_effects WHERE workspace_scope=?1"
        .to_string();
    let mut values = vec![rusqlite::types::Value::Text(workspace_scope.to_string())];
    let mut next_param = 2;
    for (field, value) in [
        ("skill_id", skill_id),
        ("request_id", request_id),
        ("task_id", task_id),
        ("trace_id", trace_id),
    ] {
        if let Some(value) = value {
            sql.push_str(&format!(" AND {field}=?{next_param}"));
            values.push(rusqlite::types::Value::Text(value));
            next_param += 1;
        }
    }
    if !outcomes.is_empty() {
        let mut placeholders = Vec::with_capacity(outcomes.len());
        for outcome in outcomes {
            placeholders.push(format!("?{next_param}"));
            values.push(rusqlite::types::Value::Text(outcome));
            next_param += 1;
        }
        let placeholders = placeholders.join(", ");
        sql.push_str(&format!(" AND outcome IN ({placeholders})"));
    }
    sql.push_str(&format!(" ORDER BY created_at DESC, id DESC LIMIT {limit}"));
    let mut statement = connection
        .prepare(&sql)
        .map_err(|error| format!("无法准备 Skill 执行效果查询：{error}"))?;
    let base = statement
        .query_map(
            rusqlite::params_from_iter(values),
            map_skill_execution_effect_base,
        )
        .map_err(|error| format!("无法查询 Skill 执行效果：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法读取 Skill 执行效果：{error}"))?;
    base.into_iter()
        .map(|mut effect| {
            let mut with_feedback =
                read_skill_execution_effect(connection, workspace_scope, &effect.id)?;
            effect.feedback = std::mem::take(&mut with_feedback.feedback);
            Ok(effect)
        })
        .collect()
}

fn normalized_effect_query_string(value: &str, label: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 160 || value.chars().any(char::is_control) {
        return Err(format!("Skill 执行查询 {label} 无效"));
    }
    Ok(value.to_string())
}

fn record_skill_execution_effect_feedback_in_connection(
    connection: &Connection,
    workspace_scope: &str,
    input: &SkillExecutionEffectFeedbackInput,
) -> Result<SkillExecutionEffect, String> {
    let effect_id = normalized_effect_query_string(&input.effect_id, "effectId")?;
    let relation_kind = input.relation_kind.trim();
    if !matches!(relation_kind, "correction" | "acceptance") {
        return Err("Skill 执行反馈 relationKind 无效".to_string());
    }
    let reference_id = normalized_effect_query_string(&input.reference_id, "referenceId")?;
    let note = input.note.trim().chars().take(4_000).collect::<String>();
    connection
        .execute(
            "INSERT OR IGNORE INTO skill_execution_effect_feedback
             (id, workspace_scope, effect_id, relation_kind, reference_id, note, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                Uuid::new_v4().to_string(),
                workspace_scope,
                effect_id,
                relation_kind,
                reference_id,
                note,
                Utc::now().to_rfc3339(),
            ],
        )
        .map_err(|error| format!("无法保存 Skill 执行反馈：{error}"))?;
    read_skill_execution_effect(connection, workspace_scope, &effect_id)
}

pub(crate) fn record_skill_execution_feedback_link_in_connection(
    connection: &Connection,
    workspace_scope: &str,
    effect_id: &str,
    relation_kind: &str,
    reference_id: &str,
    note: &str,
) -> Result<(), String> {
    record_skill_execution_effect_feedback_in_connection(
        connection,
        workspace_scope,
        &SkillExecutionEffectFeedbackInput {
            effect_id: effect_id.to_string(),
            relation_kind: relation_kind.to_string(),
            reference_id: reference_id.to_string(),
            note: note.to_string(),
        },
    )?;
    Ok(())
}

fn parsed_schema(value: &str, label: &str) -> Result<Option<Value>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let schema = serde_json::from_str::<Value>(value)
        .map_err(|error| format!("Skill {label} Schema 已损坏：{error}"))?;
    if !schema.is_object() {
        return Err(format!("Skill {label} Schema 必须是 JSON 对象"));
    }
    Ok(Some(schema))
}

fn json_value_matches_type(value: &Value, expected_type: &str) -> bool {
    match expected_type {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => false,
    }
}

fn validate_schema_value(value: &Value, schema: &Value, path: &str) -> Result<(), String> {
    if let Some(expected_type) = schema.get("type").and_then(Value::as_str) {
        if !json_value_matches_type(value, expected_type) {
            return Err(format!(
                "{path} 类型不符合 Skill Schema，要求 {expected_type}"
            ));
        }
    }
    if let Some(required) = schema.get("required") {
        let required = required
            .as_array()
            .ok_or_else(|| format!("{path} Schema 的 required 必须是字符串数组"))?;
        let object = value
            .as_object()
            .ok_or_else(|| format!("{path} Schema 声明 required 时数据必须是对象"))?;
        for field in required {
            let field = field
                .as_str()
                .ok_or_else(|| format!("{path} Schema 的 required 必须只包含字符串"))?;
            if !object.contains_key(field) {
                return Err(format!("{path} 缺少 Skill Schema 必填字段 {field}"));
            }
        }
    }
    if let Some(properties) = schema.get("properties") {
        let properties = properties
            .as_object()
            .ok_or_else(|| format!("{path} Schema 的 properties 必须是对象"))?;
        let object = value
            .as_object()
            .ok_or_else(|| format!("{path} Schema 声明 properties 时数据必须是对象"))?;
        for (field, property_schema) in properties {
            if !property_schema.is_object() {
                return Err(format!("{path}.{field} 的 Schema 必须是对象"));
            }
            if let Some(property_value) = object.get(field) {
                validate_schema_value(property_value, property_schema, &format!("{path}.{field}"))?;
            }
        }
    }
    Ok(())
}

fn validate_declared_schema(
    value: &Value,
    schema: Option<&Value>,
    label: &str,
) -> Result<(), String> {
    if let Some(schema) = schema {
        validate_schema_value(value, schema, label)?;
    }
    Ok(())
}

fn execution_snapshot_in_connection(
    connection: &Connection,
    workspace_scope: &str,
    skill_id: &str,
    expected_version: i64,
    expected_payload_hash: &str,
) -> Result<SkillExecutionSnapshot, String> {
    let skill_id = skill_id.trim();
    if !valid_skill_id(skill_id) {
        return Err("Skill 执行 ID 无效".to_string());
    }
    if expected_version <= 0 {
        return Err("Skill 执行 expectedVersion 无效".to_string());
    }
    let expected_payload_hash = expected_payload_hash.trim();
    if expected_payload_hash.is_empty() || !expected_payload_hash.starts_with("sha256:") {
        return Err("Skill 执行 expectedPayloadHash 无效".to_string());
    }
    let current = current_skill(connection, workspace_scope, skill_id)?;
    if current.version != expected_version {
        return Err("Skill 执行版本已变化，已拒绝使用过期版本".to_string());
    }
    if current.payload_hash != expected_payload_hash {
        return Err("Skill 执行负载哈希已变化，已拒绝继续".to_string());
    }
    let (payload_json, version_payload_hash) = connection
        .query_row(
            "SELECT payload_json, payload_hash FROM skill_versions
             WHERE workspace_scope=?1 AND skill_id=?2 AND version=?3",
            params![workspace_scope, current.id, current.version],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(|error| format!("无法读取 Skill 执行版本：{error}"))?;
    if version_payload_hash != current.payload_hash {
        return Err("Skill 注册表与版本负载哈希不一致，已关闭执行".to_string());
    }
    let payload = serde_json::from_str::<Value>(&payload_json)
        .map_err(|error| format!("Skill 执行版本负载损坏：{error}"))?;
    if payload_hash(&payload)? != current.payload_hash {
        return Err("Skill 当前负载内容与已批准哈希不一致，已关闭执行".to_string());
    }
    let evaluation_passed = connection
        .query_row(
            "SELECT passed FROM skill_evaluations
             WHERE workspace_scope=?1 AND skill_id=?2 AND version=?3
               AND evaluator_version=?4 AND payload_hash=?5",
            params![
                workspace_scope,
                current.id,
                current.version,
                EVALUATOR_VERSION,
                current.payload_hash
            ],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|error| format!("无法验证 Skill 执行评估：{error}"))?
        == Some(1);
    let approval = connection
        .query_row(
            "SELECT decision FROM skill_approvals
             WHERE workspace_scope=?1 AND skill_id=?2 AND version=?3
             ORDER BY decided_at DESC, id DESC LIMIT 1",
            params![workspace_scope, current.id, current.version],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("无法验证 Skill 执行审批：{error}"))?;
    let routing_eligible =
        current.state == "enabled" && evaluation_passed && approval.as_deref() == Some("approved");
    if !routing_eligible {
        return Err("Skill 执行前验证失败：必须已启用、评估通过、用户批准且可路由".to_string());
    }
    let instructions = payload_string(&payload, "instructions");
    if instructions.trim().is_empty() {
        return Err("Skill 已批准指令为空，已关闭执行".to_string());
    }
    Ok(SkillExecutionSnapshot {
        id: current.id,
        name: payload_string(&payload, "name"),
        version: current.version,
        payload_hash: current.payload_hash,
        instructions,
        input_schema: payload_string(&payload, "inputSchema"),
        output_schema: payload_string(&payload, "outputSchema"),
        declared_capabilities: payload_capabilities(&payload),
    })
}

fn current_skill(
    connection: &Connection,
    workspace_scope: &str,
    skill_id: &str,
) -> Result<CurrentSkill, String> {
    connection
        .query_row(
            "SELECT id, current_version, state, payload_hash, trace_id, created_at
             FROM skill_registry WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, skill_id],
            |row| {
                Ok(CurrentSkill {
                    id: row.get(0)?,
                    version: row.get(1)?,
                    state: row.get(2)?,
                    payload_hash: row.get(3)?,
                    trace_id: row.get(4)?,
                    created_at: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("无法读取 Skill：{error}"))?
        .ok_or_else(|| "未找到 Skill".to_string())
}

fn checked_trace_id(input: Option<&str>, fallback: Option<&str>) -> Result<String, String> {
    let trace_id = input
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or(fallback)
        .map(str::to_string)
        .unwrap_or_else(trace::new_trace_id);
    trace::validate_trace_id(&trace_id)?;
    Ok(trace_id)
}

#[allow(clippy::too_many_arguments)]
fn append_audit(
    transaction: &Connection,
    workspace_scope: &str,
    skill_id: &str,
    version: i64,
    trace_id: &str,
    event_type: &str,
    from_state: Option<&str>,
    to_state: &str,
    detail: &str,
    created_at: &str,
) -> Result<(), String> {
    let audit_id = Uuid::new_v4().to_string();
    transaction
        .execute(
            "INSERT INTO skill_lifecycle_audit
             (id, workspace_scope, skill_id, version, trace_id, event_type,
              from_state, to_state, detail, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                audit_id,
                workspace_scope,
                skill_id,
                version,
                trace_id,
                event_type,
                from_state,
                to_state,
                detail.chars().take(2_000).collect::<String>(),
                created_at
            ],
        )
        .map_err(|error| format!("无法保存 Skill 审计：{error}"))?;
    trace::record_trace_event_in_connection(
        transaction,
        workspace_scope,
        &TraceEventRecord {
            trace_id,
            entity_kind: "skill_version",
            entity_id: &format!("{skill_id}@{version}"),
            event_type,
            state: to_state,
            payload: &serde_json::json!({
                "auditId": audit_id,
                "skillId": skill_id,
                "version": version,
                "fromState": from_state,
                "toState": to_state,
                "detail": detail,
            }),
            created_at,
        },
    )?;
    Ok(())
}

fn append_execution_audit(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    snapshot: &SkillExecutionSnapshot,
    trace_id: &str,
    event_type: &str,
    detail: &str,
    created_at: &str,
) -> Result<(), String> {
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始 Skill 执行审计事务：{error}"))?;
    append_audit(
        &transaction,
        workspace_scope,
        &snapshot.id,
        snapshot.version,
        trace_id,
        event_type,
        Some("enabled"),
        "enabled",
        detail,
        created_at,
    )?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交 Skill 执行审计：{error}"))
}

fn append_checked_execution_audit(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    snapshot: &SkillExecutionSnapshot,
    trace_id: &str,
    event_type: &str,
    detail: &str,
    created_at: &str,
) -> Result<(), String> {
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始 Skill 执行结束事务：{error}"))?;
    let current = execution_snapshot_in_connection(
        &transaction,
        workspace_scope,
        &snapshot.id,
        snapshot.version,
        &snapshot.payload_hash,
    )?;
    if current.instructions != snapshot.instructions
        || current.output_schema != snapshot.output_schema
        || current.input_schema != snapshot.input_schema
    {
        return Err("Skill 执行期间定义已变化，已关闭返回".to_string());
    }
    append_audit(
        &transaction,
        workspace_scope,
        &snapshot.id,
        snapshot.version,
        trace_id,
        event_type,
        Some("enabled"),
        "enabled",
        detail,
        created_at,
    )?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交 Skill 执行结束审计：{error}"))
}

#[allow(clippy::too_many_arguments)]
fn record_execution_effect(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    snapshot: &SkillExecutionSnapshot,
    context: &SkillExecutionEffectContext,
    outcome: &str,
    output_hash: Option<&str>,
    warnings: &[String],
    error: Option<&str>,
    completed_at: Option<&str>,
) -> Result<SkillExecutionEffect, String> {
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|database_error| format!("无法开始 Skill 执行效果事务：{database_error}"))?;
    let effect = record_skill_execution_effect_in_connection(
        &transaction,
        workspace_scope,
        &context.execution_id,
        &snapshot.id,
        snapshot.version,
        &context.request_id,
        context.task_id.as_deref(),
        &context.trace_id,
        &context.input_hash,
        output_hash,
        outcome,
        &context.started_at,
        completed_at,
        warnings,
        error,
    )?;
    transaction
        .commit()
        .map_err(|database_error| format!("无法提交 Skill 执行效果事务：{database_error}"))?;
    Ok(effect)
}

fn append_execution_failure_audit_best_effort(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    snapshot: &SkillExecutionSnapshot,
    trace_id: &str,
    context: Option<&SkillExecutionEffectContext>,
    detail: &str,
) {
    let completed_at = Utc::now().to_rfc3339();
    let outcome = execution_effect_outcome_for_error(detail);
    let event_type = if outcome == "cancelled" {
        "skill.execution.cancelled"
    } else {
        "skill.execution.failed"
    };
    let _ = append_execution_audit(
        database,
        workspace_scope,
        snapshot,
        trace_id,
        event_type,
        detail,
        &completed_at,
    );
    if let Some(context) = context {
        let _ = record_execution_effect(
            database,
            workspace_scope,
            snapshot,
            context,
            outcome,
            None,
            &[],
            Some(detail),
            Some(&completed_at),
        );
    }
}

fn execution_effect_outcome_for_error(error: &str) -> &'static str {
    let lower = error.to_ascii_lowercase();
    if error.contains("取消") || lower.contains("cancelled") || lower.contains("canceled") {
        "cancelled"
    } else {
        "failed"
    }
}

fn payload_string(payload: &Value, key: &str) -> String {
    payload
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn payload_capabilities(payload: &Value) -> Vec<String> {
    payload
        .get("capabilities")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn payload_optional_string(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn read_skill(
    connection: &Connection,
    workspace_scope: &str,
    skill_id: &str,
) -> Result<SkillRecord, String> {
    connection
        .query_row(
            "SELECT r.id, r.state, r.current_version, r.payload_hash, v.payload_json,
                    EXISTS(SELECT 1 FROM skill_evaluations e
                           WHERE e.workspace_scope=r.workspace_scope AND e.skill_id=r.id
                             AND e.version=r.current_version AND e.passed=1),
                    (SELECT a.decision FROM skill_approvals a
                     WHERE a.workspace_scope=r.workspace_scope AND a.skill_id=r.id
                       AND a.version=r.current_version ORDER BY a.decided_at DESC, a.id DESC LIMIT 1),
                    r.replacement_skill_id, v.rollback_of_version, r.trace_id,
                    r.created_at, r.updated_at
             FROM skill_registry r
             JOIN skill_versions v ON v.workspace_scope=r.workspace_scope
               AND v.skill_id=r.id AND v.version=r.current_version
             WHERE r.workspace_scope=?1 AND r.id=?2",
            params![workspace_scope, skill_id],
            |row| {
                let state: String = row.get(1)?;
                let evaluation_passed = row.get::<_, i64>(5)? != 0;
                let approval_state = row.get::<_, Option<String>>(6)?;
                let payload_json: String = row.get(4)?;
                let payload = serde_json::from_str::<Value>(&payload_json).unwrap_or(Value::Null);
                Ok(SkillRecord {
                    id: row.get(0)?,
                    name: payload_string(&payload, "name"),
                    description: payload_string(&payload, "description"),
                    instructions: payload_string(&payload, "instructions"),
                    input_schema: payload_string(&payload, "inputSchema"),
                    output_schema: payload_string(&payload, "outputSchema"),
                    capabilities: payload_capabilities(&payload),
                    source_url: payload_optional_string(&payload, "sourceUrl"),
                    source_hash: payload_optional_string(&payload, "sourceHash"),
                    source_kind: payload_optional_string(&payload, "sourceKind"),
                    source_revision: payload_optional_string(&payload, "sourceRevision"),
                    status: state.clone(),
                    version: row.get(2)?,
                    payload_hash: row.get(3)?,
                    evaluation_passed,
                    routing_eligible: state == "enabled"
                        && evaluation_passed
                        && approval_state.as_deref() == Some("approved"),
                    approval_state,
                    replacement_skill_id: row.get(7)?,
                    rollback_of_version: row.get(8)?,
                    trace_id: row.get(9)?,
                    created_at: row.get(10)?,
                    updated_at: row.get(11)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("无法读取 Skill 当前版本：{error}"))?
        .ok_or_else(|| "未找到 Skill".to_string())
}

pub(crate) fn list_skills_in_connection(
    connection: &Connection,
    workspace_scope: &str,
    routing_only: bool,
) -> Result<Vec<SkillRecord>, String> {
    let sql = if routing_only {
        "SELECT id FROM skill_registry r
         WHERE workspace_scope=?1 AND state='enabled'
           AND EXISTS(SELECT 1 FROM skill_evaluations e
                      WHERE e.workspace_scope=r.workspace_scope AND e.skill_id=r.id
                        AND e.version=r.current_version AND e.passed=1)
           AND (SELECT a.decision FROM skill_approvals a
                WHERE a.workspace_scope=r.workspace_scope AND a.skill_id=r.id
                  AND a.version=r.current_version ORDER BY a.decided_at DESC, a.id DESC LIMIT 1)='approved'
         ORDER BY updated_at DESC, id"
    } else {
        "SELECT id FROM skill_registry WHERE workspace_scope=?1 ORDER BY updated_at DESC, id"
    };
    let ids = {
        let mut statement = connection
            .prepare(sql)
            .map_err(|error| format!("无法准备 Skill 列表：{error}"))?;
        let ids = statement
            .query_map([workspace_scope], |row| row.get::<_, String>(0))
            .map_err(|error| format!("无法读取 Skill 列表：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析 Skill 列表：{error}"))?;
        ids
    };
    ids.iter()
        .map(|id| read_skill(connection, workspace_scope, id))
        .collect()
}

pub(crate) fn list_skills_for_workspace(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    routing_only: bool,
) -> Result<Vec<SkillRecord>, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    list_skills_in_connection(&connection, workspace_scope, routing_only)
}

fn save_draft(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    input: &SkillDraftInput,
) -> Result<SkillRecord, String> {
    save_draft_with_source(database, workspace_scope, input, None)
}

fn save_draft_with_source(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    input: &SkillDraftInput,
    source: Option<&SkillSourceEvidence>,
) -> Result<SkillRecord, String> {
    let payload = match source {
        Some(source) => draft_payload_with_source(input, Some(source))?,
        None => draft_payload(input)?,
    };
    let payload_hash = payload_hash(&payload)?;
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始 Skill 草稿事务：{error}"))?;
    let existing = transaction
        .query_row(
            "SELECT id, current_version, state, payload_hash, trace_id, created_at
             FROM skill_registry WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, input.id.trim()],
            |row| {
                Ok(CurrentSkill {
                    id: row.get(0)?,
                    version: row.get(1)?,
                    state: row.get(2)?,
                    payload_hash: row.get(3)?,
                    trace_id: row.get(4)?,
                    created_at: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(|error| format!("无法检查 Skill 草稿：{error}"))?;
    if let Some(current) = existing.as_ref() {
        if current.state == "retired" {
            return Err("已退役 Skill 不可修改，请创建替代 Skill".to_string());
        }
        if input.expected_version != Some(current.version) {
            return Err(format!(
                "Skill 版本已变化，当前版本为 {}，请刷新后重试",
                current.version
            ));
        }
        if current.payload_hash == payload_hash {
            transaction
                .commit()
                .map_err(|error| format!("无法完成 Skill 幂等保存：{error}"))?;
            drop(connection);
            return list_skills_for_workspace(database, workspace_scope, false)?
                .into_iter()
                .find(|skill| skill.id == input.id.trim())
                .ok_or_else(|| "无法读取幂等 Skill 保存结果".to_string());
        }
    } else if input.expected_version.is_some() {
        return Err("Skill 不存在，不能按既有版本更新".to_string());
    }
    let version = existing.as_ref().map_or(1, |current| current.version + 1);
    let now = Utc::now().to_rfc3339();
    let trace_id = checked_trace_id(input.trace_id.as_deref(), None)?;
    let created_at = existing
        .as_ref()
        .map(|current| current.created_at.as_str())
        .unwrap_or(&now);
    transaction
        .execute(
            "INSERT INTO skill_registry
             (workspace_scope, id, current_version, state, name, description, payload_hash,
              trace_id, replacement_skill_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'draft', ?4, ?5, ?6, ?7, NULL, ?8, ?9)
             ON CONFLICT(workspace_scope, id) DO UPDATE SET
               current_version=excluded.current_version, state='draft', name=excluded.name,
               description=excluded.description, payload_hash=excluded.payload_hash,
               trace_id=excluded.trace_id, replacement_skill_id=NULL, updated_at=excluded.updated_at",
            params![
                workspace_scope,
                input.id.trim(),
                version,
                input.name.trim(),
                input.description.trim(),
                payload_hash,
                trace_id,
                created_at,
                now
            ],
        )
        .map_err(|error| format!("无法保存 Skill 注册记录：{error}"))?;
    transaction
        .execute(
            "INSERT INTO skill_versions
             (workspace_scope, skill_id, version, payload_json, payload_hash,
              supersedes_version, rollback_of_version, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7)",
            params![
                workspace_scope,
                input.id.trim(),
                version,
                serde_json::to_string(&payload)
                    .map_err(|error| format!("无法序列化 Skill 版本：{error}"))?,
                payload_hash,
                existing.as_ref().map(|current| current.version),
                now
            ],
        )
        .map_err(|error| format!("无法保存 Skill 版本：{error}"))?;
    append_audit(
        &transaction,
        workspace_scope,
        input.id.trim(),
        version,
        &trace_id,
        if existing.is_some() {
            "skill.version.created"
        } else {
            "skill.draft.created"
        },
        existing.as_ref().map(|current| current.state.as_str()),
        "draft",
        "Skill 内容已保存为不可直接路由的草稿",
        &now,
    )?;
    if let Some(source) = source {
        append_audit(
            &transaction,
            workspace_scope,
            input.id.trim(),
            version,
            &trace_id,
            "skill.source.imported",
            Some("draft"),
            "draft",
            &format!(
                "source_url={};source_hash={};source_kind={};source_revision={};downloaded_files=1;capabilities=[];scripts_executed=false",
                source.source_url,
                source.source_hash,
                source.source_kind,
                source.source_revision
            ),
            &now,
        )?;
    }
    transaction
        .commit()
        .map_err(|error| format!("无法提交 Skill 草稿：{error}"))?;
    drop(connection);
    list_skills_for_workspace(database, workspace_scope, false)?
        .into_iter()
        .find(|skill| skill.id == input.id.trim())
        .ok_or_else(|| "无法读取 Skill 保存结果".to_string())
}

fn evaluation_checks(payload: &Value) -> Vec<SkillEvaluationCheck> {
    let instructions = payload_string(payload, "instructions");
    let capabilities = payload_capabilities(payload);
    let mut checks = Vec::new();
    let instruction_length = instructions.chars().count();
    checks.push(SkillEvaluationCheck {
        code: "instructions.length".to_string(),
        passed: (20..=32_000).contains(&instruction_length),
        detail: format!("Skill 指令长度为 {instruction_length} 个字符，要求 20 至 32000"),
    });
    for (key, label) in [("inputSchema", "输入"), ("outputSchema", "输出")] {
        let schema = payload_string(payload, key);
        let result = normalized_schema(&schema);
        checks.push(SkillEvaluationCheck {
            code: format!(
                "schema.{}",
                if key == "inputSchema" {
                    "input"
                } else {
                    "output"
                }
            ),
            passed: result.is_ok(),
            detail: result
                .map(|_| format!("{label} Schema 为空或为有效 JSON 对象"))
                .unwrap_or_else(|error| format!("{label} {error}")),
        });
    }
    let capabilities_valid = capabilities.len() <= ALLOWED_CAPABILITIES.len()
        && capabilities
            .iter()
            .all(|capability| ALLOWED_CAPABILITIES.contains(&capability.as_str()));
    checks.push(SkillEvaluationCheck {
        code: "capabilities.allowlist".to_string(),
        passed: capabilities_valid,
        detail: if capabilities_valid {
            "所有能力均在云枢本地策略允许声明的集合内".to_string()
        } else {
            "Skill 包含未知或重复的能力声明".to_string()
        },
    });
    checks.push(SkillEvaluationCheck {
        code: "permissions.default-deny".to_string(),
        passed: true,
        detail: "能力声明不等于授权，运行时仍执行默认拒绝策略".to_string(),
    });
    checks
}

fn evaluate_candidate(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    input: &SkillVersionInput,
) -> Result<SkillEvaluationResult, String> {
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始 Skill 评估事务：{error}"))?;
    let current = current_skill(&transaction, workspace_scope, input.skill_id.trim())?;
    if input
        .expected_version
        .is_some_and(|version| version != current.version)
    {
        return Err("Skill 评估版本已过期，请刷新后重试".to_string());
    }
    if !matches!(
        current.state.as_str(),
        "draft" | "candidate" | "rejected" | "disabled"
    ) {
        return Err("只有草稿、候选、停用或被拒绝的 Skill 可以重新评估".to_string());
    }
    let payload_json = transaction
        .query_row(
            "SELECT payload_json FROM skill_versions
             WHERE workspace_scope=?1 AND skill_id=?2 AND version=?3",
            params![workspace_scope, current.id, current.version],
            |row| row.get::<_, String>(0),
        )
        .map_err(|error| format!("无法读取待评估 Skill：{error}"))?;
    let payload = serde_json::from_str::<Value>(&payload_json)
        .map_err(|error| format!("Skill 版本负载损坏：{error}"))?;
    let checks = evaluation_checks(&payload);
    let passed = checks.iter().all(|check| check.passed);
    let target_state = if passed { "candidate" } else { "rejected" };
    let now = Utc::now().to_rfc3339();
    let trace_id = checked_trace_id(input.trace_id.as_deref(), Some(&current.trace_id))?;
    transaction
        .execute(
            "INSERT INTO skill_evaluations
             (id, workspace_scope, skill_id, version, evaluator_version, passed,
              checks_json, payload_hash, evaluated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(workspace_scope, skill_id, version, evaluator_version) DO UPDATE SET
               passed=excluded.passed, checks_json=excluded.checks_json,
               payload_hash=excluded.payload_hash, evaluated_at=excluded.evaluated_at",
            params![
                Uuid::new_v4().to_string(),
                workspace_scope,
                current.id,
                current.version,
                EVALUATOR_VERSION,
                i64::from(passed),
                serde_json::to_string(&checks)
                    .map_err(|error| format!("无法序列化 Skill 评估：{error}"))?,
                current.payload_hash,
                now
            ],
        )
        .map_err(|error| format!("无法保存 Skill 评估：{error}"))?;
    transaction
        .execute(
            "UPDATE skill_registry SET state=?3, updated_at=?4
             WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, current.id, target_state, now],
        )
        .map_err(|error| format!("无法更新 Skill 评估状态：{error}"))?;
    append_audit(
        &transaction,
        workspace_scope,
        &current.id,
        current.version,
        &trace_id,
        "skill.evaluated",
        Some(&current.state),
        target_state,
        if passed {
            "确定性评估通过"
        } else {
            "确定性评估未通过"
        },
        &now,
    )?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交 Skill 评估：{error}"))?;
    drop(connection);
    let skill = list_skills_for_workspace(database, workspace_scope, false)?
        .into_iter()
        .find(|skill| skill.id == input.skill_id.trim())
        .ok_or_else(|| "无法读取 Skill 评估结果".to_string())?;
    Ok(SkillEvaluationResult {
        skill_id: input.skill_id.trim().to_string(),
        version: current.version,
        evaluator_version: EVALUATOR_VERSION.to_string(),
        passed,
        checks,
        evaluated_at: now,
        skill,
    })
}

fn decide_candidate(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    input: &SkillApprovalInput,
) -> Result<SkillRecord, String> {
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始 Skill 审批事务：{error}"))?;
    let current = current_skill(&transaction, workspace_scope, input.skill_id.trim())?;
    if current.version != input.expected_version {
        return Err("Skill 审批版本已过期，请重新评估当前版本".to_string());
    }
    if current.state != "candidate" {
        return Err("只有通过评估的候选 Skill 可以审批".to_string());
    }
    let evaluation_passed = transaction
        .query_row(
            "SELECT passed FROM skill_evaluations
             WHERE workspace_scope=?1 AND skill_id=?2 AND version=?3
               AND evaluator_version=?4",
            params![
                workspace_scope,
                current.id,
                current.version,
                EVALUATOR_VERSION
            ],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|error| format!("无法读取 Skill 评估：{error}"))?
        == Some(1);
    if !evaluation_passed {
        return Err("Skill 当前版本没有通过确定性评估".to_string());
    }
    if input.note.chars().count() > 2_000 {
        return Err("Skill 审批说明超过 2000 个字符".to_string());
    }
    let decision = if input.approved {
        "approved"
    } else {
        "rejected"
    };
    let target_state = if input.approved {
        "candidate"
    } else {
        "rejected"
    };
    let now = Utc::now().to_rfc3339();
    let trace_id = checked_trace_id(input.trace_id.as_deref(), Some(&current.trace_id))?;
    transaction
        .execute(
            "INSERT INTO skill_approvals
             (id, workspace_scope, skill_id, version, decision, actor, note, decided_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'user', ?6, ?7)",
            params![
                Uuid::new_v4().to_string(),
                workspace_scope,
                current.id,
                current.version,
                decision,
                input.note.trim(),
                now
            ],
        )
        .map_err(|error| format!("无法保存 Skill 用户审批：{error}"))?;
    transaction
        .execute(
            "UPDATE skill_registry SET state=?3, updated_at=?4
             WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, current.id, target_state, now],
        )
        .map_err(|error| format!("无法更新 Skill 审批状态：{error}"))?;
    append_audit(
        &transaction,
        workspace_scope,
        &current.id,
        current.version,
        &trace_id,
        "skill.user_decided",
        Some(&current.state),
        target_state,
        if input.approved {
            "用户明确批准候选 Skill"
        } else {
            "用户拒绝候选 Skill"
        },
        &now,
    )?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交 Skill 审批：{error}"))?;
    drop(connection);
    list_skills_for_workspace(database, workspace_scope, false)?
        .into_iter()
        .find(|skill| skill.id == input.skill_id.trim())
        .ok_or_else(|| "无法读取 Skill 审批结果".to_string())
}

fn change_activation(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    input: &SkillActivationInput,
) -> Result<SkillRecord, String> {
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始 Skill 启停事务：{error}"))?;
    let current = current_skill(&transaction, workspace_scope, input.skill_id.trim())?;
    if current.version != input.expected_version {
        return Err("Skill 启停版本已过期，请刷新后重试".to_string());
    }
    let (target_state, detail) = match input.action {
        SkillActivationAction::Enable => {
            if !matches!(current.state.as_str(), "candidate" | "disabled") {
                return Err("只有已批准候选或已停用 Skill 可以启用".to_string());
            }
            let passed = transaction
                .query_row(
                    "SELECT passed FROM skill_evaluations
                     WHERE workspace_scope=?1 AND skill_id=?2 AND version=?3
                       AND evaluator_version=?4",
                    params![
                        workspace_scope,
                        current.id,
                        current.version,
                        EVALUATOR_VERSION
                    ],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(|error| format!("无法校验 Skill 启用评估：{error}"))?
                == Some(1);
            let approval = transaction
                .query_row(
                    "SELECT decision FROM skill_approvals
                     WHERE workspace_scope=?1 AND skill_id=?2 AND version=?3
                     ORDER BY decided_at DESC, id DESC LIMIT 1",
                    params![workspace_scope, current.id, current.version],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|error| format!("无法校验 Skill 启用审批：{error}"))?;
            if !passed || approval.as_deref() != Some("approved") {
                return Err("Skill 启用需要当前版本通过确定性评估并获得用户明确批准".to_string());
            }
            ("enabled", "Skill 已启用并进入可路由集合")
        }
        SkillActivationAction::Disable => {
            if current.state != "enabled" {
                return Err("只有已启用 Skill 可以停用".to_string());
            }
            ("disabled", "Skill 已停用并从路由集合移除")
        }
    };
    let now = Utc::now().to_rfc3339();
    let trace_id = checked_trace_id(input.trace_id.as_deref(), Some(&current.trace_id))?;
    transaction
        .execute(
            "UPDATE skill_registry SET state=?3, updated_at=?4
             WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, current.id, target_state, now],
        )
        .map_err(|error| format!("无法更新 Skill 启停状态：{error}"))?;
    append_audit(
        &transaction,
        workspace_scope,
        &current.id,
        current.version,
        &trace_id,
        if target_state == "enabled" {
            "skill.enabled"
        } else {
            "skill.disabled"
        },
        Some(&current.state),
        target_state,
        detail,
        &now,
    )?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交 Skill 启停：{error}"))?;
    drop(connection);
    list_skills_for_workspace(database, workspace_scope, false)?
        .into_iter()
        .find(|skill| skill.id == input.skill_id.trim())
        .ok_or_else(|| "无法读取 Skill 启停结果".to_string())
}

fn finalize_confirmed_import(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    saved: &SkillRecord,
    trace_id: &str,
) -> Result<SkillRecord, String> {
    let evaluation = evaluate_candidate(
        database,
        workspace_scope,
        &SkillVersionInput {
            skill_id: saved.id.clone(),
            expected_version: Some(saved.version),
            trace_id: Some(trace_id.to_string()),
        },
    )?;
    if !evaluation.passed {
        return Ok(evaluation.skill);
    }
    decide_candidate(
        database,
        workspace_scope,
        &SkillApprovalInput {
            skill_id: saved.id.clone(),
            expected_version: evaluation.version,
            approved: true,
            note: "用户已确认安装；确定性评估通过后默认批准".to_string(),
            trace_id: Some(trace_id.to_string()),
        },
    )?;
    change_activation(
        database,
        workspace_scope,
        &SkillActivationInput {
            skill_id: saved.id.clone(),
            expected_version: evaluation.version,
            action: SkillActivationAction::Enable,
            trace_id: Some(trace_id.to_string()),
        },
    )
}

fn retire_skill_record(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    input: &SkillRetirementInput,
) -> Result<SkillRecord, String> {
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始 Skill 退役事务：{error}"))?;
    let current = current_skill(&transaction, workspace_scope, input.skill_id.trim())?;
    if current.version != input.expected_version {
        return Err("Skill 退役版本已过期，请刷新后重试".to_string());
    }
    if current.state == "retired" {
        return Err("Skill 已经退役".to_string());
    }
    let replacement = input
        .replacement_skill_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if replacement == Some(current.id.as_str()) {
        return Err("Skill 不能替代自身".to_string());
    }
    if let Some(replacement) = replacement {
        let replacement_state = transaction
            .query_row(
                "SELECT state FROM skill_registry WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, replacement],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| format!("无法校验替代 Skill：{error}"))?;
        if replacement_state.as_deref() != Some("enabled") {
            return Err("替代 Skill 必须存在且处于已启用状态".to_string());
        }
    }
    let now = Utc::now().to_rfc3339();
    let trace_id = checked_trace_id(input.trace_id.as_deref(), Some(&current.trace_id))?;
    transaction
        .execute(
            "UPDATE skill_registry SET state='retired', replacement_skill_id=?3, updated_at=?4
             WHERE workspace_scope=?1 AND id=?2",
            params![workspace_scope, current.id, replacement, now],
        )
        .map_err(|error| format!("无法退役 Skill：{error}"))?;
    append_audit(
        &transaction,
        workspace_scope,
        &current.id,
        current.version,
        &trace_id,
        "skill.retired",
        Some(&current.state),
        "retired",
        if input.reason.trim().is_empty() {
            "用户退役 Skill"
        } else {
            input.reason.trim()
        },
        &now,
    )?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交 Skill 退役：{error}"))?;
    drop(connection);
    list_skills_for_workspace(database, workspace_scope, false)?
        .into_iter()
        .find(|skill| skill.id == input.skill_id.trim())
        .ok_or_else(|| "无法读取 Skill 退役结果".to_string())
}

fn rollback_skill_record(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    input: &SkillRollbackInput,
) -> Result<SkillRecord, String> {
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始 Skill 回滚事务：{error}"))?;
    let current = current_skill(&transaction, workspace_scope, input.skill_id.trim())?;
    if current.version != input.expected_version {
        return Err("Skill 回滚版本已过期，请刷新后重试".to_string());
    }
    if current.state == "retired" {
        return Err("已退役 Skill 不可恢复路由，请创建替代 Skill".to_string());
    }
    if input.target_version <= 0 || input.target_version >= current.version {
        return Err("回滚目标必须是当前版本之前的有效版本".to_string());
    }
    let (payload_json, payload_hash) = transaction
        .query_row(
            "SELECT payload_json, payload_hash FROM skill_versions
             WHERE workspace_scope=?1 AND skill_id=?2 AND version=?3",
            params![workspace_scope, current.id, input.target_version],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|error| format!("无法读取 Skill 回滚版本：{error}"))?
        .ok_or_else(|| "未找到 Skill 回滚目标版本".to_string())?;
    let payload = serde_json::from_str::<Value>(&payload_json)
        .map_err(|error| format!("Skill 回滚版本损坏：{error}"))?;
    let version = current.version + 1;
    let now = Utc::now().to_rfc3339();
    let trace_id = checked_trace_id(input.trace_id.as_deref(), None)?;
    transaction
        .execute(
            "INSERT INTO skill_versions
             (workspace_scope, skill_id, version, payload_json, payload_hash,
              supersedes_version, rollback_of_version, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                workspace_scope,
                current.id,
                version,
                payload_json,
                payload_hash,
                current.version,
                input.target_version,
                now
            ],
        )
        .map_err(|error| format!("无法保存 Skill 回滚版本：{error}"))?;
    transaction
        .execute(
            "UPDATE skill_registry SET current_version=?3, state='draft', name=?4,
                    description=?5, payload_hash=?6, trace_id=?7,
                    replacement_skill_id=NULL, updated_at=?8
             WHERE workspace_scope=?1 AND id=?2",
            params![
                workspace_scope,
                current.id,
                version,
                payload_string(&payload, "name"),
                payload_string(&payload, "description"),
                payload_hash,
                trace_id,
                now
            ],
        )
        .map_err(|error| format!("无法应用 Skill 回滚版本：{error}"))?;
    append_audit(
        &transaction,
        workspace_scope,
        &current.id,
        version,
        &trace_id,
        "skill.rolled_back",
        Some(&current.state),
        "draft",
        &format!(
            "由版本 {} 回滚生成，必须重新评估和审批",
            input.target_version
        ),
        &now,
    )?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交 Skill 回滚：{error}"))?;
    drop(connection);
    list_skills_for_workspace(database, workspace_scope, false)?
        .into_iter()
        .find(|skill| skill.id == input.skill_id.trim())
        .ok_or_else(|| "无法读取 Skill 回滚结果".to_string())
}

fn list_versions(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    skill_id: &str,
) -> Result<Vec<SkillVersionRecord>, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let mut statement = connection
        .prepare(
            "SELECT skill_id, version, payload_json, payload_hash,
                    supersedes_version, rollback_of_version, created_at
             FROM skill_versions WHERE workspace_scope=?1 AND skill_id=?2
             ORDER BY version DESC",
        )
        .map_err(|error| format!("无法准备 Skill 版本历史：{error}"))?;
    let rows = statement
        .query_map(params![workspace_scope, skill_id.trim()], |row| {
            let payload_json: String = row.get(2)?;
            Ok(SkillVersionRecord {
                skill_id: row.get(0)?,
                version: row.get(1)?,
                payload: serde_json::from_str(&payload_json).unwrap_or(Value::Null),
                payload_hash: row.get(3)?,
                supersedes_version: row.get(4)?,
                rollback_of_version: row.get(5)?,
                created_at: row.get(6)?,
            })
        })
        .map_err(|error| format!("无法读取 Skill 版本历史：{error}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法解析 Skill 版本历史：{error}"))
}

pub(crate) fn migrate_legacy_skills(connection: &Connection) -> Result<(), String> {
    let legacy_rows = {
        let mut statement = connection
            .prepare(
                "SELECT workspace_scope, id, payload, created_at, updated_at
                 FROM managed_resources
                 WHERE resource_type='user_skill' AND state='active'
                 ORDER BY created_at, id",
            )
            .map_err(|error| format!("无法准备旧 Skill 迁移：{error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .map_err(|error| format!("无法读取旧 Skill：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析旧 Skill：{error}"))?;
        rows
    };
    for (workspace_scope, id, payload_json, created_at, updated_at) in legacy_rows {
        if connection
            .query_row(
                "SELECT 1 FROM skill_registry WHERE workspace_scope=?1 AND id=?2",
                params![workspace_scope, id],
                |_| Ok(()),
            )
            .optional()
            .map_err(|error| format!("无法检查旧 Skill 迁移状态：{error}"))?
            .is_some()
        {
            continue;
        }
        let Ok(payload) = serde_json::from_str::<Value>(&payload_json) else {
            log::warn!("跳过损坏的旧 Skill：{id}");
            continue;
        };
        if !valid_skill_id(&id) {
            log::warn!("跳过无效 ID 的旧 Skill：{id}");
            continue;
        }
        let name = payload_string(&payload, "name");
        let instructions = payload_string(&payload, "instructions");
        if name.trim().is_empty() || instructions.trim().is_empty() {
            log::warn!("跳过缺少名称或指令的旧 Skill：{id}");
            continue;
        }
        let hash = payload_hash(&payload)?;
        let trace_id = format!("trace-legacy-skill-{}", &hash[7..39]);
        let legacy_state = payload
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("disabled");
        connection
            .execute(
                "INSERT INTO skill_registry
                 (workspace_scope, id, current_version, state, name, description, payload_hash,
                  trace_id, replacement_skill_id, created_at, updated_at)
                 VALUES (?1, ?2, 1, 'disabled', ?3, ?4, ?5, ?6, NULL, ?7, ?8)",
                params![
                    workspace_scope,
                    id,
                    name,
                    payload_string(&payload, "description"),
                    hash,
                    trace_id,
                    created_at,
                    updated_at
                ],
            )
            .map_err(|error| format!("无法迁移旧 Skill 注册记录：{error}"))?;
        connection
            .execute(
                "INSERT INTO skill_versions
                 (workspace_scope, skill_id, version, payload_json, payload_hash,
                  supersedes_version, rollback_of_version, created_at)
                 VALUES (?1, ?2, 1, ?3, ?4, NULL, NULL, ?5)",
                params![workspace_scope, id, payload_json, hash, created_at],
            )
            .map_err(|error| format!("无法迁移旧 Skill 版本：{error}"))?;
        append_audit(
            connection,
            &workspace_scope,
            &id,
            1,
            &trace_id,
            "skill.legacy_imported",
            Some(legacy_state),
            "disabled",
            "旧版 Skill 已保留内容；重新启用前必须完成确定性评估和用户批准",
            &updated_at,
        )?;
    }
    Ok(())
}

const SKILL_CREATE_RUNTIME_OPERATIONS: &[&str] = &["create"];
const SKILL_CREATE_OR_UPDATE_RUNTIME_OPERATIONS: &[&str] = &["create", "update"];
const SKILL_UPDATE_RUNTIME_OPERATIONS: &[&str] = &["update"];

fn validate_skill_governance_runtime_handler(
    database: &RuntimeDatabase,
    ticket_state: &ExecutionTicketState,
    workspace_scope: &str,
    operation_context: Option<&OperationContext>,
    allowed_operations: &[&str],
) -> Result<Option<String>, String> {
    let Some(operation_context) = operation_context else {
        return Ok(None);
    };
    let mut last_error = None;
    for operation in allowed_operations {
        match database.validate_runtime_effectful_handler(
            ticket_state,
            workspace_scope,
            operation_context,
            "system:skills",
            operation,
        ) {
            Ok(_) => return Ok(Some((*operation).to_string())),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| "Skill 治理处理器没有允许的 Runtime 操作".to_string()))
}

fn record_skill_governance_runtime_completion(
    database: &RuntimeDatabase,
    ticket_state: &ExecutionTicketState,
    workspace_scope: &str,
    operation_context: Option<&OperationContext>,
    operation: Option<&str>,
    elapsed: Duration,
) -> Result<(), String> {
    let (Some(operation_context), Some(operation)) = (operation_context, operation) else {
        return Ok(());
    };
    database.record_runtime_effectful_handler_completion(
        ticket_state,
        workspace_scope,
        operation_context,
        "system:skills",
        operation,
        TrustedHandlerUsage {
            tool_calls: 1,
            runtime_seconds: elapsed.as_secs().max(1),
            tokens: 0,
            cost: Some(0.0),
        },
    )
}

#[tauri::command]
pub async fn install_skill_from_github(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    input: SkillInstallInput,
    operation_context: Option<OperationContext>,
) -> Result<SkillRecord, String> {
    let handler_started = Instant::now();
    let workspace_scope = database.local_workspace_scope()?;
    let runtime_operation = validate_skill_governance_runtime_handler(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        operation_context.as_ref(),
        SKILL_CREATE_RUNTIME_OPERATIONS,
    )?;
    if !input.user_confirmed {
        return Err("安装第三方 Skill 必须由用户在当前 AI助手任务中明确确认".to_string());
    }
    let normalized = normalize_github_skill_url(&input.source_url)?;
    let trace_id = checked_trace_id(input.trace_id.as_deref(), None)?;
    let bytes = download_github_skill_manifest(&normalized).await?;
    let source_hash = format!("sha256:{:x}", Sha256::digest(&bytes));
    let content = String::from_utf8(bytes)
        .map_err(|_| "第三方 SKILL.md 必须是有效 UTF-8 文本".to_string())?;
    let manifest = parse_imported_skill_manifest(&content)?;
    if !valid_skill_id(manifest.name.trim()) {
        return Err(
            "第三方 SKILL.md 的 name 必须以小写字母开头且只包含小写字母、数字和连字符".to_string(),
        );
    }
    let source = SkillSourceEvidence {
        source_url: normalized.fetch_url,
        source_hash,
        source_kind: GITHUB_SKILL_SOURCE_KIND.to_string(),
        source_revision: normalized.source_revision,
    };
    let draft = SkillDraftInput {
        id: manifest.name.clone(),
        expected_version: None,
        name: manifest.name,
        description: manifest.description,
        instructions: manifest.instructions,
        input_schema: String::new(),
        output_schema: String::new(),
        capabilities: Vec::new(),
        trace_id: Some(trace_id.clone()),
    };
    let saved = save_draft_with_source(database.inner(), &workspace_scope, &draft, Some(&source))?;
    let result = finalize_confirmed_import(database.inner(), &workspace_scope, &saved, &trace_id)?;
    record_skill_governance_runtime_completion(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        operation_context.as_ref(),
        runtime_operation.as_deref(),
        handler_started.elapsed(),
    )?;
    Ok(result)
}

#[tauri::command]
pub fn save_skill_draft(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    input: SkillDraftInput,
    operation_context: Option<OperationContext>,
) -> Result<SkillRecord, String> {
    let handler_started = Instant::now();
    let workspace_scope = database.local_workspace_scope()?;
    let runtime_operation = validate_skill_governance_runtime_handler(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        operation_context.as_ref(),
        SKILL_CREATE_OR_UPDATE_RUNTIME_OPERATIONS,
    )?;
    let result = save_draft(database.inner(), &workspace_scope, &input)?;
    record_skill_governance_runtime_completion(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        operation_context.as_ref(),
        runtime_operation.as_deref(),
        handler_started.elapsed(),
    )?;
    Ok(result)
}

#[tauri::command]
pub fn list_user_skills(database: State<'_, RuntimeDatabase>) -> Result<Vec<SkillRecord>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    list_skills_for_workspace(database.inner(), &workspace_scope, false)
}

#[tauri::command]
pub fn list_routable_skills(
    database: State<'_, RuntimeDatabase>,
) -> Result<Vec<SkillRecord>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    list_skills_for_workspace(database.inner(), &workspace_scope, true)
}

#[tauri::command]
pub fn evaluate_skill_candidate(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    input: SkillVersionInput,
    operation_context: Option<OperationContext>,
) -> Result<SkillEvaluationResult, String> {
    let handler_started = Instant::now();
    let workspace_scope = database.local_workspace_scope()?;
    let runtime_operation = validate_skill_governance_runtime_handler(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        operation_context.as_ref(),
        SKILL_CREATE_OR_UPDATE_RUNTIME_OPERATIONS,
    )?;
    let result = evaluate_candidate(database.inner(), &workspace_scope, &input)?;
    record_skill_governance_runtime_completion(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        operation_context.as_ref(),
        runtime_operation.as_deref(),
        handler_started.elapsed(),
    )?;
    Ok(result)
}

#[tauri::command]
pub fn decide_skill_candidate(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    input: SkillApprovalInput,
    operation_context: Option<OperationContext>,
) -> Result<SkillRecord, String> {
    let handler_started = Instant::now();
    let workspace_scope = database.local_workspace_scope()?;
    let runtime_operation = validate_skill_governance_runtime_handler(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        operation_context.as_ref(),
        SKILL_UPDATE_RUNTIME_OPERATIONS,
    )?;
    let result = decide_candidate(database.inner(), &workspace_scope, &input)?;
    record_skill_governance_runtime_completion(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        operation_context.as_ref(),
        runtime_operation.as_deref(),
        handler_started.elapsed(),
    )?;
    Ok(result)
}

#[tauri::command]
pub fn change_skill_activation(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    input: SkillActivationInput,
    operation_context: Option<OperationContext>,
) -> Result<SkillRecord, String> {
    let handler_started = Instant::now();
    let workspace_scope = database.local_workspace_scope()?;
    let runtime_operation = validate_skill_governance_runtime_handler(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        operation_context.as_ref(),
        SKILL_UPDATE_RUNTIME_OPERATIONS,
    )?;
    let result = change_activation(database.inner(), &workspace_scope, &input)?;
    record_skill_governance_runtime_completion(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        operation_context.as_ref(),
        runtime_operation.as_deref(),
        handler_started.elapsed(),
    )?;
    Ok(result)
}

#[tauri::command]
pub fn retire_skill(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    input: SkillRetirementInput,
    operation_context: Option<OperationContext>,
) -> Result<SkillRecord, String> {
    let handler_started = Instant::now();
    let workspace_scope = database.local_workspace_scope()?;
    let runtime_operation = validate_skill_governance_runtime_handler(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        operation_context.as_ref(),
        SKILL_UPDATE_RUNTIME_OPERATIONS,
    )?;
    let result = retire_skill_record(database.inner(), &workspace_scope, &input)?;
    record_skill_governance_runtime_completion(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        operation_context.as_ref(),
        runtime_operation.as_deref(),
        handler_started.elapsed(),
    )?;
    Ok(result)
}

#[tauri::command]
pub fn rollback_skill(
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    input: SkillRollbackInput,
    operation_context: Option<OperationContext>,
) -> Result<SkillRecord, String> {
    let handler_started = Instant::now();
    let workspace_scope = database.local_workspace_scope()?;
    let runtime_operation = validate_skill_governance_runtime_handler(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        operation_context.as_ref(),
        SKILL_UPDATE_RUNTIME_OPERATIONS,
    )?;
    let result = rollback_skill_record(database.inner(), &workspace_scope, &input)?;
    record_skill_governance_runtime_completion(
        database.inner(),
        ticket_state.inner(),
        &workspace_scope,
        operation_context.as_ref(),
        runtime_operation.as_deref(),
        handler_started.elapsed(),
    )?;
    Ok(result)
}

#[tauri::command]
pub fn list_skill_versions(
    database: State<'_, RuntimeDatabase>,
    skill_id: String,
) -> Result<Vec<SkillVersionRecord>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    list_versions(database.inner(), &workspace_scope, &skill_id)
}

#[tauri::command]
pub fn list_skill_execution_effects(
    database: State<'_, RuntimeDatabase>,
    query: SkillExecutionEffectQuery,
) -> Result<Vec<SkillExecutionEffect>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    list_skill_execution_effects_in_connection(&connection, &workspace_scope, &query)
}

#[tauri::command]
pub fn record_skill_execution_effect_feedback(
    database: State<'_, RuntimeDatabase>,
    input: SkillExecutionEffectFeedbackInput,
) -> Result<SkillExecutionEffect, String> {
    let workspace_scope = database.local_workspace_scope()?;
    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始 Skill 执行反馈事务：{error}"))?;
    let effect = record_skill_execution_effect_feedback_in_connection(
        &transaction,
        &workspace_scope,
        &input,
    )?;
    transaction
        .commit()
        .map_err(|error| format!("无法提交 Skill 执行反馈事务：{error}"))?;
    Ok(effect)
}

#[tauri::command]
pub async fn execute_skill(
    app: AppHandle,
    request_state: State<'_, ModelRequestState>,
    database: State<'_, RuntimeDatabase>,
    ticket_state: State<'_, ExecutionTicketState>,
    input: SkillExecutionInput,
) -> Result<SkillExecutionResult, String> {
    let handler_started = Instant::now();
    let workspace_scope = database.local_workspace_scope()?;
    if let Some(context) = input.operation_context.as_ref() {
        database.validate_runtime_effectful_handler(
            ticket_state.inner(),
            &workspace_scope,
            context,
            "system:skills",
            "run",
        )?;
    }
    let snapshot = {
        let connection = database
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;
        execution_snapshot_in_connection(
            &connection,
            &workspace_scope,
            &input.skill_id,
            input.expected_version,
            &input.expected_payload_hash,
        )?
    };
    let trace_id = checked_trace_id(input.trace_id.as_deref(), None)?;
    let request_id = input
        .request_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let task_id = input
        .task_id
        .as_deref()
        .map(|value| normalized_effect_query_string(value, "taskId"))
        .transpose()?;
    let input_hash = value_hash(&input.input)?;
    let started_at = Utc::now().to_rfc3339();
    let effect_context = SkillExecutionEffectContext {
        execution_id: Uuid::new_v4().to_string(),
        request_id: request_id.clone(),
        task_id,
        trace_id: trace_id.clone(),
        input_hash: input_hash.clone(),
        started_at: started_at.clone(),
    };
    append_execution_audit(
        database.inner(),
        &workspace_scope,
        &snapshot,
        &trace_id,
        "skill.execution.started",
        &format!("受控 Skill 执行已开始；request_id={request_id}；input_hash={input_hash}"),
        &started_at,
    )?;
    record_execution_effect(
        database.inner(),
        &workspace_scope,
        &snapshot,
        &effect_context,
        "started",
        None,
        &[],
        None,
        None,
    )?;

    let input_bytes = serde_json::to_vec(&input.input)
        .map_err(|error| format!("无法序列化 Skill 用户输入：{error}"))?;
    if input_bytes.len() > MAX_SKILL_EXECUTION_INPUT_BYTES {
        let error = "Skill 用户输入超过 512 KB 安全上限".to_string();
        append_execution_failure_audit_best_effort(
            database.inner(),
            &workspace_scope,
            &snapshot,
            &trace_id,
            Some(&effect_context),
            &error,
        );
        return Err(error);
    }
    let input_schema = match parsed_schema(&snapshot.input_schema, "输入") {
        Ok(schema) => schema,
        Err(error) => {
            append_execution_failure_audit_best_effort(
                database.inner(),
                &workspace_scope,
                &snapshot,
                &trace_id,
                Some(&effect_context),
                &error,
            );
            return Err(error);
        }
    };
    if let Err(error) = validate_declared_schema(&input.input, input_schema.as_ref(), "input") {
        append_execution_failure_audit_best_effort(
            database.inner(),
            &workspace_scope,
            &snapshot,
            &trace_id,
            Some(&effect_context),
            &error,
        );
        return Err(error);
    }
    let output_schema = match parsed_schema(&snapshot.output_schema, "输出") {
        Ok(schema) => schema,
        Err(error) => {
            append_execution_failure_audit_best_effort(
                database.inner(),
                &workspace_scope,
                &snapshot,
                &trace_id,
                Some(&effect_context),
                &error,
            );
            return Err(error);
        }
    };
    let model_result = model_provider::execute_approved_skill_model(
        &app,
        request_state.inner(),
        database.inner(),
        &workspace_scope,
        ApprovedSkillModelInput {
            skill_id: snapshot.id.clone(),
            skill_name: snapshot.name.clone(),
            version: snapshot.version,
            payload_hash: snapshot.payload_hash.clone(),
            instructions: snapshot.instructions.clone(),
            input_schema: snapshot.input_schema.clone(),
            output_schema: snapshot.output_schema.clone(),
            declared_capabilities: snapshot.declared_capabilities.clone(),
            user_input: input.input,
            request_id: Some(request_id.clone()),
            trace_id: trace_id.clone(),
        },
    )
    .await;
    let model_result = match model_result {
        Ok(result) => result,
        Err(error) => {
            append_execution_failure_audit_best_effort(
                database.inner(),
                &workspace_scope,
                &snapshot,
                &trace_id,
                Some(&effect_context),
                &error,
            );
            return Err(error);
        }
    };
    if let Err(error) = validate_declared_schema(
        &model_result.output_data,
        output_schema.as_ref(),
        "outputData",
    ) {
        append_execution_failure_audit_best_effort(
            database.inner(),
            &workspace_scope,
            &snapshot,
            &trace_id,
            Some(&effect_context),
            &error,
        );
        return Err(error);
    }
    let output_bytes = serde_json::to_vec(&model_result.output_data)
        .map_err(|error| format!("无法序列化 Skill 输出：{error}"))?;
    if output_bytes.len() > MAX_SKILL_EXECUTION_INPUT_BYTES {
        let error = "Skill outputData 超过 512 KB 安全上限".to_string();
        append_execution_failure_audit_best_effort(
            database.inner(),
            &workspace_scope,
            &snapshot,
            &trace_id,
            Some(&effect_context),
            &error,
        );
        return Err(error);
    }
    let completed_at = Utc::now().to_rfc3339();
    let output_hash = value_hash(&model_result.output_data)?;
    let detail = format!(
        "受控 Skill 执行已完成；request_id={}；output_hash={}；warnings={}",
        model_result.request_id,
        output_hash,
        model_result.warnings.len()
    );
    if let Err(error) = append_checked_execution_audit(
        database.inner(),
        &workspace_scope,
        &snapshot,
        &trace_id,
        "skill.execution.succeeded",
        &detail,
        &completed_at,
    ) {
        append_execution_failure_audit_best_effort(
            database.inner(),
            &workspace_scope,
            &snapshot,
            &trace_id,
            Some(&effect_context),
            &error,
        );
        return Err(error);
    }
    if let Err(error) = record_execution_effect(
        database.inner(),
        &workspace_scope,
        &snapshot,
        &effect_context,
        "succeeded",
        Some(&output_hash),
        &model_result.warnings,
        None,
        Some(&completed_at),
    ) {
        append_execution_failure_audit_best_effort(
            database.inner(),
            &workspace_scope,
            &snapshot,
            &trace_id,
            Some(&effect_context),
            &error,
        );
        return Err(error);
    }
    if let Some(context) = input.operation_context.as_ref() {
        if let Err(error) = database.record_runtime_effectful_handler_completion(
            ticket_state.inner(),
            &workspace_scope,
            context,
            "system:skills",
            "run",
            model_result
                .usage
                .trusted_handler_usage(handler_started.elapsed()),
        ) {
            append_execution_failure_audit_best_effort(
                database.inner(),
                &workspace_scope,
                &snapshot,
                &trace_id,
                Some(&effect_context),
                &error,
            );
            return Err(error);
        }
    }
    Ok(SkillExecutionResult {
        output_text: model_result.output_text,
        output_data: model_result.output_data,
        warnings: model_result.warnings,
        skill: SkillExecutionIdentity {
            id: snapshot.id,
            name: snapshot.name,
            version: snapshot.version,
            payload_hash: snapshot.payload_hash,
        },
        trace: SkillExecutionTrace {
            trace_id,
            request_id: model_result.request_id,
            started_at,
            completed_at,
        },
        model: SkillExecutionModel {
            provider: model_result.provider,
            model: model_result.model,
        },
        usage: model_result.usage,
    })
}
