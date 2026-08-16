/// TaskPlugin - 任务管理插件
///
/// 提供完整的任务管理功能

use crate::core::plugin::{Capability, Command, Migration, PluginContext, YunspirePlugin};
use serde_json::Value;

/// TaskPlugin - 任务管理插件
pub struct TaskPlugin {
    /// 插件是否已初始化
    initialized: bool,
}

impl TaskPlugin {
    /// 创建新的任务管理插件实例
    pub fn new() -> Self {
        Self {
            initialized: false,
        }
    }
}

impl Default for TaskPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl YunspirePlugin for TaskPlugin {
    fn id(&self) -> &str {
        "yunspire.task"
    }

    fn name(&self) -> &str {
        "任务管理"
    }

    fn version(&self) -> &str {
        "1.0.0"
    }

    fn description(&self) -> &str {
        "云枢任务管理插件，提供任务生命周期、步骤管理和恢复机制"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::DatabaseRead, Capability::DatabaseWrite]
    }

    fn dependencies(&self) -> Vec<String> {
        vec![]
    }

    fn on_load(&mut self, _context: &PluginContext) -> Result<(), String> {
        log::info!("任务管理插件正在初始化...");

        // 插件初始化逻辑
        // 例如：初始化任务队列、恢复中断任务等

        self.initialized = true;
        log::info!("任务管理插件初始化成功");

        Ok(())
    }

    fn on_unload(&mut self) -> Result<(), String> {
        log::info!("任务管理插件正在卸载...");

        // 清理资源
        // 例如：保存任务状态、清理租约等

        self.initialized = false;
        log::info!("任务管理插件卸载成功");

        Ok(())
    }

    fn commands(&self) -> Vec<Command> {
        vec![
            // 生命周期命令
            Command::new("create_task", |_params| {
                Err("create_task 需要通过桥接层实现".to_string())
            }),
            Command::new("list_tasks", |_params| {
                Err("list_tasks 需要通过桥接层实现".to_string())
            }),
            Command::new("get_task", |_params| {
                Err("get_task 需要通过桥接层实现".to_string())
            }),
            // 步骤管理命令
            Command::new("claim_steps", |_params| {
                Err("claim_steps 需要通过桥接层实现".to_string())
            }),
            Command::new("complete_step", |_params| {
                Err("complete_step 需要通过桥接层实现".to_string())
            }),
            // 恢复命令
            Command::new("recover_tasks", |_params| {
                Err("recover_tasks 需要通过桥接层实现".to_string())
            }),
        ]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            // 迁移 1: 创建任务表
            Migration::new(
                1,
                "CREATE TABLE IF NOT EXISTS runtime_tasks (
                     task_id TEXT PRIMARY KEY NOT NULL,
                     workspace_scope TEXT NOT NULL,
                     task_kind TEXT NOT NULL,
                     state TEXT NOT NULL,
                     payload TEXT,
                     result TEXT,
                     error TEXT,
                     created_at TEXT NOT NULL,
                     updated_at TEXT NOT NULL,
                     plan_revision INTEGER
                 );
                 CREATE INDEX IF NOT EXISTS idx_runtime_tasks_workspace
                   ON runtime_tasks(workspace_scope, state);
                 CREATE INDEX IF NOT EXISTS idx_runtime_tasks_created
                   ON runtime_tasks(created_at);",
                "创建运行时任务表",
            ),
            // 迁移 2: 创建任务步骤表
            Migration::new(
                2,
                "CREATE TABLE IF NOT EXISTS runtime_task_steps (
                     step_id TEXT PRIMARY KEY NOT NULL,
                     task_id TEXT NOT NULL,
                     step_kind TEXT NOT NULL,
                     title TEXT NOT NULL,
                     state TEXT NOT NULL,
                     depends_on TEXT,
                     parameters TEXT,
                     result TEXT,
                     error TEXT,
                     created_at TEXT NOT NULL,
                     updated_at TEXT NOT NULL,
                     FOREIGN KEY(task_id) REFERENCES runtime_tasks(task_id)
                 );
                 CREATE INDEX IF NOT EXISTS idx_task_steps_task
                   ON runtime_task_steps(task_id, state);",
                "创建运行时任务步骤表",
            ),
            // 迁移 3: 创建步骤租约表
            Migration::new(
                3,
                "CREATE TABLE IF NOT EXISTS runtime_step_leases (
                     step_id TEXT PRIMARY KEY NOT NULL,
                     holder TEXT NOT NULL,
                     expires_at TEXT NOT NULL,
                     renewal_count INTEGER NOT NULL DEFAULT 0,
                     created_at TEXT NOT NULL,
                     FOREIGN KEY(step_id) REFERENCES runtime_task_steps(step_id)
                 );
                 CREATE INDEX IF NOT EXISTS idx_step_leases_expires
                   ON runtime_step_leases(expires_at);",
                "创建步骤租约表",
            ),
            // 迁移 4: 创建任务恢复表
            Migration::new(
                4,
                "CREATE TABLE IF NOT EXISTS runtime_task_recovery (
                     task_id TEXT PRIMARY KEY NOT NULL,
                     recommendation TEXT NOT NULL,
                     resume_step_id TEXT,
                     evidence TEXT,
                     detail TEXT,
                     detected_at TEXT NOT NULL,
                     FOREIGN KEY(task_id) REFERENCES runtime_tasks(task_id)
                 );",
                "创建任务恢复表",
            ),
        ]
    }

    fn config_schema(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "max_concurrent_tasks": {
                    "type": "integer",
                    "description": "最大并发任务数",
                    "default": 10,
                    "minimum": 1,
                    "maximum": 100
                },
                "default_lease_duration_seconds": {
                    "type": "integer",
                    "description": "默认租约时长（秒）",
                    "default": 300,
                    "minimum": 60,
                    "maximum": 3600
                },
                "auto_recover_interrupted_tasks": {
                    "type": "boolean",
                    "description": "是否自动恢复中断的任务",
                    "default": true
                }
            }
        }))
    }

    fn health_check(&self) -> Result<(), String> {
        if !self.initialized {
            return Err("任务管理插件未初始化".to_string());
        }

        // TODO: 添加更多健康检查
        // - 检查数据库表结构
        // - 检查是否有过期的租约需要清理
        // - 检查是否有中断的任务需要恢复

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_metadata() {
        let plugin = TaskPlugin::new();
        assert_eq!(plugin.id(), "yunspire.task");
        assert_eq!(plugin.name(), "任务管理");
        assert_eq!(plugin.version(), "1.0.0");
        assert!(!plugin.description().is_empty());
    }

    #[test]
    fn test_plugin_capabilities() {
        let plugin = TaskPlugin::new();
        let caps = plugin.capabilities();
        assert_eq!(caps.len(), 2);
        assert!(caps.contains(&Capability::DatabaseRead));
        assert!(caps.contains(&Capability::DatabaseWrite));
    }

    #[test]
    fn test_plugin_commands() {
        let plugin = TaskPlugin::new();
        let commands = plugin.commands();
        assert_eq!(commands.len(), 6);
        assert_eq!(commands[0].name, "create_task");
        assert_eq!(commands[1].name, "list_tasks");
        assert_eq!(commands[2].name, "get_task");
        assert_eq!(commands[3].name, "claim_steps");
        assert_eq!(commands[4].name, "complete_step");
        assert_eq!(commands[5].name, "recover_tasks");
    }

    #[test]
    fn test_plugin_migrations() {
        let plugin = TaskPlugin::new();
        let migrations = plugin.migrations();
        assert_eq!(migrations.len(), 4);
        assert_eq!(migrations[0].version, 1);
        assert_eq!(migrations[1].version, 2);
        assert_eq!(migrations[2].version, 3);
        assert_eq!(migrations[3].version, 4);
    }

    #[test]
    fn test_plugin_config_schema() {
        let plugin = TaskPlugin::new();
        let schema = plugin.config_schema();
        assert!(schema.is_some());

        let schema = schema.unwrap();
        assert!(schema["properties"]["max_concurrent_tasks"].is_object());
        assert!(schema["properties"]["default_lease_duration_seconds"].is_object());
        assert!(schema["properties"]["auto_recover_interrupted_tasks"].is_object());
    }

    #[test]
    fn test_health_check_before_init() {
        let plugin = TaskPlugin::new();
        let result = plugin.health_check();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("未初始化"));
    }

    #[test]
    fn test_plugin_default() {
        let plugin = TaskPlugin::default();
        assert_eq!(plugin.id(), "yunspire.task");
    }
}
