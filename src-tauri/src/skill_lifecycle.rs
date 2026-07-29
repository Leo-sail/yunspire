use crate::{
    runtime_db::RuntimeDatabase,
    trace::{self, TraceEventRecord},
};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use tauri::State;
use uuid::Uuid;

const EVALUATOR_VERSION: &str = "yunspire-deterministic-v1";
const MAX_SKILL_PAYLOAD_BYTES: usize = 512 * 1024;
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
    status: String,
    version: i64,
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

fn draft_payload(input: &SkillDraftInput) -> Result<Value, String> {
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
    let payload = serde_json::json!({
        "id": id,
        "name": name,
        "description": description,
        "instructions": instructions,
        "inputSchema": input.input_schema.trim(),
        "outputSchema": input.output_schema.trim(),
        "capabilities": normalize_capabilities(&input.capabilities)?,
    });
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

fn read_skill(
    connection: &Connection,
    workspace_scope: &str,
    skill_id: &str,
) -> Result<SkillRecord, String> {
    connection
        .query_row(
            "SELECT r.id, r.state, r.current_version, v.payload_json,
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
                let evaluation_passed = row.get::<_, i64>(4)? != 0;
                let approval_state = row.get::<_, Option<String>>(5)?;
                let payload_json: String = row.get(3)?;
                let payload = serde_json::from_str::<Value>(&payload_json).unwrap_or(Value::Null);
                Ok(SkillRecord {
                    id: row.get(0)?,
                    name: payload_string(&payload, "name"),
                    description: payload_string(&payload, "description"),
                    instructions: payload_string(&payload, "instructions"),
                    input_schema: payload_string(&payload, "inputSchema"),
                    output_schema: payload_string(&payload, "outputSchema"),
                    capabilities: payload_capabilities(&payload),
                    status: state.clone(),
                    version: row.get(2)?,
                    evaluation_passed,
                    routing_eligible: state == "enabled"
                        && evaluation_passed
                        && approval_state.as_deref() == Some("approved"),
                    approval_state,
                    replacement_skill_id: row.get(6)?,
                    rollback_of_version: row.get(7)?,
                    trace_id: row.get(8)?,
                    created_at: row.get(9)?,
                    updated_at: row.get(10)?,
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
    let payload = draft_payload(input)?;
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

#[tauri::command]
pub fn save_skill_draft(
    database: State<'_, RuntimeDatabase>,
    input: SkillDraftInput,
) -> Result<SkillRecord, String> {
    let workspace_scope = database.local_workspace_scope()?;
    save_draft(database.inner(), &workspace_scope, &input)
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
    input: SkillVersionInput,
) -> Result<SkillEvaluationResult, String> {
    let workspace_scope = database.local_workspace_scope()?;
    evaluate_candidate(database.inner(), &workspace_scope, &input)
}

#[tauri::command]
pub fn decide_skill_candidate(
    database: State<'_, RuntimeDatabase>,
    input: SkillApprovalInput,
) -> Result<SkillRecord, String> {
    let workspace_scope = database.local_workspace_scope()?;
    decide_candidate(database.inner(), &workspace_scope, &input)
}

#[tauri::command]
pub fn change_skill_activation(
    database: State<'_, RuntimeDatabase>,
    input: SkillActivationInput,
) -> Result<SkillRecord, String> {
    let workspace_scope = database.local_workspace_scope()?;
    change_activation(database.inner(), &workspace_scope, &input)
}

#[tauri::command]
pub fn retire_skill(
    database: State<'_, RuntimeDatabase>,
    input: SkillRetirementInput,
) -> Result<SkillRecord, String> {
    let workspace_scope = database.local_workspace_scope()?;
    retire_skill_record(database.inner(), &workspace_scope, &input)
}

#[tauri::command]
pub fn rollback_skill(
    database: State<'_, RuntimeDatabase>,
    input: SkillRollbackInput,
) -> Result<SkillRecord, String> {
    let workspace_scope = database.local_workspace_scope()?;
    rollback_skill_record(database.inner(), &workspace_scope, &input)
}

#[tauri::command]
pub fn list_skill_versions(
    database: State<'_, RuntimeDatabase>,
    skill_id: String,
) -> Result<Vec<SkillVersionRecord>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    list_versions(database.inner(), &workspace_scope, &skill_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> (tempfile::TempDir, RuntimeDatabase) {
        let directory = tempfile::tempdir().expect("temp directory");
        let database = RuntimeDatabase::open_test(&directory.path().join("runtime.sqlite"))
            .expect("open database");
        (directory, database)
    }

    fn draft(id: &str, expected_version: Option<i64>) -> SkillDraftInput {
        SkillDraftInput {
            id: id.to_string(),
            expected_version,
            name: "会议整理".to_string(),
            description: "把会议记录整理为行动项".to_string(),
            instructions: "提取明确结论、负责人、截止时间，并区分事实和待确认内容。".to_string(),
            input_schema: String::new(),
            output_schema: String::new(),
            capabilities: vec!["vault_read".to_string()],
            trace_id: Some("trace-skill-test".to_string()),
        }
    }

    #[test]
    fn enable_requires_evaluation_and_explicit_user_approval() {
        let (_directory, database) = database();
        let workspace = database.local_workspace_scope().expect("workspace");
        let saved =
            save_draft(&database, &workspace, &draft("meeting-notes", None)).expect("save draft");
        assert_eq!(saved.status, "draft");
        let early_enable = change_activation(
            &database,
            &workspace,
            &SkillActivationInput {
                skill_id: saved.id.clone(),
                expected_version: saved.version,
                action: SkillActivationAction::Enable,
                trace_id: None,
            },
        );
        assert!(early_enable.is_err());
        let evaluation = evaluate_candidate(
            &database,
            &workspace,
            &SkillVersionInput {
                skill_id: saved.id.clone(),
                expected_version: Some(saved.version),
                trace_id: None,
            },
        )
        .expect("evaluate");
        assert!(evaluation.passed);
        let approved = decide_candidate(
            &database,
            &workspace,
            &SkillApprovalInput {
                skill_id: saved.id.clone(),
                expected_version: saved.version,
                approved: true,
                note: "确认启用".to_string(),
                trace_id: None,
            },
        )
        .expect("approve");
        assert_eq!(approved.approval_state.as_deref(), Some("approved"));
        let enabled = change_activation(
            &database,
            &workspace,
            &SkillActivationInput {
                skill_id: saved.id,
                expected_version: saved.version,
                action: SkillActivationAction::Enable,
                trace_id: None,
            },
        )
        .expect("enable");
        assert!(enabled.routing_eligible);
        assert_eq!(
            list_skills_for_workspace(&database, &workspace, true)
                .expect("routes")
                .len(),
            1
        );
    }

    #[test]
    fn editing_and_rollback_require_fresh_evaluation_and_approval() {
        let (_directory, database) = database();
        let workspace = database.local_workspace_scope().expect("workspace");
        let first = save_draft(&database, &workspace, &draft("versioned-skill", None))
            .expect("first version");
        let mut second_input = draft("versioned-skill", Some(first.version));
        second_input.instructions.push_str(" 输出按优先级排序。");
        second_input.trace_id = Some("trace-skill-version-two".to_string());
        let second = save_draft(&database, &workspace, &second_input).expect("second version");
        assert_eq!(second.version, 2);
        let rolled_back = rollback_skill_record(
            &database,
            &workspace,
            &SkillRollbackInput {
                skill_id: second.id,
                expected_version: 2,
                target_version: 1,
                trace_id: Some("trace-skill-rollback".to_string()),
            },
        )
        .expect("rollback");
        assert_eq!(rolled_back.version, 3);
        assert_eq!(rolled_back.rollback_of_version, Some(1));
        assert_eq!(rolled_back.status, "draft");
        assert!(!rolled_back.evaluation_passed);
        assert_eq!(rolled_back.approval_state, None);
    }

    #[test]
    fn retired_skill_never_returns_to_routing() {
        let (_directory, database) = database();
        let workspace = database.local_workspace_scope().expect("workspace");
        let saved = save_draft(&database, &workspace, &draft("retired-skill", None)).expect("save");
        evaluate_candidate(
            &database,
            &workspace,
            &SkillVersionInput {
                skill_id: saved.id.clone(),
                expected_version: Some(1),
                trace_id: None,
            },
        )
        .expect("evaluate");
        decide_candidate(
            &database,
            &workspace,
            &SkillApprovalInput {
                skill_id: saved.id.clone(),
                expected_version: 1,
                approved: true,
                note: String::new(),
                trace_id: None,
            },
        )
        .expect("approve");
        change_activation(
            &database,
            &workspace,
            &SkillActivationInput {
                skill_id: saved.id.clone(),
                expected_version: 1,
                action: SkillActivationAction::Enable,
                trace_id: None,
            },
        )
        .expect("enable");
        let retired = retire_skill_record(
            &database,
            &workspace,
            &SkillRetirementInput {
                skill_id: saved.id,
                expected_version: 1,
                replacement_skill_id: None,
                reason: "不再使用".to_string(),
                trace_id: None,
            },
        )
        .expect("retire");
        assert_eq!(retired.status, "retired");
        assert!(list_skills_for_workspace(&database, &workspace, true)
            .expect("routes")
            .is_empty());
    }
}
