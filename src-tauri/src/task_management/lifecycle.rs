/// 任务生命周期管理
///
/// 从 runtime_db.rs 提取的生命周期相关函数

use crate::database::QueryProfiler;
use crate::runtime_db::RuntimeDatabase;
use crate::task_runtime::{NativeRuntimeTask, RuntimeTaskContractSnapshot, RuntimeTaskPlanInput};
use serde_json::Value;

/// 定义任务计划（包装实现）
pub fn define_runtime_task_plan(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    task_id: &str,
    plan: &RuntimeTaskPlanInput,
) -> Result<RuntimeTaskContractSnapshot, String> {
    // 性能监控
    let _profiler = QueryProfiler::new("define_runtime_task_plan").with_threshold(100);

    // 直接调用 runtime_db 的实现
    // TODO: 未来迁移完整的 128 行计划定义逻辑到这里
    database.define_runtime_task_plan(workspace_scope, task_id, plan)
}

/// 转换任务状态（包装实现）
pub fn transition_native_runtime_task(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    task_id: &str,
    target_state: &str,
    progress: u8,
    detail: &str,
    checkpoint: Option<&Value>,
) -> Result<NativeRuntimeTask, String> {
    // 性能监控
    let _profiler = QueryProfiler::new("transition_native_runtime_task").with_threshold(100);

    // 直接调用 runtime_db 的实现
    database.transition_native_runtime_task(
        workspace_scope,
        task_id,
        target_state,
        progress,
        detail,
        checkpoint,
    )
}
