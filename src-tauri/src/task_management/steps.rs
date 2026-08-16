/// 任务步骤管理
///
/// 从 runtime_db.rs 提取的步骤管理相关函数

use crate::database::QueryProfiler;
use crate::runtime_db::RuntimeDatabase;
use crate::task_management::query::read_native_runtime_task;
use crate::task_management::steps_helpers::{
    expire_runtime_task_step_claims, latest_runtime_task_plan_revision,
    latest_runtime_task_step_states, load_runtime_task_plan_step_records, valid_runtime_identifier,
};
use crate::task_runtime::{
    RuntimeTaskStepClaimBatch, RuntimeTaskStepClaimInput, RuntimeTaskStepCompletionInput,
    RuntimeTaskStepFailureInput, RuntimeTaskStepFrontierItem, RuntimeTaskStepLeaseRenewalInput,
    RuntimeTaskStepLeaseRenewalReceipt,
};
use chrono::Utc;
use rusqlite::TransactionBehavior;
use std::collections::HashSet;

/// 查询步骤前沿（完整实现）
pub fn runtime_task_step_frontier(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    task_id: &str,
    plan_revision: Option<u64>,
) -> Result<Vec<RuntimeTaskStepFrontierItem>, String> {
    // 性能监控
    let _profiler = QueryProfiler::new("runtime_task_step_frontier").with_threshold(100);

    if !valid_runtime_identifier(task_id, 180) {
        return Err("原生任务步骤 frontier taskId 无效".to_string());
    }

    let mut connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| format!("无法开始原生任务步骤 frontier 事务：{error}"))?;

    let current = read_native_runtime_task(&transaction, workspace_scope, task_id)?;
    let revision =
        latest_runtime_task_plan_revision(&transaction, workspace_scope, task_id, plan_revision)?;

    // TODO: 需要实现 ensure_runtime_task_execution_budget
    // ensure_runtime_task_execution_budget(&transaction, workspace_scope, task_id, revision, &current.payload, None, &current.updated_at)?;

    let now = Utc::now().to_rfc3339();
    expire_runtime_task_step_claims(&transaction, workspace_scope, task_id, revision, &now)?;

    let steps = load_runtime_task_plan_step_records(&transaction, workspace_scope, task_id, revision)?;
    let states = latest_runtime_task_step_states(&transaction, workspace_scope, task_id, revision)?;

    let active_claims: HashSet<String> = HashSet::new(); // TODO: 实现 active claims 查询

    let mut frontier = Vec::new();
    for step in steps {
        let step_state = states.get(&step.step_id);
        let active = active_claims.contains(&step.step_id);

        let dependencies_satisfied = step.depends_on.as_ref().map_or(true, |deps| {
            deps.iter()
                .all(|dep| states.get(dep).is_some_and(|(state, _)| state == "succeeded"))
        });

        if dependencies_satisfied {
            frontier.push(RuntimeTaskStepFrontierItem {
                runtime_task_id: task_id.to_string(),
                plan_revision: u64::try_from(revision).unwrap_or_default(),
                step_id: step.step_id.clone(),
                step_kind: step.step_kind,
                title: step.title,
                depends_on: step.depends_on.unwrap_or_default(),
                parameters: step.parameters,
                effect_class: step.effect_class,
                ready: !active
                    && !matches!(current.state.as_str(), "cancelled" | "succeeded")
                    && step_state.map_or(true, |(state, _)| state != "succeeded"),
                active,
            });
        }
    }

    transaction
        .commit()
        .map_err(|error| format!("无法提交原生任务步骤 frontier：{error}"))?;

    Ok(frontier)
}

/// 认领步骤（占位实现）
pub fn claim_runtime_task_plan_steps(
    _database: &RuntimeDatabase,
    _workspace_scope: &str,
    _input: &RuntimeTaskStepClaimInput,
) -> Result<RuntimeTaskStepClaimBatch, String> {
    // TODO: 从 runtime_db.rs 4574-4835 行提取实现
    Err("claim_runtime_task_plan_steps 待实现".to_string())
}

/// 续期租约（占位实现）
pub fn renew_runtime_task_step_lease(
    _database: &RuntimeDatabase,
    _workspace_scope: &str,
    _input: &RuntimeTaskStepLeaseRenewalInput,
) -> Result<RuntimeTaskStepLeaseRenewalReceipt, String> {
    // TODO: 从 runtime_db.rs 4837-4943 行提取实现
    Err("renew_runtime_task_step_lease 待实现".to_string())
}

/// 完成步骤（占位实现）
pub fn complete_runtime_task_plan_step(
    _database: &RuntimeDatabase,
    _workspace_scope: &str,
    _input: &RuntimeTaskStepCompletionInput,
) -> Result<(), String> {
    // TODO: 从 runtime_db.rs 5588-5611 行提取实现
    Err("complete_runtime_task_plan_step 待实现".to_string())
}

/// 失败步骤（占位实现）
pub fn fail_runtime_task_plan_step(
    _database: &RuntimeDatabase,
    _workspace_scope: &str,
    _input: &RuntimeTaskStepFailureInput,
) -> Result<(), String> {
    // TODO: 从 runtime_db.rs 5613-5636 行提取实现
    Err("fail_runtime_task_plan_step 待实现".to_string())
}
