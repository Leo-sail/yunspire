/// TaskPlugin 桥接层
///
/// 将 runtime_db.rs 中的任务管理功能桥接到 TaskPlugin
/// 保持向后兼容，同时实现模块化架构

use crate::runtime_db::{RuntimeDatabase, RuntimeTaskRecovery, RuntimeTaskRecoveryReplacement};
use crate::task_runtime::{
    NativeRuntimeTask, RuntimeTaskContractSnapshot, RuntimeTaskStepClaimBatch,
    RuntimeTaskStepClaimInput, RuntimeTaskStepCompletionInput, RuntimeTaskStepFailureInput,
    RuntimeTaskStepLeaseRenewalInput, RuntimeTaskStepLeaseRenewalReceipt,
};

/// 桥接：查询任务
pub fn get_runtime_task(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    task_id: &str,
) -> Result<NativeRuntimeTask, String> {
    database.runtime_task(workspace_scope, task_id)
}

/// 桥接：查询任务契约
pub fn get_runtime_task_contract(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    task_id: &str,
) -> Result<Option<RuntimeTaskContractSnapshot>, String> {
    database.runtime_task_contract(workspace_scope, task_id)
}

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

/// 桥接：认领步骤
pub fn claim_runtime_task_steps(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    input: &RuntimeTaskStepClaimInput,
) -> Result<RuntimeTaskStepClaimBatch, String> {
    database.claim_runtime_task_plan_steps(workspace_scope, input)
}

/// 桥接：续期租约
pub fn renew_runtime_task_step_lease(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    input: &RuntimeTaskStepLeaseRenewalInput,
) -> Result<RuntimeTaskStepLeaseRenewalReceipt, String> {
    database.renew_runtime_task_step_lease(workspace_scope, input)
}

/// 桥接：完成步骤
pub fn complete_runtime_task_step(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    input: &RuntimeTaskStepCompletionInput,
) -> Result<bool, String> {
    database
        .complete_runtime_task_plan_step(workspace_scope, input)
        .map(|_| true)
}

/// 桥接：失败步骤
pub fn fail_runtime_task_step(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    input: &RuntimeTaskStepFailureInput,
) -> Result<bool, String> {
    database
        .fail_runtime_task_plan_step(workspace_scope, input)
        .map(|_| true)
}

/// 桥接：恢复中断任务
pub fn recover_interrupted_tasks(
    database: &RuntimeDatabase,
    workspace_scope: &str,
) -> Result<Vec<RuntimeTaskRecovery>, String> {
    database.recover_interrupted_runtime_tasks(workspace_scope)
}

/// 桥接：解决恢复
pub fn resolve_task_recovery(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    task_id: &str,
    action: &str,
) -> Result<bool, String> {
    database
        .resolve_runtime_task_recovery(workspace_scope, task_id, action)
        .map(|_| true)
}

/// 桥接：替代任务
pub fn supersede_task_for_recovery(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    interrupted_task_id: &str,
    replacement_key: &str,
) -> Result<bool, String> {
    database
        .supersede_runtime_task_for_recovery(workspace_scope, interrupted_task_id, replacement_key)
        .map(|_| true)
}

/// 桥接：绑定替换
pub fn bind_recovery_replacement(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    interrupted_task_id: &str,
    replacement_task_id: &str,
    replacement_key: &str,
) -> Result<bool, String> {
    database
        .bind_runtime_task_recovery_replacement(
            workspace_scope,
            interrupted_task_id,
            replacement_task_id,
            replacement_key,
        )
        .map(|_| true)
}

/// 桥接：列出任务
pub fn list_runtime_tasks(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    state: Option<&str>,
    limit: usize,
) -> Result<Vec<NativeRuntimeTask>, String> {
    database.list_runtime_tasks(workspace_scope, state, limit)
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
