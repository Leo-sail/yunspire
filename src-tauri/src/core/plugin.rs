use rusqlite::Connection;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tauri::AppHandle;

/// 插件能力标识
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Capability {
    /// 数据库读取
    DatabaseRead,
    /// 数据库写入
    DatabaseWrite,
    /// Vault 读取
    VaultRead,
    /// Vault 写入
    VaultWrite,
    /// 网络访问
    Network,
    /// Shell 执行
    Shell,
    /// 模型访问
    ModelAccess,
}

/// 插件配置
#[derive(Debug, Clone)]
pub struct PluginConfig {
    /// 插件数据目录
    pub data_dir: std::path::PathBuf,
    /// 插件配置（JSON）
    pub config: Value,
}

/// 插件上下文（插件运行环境）
pub struct PluginContext {
    /// Tauri 应用句柄
    pub app_handle: AppHandle,
    /// 数据库连接（共享）
    pub database: Arc<Mutex<Connection>>,
    /// 插件配置
    pub config: PluginConfig,
}

/// Tauri 命令定义
pub struct Command {
    /// 命令名称（对应前端调用的名字）
    pub name: String,
    /// 命令处理函数
    pub handler: Arc<dyn Fn(Value) -> Result<Value, String> + Send + Sync>,
}

impl Command {
    /// 创建新命令
    pub fn new<F>(name: impl Into<String>, handler: F) -> Self
    where
        F: Fn(Value) -> Result<Value, String> + Send + Sync + 'static,
    {
        Self {
            name: name.into(),
            handler: Arc::new(handler),
        }
    }
}

/// 数据库迁移定义
#[derive(Debug, Clone)]
pub struct Migration {
    /// 迁移版本号（递增）
    pub version: i64,
    /// SQL 语句
    pub sql: String,
    /// 迁移描述
    pub description: String,
}

impl Migration {
    /// 创建新迁移
    pub fn new(version: i64, sql: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            version,
            sql: sql.into(),
            description: description.into(),
        }
    }
}

/// 云枢插件 Trait（所有插件必须实现）
pub trait YunspirePlugin: Send + Sync {
    /// 插件唯一标识（如 "yunspire.search"）
    fn id(&self) -> &str;

    /// 插件名称（如 "搜索引擎"）
    fn name(&self) -> &str;

    /// 插件版本（如 "1.0.0"）
    fn version(&self) -> &str;

    /// 插件描述
    fn description(&self) -> &str;

    /// 插件所需能力
    fn capabilities(&self) -> Vec<Capability>;

    /// 依赖的其他插件 ID
    fn dependencies(&self) -> Vec<String> {
        vec![]
    }

    /// 插件加载时调用（初始化）
    ///
    /// # 参数
    /// - `context`: 插件上下文，包含应用句柄、数据库连接等
    ///
    /// # 返回
    /// - `Ok(())`: 初始化成功
    /// - `Err(String)`: 初始化失败，错误信息
    fn on_load(&mut self, context: &PluginContext) -> Result<(), String>;

    /// 插件卸载时调用（清理资源）
    ///
    /// # 返回
    /// - `Ok(())`: 卸载成功
    /// - `Err(String)`: 卸载失败，错误信息
    fn on_unload(&mut self) -> Result<(), String>;

    /// 注册 Tauri 命令
    ///
    /// # 返回
    /// 命令列表，前端可以通过 `invoke(command_name, params)` 调用
    fn commands(&self) -> Vec<Command>;

    /// 数据库迁移脚本
    ///
    /// # 返回
    /// 迁移列表，按版本号排序
    fn migrations(&self) -> Vec<Migration>;

    /// 插件配置 Schema（JSON Schema）
    ///
    /// # 返回
    /// - `Some(schema)`: 插件配置的 JSON Schema
    /// - `None`: 插件不需要配置
    fn config_schema(&self) -> Option<Value> {
        None
    }

    /// 插件健康检查
    ///
    /// # 返回
    /// - `Ok(())`: 插件健康
    /// - `Err(String)`: 插件异常，错误信息
    fn health_check(&self) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 测试插件示例
    struct TestPlugin;

    impl YunspirePlugin for TestPlugin {
        fn id(&self) -> &str {
            "test.plugin"
        }

        fn name(&self) -> &str {
            "测试插件"
        }

        fn version(&self) -> &str {
            "1.0.0"
        }

        fn description(&self) -> &str {
            "用于测试的插件"
        }

        fn capabilities(&self) -> Vec<Capability> {
            vec![Capability::DatabaseRead]
        }

        fn on_load(&mut self, _context: &PluginContext) -> Result<(), String> {
            Ok(())
        }

        fn on_unload(&mut self) -> Result<(), String> {
            Ok(())
        }

        fn commands(&self) -> Vec<Command> {
            vec![Command::new("test_command", |_params| {
                Ok(serde_json::json!({"success": true}))
            })]
        }

        fn migrations(&self) -> Vec<Migration> {
            vec![Migration::new(
                1,
                "CREATE TABLE test (id INTEGER PRIMARY KEY);",
                "初始化测试表",
            )]
        }
    }

    #[test]
    fn test_plugin_trait() {
        let plugin = TestPlugin;
        assert_eq!(plugin.id(), "test.plugin");
        assert_eq!(plugin.name(), "测试插件");
        assert_eq!(plugin.version(), "1.0.0");
        assert_eq!(plugin.capabilities().len(), 1);
        assert_eq!(plugin.commands().len(), 1);
        assert_eq!(plugin.migrations().len(), 1);
    }

    #[test]
    fn test_capability_equality() {
        assert_eq!(Capability::DatabaseRead, Capability::DatabaseRead);
        assert_ne!(Capability::DatabaseRead, Capability::DatabaseWrite);
    }

    #[test]
    fn test_migration_creation() {
        let migration = Migration::new(1, "CREATE TABLE test;", "测试迁移");
        assert_eq!(migration.version, 1);
        assert_eq!(migration.sql, "CREATE TABLE test;");
        assert_eq!(migration.description, "测试迁移");
    }
}
