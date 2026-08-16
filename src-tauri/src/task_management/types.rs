/// 任务管理数据类型
///
/// 从 runtime_db.rs 提取的任务管理相关数据结构

use serde::{Deserialize, Serialize};

/// 任务计划步骤记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeTaskPlanStepRecord {
    pub step_id: String,
    pub step_kind: String,
    pub title: String,
    pub depends_on: Option<Vec<String>>,
    pub parameters: serde_json::Value,
    pub effect_class: String,
}

/// 任务恢复信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeTaskRecovery {
    pub task_id: String,
    pub recommendation: String,
    pub resume_step_id: Option<String>,
    pub resume_step_index: Option<usize>,
    pub resume_checkpoint_id: Option<String>,
    pub evidence: Option<String>,
    pub plan_revision: Option<u64>,
    pub completion_satisfied: bool,
    pub missing_requirement_ids: Vec<String>,
    pub replacement_key: Option<String>,
    pub replacement_task_id: Option<String>,
    pub detail: Option<String>,
    pub detected_at: String,
}

/// 任务恢复替换信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeTaskRecoveryReplacement {
    pub interrupted_task_id: String,
    pub replacement_key: String,
    pub replacement_task_id: String,
    pub state: String,
    pub updated_at: String,
}
