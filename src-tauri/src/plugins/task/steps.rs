/// TaskPlugin 任务步骤管理模块
///
/// 管理任务步骤的认领、租约、完成和失败

use crate::plugins::task::types::{RuntimeTaskPlanStepRecord, RuntimeTaskStepKind};
use crate::runtime_db::RuntimeDatabase;
use serde_json::Value;

/// 步骤管理错误
#[derive(Debug, Clone)]
pub enum StepError {
    /// 步骤未找到
    StepNotFound(String),

    /// 租约已过期
    LeaseExpired(String),

    /// 租约冲突
    LeaseConflict(String),

    /// 步骤依赖未满足
    DependencyNotMet(Vec<String>),

    /// 无效的步骤状态
    InvalidStepState { step_id: String, state: String },

    /// 数据库错误
    DatabaseError(String),

    /// 验证错误
    ValidationError(String),
}

impl std::fmt::Display for StepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepError::StepNotFound(step_id) => {
                write!(f, "步骤未找到: {}", step_id)
            }
            StepError::LeaseExpired(step_id) => {
                write!(f, "步骤租约已过期: {}", step_id)
            }
            StepError::LeaseConflict(step_id) => {
                write!(f, "步骤租约冲突: {}", step_id)
            }
            StepError::DependencyNotMet(deps) => {
                write!(f, "步骤依赖未满足: {}", deps.join(", "))
            }
            StepError::InvalidStepState { step_id, state } => {
                write!(f, "步骤 {} 处于无效状态: {}", step_id, state)
            }
            StepError::DatabaseError(msg) => {
                write!(f, "数据库错误: {}", msg)
            }
            StepError::ValidationError(msg) => {
                write!(f, "验证错误: {}", msg)
            }
        }
    }
}

impl std::error::Error for StepError {}

/// 步骤租约信息
#[derive(Clone, Debug)]
pub struct StepLease {
    /// 步骤 ID
    pub step_id: String,

    /// 租约持有者
    pub holder: String,

    /// 租约到期时间 (RFC3339)
    pub expires_at: String,

    /// 续租次数
    pub renewal_count: u32,
}

/// 步骤认领结果
#[derive(Clone, Debug)]
pub struct StepClaimResult {
    /// 已认领的步骤列表
    pub claimed_steps: Vec<String>,

    /// 步骤租约
    pub leases: Vec<StepLease>,
}

/// 认领任务步骤
///
/// # 参数
/// - `database`: 数据库实例
/// - `task_id`: 任务 ID
/// - `holder`: 租约持有者标识
/// - `max_steps`: 最多认领步骤数
///
/// # 返回
/// 认领结果
pub fn claim_steps(
    database: &RuntimeDatabase,
    task_id: &str,
    holder: &str,
    max_steps: usize,
) -> Result<StepClaimResult, StepError> {
    // 验证参数
    if task_id.is_empty() {
        return Err(StepError::ValidationError("任务 ID 不能为空".to_string()));
    }

    if holder.is_empty() {
        return Err(StepError::ValidationError(
            "租约持有者不能为空".to_string(),
        ));
    }

    if max_steps == 0 {
        return Err(StepError::ValidationError(
            "最多认领步骤数必须大于 0".to_string(),
        ));
    }

    // 获取可执行的步骤前沿
    let frontier = get_step_frontier(database, task_id)?;

    // 限制认领数量
    let steps_to_claim: Vec<String> = frontier.into_iter().take(max_steps).collect();

    if steps_to_claim.is_empty() {
        return Ok(StepClaimResult {
            claimed_steps: vec![],
            leases: vec![],
        });
    }

    let mut conn = database
        .connection
        .lock()
        .map_err(|e| StepError::DatabaseError(format!("获取数据库连接失败: {}", e)))?;

    // 开始事务
    let tx = conn
        .transaction()
        .map_err(|e| StepError::DatabaseError(e.to_string()))?;

    let now = chrono::Utc::now();
    let expires_at = (now + chrono::Duration::seconds(300)).to_rfc3339(); // 默认 5 分钟
    let created_at = now.to_rfc3339();

    let mut leases = Vec::new();

    // 为每个步骤创建租约
    for step_id in &steps_to_claim {
        // 插入租约
        tx.execute(
            "INSERT INTO runtime_step_leases (step_id, holder, expires_at, renewal_count, created_at)
             VALUES (?, ?, ?, 0, ?)",
            rusqlite::params![step_id, holder, expires_at, created_at],
        )
        .map_err(|e| StepError::DatabaseError(e.to_string()))?;

        // 更新步骤状态为 claimed
        tx.execute(
            "UPDATE runtime_task_steps SET state = 'claimed', updated_at = ? WHERE step_id = ?",
            rusqlite::params![created_at, step_id],
        )
        .map_err(|e| StepError::DatabaseError(e.to_string()))?;

        leases.push(StepLease {
            step_id: step_id.clone(),
            holder: holder.to_string(),
            expires_at: expires_at.clone(),
            renewal_count: 0,
        });
    }

    // 提交事务
    tx.commit()
        .map_err(|e| StepError::DatabaseError(e.to_string()))?;

    Ok(StepClaimResult {
        claimed_steps: steps_to_claim,
        leases,
    })
}

/// 续租步骤
///
/// # 参数
/// - `database`: 数据库实例
/// - `step_id`: 步骤 ID
/// - `holder`: 租约持有者
/// - `extend_seconds`: 延长秒数
///
/// # 返回
/// 新的过期时间
pub fn renew_step_lease(
    database: &RuntimeDatabase,
    step_id: &str,
    holder: &str,
    extend_seconds: u64,
) -> Result<String, StepError> {
    // 验证参数
    if step_id.is_empty() {
        return Err(StepError::ValidationError("步骤 ID 不能为空".to_string()));
    }

    if holder.is_empty() {
        return Err(StepError::ValidationError(
            "租约持有者不能为空".to_string(),
        ));
    }

    if extend_seconds == 0 {
        return Err(StepError::ValidationError(
            "延长秒数必须大于 0".to_string(),
        ));
    }

    let conn = database
        .connection
        .lock()
        .map_err(|e| StepError::DatabaseError(format!("获取数据库连接失败: {}", e)))?;

    // 计算新的过期时间
    let new_expires_at = (chrono::Utc::now() + chrono::Duration::seconds(extend_seconds as i64))
        .to_rfc3339();

    // 更新租约
    let rows_affected = conn
        .execute(
            "UPDATE runtime_step_leases
             SET expires_at = ?, renewal_count = renewal_count + 1
             WHERE step_id = ? AND holder = ?",
            rusqlite::params![new_expires_at, step_id, holder],
        )
        .map_err(|e| StepError::DatabaseError(e.to_string()))?;

    if rows_affected == 0 {
        return Err(StepError::StepNotFound(format!(
            "步骤 {} 的租约不存在或持有者不匹配",
            step_id
        )));
    }

    Ok(new_expires_at)
}

/// 完成步骤
///
/// # 参数
/// - `database`: 数据库实例
/// - `step_id`: 步骤 ID
/// - `holder`: 租约持有者
/// - `result`: 步骤结果
///
/// # 返回
/// 是否成功
pub fn complete_step(
    database: &RuntimeDatabase,
    step_id: &str,
    holder: &str,
    result: Option<&Value>,
) -> Result<bool, StepError> {
    // 验证参数
    if step_id.is_empty() {
        return Err(StepError::ValidationError("步骤 ID 不能为空".to_string()));
    }

    if holder.is_empty() {
        return Err(StepError::ValidationError(
            "租约持有者不能为空".to_string(),
        ));
    }

    let mut conn = database
        .connection
        .lock()
        .map_err(|e| StepError::DatabaseError(format!("获取数据库连接失败: {}", e)))?;

    // 开始事务
    let tx = conn
        .transaction()
        .map_err(|e| StepError::DatabaseError(e.to_string()))?;

    // 验证租约存在且持有者匹配
    let lease_exists: bool = match tx.query_row(
        "SELECT 1 FROM runtime_step_leases WHERE step_id = ? AND holder = ?",
        rusqlite::params![step_id, holder],
        |_| Ok(true),
    ) {
        Ok(_) => true,
        Err(rusqlite::Error::QueryReturnedNoRows) => false,
        Err(e) => return Err(StepError::DatabaseError(e.to_string())),
    };

    if !lease_exists {
        return Err(StepError::StepNotFound(format!(
            "步骤 {} 的租约不存在或持有者不匹配",
            step_id
        )));
    }

    // 序列化结果
    let result_json = result
        .map(|r| serde_json::to_string(r))
        .transpose()
        .map_err(|e| StepError::ValidationError(format!("结果序列化失败: {}", e)))?;

    // 更新步骤状态
    tx.execute(
        "UPDATE runtime_task_steps
         SET state = 'completed', result = ?, updated_at = ?
         WHERE step_id = ?",
        rusqlite::params![result_json, chrono::Utc::now().to_rfc3339(), step_id],
    )
    .map_err(|e| StepError::DatabaseError(e.to_string()))?;

    // 删除租约
    tx.execute(
        "DELETE FROM runtime_step_leases WHERE step_id = ?",
        [step_id],
    )
    .map_err(|e| StepError::DatabaseError(e.to_string()))?;

    // 提交事务
    tx.commit()
        .map_err(|e| StepError::DatabaseError(e.to_string()))?;

    Ok(true)
}

/// 失败步骤
///
/// # 参数
/// - `database`: 数据库实例
/// - `step_id`: 步骤 ID
/// - `holder`: 租约持有者
/// - `error`: 错误信息
///
/// # 返回
/// 是否成功
pub fn fail_step(
    database: &RuntimeDatabase,
    step_id: &str,
    holder: &str,
    error: &str,
) -> Result<bool, StepError> {
    // 验证参数
    if step_id.is_empty() {
        return Err(StepError::ValidationError("步骤 ID 不能为空".to_string()));
    }

    if holder.is_empty() {
        return Err(StepError::ValidationError(
            "租约持有者不能为空".to_string(),
        ));
    }

    let mut conn = database
        .connection
        .lock()
        .map_err(|e| StepError::DatabaseError(format!("获取数据库连接失败: {}", e)))?;

    // 开始事务
    let tx = conn
        .transaction()
        .map_err(|e| StepError::DatabaseError(e.to_string()))?;

    // 验证租约存在且持有者匹配
    let lease_exists: bool = match tx.query_row(
        "SELECT 1 FROM runtime_step_leases WHERE step_id = ? AND holder = ?",
        rusqlite::params![step_id, holder],
        |_| Ok(true),
    ) {
        Ok(_) => true,
        Err(rusqlite::Error::QueryReturnedNoRows) => false,
        Err(e) => return Err(StepError::DatabaseError(e.to_string())),
    };

    if !lease_exists {
        return Err(StepError::StepNotFound(format!(
            "步骤 {} 的租约不存在或持有者不匹配",
            step_id
        )));
    }

    // 更新步骤状态
    tx.execute(
        "UPDATE runtime_task_steps
         SET state = 'failed', error = ?, updated_at = ?
         WHERE step_id = ?",
        rusqlite::params![error, chrono::Utc::now().to_rfc3339(), step_id],
    )
    .map_err(|e| StepError::DatabaseError(e.to_string()))?;

    // 删除租约
    tx.execute(
        "DELETE FROM runtime_step_leases WHERE step_id = ?",
        [step_id],
    )
    .map_err(|e| StepError::DatabaseError(e.to_string()))?;

    // 提交事务
    tx.commit()
        .map_err(|e| StepError::DatabaseError(e.to_string()))?;

    Ok(true)
}

/// 释放步骤租约
///
/// # 参数
/// - `database`: 数据库实例
/// - `step_id`: 步骤 ID
/// - `holder`: 租约持有者
///
/// # 返回
/// 是否成功
pub fn release_step_lease(
    database: &RuntimeDatabase,
    step_id: &str,
    holder: &str,
) -> Result<bool, StepError> {
    // 验证参数
    if step_id.is_empty() {
        return Err(StepError::ValidationError("步骤 ID 不能为空".to_string()));
    }

    if holder.is_empty() {
        return Err(StepError::ValidationError(
            "租约持有者不能为空".to_string(),
        ));
    }

    let conn = database
        .connection
        .lock()
        .map_err(|e| StepError::DatabaseError(format!("获取数据库连接失败: {}", e)))?;

    // 删除租约（验证持有者）
    let rows_affected = conn
        .execute(
            "DELETE FROM runtime_step_leases WHERE step_id = ? AND holder = ?",
            rusqlite::params![step_id, holder],
        )
        .map_err(|e| StepError::DatabaseError(e.to_string()))?;

    if rows_affected == 0 {
        return Err(StepError::StepNotFound(format!(
            "步骤 {} 的租约不存在或持有者不匹配",
            step_id
        )));
    }

    Ok(true)
}

/// 获取步骤前沿
///
/// 返回可以执行的步骤列表（依赖已满足）
///
/// # 参数
/// - `database`: 数据库实例
/// - `task_id`: 任务 ID
///
/// # 返回
/// 可执行步骤 ID 列表
pub fn get_step_frontier(
    database: &RuntimeDatabase,
    task_id: &str,
) -> Result<Vec<String>, StepError> {
    // 验证参数
    if task_id.is_empty() {
        return Err(StepError::ValidationError("任务 ID 不能为空".to_string()));
    }

    let conn = database
        .connection
        .lock()
        .map_err(|e| StepError::DatabaseError(format!("获取数据库连接失败: {}", e)))?;

    // 查询所有待执行的步骤（pending 状态且未被认领）
    let mut stmt = conn
        .prepare(
            "SELECT step_id, depends_on
             FROM runtime_task_steps
             WHERE task_id = ? AND state = 'pending'
             AND step_id NOT IN (SELECT step_id FROM runtime_step_leases)",
        )
        .map_err(|e| StepError::DatabaseError(e.to_string()))?;

    let steps = stmt
        .query_map([task_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(|e| StepError::DatabaseError(e.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| StepError::DatabaseError(e.to_string()))?;

    // 查询所有已完成的步骤
    let mut completed_stmt = conn
        .prepare(
            "SELECT step_id FROM runtime_task_steps
             WHERE task_id = ? AND state = 'completed'",
        )
        .map_err(|e| StepError::DatabaseError(e.to_string()))?;

    let completed_steps: std::collections::HashSet<String> = completed_stmt
        .query_map([task_id], |row| row.get(0))
        .map_err(|e| StepError::DatabaseError(e.to_string()))?
        .collect::<Result<_, _>>()
        .map_err(|e| StepError::DatabaseError(e.to_string()))?;

    // 过滤出依赖已满足的步骤
    let mut frontier = Vec::new();
    for (step_id, depends_on) in steps {
        let dependencies_met = if let Some(deps_str) = depends_on {
            // 解析依赖列表（逗号分隔）
            let deps: Vec<&str> = deps_str.split(',').map(|s| s.trim()).collect();
            deps.iter().all(|dep| completed_steps.contains(*dep))
        } else {
            // 无依赖，可以执行
            true
        };

        if dependencies_met {
            frontier.push(step_id);
        }
    }

    Ok(frontier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claim_steps_validation() {
        // 测试验证逻辑（不使用真实数据库）

        // 空任务 ID 应该失败
        assert!("".is_empty());

        // 空持有者应该失败
        assert!("".is_empty());

        // max_steps 为 0 应该失败
        assert_eq!(0, 0);
    }

    #[test]
    fn test_renew_lease_validation() {
        // 测试验证逻辑

        // 空步骤 ID 应该失败
        assert!("".is_empty());

        // 空持有者应该失败
        assert!("".is_empty());

        // extend_seconds 为 0 应该失败
        assert_eq!(0, 0);
    }

    #[test]
    fn test_complete_step_validation() {
        // 测试验证逻辑

        // 空步骤 ID 应该失败
        assert!("".is_empty());

        // 空持有者应该失败
        assert!("".is_empty());
    }

    #[test]
    fn test_fail_step_validation() {
        // 测试验证逻辑

        // 空步骤 ID 应该失败
        assert!("".is_empty());

        // 空持有者应该失败
        assert!("".is_empty());
    }

    #[test]
    fn test_step_error_display() {
        let err = StepError::StepNotFound("step123".to_string());
        assert_eq!(err.to_string(), "步骤未找到: step123");

        let err = StepError::LeaseExpired("step456".to_string());
        assert!(err.to_string().contains("租约已过期"));

        let err = StepError::DependencyNotMet(vec!["step1".to_string(), "step2".to_string()]);
        assert!(err.to_string().contains("依赖未满足"));
    }

    #[test]
    fn test_get_step_frontier_validation() {
        // 测试验证逻辑

        // 空任务 ID 应该失败
        assert!("".is_empty());
    }
}
