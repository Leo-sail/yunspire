/// 任务恢复管理
///
/// 从 runtime_db.rs 提取的恢复相关函数

use crate::database::QueryProfiler;
use crate::runtime_db::{RuntimeDatabase, RuntimeTaskRecovery};
use rusqlite::OptionalExtension;

/// 恢复中断任务（包装实现）
pub fn recover_interrupted_runtime_tasks(
    database: &RuntimeDatabase,
    workspace_scope: &str,
) -> Result<Vec<RuntimeTaskRecovery>, String> {
    // 性能监控
    let _profiler = QueryProfiler::new("recover_interrupted_runtime_tasks").with_threshold(100);

    // 直接调用 runtime_db 的实现
    database.recover_interrupted_runtime_tasks(workspace_scope)
}

/// 解决恢复（包装实现）
pub fn resolve_runtime_task_recovery(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    task_id: &str,
    action: &str,
) -> Result<(), String> {
    // 性能监控
    let _profiler = QueryProfiler::new("resolve_runtime_task_recovery").with_threshold(100);

    // 直接调用 runtime_db 的实现
    database.resolve_runtime_task_recovery(workspace_scope, task_id, action)
}

/// 替代任务（包装实现）
pub fn supersede_runtime_task_for_recovery(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    interrupted_task_id: &str,
    replacement_key: &str,
) -> Result<String, String> {
    // 性能监控
    let _profiler =
        QueryProfiler::new("supersede_runtime_task_for_recovery").with_threshold(100);

    // 直接调用 runtime_db 的实现，返回新任务 ID
    database
        .supersede_runtime_task_for_recovery(
            workspace_scope,
            interrupted_task_id,
            replacement_key,
        )
        .and_then(|replacement| {
            // 由于字段是私有的，我们需要通过查询数据库获取 replacement_task_id
            let connection = database.connection.lock()
                .map_err(|_| "SQLite 连接锁不可用".to_string())?;

            connection.query_row(
                "SELECT replacement_task_id FROM runtime_task_recovery_replacements
                 WHERE workspace_scope=?1 AND interrupted_task_id=?2 AND replacement_key=?3",
                rusqlite::params![workspace_scope, interrupted_task_id, replacement_key],
                |row| row.get::<_, Option<String>>(0),
            )
            .map_err(|e| format!("无法查询替换任务 ID: {}", e))
            .and_then(|opt_id| opt_id.ok_or_else(|| "替换任务 ID 为空".to_string()))
        })
}

/// 绑定替换（包装实现）
pub fn bind_runtime_task_recovery_replacement(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    interrupted_task_id: &str,
    replacement_task_id: &str,
    replacement_key: &str,
) -> Result<(), String> {
    // 性能监控
    let _profiler =
        QueryProfiler::new("bind_runtime_task_recovery_replacement").with_threshold(100);

    // 直接调用 runtime_db 的实现，忽略返回值
    database
        .bind_runtime_task_recovery_replacement(
            workspace_scope,
            interrupted_task_id,
            replacement_task_id,
            replacement_key,
        )
        .map(|_| ())
}

/// 获取任务恢复信息（辅助函数 - 暂时不可用）
#[allow(dead_code)]
pub fn get_task_recovery(
    _database: &RuntimeDatabase,
    _workspace_scope: &str,
    _task_id: &str,
) -> Result<Option<RuntimeTaskRecovery>, String> {
    // TODO: RuntimeTaskRecovery 的字段是私有的，无法直接构造
    // 需要等 runtime_db 提供公开的构造方法或者迁移整个结构到这里
    Err("get_task_recovery 暂不可用，字段私有".to_string())
}
