/// TaskPlugin 数据类型定义
///
/// 包含任务管理所需的核心数据结构

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 运行时任务恢复信息
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskRecovery {
    /// 任务 ID
    pub task_id: String,

    /// 恢复建议
    pub recommendation: String,

    /// 恢复步骤 ID
    pub resume_step_id: Option<String>,

    /// 恢复步骤索引
    pub resume_step_index: Option<i64>,

    /// 恢复检查点 ID
    pub resume_checkpoint_id: Option<String>,

    /// 证据列表
    pub evidence: Vec<String>,

    /// 计划版本
    pub plan_revision: Option<u64>,

    /// 完成是否满足
    pub completion_satisfied: Option<bool>,

    /// 缺失的需求 ID
    pub missing_requirement_ids: Vec<String>,

    /// 替换密钥
    pub replacement_key: Option<String>,

    /// 替换任务 ID
    pub replacement_task_id: Option<String>,

    /// 详细信息
    pub detail: String,

    /// 检测时间 (RFC3339)
    pub detected_at: String,
}

/// 运行时任务恢复替换
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskRecoveryReplacement {
    /// 中断的任务 ID
    pub interrupted_task_id: String,

    /// 替换密钥
    pub replacement_key: String,

    /// 替换任务 ID
    pub replacement_task_id: Option<String>,

    /// 状态
    pub state: String,

    /// 更新时间 (RFC3339)
    pub updated_at: String,
}

/// 调度任务
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleOccurrenceTask {
    /// 发生 ID
    pub occurrence_id: String,

    /// 运行时任务 ID
    pub runtime_task_id: String,

    /// 调度版本
    pub schedule_revision: u64,

    /// 负载
    pub payload: Value,

    /// 负载哈希
    pub payload_hash: String,
}

/// 运行时任务计划步骤记录
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskPlanStepRecord {
    /// 步骤 ID
    pub step_id: String,

    /// 步骤类型
    pub step_kind: RuntimeTaskStepKind,

    /// 步骤标题
    pub title: String,

    /// 依赖的步骤 ID 列表
    pub depends_on: Vec<String>,

    /// 参数
    pub parameters: Value,

    /// 效果类别
    pub effect_class: RuntimeTaskStepEffectClass,
}

/// 运行时任务步骤类型
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeTaskStepKind {
    /// 命令执行
    Command,

    /// 并行执行
    Parallel,

    /// 顺序执行
    Sequential,

    /// 条件执行
    Conditional,
}

/// 运行时任务步骤效果类别
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeTaskStepEffectClass {
    /// 只读
    ReadOnly,

    /// 有副作用
    Effectful,

    /// 混合
    Mixed,
}

/// 运行时任务步骤命令绑定
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeTaskStepCommandBinding {
    /// 原生绑定
    Native,

    /// 子任务绑定
    Child,

    /// 外部绑定
    External,
}

/// 运行时任务状态
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeTaskState {
    /// 等待中
    Pending,

    /// 运行中
    Running,

    /// 已完成
    Completed,

    /// 已失败
    Failed,

    /// 已取消
    Cancelled,
}

impl RuntimeTaskState {
    /// 验证状态字符串是否有效
    pub fn is_valid(state: &str) -> bool {
        matches!(
            state,
            "pending" | "running" | "completed" | "failed" | "cancelled"
        )
    }

    /// 从字符串解析状态
    pub fn from_str(state: &str) -> Option<Self> {
        match state {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    /// 转换为字符串
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// 运行时任务契约
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTaskContract {
    /// 任务 ID
    pub task_id: String,

    /// 工作区范围
    pub workspace_scope: String,

    /// 任务类型
    pub task_kind: String,

    /// 任务状态
    pub state: String,

    /// 创建时间 (RFC3339)
    pub created_at: String,

    /// 更新时间 (RFC3339)
    pub updated_at: String,

    /// 计划版本
    pub plan_revision: Option<u64>,
}

/// 运行时任务
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTask {
    /// 任务契约
    pub contract: RuntimeTaskContract,

    /// 任务负载
    pub payload: Value,

    /// 任务结果
    pub result: Option<Value>,

    /// 错误信息
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_task_state_is_valid() {
        assert!(RuntimeTaskState::is_valid("pending"));
        assert!(RuntimeTaskState::is_valid("running"));
        assert!(RuntimeTaskState::is_valid("completed"));
        assert!(RuntimeTaskState::is_valid("failed"));
        assert!(RuntimeTaskState::is_valid("cancelled"));
        assert!(!RuntimeTaskState::is_valid("unknown"));
    }

    #[test]
    fn test_runtime_task_state_from_str() {
        assert_eq!(
            RuntimeTaskState::from_str("pending"),
            Some(RuntimeTaskState::Pending)
        );
        assert_eq!(
            RuntimeTaskState::from_str("running"),
            Some(RuntimeTaskState::Running)
        );
        assert_eq!(RuntimeTaskState::from_str("unknown"), None);
    }

    #[test]
    fn test_runtime_task_state_as_str() {
        assert_eq!(RuntimeTaskState::Pending.as_str(), "pending");
        assert_eq!(RuntimeTaskState::Running.as_str(), "running");
        assert_eq!(RuntimeTaskState::Completed.as_str(), "completed");
    }

    #[test]
    fn test_runtime_task_state_serialization() {
        let state = RuntimeTaskState::Running;
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(json, "\"running\"");
    }

    #[test]
    fn test_runtime_task_step_kind_serialization() {
        let kind = RuntimeTaskStepKind::Command;
        let json = serde_json::to_string(&kind).unwrap();
        assert_eq!(json, "\"command\"");
    }

    #[test]
    fn test_runtime_task_step_effect_class() {
        let effect = RuntimeTaskStepEffectClass::ReadOnly;
        let json = serde_json::to_string(&effect).unwrap();
        assert_eq!(json, "\"readonly\"");
    }
}
