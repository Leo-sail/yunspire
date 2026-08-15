use crate::core::plugin::{Capability, Command, Migration, PluginContext, YunspirePlugin};
use serde_json::Value;

/// 示例插件（展示如何实现 YunspirePlugin trait）
pub struct ExamplePlugin {
    /// 插件是否已初始化
    initialized: bool,
}

impl ExamplePlugin {
    /// 创建新的示例插件实例
    pub fn new() -> Self {
        Self { initialized: false }
    }
}

impl YunspirePlugin for ExamplePlugin {
    fn id(&self) -> &str {
        "yunspire.example"
    }

    fn name(&self) -> &str {
        "示例插件"
    }

    fn version(&self) -> &str {
        "1.0.0"
    }

    fn description(&self) -> &str {
        "这是一个示例插件，展示如何实现云枢插件接口"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::DatabaseRead,
            Capability::DatabaseWrite,
        ]
    }

    fn dependencies(&self) -> Vec<String> {
        // 如果依赖其他插件，在这里返回
        vec![]
    }

    fn on_load(&mut self, context: &PluginContext) -> Result<(), String> {
        log::info!("示例插件正在初始化...");

        // 在这里进行插件初始化
        // 例如：初始化缓存、连接外部服务等

        self.initialized = true;
        log::info!("示例插件初始化成功");

        Ok(())
    }

    fn on_unload(&mut self) -> Result<(), String> {
        log::info!("示例插件正在卸载...");

        // 在这里进行清理工作
        // 例如：关闭连接、保存状态等

        self.initialized = false;
        log::info!("示例插件卸载成功");

        Ok(())
    }

    fn commands(&self) -> Vec<Command> {
        vec![
            // 命令 1: 简单的 Hello World
            Command::new("example_hello", |params| {
                let name = params.get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("World");

                Ok(serde_json::json!({
                    "message": format!("Hello, {}!", name)
                }))
            }),

            // 命令 2: 计算平方
            Command::new("example_square", |params| {
                let number = params.get("number")
                    .and_then(|v| v.as_f64())
                    .ok_or("缺少参数: number")?;

                let result = number * number;

                Ok(serde_json::json!({
                    "number": number,
                    "square": result
                }))
            }),

            // 命令 3: 获取插件状态
            Command::new("example_get_status", |_params| {
                Ok(serde_json::json!({
                    "plugin": "yunspire.example",
                    "version": "1.0.0",
                    "status": "running"
                }))
            }),
        ]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            // 迁移 1: 创建示例表
            Migration::new(
                1,
                "CREATE TABLE IF NOT EXISTS example_data (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    value TEXT NOT NULL,
                    created_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_example_data_name ON example_data(name);",
                "创建示例数据表",
            ),

            // 迁移 2: 添加新列
            Migration::new(
                2,
                "ALTER TABLE example_data ADD COLUMN updated_at TEXT;",
                "添加更新时间列",
            ),
        ]
    }

    fn config_schema(&self) -> Option<Value> {
        // 插件配置的 JSON Schema
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "enabled": {
                    "type": "boolean",
                    "description": "是否启用示例插件",
                    "default": true
                },
                "max_cache_size": {
                    "type": "integer",
                    "description": "最大缓存大小（MB）",
                    "default": 100,
                    "minimum": 10,
                    "maximum": 1000
                }
            }
        }))
    }

    fn health_check(&self) -> Result<(), String> {
        if !self.initialized {
            return Err("插件未初始化".to_string());
        }

        // 在这里进行健康检查
        // 例如：检查数据库连接、外部服务状态等

        Ok(())
    }
}

impl Default for ExamplePlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_metadata() {
        let plugin = ExamplePlugin::new();
        assert_eq!(plugin.id(), "yunspire.example");
        assert_eq!(plugin.name(), "示例插件");
        assert_eq!(plugin.version(), "1.0.0");
    }

    #[test]
    fn test_plugin_capabilities() {
        let plugin = ExamplePlugin::new();
        let caps = plugin.capabilities();
        assert_eq!(caps.len(), 2);
        assert!(caps.contains(&Capability::DatabaseRead));
        assert!(caps.contains(&Capability::DatabaseWrite));
    }

    #[test]
    fn test_plugin_commands() {
        let plugin = ExamplePlugin::new();
        let commands = plugin.commands();
        assert_eq!(commands.len(), 3);

        // 测试命令名称
        assert_eq!(commands[0].name, "example_hello");
        assert_eq!(commands[1].name, "example_square");
        assert_eq!(commands[2].name, "example_get_status");
    }

    #[test]
    fn test_plugin_migrations() {
        let plugin = ExamplePlugin::new();
        let migrations = plugin.migrations();
        assert_eq!(migrations.len(), 2);
        assert_eq!(migrations[0].version, 1);
        assert_eq!(migrations[1].version, 2);
    }

    #[test]
    fn test_hello_command() {
        let plugin = ExamplePlugin::new();
        let commands = plugin.commands();
        let hello_cmd = &commands[0];

        // 测试不带参数
        let result = (hello_cmd.handler)(serde_json::json!({})).unwrap();
        assert_eq!(result["message"], "Hello, World!");

        // 测试带参数
        let result = (hello_cmd.handler)(serde_json::json!({"name": "Alice"})).unwrap();
        assert_eq!(result["message"], "Hello, Alice!");
    }

    #[test]
    fn test_square_command() {
        let plugin = ExamplePlugin::new();
        let commands = plugin.commands();
        let square_cmd = &commands[1];

        // 测试正常情况
        let result = (square_cmd.handler)(serde_json::json!({"number": 5.0})).unwrap();
        assert_eq!(result["number"], 5.0);
        assert_eq!(result["square"], 25.0);

        // 测试缺少参数
        let result = (square_cmd.handler)(serde_json::json!({}));
        assert!(result.is_err());
    }
}
