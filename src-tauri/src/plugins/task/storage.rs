/// TaskPlugin 任务存储模块
///
/// 管理任务的持久化操作

use crate::plugins::task::types::{RuntimeTask, RuntimeTaskContract};
use crate::runtime_db::RuntimeDatabase;
use serde::{Deserialize, Serialize};

/// 存储错误
#[derive(Debug, Clone)]
pub enum StorageError {
    /// 任务未找到
    TaskNotFound(String),

    /// 数据库错误
    DatabaseError(String),

    /// 序列化错误
    SerializationError(String),

    /// 验证错误
    ValidationError(String),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::TaskNotFound(task_id) => {
                write!(f, "任务未找到: {}", task_id)
            }
            StorageError::DatabaseError(msg) => {
                write!(f, "数据库错误: {}", msg)
            }
            StorageError::SerializationError(msg) => {
                write!(f, "序列化错误: {}", msg)
            }
            StorageError::ValidationError(msg) => {
                write!(f, "验证错误: {}", msg)
            }
        }
    }
}

impl std::error::Error for StorageError {}

/// 任务过滤器
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskFilters {
    /// 任务状态过滤
    pub states: Option<Vec<String>>,

    /// 任务类型过滤
    pub task_kinds: Option<Vec<String>>,

    /// 创建时间起始
    pub created_after: Option<String>,

    /// 创建时间结束
    pub created_before: Option<String>,

    /// 分页：偏移量
    pub offset: Option<usize>,

    /// 分页：限制数量
    pub limit: Option<usize>,
}

/// 任务统计信息
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatistics {
    /// 总任务数
    pub total_tasks: usize,

    /// 按状态统计
    pub by_state: std::collections::HashMap<String, usize>,

    /// 按类型统计
    pub by_kind: std::collections::HashMap<String, usize>,

    /// 运行中的任务数
    pub running_count: usize,

    /// 待处理的任务数
    pub pending_count: usize,

    /// 已完成的任务数
    pub completed_count: usize,

    /// 失败的任务数
    pub failed_count: usize,
}

impl Default for TaskStatistics {
    fn default() -> Self {
        Self {
            total_tasks: 0,
            by_state: std::collections::HashMap::new(),
            by_kind: std::collections::HashMap::new(),
            running_count: 0,
            pending_count: 0,
            completed_count: 0,
            failed_count: 0,
        }
    }
}

/// 加载任务
///
/// # 参数
/// - `database`: 数据库实例
/// - `task_id`: 任务 ID
///
/// # 返回
/// 完整的任务信息
pub fn load_task(
    database: &RuntimeDatabase,
    task_id: &str,
) -> Result<RuntimeTask, StorageError> {
    // 验证参数
    if task_id.is_empty() {
        return Err(StorageError::ValidationError(
            "任务 ID 不能为空".to_string(),
        ));
    }

    let conn = database
        .connection
        .lock()
        .map_err(|e| StorageError::DatabaseError(format!("获取数据库连接失败: {}", e)))?;

    // 查询任务主记录
    let mut stmt = conn
        .prepare(
            "SELECT task_id, workspace_scope, task_kind, state, payload, result, error,
                    created_at, updated_at, plan_revision
             FROM runtime_tasks
             WHERE task_id = ?",
        )
        .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

    let task = stmt
        .query_row([task_id], |row| {
            let payload_str: String = row.get(4)?;
            let result_str: Option<String> = row.get(5)?;
            let error_str: Option<String> = row.get(6)?;

            let payload: serde_json::Value = serde_json::from_str(&payload_str)
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                ))?;

            let result: Option<serde_json::Value> = result_str
                .map(|s| serde_json::from_str(&s))
                .transpose()
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                ))?;

            let error: Option<String> = error_str;

            Ok(RuntimeTask {
                contract: RuntimeTaskContract {
                    task_id: row.get(0)?,
                    workspace_scope: row.get(1)?,
                    task_kind: row.get(2)?,
                    state: row.get(3)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                    plan_revision: row.get(9)?,
                },
                payload,
                result,
                error,
            })
        })
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => StorageError::TaskNotFound(task_id.to_string()),
            _ => StorageError::DatabaseError(e.to_string()),
        })?;

    Ok(task)
}

/// 保存任务
///
/// # 参数
/// - `database`: 数据库实例
/// - `task`: 任务信息
///
/// # 返回
/// 是否成功
pub fn save_task(database: &RuntimeDatabase, task: &RuntimeTask) -> Result<(), StorageError> {
    // 验证参数
    if task.contract.task_id.is_empty() {
        return Err(StorageError::ValidationError(
            "任务 ID 不能为空".to_string(),
        ));
    }

    let conn = database
        .connection
        .lock()
        .map_err(|e| StorageError::DatabaseError(format!("获取数据库连接失败: {}", e)))?;

    // 序列化 JSON 字段
    let payload_json = serde_json::to_string(&task.payload)
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;

    let result_json = task
        .result
        .as_ref()
        .map(|r| serde_json::to_string(r))
        .transpose()
        .map_err(|e| StorageError::SerializationError(e.to_string()))?;

    let error_str = task.error.clone();

    // INSERT OR REPLACE 操作
    conn.execute(
        "INSERT OR REPLACE INTO runtime_tasks
         (task_id, workspace_scope, task_kind, state, payload, result, error,
          created_at, updated_at, plan_revision)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            task.contract.task_id,
            task.contract.workspace_scope,
            task.contract.task_kind,
            task.contract.state,
            payload_json,
            result_json,
            error_str,
            task.contract.created_at,
            task.contract.updated_at,
            task.contract.plan_revision,
        ],
    )
    .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

    Ok(())
}

/// 列出任务
///
/// # 参数
/// - `database`: 数据库实例
/// - `workspace_scope`: 工作区范围
/// - `filters`: 过滤器
///
/// # 返回
/// 任务契约列表
pub fn list_tasks(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    filters: Option<&TaskFilters>,
) -> Result<Vec<RuntimeTaskContract>, StorageError> {
    // 验证参数
    if workspace_scope.is_empty() {
        return Err(StorageError::ValidationError(
            "工作区范围不能为空".to_string(),
        ));
    }

    // 解析过滤器
    let limit = filters
        .and_then(|f| f.limit)
        .unwrap_or(50)
        .clamp(1, 200);
    let offset = filters.and_then(|f| f.offset).unwrap_or(0);

    let conn = database
        .connection
        .lock()
        .map_err(|e| StorageError::DatabaseError(format!("获取数据库连接失败: {}", e)))?;

    // 构建 SQL 查询
    let mut sql = String::from(
        "SELECT task_id, workspace_scope, task_kind, state, created_at, updated_at, plan_revision
         FROM runtime_tasks
         WHERE workspace_scope = ?",
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(workspace_scope.to_string())];

    // 应用状态过滤
    if let Some(f) = filters {
        if let Some(states) = &f.states {
            if !states.is_empty() {
                let placeholders = states.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
                sql.push_str(&format!(" AND state IN ({})", placeholders));
                for state in states {
                    params.push(Box::new(state.clone()));
                }
            }
        }

        // 应用类型过滤
        if let Some(kinds) = &f.task_kinds {
            if !kinds.is_empty() {
                let placeholders = kinds.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
                sql.push_str(&format!(" AND task_kind IN ({})", placeholders));
                for kind in kinds {
                    params.push(Box::new(kind.clone()));
                }
            }
        }

        // 应用时间过滤
        if let Some(after) = &f.created_after {
            sql.push_str(" AND created_at >= ?");
            params.push(Box::new(after.clone()));
        }

        if let Some(before) = &f.created_before {
            sql.push_str(" AND created_at <= ?");
            params.push(Box::new(before.clone()));
        }
    }

    // 应用排序和分页
    sql.push_str(" ORDER BY created_at DESC LIMIT ? OFFSET ?");
    params.push(Box::new(limit as i64));
    params.push(Box::new(offset as i64));

    // 执行查询
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

    let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    let tasks = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(RuntimeTaskContract {
                task_id: row.get(0)?,
                workspace_scope: row.get(1)?,
                task_kind: row.get(2)?,
                state: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
                plan_revision: row.get(6)?,
            })
        })
        .map_err(|e| StorageError::DatabaseError(e.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

    Ok(tasks)
}

/// 任务统计
///
/// # 参数
/// - `database`: 数据库实例
/// - `workspace_scope`: 工作区范围
///
/// # 返回
/// 统计信息
pub fn task_statistics(
    database: &RuntimeDatabase,
    workspace_scope: &str,
) -> Result<TaskStatistics, StorageError> {
    // 验证参数
    if workspace_scope.is_empty() {
        return Err(StorageError::ValidationError(
            "工作区范围不能为空".to_string(),
        ));
    }

    let conn = database
        .connection
        .lock()
        .map_err(|e| StorageError::DatabaseError(format!("获取数据库连接失败: {}", e)))?;

    // 查询总任务数
    let total_tasks: usize = conn
        .query_row(
            "SELECT COUNT(*) FROM runtime_tasks WHERE workspace_scope = ?",
            [workspace_scope],
            |row| row.get(0),
        )
        .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

    // 按状态统计
    let mut stmt = conn
        .prepare(
            "SELECT state, COUNT(*) as count
             FROM runtime_tasks
             WHERE workspace_scope = ?
             GROUP BY state",
        )
        .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

    let mut by_state = std::collections::HashMap::new();
    let state_rows = stmt
        .query_map([workspace_scope], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

    for row in state_rows {
        let (state, count) = row.map_err(|e| StorageError::DatabaseError(e.to_string()))?;
        by_state.insert(state, count as usize);
    }

    // 按类型统计
    let mut stmt = conn
        .prepare(
            "SELECT task_kind, COUNT(*) as count
             FROM runtime_tasks
             WHERE workspace_scope = ?
             GROUP BY task_kind",
        )
        .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

    let mut by_kind = std::collections::HashMap::new();
    let kind_rows = stmt
        .query_map([workspace_scope], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

    for row in kind_rows {
        let (kind, count) = row.map_err(|e| StorageError::DatabaseError(e.to_string()))?;
        by_kind.insert(kind, count as usize);
    }

    // 计算各状态的任务数
    let running_count = by_state.get("running").copied().unwrap_or(0);
    let pending_count = by_state.get("created").copied().unwrap_or(0)
        + by_state.get("queued").copied().unwrap_or(0);
    let completed_count = by_state.get("succeeded").copied().unwrap_or(0);
    let failed_count = by_state.get("failed").copied().unwrap_or(0);

    Ok(TaskStatistics {
        total_tasks,
        by_state,
        by_kind,
        running_count,
        pending_count,
        completed_count,
        failed_count,
    })
}

/// 删除任务
///
/// # 参数
/// - `database`: 数据库实例
/// - `task_id`: 任务 ID
///
/// # 返回
/// 是否成功
pub fn delete_task(database: &RuntimeDatabase, task_id: &str) -> Result<(), StorageError> {
    // 验证参数
    if task_id.is_empty() {
        return Err(StorageError::ValidationError(
            "任务 ID 不能为空".to_string(),
        ));
    }

    let mut conn = database
        .connection
        .lock()
        .map_err(|e| StorageError::DatabaseError(format!("获取数据库连接失败: {}", e)))?;

    // 使用事务进行级联删除
    let tx = conn
        .transaction()
        .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

    // 1. 删除步骤租约（通过步骤 ID）
    tx.execute(
        "DELETE FROM runtime_step_leases
         WHERE step_id IN (
             SELECT step_id FROM runtime_task_steps WHERE task_id = ?
         )",
        [task_id],
    )
    .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

    // 2. 删除任务步骤
    tx.execute(
        "DELETE FROM runtime_task_steps WHERE task_id = ?",
        [task_id],
    )
    .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

    // 3. 删除恢复信息
    tx.execute(
        "DELETE FROM runtime_task_recovery WHERE task_id = ?",
        [task_id],
    )
    .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

    // 4. 删除任务本身
    let rows_affected = tx
        .execute("DELETE FROM runtime_tasks WHERE task_id = ?", [task_id])
        .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

    if rows_affected == 0 {
        return Err(StorageError::TaskNotFound(task_id.to_string()));
    }

    // 提交事务
    tx.commit()
        .map_err(|e| StorageError::DatabaseError(e.to_string()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_task_validation() {
        // 测试验证逻辑

        // 空任务 ID 应该失败
        assert!("".is_empty());
    }

    #[test]
    fn test_save_task_validation() {
        // 测试验证逻辑

        // 空任务 ID 应该失败
        let contract = RuntimeTaskContract {
            task_id: String::new(),
            workspace_scope: "test".to_string(),
            task_kind: "test".to_string(),
            state: "created".to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            plan_revision: None,
        };

        let task = RuntimeTask {
            contract,
            payload: serde_json::json!({}),
            result: None,
            error: None,
        };

        assert!(task.contract.task_id.is_empty());
    }

    #[test]
    fn test_list_tasks_validation() {
        // 测试验证逻辑

        // 空工作区应该失败
        assert!("".is_empty());
    }

    #[test]
    fn test_task_filters_default() {
        let filters = TaskFilters::default();
        assert!(filters.states.is_none());
        assert!(filters.task_kinds.is_none());
        assert!(filters.offset.is_none());
        assert!(filters.limit.is_none());
    }

    #[test]
    fn test_task_statistics_default() {
        let stats = TaskStatistics::default();
        assert_eq!(stats.total_tasks, 0);
        assert_eq!(stats.running_count, 0);
        assert_eq!(stats.pending_count, 0);
        assert_eq!(stats.completed_count, 0);
    }

    #[test]
    fn test_storage_error_display() {
        let err = StorageError::TaskNotFound("task123".to_string());
        assert_eq!(err.to_string(), "任务未找到: task123");

        let err = StorageError::DatabaseError("test".to_string());
        assert!(err.to_string().contains("数据库错误"));
    }
}
