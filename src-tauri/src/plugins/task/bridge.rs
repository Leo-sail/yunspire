/// TaskPlugin 桥接层
///
/// 将 runtime_db.rs 中的任务管理功能桥接到 TaskPlugin
/// 保持向后兼容，同时实现模块化架构

use crate::runtime_db::RuntimeDatabase;

/// 桥接：查询步骤前沿
pub fn get_runtime_task_step_frontier(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    task_id: &str,
    plan_revision: Option<u64>,
) -> Result<Vec<String>, String> {
    database
        .runtime_task_step_frontier(workspace_scope, task_id, plan_revision)
        .map(|items| items.into_iter().map(|item| item.step_id).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bridge_module_exists() {
        // 基本的桥接层存在性测试
        assert!(true);
    }
}
