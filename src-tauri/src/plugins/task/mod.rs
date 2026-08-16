pub mod lifecycle;
pub mod types;
pub mod validation;

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
