/// ConfigPlugin - 配置管理插件
///
/// 提供配置的读取、验证、存储功能

use crate::core::plugin::{Capability, Command, Migration, PluginContext, YunspirePlugin};
use crate::plugins::config::storage::{
    delete_runtime_settings, load_runtime_settings, save_runtime_settings,
    update_scheduler_enabled,
};
use crate::plugins::config::types::{ConfigResponse, ConfigType, RuntimeSettings};
use crate::plugins::config::validation::{validate_config, validate_runtime_settings};
use serde_json::Value;

/// ConfigPlugin - 配置管理插件
pub struct ConfigPlugin {
    /// 插件是否已初始化
    initialized: bool,
}

impl ConfigPlugin {
    /// 创建新的配置插件实例
    pub fn new() -> Self {
        Self {
            initialized: false,
        }
    }
}

impl Default for ConfigPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl YunspirePlugin for ConfigPlugin {
    fn id(&self) -> &str {
        "yunspire.config"
    }

    fn name(&self) -> &str {
        "配置管理"
    }

    fn version(&self) -> &str {
        "1.0.0"
    }

    fn description(&self) -> &str {
        "云枢配置管理插件，提供配置的读取、验证和存储功能"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::DatabaseRead, Capability::DatabaseWrite]
    }

    fn dependencies(&self) -> Vec<String> {
        vec![]
    }

    fn on_load(&mut self, _context: &PluginContext) -> Result<(), String> {
        log::info!("配置管理插件正在初始化...");

        // 插件初始化逻辑
        // 例如：加载默认配置、验证配置文件等

        self.initialized = true;
        log::info!("配置管理插件初始化成功");

        Ok(())
    }

    fn on_unload(&mut self) -> Result<(), String> {
        log::info!("配置管理插件正在卸载...");

        // 清理资源
        // 例如：保存配置、关闭连接等

        self.initialized = false;
        log::info!("配置管理插件卸载成功");

        Ok(())
    }

    fn commands(&self) -> Vec<Command> {
        vec![
            // 命令 1: get_runtime_settings
            Command::new("get_runtime_settings", |_params| {
                // TODO: 实现获取运行时设置命令
                Err("get_runtime_settings 需要通过桥接层实现".to_string())
            }),
            // 命令 2: update_runtime_settings
            Command::new("update_runtime_settings", |_params| {
                // TODO: 实现更新运行时设置命令
                Err("update_runtime_settings 需要通过桥接层实现".to_string())
            }),
            // 命令 3: update_scheduler_enabled
            Command::new("update_scheduler_enabled", |_params| {
                // TODO: 实现更新调度器状态命令
                Err("update_scheduler_enabled 需要通过桥接层实现".to_string())
            }),
        ]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            // 迁移 1: 创建 runtime_settings 表
            Migration::new(
                1,
                "CREATE TABLE IF NOT EXISTS runtime_settings (
                     workspace_scope TEXT PRIMARY KEY NOT NULL,
                     scheduler_enabled INTEGER NOT NULL DEFAULT 1,
                     updated_at TEXT NOT NULL
                 );",
                "创建运行时设置表",
            ),
            // 迁移 2: 添加索引
            Migration::new(
                2,
                "CREATE INDEX IF NOT EXISTS idx_runtime_settings_updated
                   ON runtime_settings(updated_at);",
                "为运行时设置表添加索引",
            ),
        ]
    }

    fn config_schema(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "auto_save": {
                    "type": "boolean",
                    "description": "是否自动保存配置",
                    "default": true
                },
                "validation_mode": {
                    "type": "string",
                    "description": "配置验证模式",
                    "enum": ["strict", "lenient"],
                    "default": "strict"
                }
            }
        }))
    }

    fn health_check(&self) -> Result<(), String> {
        if !self.initialized {
            return Err("配置管理插件未初始化".to_string());
        }

        // TODO: 添加更多健康检查
        // - 检查配置文件完整性
        // - 检查数据库表结构

        Ok(())
    }
}

/// 获取运行时设置（内部实现）
pub fn get_runtime_settings_impl(
    database: &crate::runtime_db::RuntimeDatabase,
    workspace_scope: &str,
) -> Result<ConfigResponse, String> {
    let settings = load_runtime_settings(database, workspace_scope)?
        .unwrap_or_else(|| RuntimeSettings::default_for_workspace(workspace_scope.to_string()));

    Ok(ConfigResponse {
        config_type: ConfigType::Runtime,
        config: serde_json::to_value(&settings)
            .map_err(|e| format!("序列化设置失败：{}", e))?,
        updated_at: settings.updated_at,
    })
}

/// 更新运行时设置（内部实现）
pub fn update_runtime_settings_impl(
    database: &crate::runtime_db::RuntimeDatabase,
    settings: RuntimeSettings,
) -> Result<(), String> {
    // 验证
    validate_runtime_settings(&settings)
        .map_err(|e| format!("配置验证失败：{}", e))?;

    // 保存
    save_runtime_settings(database, &settings)
}

/// 更新调度器状态（内部实现）
pub fn update_scheduler_enabled_impl(
    database: &crate::runtime_db::RuntimeDatabase,
    workspace_scope: &str,
    enabled: bool,
) -> Result<(), String> {
    update_scheduler_enabled(database, workspace_scope, enabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_metadata() {
        let plugin = ConfigPlugin::new();
        assert_eq!(plugin.id(), "yunspire.config");
        assert_eq!(plugin.name(), "配置管理");
        assert_eq!(plugin.version(), "1.0.0");
    }

    #[test]
    fn test_plugin_capabilities() {
        let plugin = ConfigPlugin::new();
        let caps = plugin.capabilities();
        assert_eq!(caps.len(), 2);
        assert!(caps.contains(&Capability::DatabaseRead));
        assert!(caps.contains(&Capability::DatabaseWrite));
    }

    #[test]
    fn test_plugin_commands() {
        let plugin = ConfigPlugin::new();
        let commands = plugin.commands();
        assert_eq!(commands.len(), 3);
        assert_eq!(commands[0].name, "get_runtime_settings");
        assert_eq!(commands[1].name, "update_runtime_settings");
        assert_eq!(commands[2].name, "update_scheduler_enabled");
    }

    #[test]
    fn test_plugin_migrations() {
        let plugin = ConfigPlugin::new();
        let migrations = plugin.migrations();
        assert_eq!(migrations.len(), 2);
        assert_eq!(migrations[0].version, 1);
        assert_eq!(migrations[1].version, 2);
    }

    #[test]
    fn test_plugin_config_schema() {
        let plugin = ConfigPlugin::new();
        let schema = plugin.config_schema();
        assert!(schema.is_some());

        let schema = schema.unwrap();
        assert!(schema["properties"]["auto_save"].is_object());
        assert!(schema["properties"]["validation_mode"].is_object());
    }

    #[test]
    fn test_health_check_before_init() {
        let plugin = ConfigPlugin::new();
        let result = plugin.health_check();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("未初始化"));
    }
}
