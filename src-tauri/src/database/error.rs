use thiserror::Error;

/// 数据库错误类型（统一错误处理）
#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("数据库连接失败: {0}")]
    ConnectionFailed(String),

    #[error("查询失败: {0}")]
    QueryFailed(#[from] rusqlite::Error),

    #[error("事务失败: {0}")]
    TransactionFailed(String),

    #[error("索引构建失败: {0}")]
    IndexFailed(String),

    #[error("迁移失败: {0}")]
    MigrationFailed(String),

    #[error("锁超时")]
    LockTimeout,

    #[error("数据验证失败: {0}")]
    ValidationFailed(String),

    #[error("序列化失败: {0}")]
    SerializationFailed(String),

    #[error("配置错误: {0}")]
    ConfigError(String),
}

/// 数据库操作结果类型
pub type DatabaseResult<T> = Result<T, DatabaseError>;

/// 将 DatabaseError 转换为 String（保持向后兼容）
impl From<DatabaseError> for String {
    fn from(error: DatabaseError) -> Self {
        format!("{}", error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = DatabaseError::ConnectionFailed("test".to_string());
        assert_eq!(err.to_string(), "数据库连接失败: test");
    }

    #[test]
    fn test_error_to_string() {
        let err = DatabaseError::LockTimeout;
        let s: String = err.into();
        assert_eq!(s, "锁超时");
    }
}
