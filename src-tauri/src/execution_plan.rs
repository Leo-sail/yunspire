use serde::{Deserialize, Serialize};

/// AI 执行计划
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionPlan {
    /// 任务 ID
    pub task_id: String,
    /// 用户意图
    pub intent: String,
    /// 计划步骤
    pub steps: Vec<PlannedStep>,
    /// 执行解释
    pub explanation: String,
    /// 潜在风险
    pub risks: Vec<String>,
    /// 是否需要用户确认
    pub user_choice_required: bool,
}

/// 计划步骤
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedStep {
    /// 步骤序号
    pub step_number: usize,
    /// 步骤描述
    pub description: String,
    /// 操作类型
    pub operation_type: OperationType,
    /// 预期结果
    pub expected_outcome: String,
    /// 是否可逆
    pub reversible: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationType {
    Read,
    Write,
    Delete,
    Network,
    Analysis,
}

/// 用户决策
#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserDecision {
    /// 任务 ID
    pub task_id: String,
    /// 决策
    pub decision: Decision,
    /// 修改后的计划（如果用户选择修改）
    pub modified_plan: Option<ExecutionPlan>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approve,
    Reject,
    Modify,
}

/// 执行日志
#[allow(dead_code)]
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionLog {
    /// 任务 ID
    pub task_id: String,
    /// 步骤序号
    pub step_number: usize,
    /// 执行状态
    pub status: ExecutionStatus,
    /// 实际结果
    pub actual_outcome: String,
    /// 执行时间戳
    pub timestamp: String,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Pending,
    Running,
    Success,
    Failed,
    Skipped,
}

/// 生成执行计划（示例）
#[tauri::command]
pub fn generate_execution_plan(intent: String) -> Result<ExecutionPlan, String> {
    // TODO: 实际实现需要调用 AI 模型生成计划
    Ok(ExecutionPlan {
        task_id: uuid::Uuid::new_v4().to_string(),
        intent: intent.clone(),
        steps: vec![
            PlannedStep {
                step_number: 1,
                description: "读取相关文件".to_string(),
                operation_type: OperationType::Read,
                expected_outcome: "获取文件内容".to_string(),
                reversible: true,
            },
            PlannedStep {
                step_number: 2,
                description: "分析内容并生成建议".to_string(),
                operation_type: OperationType::Analysis,
                expected_outcome: "生成改进建议".to_string(),
                reversible: true,
            },
        ],
        explanation: format!("我将通过以下步骤完成「{intent}」"),
        risks: vec![],
        user_choice_required: false,
    })
}

/// 提交用户决策
#[tauri::command]
pub fn submit_execution_decision(decision: UserDecision) -> Result<String, String> {
    // TODO: 保存决策并继续执行
    match decision.decision {
        Decision::Approve => Ok("已批准执行".to_string()),
        Decision::Reject => Ok("已拒绝执行".to_string()),
        Decision::Modify => Ok("已修改计划".to_string()),
    }
}

/// 获取执行日志
#[tauri::command]
pub fn get_execution_logs(_task_id: String) -> Result<Vec<ExecutionLog>, String> {
    // TODO: 从数据库读取执行日志
    Ok(vec![])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_plan() {
        let result = generate_execution_plan("创建新笔记".to_string());
        assert!(result.is_ok());
        let plan = result.unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert!(plan.task_id.len() > 0);
    }

    #[test]
    fn test_user_decision() {
        let decision = UserDecision {
            task_id: "test-task".to_string(),
            decision: Decision::Approve,
            modified_plan: None,
        };
        let result = submit_execution_decision(decision);
        assert!(result.is_ok());
    }
}
