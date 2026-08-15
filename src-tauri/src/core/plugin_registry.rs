use super::plugin::{Capability, Command, Migration, PluginContext, YunspirePlugin};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// 插件注册表（管理所有插件）
pub struct PluginRegistry {
    /// 已注册的插件（插件 ID -> 插件实例）
    plugins: HashMap<String, Box<dyn YunspirePlugin>>,
    /// 插件加载顺序（按依赖关系排序）
    load_order: Vec<String>,
    /// 已加载的插件 ID
    loaded: HashSet<String>,
}

impl PluginRegistry {
    /// 创建新的插件注册表
    pub fn new() -> Self {
        Self {
            plugins: HashMap::new(),
            load_order: Vec::new(),
            loaded: HashSet::new(),
        }
    }

    /// 注册插件
    ///
    /// # 参数
    /// - `plugin`: 插件实例
    ///
    /// # 返回
    /// - `Ok(())`: 注册成功
    /// - `Err(String)`: 注册失败（如依赖缺失、ID 冲突等）
    pub fn register(&mut self, plugin: Box<dyn YunspirePlugin>) -> Result<(), String> {
        let id = plugin.id().to_string();

        // 检查插件 ID 是否已存在
        if self.plugins.contains_key(&id) {
            return Err(format!("插件 ID 冲突: {}", id));
        }

        // 检查依赖是否满足
        for dep in plugin.dependencies() {
            if !self.plugins.contains_key(&dep) {
                return Err(format!(
                    "插件 {} 依赖的插件 {} 未注册",
                    plugin.name(),
                    dep
                ));
            }
        }

        log::info!(
            "注册插件: {} ({}@{})",
            plugin.name(),
            plugin.id(),
            plugin.version()
        );

        // 插入插件
        self.plugins.insert(id.clone(), plugin);

        // 计算加载顺序（拓扑排序）
        self.compute_load_order()?;

        Ok(())
    }

    /// 计算插件加载顺序（拓扑排序）
    fn compute_load_order(&mut self) -> Result<(), String> {
        let mut order = Vec::new();
        let mut visited = HashSet::new();
        let mut visiting = HashSet::new();

        for id in self.plugins.keys() {
            self.visit_plugin(id, &mut order, &mut visited, &mut visiting)?;
        }

        self.load_order = order;
        Ok(())
    }

    /// 深度优先遍历（检测循环依赖）
    fn visit_plugin(
        &self,
        id: &str,
        order: &mut Vec<String>,
        visited: &mut HashSet<String>,
        visiting: &mut HashSet<String>,
    ) -> Result<(), String> {
        if visited.contains(id) {
            return Ok(());
        }

        if visiting.contains(id) {
            return Err(format!("检测到循环依赖: {}", id));
        }

        visiting.insert(id.to_string());

        if let Some(plugin) = self.plugins.get(id) {
            // 先访问依赖
            for dep in plugin.dependencies() {
                self.visit_plugin(&dep, order, visited, visiting)?;
            }
        }

        visiting.remove(id);
        visited.insert(id.to_string());
        order.push(id.to_string());

        Ok(())
    }

    /// 按顺序加载所有插件
    ///
    /// # 参数
    /// - `context`: 插件上下文
    ///
    /// # 返回
    /// - `Ok(())`: 所有插件加载成功
    /// - `Err(String)`: 某个插件加载失败
    pub fn load_all(&mut self, context: &PluginContext) -> Result<(), String> {
        log::info!("开始加载 {} 个插件", self.load_order.len());

        for id in &self.load_order.clone() {
            if let Some(plugin) = self.plugins.get_mut(id) {
                log::info!("加载插件: {} ({})", plugin.name(), plugin.id());

                plugin.on_load(context).map_err(|e| {
                    format!("插件 {} 加载失败: {}", plugin.name(), e)
                })?;

                self.loaded.insert(id.clone());
                log::info!("插件加载成功: {}", plugin.name());
            }
        }

        log::info!("所有插件加载完成");
        Ok(())
    }

    /// 卸载所有插件
    ///
    /// # 返回
    /// - `Ok(())`: 所有插件卸载成功
    /// - `Err(String)`: 某个插件卸载失败
    pub fn unload_all(&mut self) -> Result<(), String> {
        log::info!("开始卸载 {} 个插件", self.loaded.len());

        // 反向卸载（先卸载依赖者）
        for id in self.load_order.iter().rev() {
            if !self.loaded.contains(id) {
                continue;
            }

            if let Some(plugin) = self.plugins.get_mut(id) {
                log::info!("卸载插件: {} ({})", plugin.name(), plugin.id());

                plugin.on_unload().map_err(|e| {
                    format!("插件 {} 卸载失败: {}", plugin.name(), e)
                })?;

                self.loaded.remove(id);
                log::info!("插件卸载成功: {}", plugin.name());
            }
        }

        log::info!("所有插件卸载完成");
        Ok(())
    }

    /// 获取所有已注册命令
    ///
    /// # 返回
    /// 所有插件注册的命令列表
    pub fn get_commands(&self) -> Vec<Command> {
        self.plugins
            .values()
            .flat_map(|plugin| plugin.commands())
            .collect()
    }

    /// 获取所有数据库迁移
    ///
    /// # 返回
    /// 所有插件的迁移脚本，按插件加载顺序和版本号排序
    pub fn get_migrations(&self) -> Vec<(String, Migration)> {
        let mut migrations = Vec::new();

        for id in &self.load_order {
            if let Some(plugin) = self.plugins.get(id) {
                for migration in plugin.migrations() {
                    migrations.push((plugin.id().to_string(), migration));
                }
            }
        }

        migrations
    }

    /// 执行所有数据库迁移
    ///
    /// # 参数
    /// - `connection`: 数据库连接
    ///
    /// # 返回
    /// - `Ok(usize)`: 执行的迁移数量
    /// - `Err(String)`: 迁移失败
    pub fn run_migrations(&self, connection: &Connection) -> Result<usize, String> {
        // 创建迁移记录表
        connection
            .execute(
                "CREATE TABLE IF NOT EXISTS plugin_migrations (
                    plugin_id TEXT NOT NULL,
                    version INTEGER NOT NULL,
                    description TEXT NOT NULL,
                    executed_at TEXT NOT NULL,
                    PRIMARY KEY (plugin_id, version)
                )",
                [],
            )
            .map_err(|e| format!("创建迁移表失败: {}", e))?;

        let mut executed = 0;

        for (plugin_id, migration) in self.get_migrations() {
            // 检查是否已执行
            let already_executed: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM plugin_migrations WHERE plugin_id = ?1 AND version = ?2)",
                    [&plugin_id, &migration.version.to_string()],
                    |row| row.get(0),
                )
                .unwrap_or(false);

            if already_executed {
                continue;
            }

            log::info!(
                "执行迁移: {} v{} - {}",
                plugin_id,
                migration.version,
                migration.description
            );

            // 执行迁移
            connection
                .execute_batch(&migration.sql)
                .map_err(|e| format!("迁移执行失败 ({}@{}): {}", plugin_id, migration.version, e))?;

            // 记录迁移
            let now = chrono::Utc::now().to_rfc3339();
            connection
                .execute(
                    "INSERT INTO plugin_migrations (plugin_id, version, description, executed_at) VALUES (?1, ?2, ?3, ?4)",
                    [&plugin_id, &migration.version.to_string(), &migration.description, &now],
                )
                .map_err(|e| format!("记录迁移失败: {}", e))?;

            executed += 1;
        }

        log::info!("迁移完成: 执行了 {} 个迁移", executed);
        Ok(executed)
    }

    /// 获取插件信息
    ///
    /// # 返回
    /// 所有插件的基本信息（ID、名称、版本、状态）
    pub fn get_plugin_info(&self) -> Vec<serde_json::Value> {
        self.plugins
            .values()
            .map(|plugin| {
                serde_json::json!({
                    "id": plugin.id(),
                    "name": plugin.name(),
                    "version": plugin.version(),
                    "description": plugin.description(),
                    "capabilities": plugin.capabilities().iter().map(|c| format!("{:?}", c)).collect::<Vec<_>>(),
                    "dependencies": plugin.dependencies(),
                    "loaded": self.loaded.contains(plugin.id()),
                })
            })
            .collect()
    }

    /// 健康检查所有插件
    ///
    /// # 返回
    /// - `Ok(())`: 所有插件健康
    /// - `Err(Vec<String>)`: 失败的插件列表
    pub fn health_check_all(&self) -> Result<(), Vec<String>> {
        let mut failures = Vec::new();

        for id in &self.load_order {
            if let Some(plugin) = self.plugins.get(id) {
                if let Err(e) = plugin.health_check() {
                    failures.push(format!("{}: {}", plugin.name(), e));
                }
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures)
        }
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::plugin::{Capability, Command, PluginContext};

    // 测试插件 A（无依赖）
    struct PluginA;
    impl YunspirePlugin for PluginA {
        fn id(&self) -> &str {
            "plugin.a"
        }
        fn name(&self) -> &str {
            "插件 A"
        }
        fn version(&self) -> &str {
            "1.0.0"
        }
        fn description(&self) -> &str {
            "测试插件 A"
        }
        fn capabilities(&self) -> Vec<Capability> {
            vec![]
        }
        fn on_load(&mut self, _context: &PluginContext) -> Result<(), String> {
            Ok(())
        }
        fn on_unload(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn commands(&self) -> Vec<Command> {
            vec![]
        }
        fn migrations(&self) -> Vec<Migration> {
            vec![]
        }
    }

    // 测试插件 B（依赖 A）
    struct PluginB;
    impl YunspirePlugin for PluginB {
        fn id(&self) -> &str {
            "plugin.b"
        }
        fn name(&self) -> &str {
            "插件 B"
        }
        fn version(&self) -> &str {
            "1.0.0"
        }
        fn description(&self) -> &str {
            "测试插件 B"
        }
        fn capabilities(&self) -> Vec<Capability> {
            vec![]
        }
        fn dependencies(&self) -> Vec<String> {
            vec!["plugin.a".to_string()]
        }
        fn on_load(&mut self, _context: &PluginContext) -> Result<(), String> {
            Ok(())
        }
        fn on_unload(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn commands(&self) -> Vec<Command> {
            vec![]
        }
        fn migrations(&self) -> Vec<Migration> {
            vec![]
        }
    }

    #[test]
    fn test_plugin_registration() {
        let mut registry = PluginRegistry::new();
        registry.register(Box::new(PluginA)).unwrap();
        assert_eq!(registry.plugins.len(), 1);
    }

    #[test]
    fn test_dependency_resolution() {
        let mut registry = PluginRegistry::new();
        registry.register(Box::new(PluginA)).unwrap();
        registry.register(Box::new(PluginB)).unwrap();

        // 插件 A 应该在插件 B 之前加载
        assert_eq!(registry.load_order[0], "plugin.a");
        assert_eq!(registry.load_order[1], "plugin.b");
    }

    #[test]
    fn test_dependency_missing() {
        let mut registry = PluginRegistry::new();
        // 插件 B 依赖插件 A，但 A 未注册
        let result = registry.register(Box::new(PluginB));
        assert!(result.is_err());
    }

    #[test]
    fn test_duplicate_plugin_id() {
        let mut registry = PluginRegistry::new();
        registry.register(Box::new(PluginA)).unwrap();
        let result = registry.register(Box::new(PluginA));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("插件 ID 冲突"));
    }
}
