/// ConfigPlugin 数据类型定义
///
/// 包含配置管理所需的核心数据结构

use serde::{Deserialize, Serialize};

// 重新导出 DatabaseConfig
pub use crate::database::DatabaseConfig;

/// 运行时设置
///
/// 存储在 runtime_settings 表中的运行时配置
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSettings {
    /// 工作区范围
    pub workspace_scope: String,

    /// 调度器是否启用
    pub scheduler_enabled: bool,

    /// 最后更新时间 (RFC3339 格式)
    pub updated_at: String,
}

impl RuntimeSettings {
    /// 创建新的运行时设置
    pub fn new(workspace_scope: String, scheduler_enabled: bool) -> Self {
        Self {
            workspace_scope,
            scheduler_enabled,
            updated_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// 创建默认设置
    pub fn default_for_workspace(workspace_scope: String) -> Self {
        Self::new(workspace_scope, true)
    }
}

/// 配置 Schema
///
/// 用于配置验证和类型定义
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigSchema {
    /// Schema 版本
    pub version: String,

    /// 配置类型
    pub config_type: ConfigType,

    /// Schema 定义
    pub schema: serde_json::Value,
}

/// 配置类型
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ConfigType {
    /// 数据库配置
    Database,

    /// 运行时设置
    Runtime,

    /// 插件配置
    Plugin,

    /// 自定义配置
    Custom,
}

/// 配置更新请求
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigUpdateRequest {
    /// 工作区范围
    pub workspace_scope: String,

    /// 配置类型
    pub config_type: ConfigType,

    /// 配置数据
    pub config: serde_json::Value,
}

/// 配置查询请求
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigQueryRequest {
    /// 工作区范围
    pub workspace_scope: String,

    /// 配置类型
    pub config_type: ConfigType,
}

/// 配置响应
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigResponse {
    /// 配置类型
    pub config_type: ConfigType,

    /// 配置数据
    pub config: serde_json::Value,

    /// 最后更新时间
    pub updated_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_settings_new() {
        let settings = RuntimeSettings::new("test-workspace".to_string(), true);
        assert_eq!(settings.workspace_scope, "test-workspace");
        assert!(settings.scheduler_enabled);
        assert!(!settings.updated_at.is_empty());
    }

    #[test]
    fn test_runtime_settings_default() {
        let settings = RuntimeSettings::default_for_workspace("test".to_string());
        assert!(settings.scheduler_enabled);
    }

    #[test]
    fn test_config_type_serialization() {
        let config_type = ConfigType::Database;
        let json = serde_json::to_string(&config_type).unwrap();
        assert_eq!(json, "\"database\"");
    }

    #[test]
    fn test_config_type_deserialization() {
        let json = "\"runtime\"";
        let config_type: ConfigType = serde_json::from_str(json).unwrap();
        assert_eq!(config_type, ConfigType::Runtime);
    }

    #[test]
    fn test_runtime_settings_serialization() {
        let settings = RuntimeSettings::new("test".to_string(), false);
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains("workspaceScope"));
        assert!(json.contains("schedulerEnabled"));
    }
}
