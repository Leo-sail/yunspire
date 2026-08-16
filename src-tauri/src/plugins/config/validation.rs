/// ConfigPlugin 配置验证模块
///
/// 提供配置验证功能

use crate::database::DatabaseConfig;
use crate::plugins::config::types::{ConfigType, RuntimeSettings};

/// 配置验证错误
#[derive(Debug, Clone)]
pub enum ValidationError {
    /// 数据库配置无效
    InvalidDatabaseConfig(String),

    /// 运行时设置无效
    InvalidRuntimeSettings(String),

    /// 配置类型不支持
    UnsupportedConfigType(ConfigType),

    /// 字段缺失
    MissingField(String),

    /// 字段值无效
    InvalidFieldValue(String, String),
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::InvalidDatabaseConfig(msg) => {
                write!(f, "数据库配置无效: {}", msg)
            }
            ValidationError::InvalidRuntimeSettings(msg) => {
                write!(f, "运行时设置无效: {}", msg)
            }
            ValidationError::UnsupportedConfigType(config_type) => {
                write!(f, "不支持的配置类型: {:?}", config_type)
            }
            ValidationError::MissingField(field) => {
                write!(f, "缺少必填字段: {}", field)
            }
            ValidationError::InvalidFieldValue(field, reason) => {
                write!(f, "字段 {} 的值无效: {}", field, reason)
            }
        }
    }
}

impl std::error::Error for ValidationError {}

/// 验证数据库配置
///
/// 复用 DatabaseConfig::validate()
pub fn validate_database_config(config: &DatabaseConfig) -> Result<(), ValidationError> {
    config
        .validate()
        .map_err(|e| ValidationError::InvalidDatabaseConfig(e))
}

/// 验证运行时设置
pub fn validate_runtime_settings(settings: &RuntimeSettings) -> Result<(), ValidationError> {
    // 验证工作区范围
    if settings.workspace_scope.is_empty() {
        return Err(ValidationError::MissingField("workspace_scope".to_string()));
    }

    if settings.workspace_scope.len() > 256 {
        return Err(ValidationError::InvalidFieldValue(
            "workspace_scope".to_string(),
            "长度超过 256 个字符".to_string(),
        ));
    }

    // 验证时间戳格式
    if chrono::DateTime::parse_from_rfc3339(&settings.updated_at).is_err() {
        return Err(ValidationError::InvalidFieldValue(
            "updated_at".to_string(),
            "不是有效的 RFC3339 时间戳".to_string(),
        ));
    }

    Ok(())
}

/// 验证配置通用函数
pub fn validate_config(
    config_type: &ConfigType,
    _config: &serde_json::Value,
) -> Result<(), ValidationError> {
    match config_type {
        ConfigType::Database => {
            // DatabaseConfig 验证由 DatabaseConfig::validate() 直接调用
            // 这里不需要反序列化
            Ok(())
        }
        ConfigType::Runtime => {
            // RuntimeSettings 验证由 validate_runtime_settings() 直接调用
            // 这里不需要反序列化
            Ok(())
        }
        ConfigType::Plugin | ConfigType::Custom => {
            // 插件和自定义配置由各自的插件验证
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_database_config_success() {
        let config = DatabaseConfig::default();
        assert!(validate_database_config(&config).is_ok());
    }

    #[test]
    fn test_validate_database_config_invalid() {
        let mut config = DatabaseConfig::default();
        config.connection_pool_size = 0;
        assert!(validate_database_config(&config).is_err());
    }

    #[test]
    fn test_validate_runtime_settings_success() {
        let settings = RuntimeSettings::new("test-workspace".to_string(), true);
        assert!(validate_runtime_settings(&settings).is_ok());
    }

    #[test]
    fn test_validate_runtime_settings_empty_workspace() {
        let mut settings = RuntimeSettings::new("test".to_string(), true);
        settings.workspace_scope = String::new();
        let result = validate_runtime_settings(&settings);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ValidationError::MissingField(_)));
    }

    #[test]
    fn test_validate_runtime_settings_long_workspace() {
        let mut settings = RuntimeSettings::new("test".to_string(), true);
        settings.workspace_scope = "a".repeat(300);
        let result = validate_runtime_settings(&settings);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ValidationError::InvalidFieldValue(_, _)
        ));
    }

    #[test]
    fn test_validate_runtime_settings_invalid_timestamp() {
        let mut settings = RuntimeSettings::new("test".to_string(), true);
        settings.updated_at = "invalid-timestamp".to_string();
        let result = validate_runtime_settings(&settings);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_config_database() {
        // DatabaseConfig 不需要序列化测试
        // 直接使用 validate_database_config()
        let config = DatabaseConfig::default();
        assert!(validate_database_config(&config).is_ok());
    }

    #[test]
    fn test_validate_config_runtime() {
        let settings = RuntimeSettings::new("test".to_string(), true);
        let config = serde_json::to_value(settings).unwrap();
        assert!(validate_config(&ConfigType::Runtime, &config).is_ok());
    }

    #[test]
    fn test_validation_error_display() {
        let err = ValidationError::MissingField("test".to_string());
        assert_eq!(err.to_string(), "缺少必填字段: test");
    }
}
