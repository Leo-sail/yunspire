pub mod lifecycle;
pub mod plugin;
pub mod recovery;
pub mod steps;
pub mod storage;
pub mod types;
pub mod validation;

pub use plugin::TaskPlugin;

pub use types::{
    RuntimeTask, RuntimeTaskContract, RuntimeTaskPlanStepRecord, RuntimeTaskRecovery,
    RuntimeTaskRecoveryReplacement, RuntimeTaskState, RuntimeTaskStepCommandBinding,
    RuntimeTaskStepEffectClass, RuntimeTaskStepKind, ScheduleOccurrenceTask,
};

pub use validation::{
    validate_command_binding, validate_effect_class, validate_step_dependencies,
    validate_step_kind, validate_task_state, validate_task_state_enum, ValidationError,
};

pub use lifecycle::{
    cancel_task, complete_task, create_task, fail_task, get_task_state, is_valid_transition,
    transition_task_state, LifecycleError,
};

pub use steps::{
    claim_steps, complete_step, fail_step, get_step_frontier, release_step_lease,
    renew_step_lease, StepClaimResult, StepError, StepLease,
};

pub use recovery::{
    bind_recovery_replacement, get_task_recovery, recover_interrupted_tasks, resolve_recovery,
    supersede_task, RecoveryError, RecoveryRecommendation,
};

pub use storage::{
    delete_task, list_tasks, load_task, save_task, task_statistics, StorageError, TaskFilters,
    TaskStatistics,
};
