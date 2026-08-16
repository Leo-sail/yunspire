/// 任务管理模块
///
/// 从 runtime_db.rs 提取的任务管理功能
/// 包含任务生命周期、步骤管理、恢复机制等

pub mod query;
pub mod lifecycle;
pub mod steps;
pub mod recovery;
pub mod types;

// 重新导出核心类型
pub use types::*;

// 重新导出核心函数
pub use query::{runtime_task, runtime_task_contract, list_runtime_tasks};
pub use lifecycle::{define_runtime_task_plan, transition_native_runtime_task};
pub use steps::{
    claim_runtime_task_plan_steps, complete_runtime_task_plan_step,
    fail_runtime_task_plan_step, renew_runtime_task_step_lease,
    runtime_task_step_frontier,
};
pub use recovery::{
    bind_runtime_task_recovery_replacement, recover_interrupted_runtime_tasks,
    resolve_runtime_task_recovery, supersede_runtime_task_for_recovery,
};
