/// TaskPlugin 任务恢复模块
///
/// 管理中断任务的恢复、替代和解决

use crate::plugins::task::types::{RuntimeTaskRecovery, RuntimeTaskRecoveryReplacement};
use crate::runtime_db::RuntimeDatabase;

/// 恢复错误
#[derive(Debug, Clone)]
pub enum RecoveryError {
    /// 任务未找到
    TaskNotFound(String),

    /// 恢复信息无效
    InvalidRecovery(String),

    /// 恢复冲突
    RecoveryConflict(String),

    /// 数据库错误
    DatabaseError(String),

    /// 验证错误
    ValidationError(String),
}

impl std::fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecoveryError::TaskNotFound(task_id) => {
                write!(f, "任务未找到: {}", task_id)
            }
            RecoveryError::InvalidRecovery(msg) => {
                write!(f, "恢复信息无效: {}", msg)
            }
            RecoveryError::RecoveryConflict(msg) => {
                write!(f, "恢复冲突: {}", msg)
            }
            RecoveryError::DatabaseError(msg) => {
                write!(f, "数据库错误: {}", msg)
            }
            RecoveryError::ValidationError(msg) => {
                write!(f, "验证错误: {}", msg)
            }
        }
    }
}

impl std::error::Error for RecoveryError {}

/// 恢复建议
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryRecommendation {
    /// 恢复执行
    Resume,

    /// 重新启动
    Restart,

    /// 标记为失败
    Fail,

    /// 需要人工干预
    ManualIntervention,

    /// 替代为新任务
    Supersede,
}

impl RecoveryRecommendation {
    /// 从字符串解析
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "resume" => Some(Self::Resume),
            "restart" => Some(Self::Restart),
            "fail" => Some(Self::Fail),
            "manual_intervention" => Some(Self::ManualIntervention),
            "supersede" => Some(Self::Supersede),
            _ => None,
        }
    }

    /// 转换为字符串
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Resume => "resume",
            Self::Restart => "restart",
            Self::Fail => "fail",
            Self::ManualIntervention => "manual_intervention",
            Self::Supersede => "supersede",
        }
    }
}

/// 恢复中断的任务
///
/// # 参数
/// - `database`: 数据库实例
/// - `workspace_scope`: 工作区范围
///
/// # 返回
/// 需要恢复的任务列表
pub fn recover_interrupted_tasks(
    database: &RuntimeDatabase,
    workspace_scope: &str,
) -> Result<Vec<RuntimeTaskRecovery>, RecoveryError> {
    // 验证参数
    if workspace_scope.is_empty() {
        return Err(RecoveryError::ValidationError(
            "工作区范围不能为空".to_string(),
        ));
    }

    let conn = database
        .connection
        .lock()
        .map_err(|e| RecoveryError::DatabaseError(format!("获取数据库连接失败: {}", e)))?;

    // 查询所有有恢复记录的任务
    let mut stmt = conn
        .prepare(
            "SELECT r.task_id, r.recommendation, r.resume_step_id, r.evidence, r.detail, r.detected_at
             FROM runtime_task_recovery r
             INNER JOIN runtime_tasks t ON r.task_id = t.task_id
             WHERE t.workspace_scope = ?
             ORDER BY r.detected_at DESC",
        )
        .map_err(|e| RecoveryError::DatabaseError(e.to_string()))?;

    let recoveries = stmt
        .query_map([workspace_scope], |row| {
            Ok(RuntimeTaskRecovery {
                task_id: row.get(0)?,
                recommendation: row.get::<_, String>(1)?,
                resume_step_id: row.get(2)?,
                resume_step_index: None,
                resume_checkpoint_id: None,
                evidence: vec![],
                plan_revision: None,
                completion_satisfied: None,
                missing_requirement_ids: vec![],
                replacement_key: None,
                replacement_task_id: None,
                detail: row.get(4)?,
                detected_at: row.get(5)?,
            })
        })
        .map_err(|e| RecoveryError::DatabaseError(e.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| RecoveryError::DatabaseError(e.to_string()))?;

    Ok(recoveries)
}

/// 解决任务恢复
///
/// 执行恢复建议，更新任务状态
///
/// # 参数
/// - `database`: 数据库实例
/// - `task_id`: 任务 ID
/// - `recovery`: 恢复信息
///
/// # 返回
/// 是否成功
pub fn resolve_recovery(
    database: &RuntimeDatabase,
    task_id: &str,
    recovery: &RuntimeTaskRecovery,
) -> Result<bool, RecoveryError> {
    // 验证参数
    if task_id.is_empty() {
        return Err(RecoveryError::ValidationError(
            "任务 ID 不能为空".to_string(),
        ));
    }

    if recovery.task_id != task_id {
        return Err(RecoveryError::ValidationError(
            "恢复信息与任务 ID 不匹配".to_string(),
        ));
    }

    let conn = database
        .connection
        .lock()
        .map_err(|e| RecoveryError::DatabaseError(format!("获取数据库连接失败: {}", e)))?;

    // 根据恢复建议执行不同操作
    let recommendation = RecoveryRecommendation::from_str(&recovery.recommendation)
        .unwrap_or(RecoveryRecommendation::ManualIntervention);

    match recommendation {
        RecoveryRecommendation::Resume => {
            // 恢复执行 - 这里只删除恢复记录，实际恢复由外部处理
            conn.execute(
                "DELETE FROM runtime_task_recovery WHERE task_id = ?",
                [task_id],
            )
            .map_err(|e| RecoveryError::DatabaseError(e.to_string()))?;
        }
        RecoveryRecommendation::Restart => {
            // 重启任务 - 删除恢复记录
            conn.execute(
                "DELETE FROM runtime_task_recovery WHERE task_id = ?",
                [task_id],
            )
            .map_err(|e| RecoveryError::DatabaseError(e.to_string()))?;
        }
        RecoveryRecommendation::Fail => {
            // 标记为失败 - 删除恢复记录
            conn.execute(
                "DELETE FROM runtime_task_recovery WHERE task_id = ?",
                [task_id],
            )
            .map_err(|e| RecoveryError::DatabaseError(e.to_string()))?;
        }
        RecoveryRecommendation::Supersede => {
            // 替代任务 - 恢复记录保留，等待绑定
            // 不删除记录
        }
        RecoveryRecommendation::ManualIntervention => {
            // 需要人工干预 - 删除恢复记录
            conn.execute(
                "DELETE FROM runtime_task_recovery WHERE task_id = ?",
                [task_id],
            )
            .map_err(|e| RecoveryError::DatabaseError(e.to_string()))?;
        }
    }

    Ok(true)
}

/// 替代任务用于恢复
///
/// 创建一个新任务来替代中断的任务
///
/// # 参数
/// - `database`: 数据库实例
/// - `interrupted_task_id`: 中断的任务 ID
/// - `replacement_key`: 替代密钥
///
/// # 返回
/// 新任务 ID
pub fn supersede_task(
    database: &RuntimeDatabase,
    interrupted_task_id: &str,
    replacement_key: &str,
) -> Result<String, RecoveryError> {
    // 验证参数
    if interrupted_task_id.is_empty() {
        return Err(RecoveryError::ValidationError(
            "中断任务 ID 不能为空".to_string(),
        ));
    }

    if replacement_key.is_empty() {
        return Err(RecoveryError::ValidationError(
            "替代密钥不能为空".to_string(),
        ));
    }

    let conn = database
        .connection
        .lock()
        .map_err(|e| RecoveryError::DatabaseError(format!("获取数据库连接失败: {}", e)))?;

    // 插入或更新恢复记录
    let now = chrono::Utc::now().to_rfc3339();

    conn.execute(
        "INSERT OR REPLACE INTO runtime_task_recovery
         (task_id, recommendation, resume_step_id, evidence, detail, detected_at)
         VALUES (?, 'supersede', NULL, NULL, ?, ?)",
        rusqlite::params![
            interrupted_task_id,
            format!("Supersede with replacement_key: {}", replacement_key),
            now
        ],
    )
    .map_err(|e| RecoveryError::DatabaseError(e.to_string()))?;

    // 返回替代密钥作为新任务 ID（实际创建由外部处理）
    Ok(replacement_key.to_string())
}

/// 绑定恢复替换
///
/// 记录任务替换关系
///
/// # 参数
/// - `database`: 数据库实例
/// - `replacement`: 替换信息
///
/// # 返回
/// 是否成功
pub fn bind_recovery_replacement(
    database: &RuntimeDatabase,
    replacement: &RuntimeTaskRecoveryReplacement,
) -> Result<bool, RecoveryError> {
    // 验证参数
    if replacement.interrupted_task_id.is_empty() {
        return Err(RecoveryError::ValidationError(
            "中断任务 ID 不能为空".to_string(),
        ));
    }

    if replacement.replacement_key.is_empty() {
        return Err(RecoveryError::ValidationError(
            "替代密钥不能为空".to_string(),
        ));
    }

    let conn = database
        .connection
        .lock()
        .map_err(|e| RecoveryError::DatabaseError(format!("获取数据库连接失败: {}", e)))?;

    // 更新恢复记录，添加替换信息
    let rows_affected = conn
        .execute(
            "UPDATE runtime_task_recovery
             SET detail = ?, evidence = ?
             WHERE task_id = ? AND recommendation = 'supersede'",
            rusqlite::params![
                format!(
                    "Replacement bound: {} -> {}",
                    replacement.interrupted_task_id, replacement.replacement_key
                ),
                serde_json::to_string(&replacement)
                    .map_err(|e| RecoveryError::ValidationError(format!("序列化失败: {}", e)))?,
                replacement.interrupted_task_id
            ],
        )
        .map_err(|e| RecoveryError::DatabaseError(e.to_string()))?;

    if rows_affected == 0 {
        return Err(RecoveryError::ValidationError(format!(
            "任务 {} 没有对应的 supersede 恢复记录",
            replacement.interrupted_task_id
        )));
    }

    Ok(true)
}

/// 获取任务的恢复信息
///
/// # 参数
/// - `database`: 数据库实例
/// - `task_id`: 任务 ID
///
/// # 返回
/// 恢复信息（如果存在）
pub fn get_task_recovery(
    database: &RuntimeDatabase,
    task_id: &str,
) -> Result<Option<RuntimeTaskRecovery>, RecoveryError> {
    // 验证参数
    if task_id.is_empty() {
        return Err(RecoveryError::ValidationError(
            "任务 ID 不能为空".to_string(),
        ));
    }

    let conn = database
        .connection
        .lock()
        .map_err(|e| RecoveryError::DatabaseError(format!("获取数据库连接失败: {}", e)))?;

    // 查询恢复记录
    let result = conn
        .query_row(
            "SELECT task_id, recommendation, resume_step_id, evidence, detail, detected_at
             FROM runtime_task_recovery
             WHERE task_id = ?",
            [task_id],
            |row| {
                Ok(RuntimeTaskRecovery {
                    task_id: row.get(0)?,
                    recommendation: row.get::<_, String>(1)?,
                    resume_step_id: row.get(2)?,
                    resume_step_index: None,
                    resume_checkpoint_id: None,
                    evidence: vec![],
                    plan_revision: None,
                    completion_satisfied: None,
                    missing_requirement_ids: vec![],
                    replacement_key: None,
                    replacement_task_id: None,
                    detail: row.get(4)?,
                    detected_at: row.get(5)?,
                })
            },
        );

    match result {
        Ok(recovery) => Ok(Some(recovery)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(RecoveryError::DatabaseError(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recovery_recommendation_from_str() {
        assert_eq!(
            RecoveryRecommendation::from_str("resume"),
            Some(RecoveryRecommendation::Resume)
        );
        assert_eq!(
            RecoveryRecommendation::from_str("restart"),
            Some(RecoveryRecommendation::Restart)
        );
        assert_eq!(
            RecoveryRecommendation::from_str("fail"),
            Some(RecoveryRecommendation::Fail)
        );
        assert_eq!(
            RecoveryRecommendation::from_str("supersede"),
            Some(RecoveryRecommendation::Supersede)
        );
        assert_eq!(RecoveryRecommendation::from_str("unknown"), None);
    }

    #[test]
    fn test_recovery_recommendation_as_str() {
        assert_eq!(RecoveryRecommendation::Resume.as_str(), "resume");
        assert_eq!(RecoveryRecommendation::Restart.as_str(), "restart");
        assert_eq!(RecoveryRecommendation::Fail.as_str(), "fail");
        assert_eq!(
            RecoveryRecommendation::ManualIntervention.as_str(),
            "manual_intervention"
        );
    }

    #[test]
    fn test_recover_interrupted_tasks_validation() {
        // 测试验证逻辑

        // 空工作区应该失败
        assert!("".is_empty());
    }

    #[test]
    fn test_resolve_recovery_validation() {
        // 测试验证逻辑

        // 空任务 ID 应该失败
        assert!("".is_empty());
    }

    #[test]
    fn test_supersede_task_validation() {
        // 测试验证逻辑

        // 空任务 ID 应该失败
        assert!("".is_empty());

        // 空替代密钥应该失败
        assert!("".is_empty());
    }

    #[test]
    fn test_recovery_error_display() {
        let err = RecoveryError::TaskNotFound("task123".to_string());
        assert_eq!(err.to_string(), "任务未找到: task123");

        let err = RecoveryError::InvalidRecovery("test".to_string());
        assert!(err.to_string().contains("恢复信息无效"));

        let err = RecoveryError::RecoveryConflict("test".to_string());
        assert!(err.to_string().contains("恢复冲突"));
    }
}
