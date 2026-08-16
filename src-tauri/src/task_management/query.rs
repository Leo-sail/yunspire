/// 任务查询模块
///
/// 从 runtime_db.rs 提取的任务查询相关函数

use crate::database::QueryProfiler;
use crate::runtime_db::RuntimeDatabase;
use crate::task_runtime::{NativeRuntimeTask, RuntimeTaskContractSnapshot};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

/// 映射任务行到 NativeRuntimeTask
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

/// 读取原生任务（公开给 steps.rs 使用）
pub(crate) fn read_native_runtime_task(
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

/// 查询单个任务
pub fn runtime_task(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    task_id: &str,
) -> Result<NativeRuntimeTask, String> {
    // 性能监控（使用默认配置）
    let _profiler = QueryProfiler::new("runtime_task").with_threshold(100);

    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;
    read_native_runtime_task(&connection, workspace_scope, task_id)
}

/// 查询任务契约（占位实现）
pub fn runtime_task_contract(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    task_id: &str,
) -> Result<Option<RuntimeTaskContractSnapshot>, String> {
    // 性能监控（使用默认配置）
    let _profiler = QueryProfiler::new("runtime_task_contract").with_threshold(100);

    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    // TODO: 实现完整的契约查询逻辑
    // 这里是简化实现，先返回 None
    let _ = (connection, workspace_scope, task_id);
    Ok(None)
}

/// 列出任务（占位实现）
pub fn list_runtime_tasks(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    state: Option<&str>,
    limit: usize,
) -> Result<Vec<NativeRuntimeTask>, String> {
    // 性能监控（使用默认配置）
    let _profiler = QueryProfiler::new("list_runtime_tasks").with_threshold(100);

    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    let mut query = String::from(
        "SELECT id, state, title, trace_id, payload, created_at, updated_at
         FROM runtime_tasks WHERE workspace_scope = ?1",
    );

    let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(workspace_scope.to_string())];

    if let Some(state_filter) = state {
        query.push_str(" AND state = ?2");
        params.push(Box::new(state_filter.to_string()));
    }

    query.push_str(&format!(" ORDER BY created_at DESC LIMIT {}", limit));

    let mut stmt = connection
        .prepare(&query)
        .map_err(|e| format!("准备查询失败: {}", e))?;

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let tasks = stmt
        .query_map(param_refs.as_slice(), map_native_runtime_task)
        .map_err(|e| format!("查询任务失败: {}", e))?
        .filter_map(|r| r.ok())
        .collect();

    Ok(tasks)
}
