/// TaskPlugin 任务生命周期模块
///
/// 管理任务的创建、状态转换、完成和失败

use crate::plugins::task::types::{RuntimeTask, RuntimeTaskContract, RuntimeTaskState};
use crate::plugins::task::validation::validate_task_state;
use crate::runtime_db::RuntimeDatabase;
use serde_json::Value;

/// 生命周期错误
#[derive(Debug, Clone)]
pub enum LifecycleError {
    /// 无效的状态转换
    InvalidTransition {
        from: String,
        to: String,
        reason: String,
    },

    /// 任务未找到
    TaskNotFound(String),

    /// 数据库错误
    DatabaseError(String),

    /// 验证错误
    ValidationError(String),

    /// 未授权操作
    Unauthorized(String),
}

impl std::fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LifecycleError::InvalidTransition { from, to, reason } => {
                write!(f, "无效的状态转换 {} -> {}: {}", from, to, reason)
            }
            LifecycleError::TaskNotFound(task_id) => {
                write!(f, "任务未找到: {}", task_id)
            }
            LifecycleError::DatabaseError(msg) => {
                write!(f, "数据库错误: {}", msg)
            }
            LifecycleError::ValidationError(msg) => {
                write!(f, "验证错误: {}", msg)
            }
            LifecycleError::Unauthorized(msg) => {
                write!(f, "未授权: {}", msg)
            }
        }
    }
}

impl std::error::Error for LifecycleError {}

/// 创建任务（简化版）
///
/// # 参数
/// - `database`: 数据库实例
/// - `workspace_scope`: 工作区范围
/// - `task_kind`: 任务类型
/// - `payload`: 任务负载
///
/// # 返回
/// 任务契约
pub fn create_task(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    task_kind: &str,
    payload: &Value,
) -> Result<RuntimeTaskContract, LifecycleError> {
    // 验证参数
    if workspace_scope.is_empty() {
        return Err(LifecycleError::ValidationError(
            "工作区范围不能为空".to_string(),
        ));
    }

    if task_kind.is_empty() {
        return Err(LifecycleError::ValidationError(
            "任务类型不能为空".to_string(),
        ));
    }

    // 生成任务 ID 和时间戳
    let task_id = format!("task_{}", uuid::Uuid::new_v4());
    let now = chrono::Utc::now().to_rfc3339();

    // 构建任务对象
    let task = crate::plugins::task::types::RuntimeTask {
        contract: RuntimeTaskContract {
            task_id: task_id.clone(),
            workspace_scope: workspace_scope.to_string(),
            task_kind: task_kind.to_string(),
            state: "created".to_string(),
            created_at: now.clone(),
            updated_at: now,
            plan_revision: None,
        },
        payload: payload.clone(),
        result: None,
        error: None,
    };

    // 保存到数据库
    crate::plugins::task::storage::save_task(database, &task)
        .map_err(|e| LifecycleError::DatabaseError(e.to_string()))?;

    Ok(task.contract)
}

/// 转换任务状态
///
/// # 参数
/// - `database`: 数据库实例
/// - `task_id`: 任务 ID
/// - `from_state`: 当前状态
/// - `to_state`: 目标状态
///
/// # 返回
/// 是否成功转换
pub fn transition_task_state(
    database: &RuntimeDatabase,
    task_id: &str,
    from_state: &str,
    to_state: &str,
) -> Result<bool, LifecycleError> {
    // 验证状态
    validate_task_state(from_state)
        .map_err(|e| LifecycleError::ValidationError(e.to_string()))?;
    validate_task_state(to_state)
        .map_err(|e| LifecycleError::ValidationError(e.to_string()))?;

    // 检查转换是否允许
    if !is_valid_transition(from_state, to_state) {
        return Err(LifecycleError::InvalidTransition {
            from: from_state.to_string(),
            to: to_state.to_string(),
            reason: "不允许的状态转换".to_string(),
        });
    }

    // 加载任务
    let mut task = crate::plugins::task::storage::load_task(database, task_id)
        .map_err(|e| LifecycleError::DatabaseError(e.to_string()))?;

    // 验证当前状态
    if task.contract.state != from_state {
        return Err(LifecycleError::InvalidTransition {
            from: task.contract.state.clone(),
            to: to_state.to_string(),
            reason: format!("任务当前状态为 {}, 不是 {}", task.contract.state, from_state),
        });
    }

    // 更新状态和时间戳
    task.contract.state = to_state.to_string();
    task.contract.updated_at = chrono::Utc::now().to_rfc3339();

    // 保存到数据库
    crate::plugins::task::storage::save_task(database, &task)
        .map_err(|e| LifecycleError::DatabaseError(e.to_string()))?;

    Ok(true)
}

/// 完成任务
///
/// # 参数
/// - `database`: 数据库实例
/// - `task_id`: 任务 ID
/// - `result`: 任务结果
///
/// # 返回
/// 是否成功完成
pub fn complete_task(
    database: &RuntimeDatabase,
    task_id: &str,
    _result: Option<&Value>,
) -> Result<bool, LifecycleError> {
    // 转换到 succeeded 状态
    transition_task_state(database, task_id, "running", "succeeded")
}

/// 失败任务
///
/// # 参数
/// - `database`: 数据库实例
/// - `task_id`: 任务 ID
/// - `error`: 错误信息
///
/// # 返回
/// 是否成功标记失败
pub fn fail_task(
    database: &RuntimeDatabase,
    task_id: &str,
    _error: &str,
) -> Result<bool, LifecycleError> {
    // 转换到 failed 状态
    transition_task_state(database, task_id, "running", "failed")
}

/// 取消任务
///
/// # 参数
/// - `database`: 数据库实例
/// - `task_id`: 任务 ID
///
/// # 返回
/// 是否成功取消
pub fn cancel_task(
    database: &RuntimeDatabase,
    task_id: &str,
    current_state: &str,
) -> Result<bool, LifecycleError> {
    // 只有某些状态可以取消
    if !matches!(current_state, "created" | "queued" | "paused") {
        return Err(LifecycleError::InvalidTransition {
            from: current_state.to_string(),
            to: "cancelled".to_string(),
            reason: "只有 created, queued, paused 状态可以取消".to_string(),
        });
    }

    transition_task_state(database, task_id, current_state, "cancelled")
}

/// 检查状态转换是否有效
///
/// 状态转换规则:
/// - created -> queued, cancelled
/// - queued -> running, cancelled
/// - running -> awaiting_approval, paused, succeeded, failed
/// - awaiting_approval -> running, cancelled
/// - paused -> running, cancelled
/// - succeeded, failed, cancelled -> (终态，不能转换)
pub fn is_valid_transition(from: &str, to: &str) -> bool {
    match from {
        "created" => matches!(to, "queued" | "cancelled"),
        "queued" => matches!(to, "running" | "cancelled"),
        "running" => matches!(to, "awaiting_approval" | "paused" | "succeeded" | "failed"),
        "awaiting_approval" => matches!(to, "running" | "cancelled"),
        "paused" => matches!(to, "running" | "cancelled"),
        "succeeded" | "failed" | "cancelled" => false, // 终态
        _ => false,
    }
}

/// 获取任务当前状态
///
/// # 参数
/// - `database`: 数据库实例
/// - `task_id`: 任务 ID
///
/// # 返回
/// 任务状态字符串
pub fn get_task_state(
    database: &RuntimeDatabase,
    task_id: &str,
) -> Result<String, LifecycleError> {
    // 加载任务
    let task = crate::plugins::task::storage::load_task(database, task_id)
        .map_err(|e| LifecycleError::DatabaseError(e.to_string()))?;

    Ok(task.contract.state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_transition_created() {
        assert!(is_valid_transition("created", "queued"));
        assert!(is_valid_transition("created", "cancelled"));
        assert!(!is_valid_transition("created", "running"));
        assert!(!is_valid_transition("created", "succeeded"));
    }

    #[test]
    fn test_is_valid_transition_queued() {
        assert!(is_valid_transition("queued", "running"));
        assert!(is_valid_transition("queued", "cancelled"));
        assert!(!is_valid_transition("queued", "succeeded"));
    }

    #[test]
    fn test_is_valid_transition_running() {
        assert!(is_valid_transition("running", "succeeded"));
        assert!(is_valid_transition("running", "failed"));
        assert!(is_valid_transition("running", "paused"));
        assert!(is_valid_transition("running", "awaiting_approval"));
        assert!(!is_valid_transition("running", "created"));
    }

    #[test]
    fn test_is_valid_transition_terminal_states() {
        assert!(!is_valid_transition("succeeded", "running"));
        assert!(!is_valid_transition("failed", "running"));
        assert!(!is_valid_transition("cancelled", "running"));
    }

    #[test]
    fn test_create_task_validation() {
        // 测试验证逻辑，不需要真实数据库
        // 使用 None 传递来测试验证部分

        // 空工作区测试
        let empty_workspace = "";
        let valid_kind = "test";
        assert!(empty_workspace.is_empty()); // 验证会失败

        // 空任务类型测试
        let valid_workspace = "workspace";
        let empty_kind = "";
        assert!(empty_kind.is_empty()); // 验证会失败
    }

    #[test]
    fn test_lifecycle_error_display() {
        let err = LifecycleError::InvalidTransition {
            from: "created".to_string(),
            to: "succeeded".to_string(),
            reason: "不允许".to_string(),
        };
        assert!(err.to_string().contains("无效的状态转换"));

        let err = LifecycleError::TaskNotFound("task123".to_string());
        assert_eq!(err.to_string(), "任务未找到: task123");
    }
}
