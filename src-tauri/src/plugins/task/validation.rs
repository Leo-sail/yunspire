/// TaskPlugin 任务验证模块
///
/// 提供任务状态、计划、步骤的验证功能

use crate::plugins::task::types::{
    RuntimeTaskState, RuntimeTaskStepCommandBinding, RuntimeTaskStepEffectClass,
    RuntimeTaskStepKind,
};

/// 验证错误
#[derive(Debug, Clone)]
pub enum ValidationError {
    /// 无效的任务状态
    InvalidTaskState(String),

    /// 无效的步骤类型
    InvalidStepKind(String),

    /// 无效的命令绑定
    InvalidCommandBinding(String),

    /// 无效的效果类别
    InvalidEffectClass(String),

    /// 缺失必填字段
    MissingField(String),

    /// 无效的步骤依赖
    InvalidDependency(String),

    /// 循环依赖
    CircularDependency(Vec<String>),
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::InvalidTaskState(state) => {
                write!(f, "无效的任务状态: {}", state)
            }
            ValidationError::InvalidStepKind(kind) => {
                write!(f, "无效的步骤类型: {}", kind)
            }
            ValidationError::InvalidCommandBinding(binding) => {
                write!(f, "无效的命令绑定: {}", binding)
            }
            ValidationError::InvalidEffectClass(class) => {
                write!(f, "无效的效果类别: {}", class)
            }
            ValidationError::MissingField(field) => {
                write!(f, "缺失必填字段: {}", field)
            }
            ValidationError::InvalidDependency(msg) => {
                write!(f, "无效的步骤依赖: {}", msg)
            }
            ValidationError::CircularDependency(steps) => {
                write!(f, "检测到循环依赖: {}", steps.join(" -> "))
            }
        }
    }
}

impl std::error::Error for ValidationError {}

/// 验证任务状态
///
/// 检查状态字符串是否为有效的任务状态
///
/// 有效状态:
/// - created, queued, running
/// - awaiting_approval, paused
/// - succeeded, failed, cancelled
pub fn validate_task_state(state: &str) -> Result<(), ValidationError> {
    let valid_states = [
        "created",
        "queued",
        "running",
        "awaiting_approval",
        "paused",
        "succeeded",
        "failed",
        "cancelled",
    ];

    if valid_states.contains(&state) {
        Ok(())
    } else {
        Err(ValidationError::InvalidTaskState(state.to_string()))
    }
}

/// 验证任务状态（使用枚举）
pub fn validate_task_state_enum(state: &RuntimeTaskState) -> Result<(), ValidationError> {
    // RuntimeTaskState 是类型安全的枚举，总是有效
    let _ = state;
    Ok(())
}

/// 验证步骤类型
pub fn validate_step_kind(kind: &RuntimeTaskStepKind) -> Result<(), ValidationError> {
    // RuntimeTaskStepKind 是类型安全的枚举，总是有效
    let _ = kind;
    Ok(())
}

/// 验证命令绑定
pub fn validate_command_binding(
    binding: &RuntimeTaskStepCommandBinding,
) -> Result<(), ValidationError> {
    // RuntimeTaskStepCommandBinding 是类型安全的枚举，总是有效
    let _ = binding;
    Ok(())
}

/// 验证效果类别
pub fn validate_effect_class(
    class: &RuntimeTaskStepEffectClass,
) -> Result<(), ValidationError> {
    // RuntimeTaskStepEffectClass 是类型安全的枚举，总是有效
    let _ = class;
    Ok(())
}

/// 验证步骤依赖
///
/// 检查步骤依赖是否存在循环
pub fn validate_step_dependencies(
    steps: &[(String, Vec<String>)],
) -> Result<(), ValidationError> {
    // 构建依赖图
    let mut graph: std::collections::HashMap<&str, Vec<&str>> =
        std::collections::HashMap::new();

    for (step_id, deps) in steps {
        graph.insert(
            step_id.as_str(),
            deps.iter().map(|s| s.as_str()).collect(),
        );
    }

    // 检测循环依赖（使用 DFS）
    for (step_id, _) in steps {
        let mut visited = std::collections::HashSet::new();
        let mut path = Vec::new();

        if has_cycle(&graph, step_id.as_str(), &mut visited, &mut path) {
            return Err(ValidationError::CircularDependency(
                path.iter().map(|s| s.to_string()).collect(),
            ));
        }
    }

    Ok(())
}

/// 检测循环依赖（DFS）
fn has_cycle(
    graph: &std::collections::HashMap<&str, Vec<&str>>,
    node: &str,
    visited: &mut std::collections::HashSet<String>,
    path: &mut Vec<String>,
) -> bool {
    if path.contains(&node.to_string()) {
        path.push(node.to_string());
        return true;
    }

    if visited.contains(node) {
        return false;
    }

    visited.insert(node.to_string());
    path.push(node.to_string());

    if let Some(deps) = graph.get(node) {
        for dep in deps {
            if has_cycle(graph, dep, visited, path) {
                return true;
            }
        }
    }

    path.pop();
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_task_state_valid() {
        assert!(validate_task_state("created").is_ok());
        assert!(validate_task_state("running").is_ok());
        assert!(validate_task_state("succeeded").is_ok());
        assert!(validate_task_state("failed").is_ok());
    }

    #[test]
    fn test_validate_task_state_invalid() {
        let result = validate_task_state("unknown");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ValidationError::InvalidTaskState(_)
        ));
    }

    #[test]
    fn test_validate_task_state_enum() {
        assert!(validate_task_state_enum(&RuntimeTaskState::Pending).is_ok());
        assert!(validate_task_state_enum(&RuntimeTaskState::Running).is_ok());
        assert!(validate_task_state_enum(&RuntimeTaskState::Completed).is_ok());
    }

    #[test]
    fn test_validate_step_dependencies_valid() {
        let steps = vec![
            ("step1".to_string(), vec![]),
            ("step2".to_string(), vec!["step1".to_string()]),
            ("step3".to_string(), vec!["step2".to_string()]),
        ];

        assert!(validate_step_dependencies(&steps).is_ok());
    }

    #[test]
    fn test_validate_step_dependencies_circular() {
        let steps = vec![
            ("step1".to_string(), vec!["step2".to_string()]),
            ("step2".to_string(), vec!["step1".to_string()]),
        ];

        let result = validate_step_dependencies(&steps);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ValidationError::CircularDependency(_)
        ));
    }

    #[test]
    fn test_validate_step_dependencies_complex_cycle() {
        let steps = vec![
            ("step1".to_string(), vec!["step2".to_string()]),
            ("step2".to_string(), vec!["step3".to_string()]),
            ("step3".to_string(), vec!["step1".to_string()]),
        ];

        let result = validate_step_dependencies(&steps);
        assert!(result.is_err());
    }

    #[test]
    fn test_validation_error_display() {
        let err = ValidationError::InvalidTaskState("unknown".to_string());
        assert_eq!(err.to_string(), "无效的任务状态: unknown");

        let err = ValidationError::CircularDependency(vec![
            "step1".to_string(),
            "step2".to_string(),
            "step1".to_string(),
        ]);
        assert!(err.to_string().contains("循环依赖"));
    }
}
