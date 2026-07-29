use crate::runtime_db::RuntimeDatabase;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use tauri::State;
use uuid::Uuid;

const MAX_TRACE_ID_CHARS: usize = 160;
const MAX_TRACE_PAYLOAD_BYTES: usize = 256 * 1024;

pub(crate) fn migrate_schema(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS runtime_traces (
               workspace_scope TEXT NOT NULL,
               trace_id TEXT NOT NULL,
               root_entity_kind TEXT NOT NULL,
               root_entity_id TEXT NOT NULL,
               created_at TEXT NOT NULL,
               updated_at TEXT NOT NULL,
               PRIMARY KEY(workspace_scope, trace_id),
               FOREIGN KEY(workspace_scope) REFERENCES local_workspace_scopes(id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS runtime_trace_bindings (
               workspace_scope TEXT NOT NULL,
               entity_kind TEXT NOT NULL,
               entity_id TEXT NOT NULL,
               trace_id TEXT NOT NULL,
               created_at TEXT NOT NULL,
               PRIMARY KEY(workspace_scope, entity_kind, entity_id),
               FOREIGN KEY(workspace_scope, trace_id)
                 REFERENCES runtime_traces(workspace_scope, trace_id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_runtime_trace_bindings_trace
               ON runtime_trace_bindings(workspace_scope, trace_id, created_at);
             CREATE TABLE IF NOT EXISTS runtime_trace_events (
               id TEXT PRIMARY KEY,
               workspace_scope TEXT NOT NULL,
               trace_id TEXT NOT NULL,
               entity_kind TEXT NOT NULL,
               entity_id TEXT NOT NULL,
               event_type TEXT NOT NULL,
               state TEXT NOT NULL,
               payload_json TEXT NOT NULL,
               created_at TEXT NOT NULL,
               FOREIGN KEY(workspace_scope, trace_id)
                 REFERENCES runtime_traces(workspace_scope, trace_id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_runtime_trace_events_trace
               ON runtime_trace_events(workspace_scope, trace_id, created_at, id);",
        )
        .map_err(|error| format!("无法创建统一 Trace 表：{error}"))
}

pub(crate) fn migrate_legacy_events(connection: &Connection) -> Result<(), String> {
    let commands = {
        let mut statement = connection
            .prepare(
                "SELECT workspace_scope, id, trace_id, state, created_at
                 FROM application_commands WHERE trace_id IS NOT NULL AND trim(trace_id)<>''",
            )
            .map_err(|error| format!("无法准备应用命令 Trace 迁移：{error}"))?;
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
            .map_err(|error| format!("无法读取应用命令 Trace：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析应用命令 Trace：{error}"))?;
        rows
    };
    for (workspace_scope, id, trace_id, state, created_at) in commands {
        record_trace_event_in_connection(
            connection,
            &workspace_scope,
            &TraceEventRecord {
                trace_id: &trace_id,
                entity_kind: "application_command",
                entity_id: &id,
                event_type: "command.migrated",
                state: &state,
                payload: &serde_json::json!({"source": "runtime-migration"}),
                created_at: &created_at,
            },
        )?;
    }

    let tasks = {
        let mut statement = connection
            .prepare(
                "SELECT workspace_scope, id, trace_id, state, created_at
                 FROM runtime_tasks WHERE trace_id IS NOT NULL AND trim(trace_id)<>''",
            )
            .map_err(|error| format!("无法准备任务 Trace 迁移：{error}"))?;
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
            .map_err(|error| format!("无法读取任务 Trace：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析任务 Trace：{error}"))?;
        rows
    };
    for (workspace_scope, id, trace_id, state, created_at) in tasks {
        record_trace_event_in_connection(
            connection,
            &workspace_scope,
            &TraceEventRecord {
                trace_id: &trace_id,
                entity_kind: "runtime_task",
                entity_id: &id,
                event_type: "task.migrated",
                state: &state,
                payload: &serde_json::json!({"source": "runtime-migration"}),
                created_at: &created_at,
            },
        )?;
    }

    let model_requests = {
        let mut statement = connection
            .prepare(
                "SELECT workspace_scope, request_id, trace_id, state, created_at
                 FROM model_usage_events WHERE trace_id IS NOT NULL AND trim(trace_id)<>''",
            )
            .map_err(|error| format!("无法准备模型 Trace 迁移：{error}"))?;
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
            .map_err(|error| format!("无法读取模型 Trace：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析模型 Trace：{error}"))?;
        rows
    };
    for (workspace_scope, id, trace_id, state, created_at) in model_requests {
        record_trace_event_in_connection(
            connection,
            &workspace_scope,
            &TraceEventRecord {
                trace_id: &trace_id,
                entity_kind: "model_request",
                entity_id: &id,
                event_type: "model.migrated",
                state: &state,
                payload: &serde_json::json!({"source": "runtime-migration"}),
                created_at: &created_at,
            },
        )?;
    }

    let operations = {
        let mut statement = connection
            .prepare(
                "SELECT id, trace_id, event_type, state, created_at
                 FROM operation_events WHERE trace_id IS NOT NULL AND trim(trace_id)<>''",
            )
            .map_err(|error| format!("无法准备操作事件 Trace 迁移：{error}"))?;
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
            .map_err(|error| format!("无法读取操作事件 Trace：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析操作事件 Trace：{error}"))?;
        rows
    };
    for (id, trace_id, event_type, state, created_at) in operations {
        record_trace_event_in_connection(
            connection,
            "local",
            &TraceEventRecord {
                trace_id: &trace_id,
                entity_kind: "operation_event",
                entity_id: &id,
                event_type: &event_type,
                state: &state,
                payload: &serde_json::json!({"source": "runtime-migration"}),
                created_at: &created_at,
            },
        )?;
    }

    let index_changes = {
        let mut statement = connection
            .prepare(
                "SELECT id, generation, trace_id, state, vault_id, relative_path, created_at
                 FROM vault_index_changes WHERE trace_id IS NOT NULL AND trim(trace_id)<>''",
            )
            .map_err(|error| format!("无法准备索引 Trace 迁移：{error}"))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })
            .map_err(|error| format!("无法读取索引 Trace：{error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("无法解析索引 Trace：{error}"))?;
        rows
    };
    for (id, generation, trace_id, state, vault_id, relative_path, created_at) in index_changes {
        record_trace_event_in_connection(
            connection,
            "local",
            &TraceEventRecord {
                trace_id: &trace_id,
                entity_kind: "index_change",
                entity_id: &format!("{id}:{generation}"),
                event_type: "index.migrated",
                state: &state,
                payload: &serde_json::json!({
                    "vaultId": vault_id,
                    "relativePath": relative_path,
                }),
                created_at: &created_at,
            },
        )?;
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceEvent {
    id: String,
    trace_id: String,
    entity_kind: String,
    entity_id: String,
    event_type: String,
    state: String,
    payload: Value,
    created_at: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceValidationResult {
    trace_id: String,
    valid: bool,
    binding_count: i64,
    event_count: i64,
    violations: Vec<String>,
}

pub(crate) struct TraceEventRecord<'a> {
    pub(crate) trace_id: &'a str,
    pub(crate) entity_kind: &'a str,
    pub(crate) entity_id: &'a str,
    pub(crate) event_type: &'a str,
    pub(crate) state: &'a str,
    pub(crate) payload: &'a Value,
    pub(crate) created_at: &'a str,
}

pub(crate) fn new_trace_id() -> String {
    format!("trace-{}", Uuid::new_v4())
}

pub(crate) fn validate_trace_id(trace_id: &str) -> Result<&str, String> {
    let trace_id = trace_id.trim();
    if trace_id.is_empty()
        || trace_id.chars().count() > MAX_TRACE_ID_CHARS
        || !trace_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | ':')
        })
    {
        return Err("Trace ID 无效".to_string());
    }
    Ok(trace_id)
}

fn validate_trace_label<'a>(value: &'a str, label: &str) -> Result<&'a str, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 160 || value.chars().any(char::is_control) {
        return Err(format!("{label}无效"));
    }
    Ok(value)
}

pub(crate) fn record_trace_event_in_connection(
    connection: &Connection,
    workspace_scope: &str,
    record: &TraceEventRecord<'_>,
) -> Result<String, String> {
    let trace_id = validate_trace_id(record.trace_id)?;
    let entity_kind = validate_trace_label(record.entity_kind, "Trace 实体类型")?;
    let entity_id = validate_trace_label(record.entity_id, "Trace 实体 ID")?;
    let event_type = validate_trace_label(record.event_type, "Trace 事件类型")?;
    let state = validate_trace_label(record.state, "Trace 事件状态")?;
    let payload_json = serde_json::to_string(record.payload)
        .map_err(|error| format!("无法序列化 Trace 事件：{error}"))?;
    if payload_json.len() > MAX_TRACE_PAYLOAD_BYTES {
        return Err("Trace 事件负载超过 256 KB 安全上限".to_string());
    }
    let existing_trace = connection
        .query_row(
            "SELECT trace_id FROM runtime_trace_bindings
             WHERE workspace_scope=?1 AND entity_kind=?2 AND entity_id=?3",
            params![workspace_scope, entity_kind, entity_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|error| format!("无法校验 Trace 继承关系：{error}"))?;
    if existing_trace
        .as_deref()
        .is_some_and(|value| value != trace_id)
    {
        return Err(format!(
            "{} {} 已绑定其他 Trace，拒绝重新生成追踪链",
            entity_kind, entity_id
        ));
    }
    let event_id = Uuid::new_v4().to_string();
    connection
        .execute(
            "INSERT INTO runtime_traces
             (workspace_scope, trace_id, root_entity_kind, root_entity_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(workspace_scope, trace_id) DO UPDATE SET updated_at=excluded.updated_at",
            params![
                workspace_scope,
                trace_id,
                entity_kind,
                entity_id,
                record.created_at
            ],
        )
        .map_err(|error| format!("无法登记 Trace：{error}"))?;
    connection
        .execute(
            "INSERT OR IGNORE INTO runtime_trace_bindings
             (workspace_scope, entity_kind, entity_id, trace_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                workspace_scope,
                entity_kind,
                entity_id,
                trace_id,
                record.created_at
            ],
        )
        .map_err(|error| format!("无法保存 Trace 实体绑定：{error}"))?;
    connection
        .execute(
            "INSERT INTO runtime_trace_events
             (id, workspace_scope, trace_id, entity_kind, entity_id, event_type,
              state, payload_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                event_id,
                workspace_scope,
                trace_id,
                entity_kind,
                entity_id,
                event_type,
                state,
                payload_json,
                record.created_at
            ],
        )
        .map_err(|error| format!("无法写入 Trace 事件：{error}"))?;
    Ok(event_id)
}

#[cfg(test)]
pub(crate) fn record_trace_event(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    record: &TraceEventRecord<'_>,
) -> Result<String, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    record_trace_event_in_connection(&connection, workspace_scope, record)
}

fn read_trace_events(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    trace_id: &str,
    limit: usize,
) -> Result<Vec<TraceEvent>, String> {
    let trace_id = validate_trace_id(trace_id)?;
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let mut statement = connection
        .prepare(
            "SELECT id, trace_id, entity_kind, entity_id, event_type, state,
                    payload_json, created_at
             FROM runtime_trace_events
             WHERE workspace_scope=?1 AND trace_id=?2
             ORDER BY created_at, id LIMIT ?3",
        )
        .map_err(|error| format!("无法准备 Trace 查询：{error}"))?;
    let rows = statement
        .query_map(
            params![workspace_scope, trace_id, limit.clamp(1, 2_000) as i64],
            |row| {
                let payload: String = row.get(6)?;
                Ok(TraceEvent {
                    id: row.get(0)?,
                    trace_id: row.get(1)?,
                    entity_kind: row.get(2)?,
                    entity_id: row.get(3)?,
                    event_type: row.get(4)?,
                    state: row.get(5)?,
                    payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
                    created_at: row.get(7)?,
                })
            },
        )
        .map_err(|error| format!("无法读取 Trace 事件：{error}"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法解析 Trace 事件：{error}"))
}

fn validate_trace_chain(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    trace_id: &str,
) -> Result<TraceValidationResult, String> {
    let trace_id = validate_trace_id(trace_id)?.to_string();
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    let binding_count = connection
        .query_row(
            "SELECT COUNT(*) FROM runtime_trace_bindings
             WHERE workspace_scope=?1 AND trace_id=?2",
            params![workspace_scope, trace_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| format!("无法统计 Trace 绑定：{error}"))?;
    let event_count = connection
        .query_row(
            "SELECT COUNT(*) FROM runtime_trace_events
             WHERE workspace_scope=?1 AND trace_id=?2",
            params![workspace_scope, trace_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| format!("无法统计 Trace 事件：{error}"))?;
    let orphan_events = connection
        .query_row(
            "SELECT COUNT(*) FROM runtime_trace_events e
             LEFT JOIN runtime_trace_bindings b
               ON b.workspace_scope=e.workspace_scope AND b.entity_kind=e.entity_kind
              AND b.entity_id=e.entity_id AND b.trace_id=e.trace_id
             WHERE e.workspace_scope=?1 AND e.trace_id=?2 AND b.entity_id IS NULL",
            params![workspace_scope, trace_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| format!("无法校验 Trace 孤立事件：{error}"))?;
    let mut violations = Vec::new();
    if binding_count == 0 {
        violations.push("Trace 没有实体绑定".to_string());
    }
    if event_count == 0 {
        violations.push("Trace 没有事件".to_string());
    }
    if orphan_events > 0 {
        violations.push(format!("Trace 包含 {orphan_events} 个孤立事件"));
    }
    Ok(TraceValidationResult {
        trace_id,
        valid: violations.is_empty(),
        binding_count,
        event_count,
        violations,
    })
}

#[tauri::command]
pub fn query_runtime_trace(
    database: State<'_, RuntimeDatabase>,
    trace_id: String,
    limit: Option<usize>,
) -> Result<Vec<TraceEvent>, String> {
    let workspace_scope = database.local_workspace_scope()?;
    read_trace_events(
        database.inner(),
        &workspace_scope,
        &trace_id,
        limit.unwrap_or(500),
    )
}

#[tauri::command]
pub fn validate_runtime_trace(
    database: State<'_, RuntimeDatabase>,
    trace_id: String,
) -> Result<TraceValidationResult, String> {
    let workspace_scope = database.local_workspace_scope()?;
    validate_trace_chain(database.inner(), &workspace_scope, &trace_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obsidian::OperationEvent;
    use chrono::Utc;

    #[test]
    fn validates_trace_id_without_rewriting_it() {
        assert_eq!(validate_trace_id("trace-request-1"), Ok("trace-request-1"));
        assert!(validate_trace_id(" ").is_err());
        assert!(validate_trace_id("trace/id").is_err());
    }

    #[test]
    fn rejects_rebinding_an_entity_to_another_trace() {
        let directory = tempfile::tempdir().expect("temp directory");
        let database = RuntimeDatabase::open_test(&directory.path().join("runtime.sqlite"))
            .expect("open database");
        let workspace_scope = database.local_workspace_scope().expect("workspace");
        let now = Utc::now().to_rfc3339();
        let payload = serde_json::json!({"source": "test"});
        record_trace_event(
            &database,
            &workspace_scope,
            &TraceEventRecord {
                trace_id: "trace-one",
                entity_kind: "model_request",
                entity_id: "request-one",
                event_type: "model.started",
                state: "started",
                payload: &payload,
                created_at: &now,
            },
        )
        .expect("first binding");
        let rebound = record_trace_event(
            &database,
            &workspace_scope,
            &TraceEventRecord {
                trace_id: "trace-two",
                entity_kind: "model_request",
                entity_id: "request-one",
                event_type: "model.completed",
                state: "succeeded",
                payload: &payload,
                created_at: &now,
            },
        );
        assert!(rebound.is_err());
        let validation =
            validate_trace_chain(&database, &workspace_scope, "trace-one").expect("validate");
        assert!(validation.valid);
        assert_eq!(validation.binding_count, 1);
        assert_eq!(validation.event_count, 1);
    }

    #[test]
    fn vault_operation_is_bound_to_the_same_trace_as_its_audit_event() {
        let directory = tempfile::tempdir().expect("temp directory");
        let database = RuntimeDatabase::open_test(&directory.path().join("runtime.sqlite"))
            .expect("open database");
        database
            .append_operation_event(&OperationEvent {
                id: "operation-vault-test".to_string(),
                task_id: Some("task-vault-test".to_string()),
                trace_id: Some("trace-vault-test".to_string()),
                event_type: "vault.note.write".to_string(),
                state: "succeeded".to_string(),
                created_at: Utc::now().to_rfc3339(),
                vault_id: Some("vault-test".to_string()),
                relative_path: Some("notes/test.md".to_string()),
                detail: "test".to_string(),
            })
            .expect("append vault operation");
        let connection = database.connection.lock().expect("lock database");
        let bindings = connection
            .query_row(
                "SELECT COUNT(*) FROM runtime_trace_bindings
                 WHERE workspace_scope='local' AND trace_id='trace-vault-test'
                   AND entity_id='operation-vault-test'
                   AND entity_kind IN ('operation_event', 'vault_operation')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count vault bindings");
        assert_eq!(bindings, 2);
    }
}
