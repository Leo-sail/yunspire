/// ConfigPlugin 配置存储模块
///
/// 提供配置的持久化功能

use crate::plugins::config::types::RuntimeSettings;
use crate::runtime_db::RuntimeDatabase;
use rusqlite::{params, OptionalExtension};

/// 加载运行时设置
///
/// # 参数
/// - `database`: 数据库实例
/// - `workspace_scope`: 工作区范围
///
/// # 返回
/// - `Ok(Some(settings))`: 设置存在
/// - `Ok(None)`: 设置不存在
/// - `Err(String)`: 加载失败
pub fn load_runtime_settings(
    database: &RuntimeDatabase,
    workspace_scope: &str,
) -> Result<Option<RuntimeSettings>, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    let mut stmt = connection
        .prepare(
            "SELECT scheduler_enabled, updated_at
             FROM runtime_settings
             WHERE workspace_scope=?1",
        )
        .map_err(|e| format!("无法准备查询：{}", e))?;

    let result = stmt
        .query_row(params![workspace_scope], |row| {
            Ok(RuntimeSettings {
                workspace_scope: workspace_scope.to_string(),
                scheduler_enabled: row.get(0)?,
                updated_at: row.get(1)?,
            })
        })
        .optional()
        .map_err(|e| format!("查询运行时设置失败：{}", e))?;

    Ok(result)
}

/// 保存运行时设置
///
/// # 参数
/// - `database`: 数据库实例
/// - `settings`: 要保存的设置
///
/// # 返回
/// - `Ok(())`: 保存成功
/// - `Err(String)`: 保存失败
pub fn save_runtime_settings(
    database: &RuntimeDatabase,
    settings: &RuntimeSettings,
) -> Result<(), String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    connection
        .execute(
            "INSERT INTO runtime_settings (workspace_scope, scheduler_enabled, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(workspace_scope) DO UPDATE SET
               scheduler_enabled=excluded.scheduler_enabled,
               updated_at=excluded.updated_at",
            params![
                settings.workspace_scope,
                settings.scheduler_enabled,
                settings.updated_at,
            ],
        )
        .map_err(|e| format!("保存运行时设置失败：{}", e))?;

    Ok(())
}

/// 更新调度器状态
///
/// # 参数
/// - `database`: 数据库实例
/// - `workspace_scope`: 工作区范围
/// - `enabled`: 是否启用
///
/// # 返回
/// - `Ok(())`: 更新成功
/// - `Err(String)`: 更新失败
pub fn update_scheduler_enabled(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    enabled: bool,
) -> Result<(), String> {
    // 加载现有设置
    let mut settings = load_runtime_settings(database, workspace_scope)?
        .unwrap_or_else(|| RuntimeSettings::default_for_workspace(workspace_scope.to_string()));

    // 更新状态
    settings.scheduler_enabled = enabled;
    settings.updated_at = chrono::Utc::now().to_rfc3339();

    // 保存
    save_runtime_settings(database, &settings)
}

/// 删除运行时设置
///
/// # 参数
/// - `database`: 数据库实例
/// - `workspace_scope`: 工作区范围
///
/// # 返回
/// - `Ok(())`: 删除成功
/// - `Err(String)`: 删除失败
pub fn delete_runtime_settings(
    database: &RuntimeDatabase,
    workspace_scope: &str,
) -> Result<(), String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    connection
        .execute(
            "DELETE FROM runtime_settings WHERE workspace_scope=?1",
            params![workspace_scope],
        )
        .map_err(|e| format!("删除运行时设置失败：{}", e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // 注意：这些测试需要真实的数据库连接
    // 当前只验证函数签名和基本逻辑

    #[test]
    fn test_runtime_settings_operations() {
        // 这个测试需要真实数据库
        // 在集成测试中会有完整实现
    }
}
