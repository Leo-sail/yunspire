use crate::core::plugin::{Capability, Command, Migration, PluginContext, YunspirePlugin};
use serde_json::Value;

/// SearchPlugin - 搜索引擎插件
///
/// 负责云枢的搜索功能：
/// - 索引搜索
/// - 神经嵌入搜索
/// - 混合搜索（词法 + 向量）
pub struct SearchPlugin {
    /// 插件是否已初始化
    initialized: bool,
}

impl SearchPlugin {
    /// 创建新的搜索插件实例
    pub fn new() -> Self {
        Self {
            initialized: false,
        }
    }
}

impl Default for SearchPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl YunspirePlugin for SearchPlugin {
    fn id(&self) -> &str {
        "yunspire.search"
    }

    fn name(&self) -> &str {
        "搜索引擎"
    }

    fn version(&self) -> &str {
        "1.0.0"
    }

    fn description(&self) -> &str {
        "云枢搜索引擎插件，提供索引搜索、神经嵌入搜索和混合搜索功能"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::DatabaseRead,
            Capability::DatabaseWrite,
            Capability::ModelAccess, // 需要访问嵌入模型
        ]
    }

    fn dependencies(&self) -> Vec<String> {
        // 未来可能依赖其他插件
        vec![]
    }

    fn on_load(&mut self, _context: &PluginContext) -> Result<(), String> {
        log::info!("搜索引擎插件正在初始化...");

        // 在这里进行搜索引擎初始化
        // 例如：预热缓存、检查索引状态等

        self.initialized = true;
        log::info!("搜索引擎插件初始化成功");

        Ok(())
    }

    fn on_unload(&mut self) -> Result<(), String> {
        log::info!("搜索引擎插件正在卸载...");

        // 清理资源
        // 例如：保存缓存、关闭连接等

        self.initialized = false;
        log::info!("搜索引擎插件卸载成功");

        Ok(())
    }

    fn commands(&self) -> Vec<Command> {
        vec![
            // 命令 1: indexed_search
            Command::new("indexed_search", |_params| {
                // TODO: 实现搜索命令
                // 当前委托给 runtime_db.rs 中的实现
                Err("indexed_search 命令尚未完全迁移".to_string())
            }),

            // 命令 2: get_neural_embedding_index_status
            Command::new("get_neural_embedding_index_status", |_params| {
                // TODO: 实现状态查询命令
                Err("get_neural_embedding_index_status 命令尚未完全迁移".to_string())
            }),

            // 命令 3: rebuild_neural_embedding_index
            Command::new("rebuild_neural_embedding_index", |_params| {
                // TODO: 实现索引重建命令
                Err("rebuild_neural_embedding_index 命令尚未完全迁移".to_string())
            }),
        ]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            // 迁移 1: 创建搜索索引表
            Migration::new(
                1,
                "-- 笔记索引表（已存在于 runtime_db.rs，这里记录 schema）
                 -- CREATE TABLE IF NOT EXISTS note_index (...)

                 -- 笔记全文搜索表
                 -- CREATE VIRTUAL TABLE IF NOT EXISTS note_fts USING fts5 (...)

                 -- 本地特征向量表
                 -- CREATE TABLE IF NOT EXISTS local_feature_vectors (...)

                 SELECT 1; -- 占位 SQL（schema 已存在）",
                "搜索索引表（schema 已存在）",
            ),

            // 迁移 2: 创建神经嵌入缓存表
            Migration::new(
                2,
                "-- 神经嵌入缓存表
                 CREATE TABLE IF NOT EXISTS neural_embedding_cache (
                     workspace_scope TEXT NOT NULL,
                     provider_id TEXT NOT NULL,
                     model TEXT NOT NULL,
                     input_hash TEXT NOT NULL,
                     dimensions INTEGER NOT NULL,
                     vector_blob BLOB NOT NULL,
                     created_at TEXT NOT NULL,
                     last_used_at TEXT NOT NULL,
                     PRIMARY KEY(workspace_scope, provider_id, model, input_hash)
                 );

                 CREATE INDEX IF NOT EXISTS idx_neural_embedding_cache_last_used
                   ON neural_embedding_cache(last_used_at);",
                "创建神经嵌入缓存表",
            ),

            // 迁移 3: 创建笔记神经嵌入绑定表
            Migration::new(
                3,
                "CREATE TABLE IF NOT EXISTS note_neural_embeddings (
                     workspace_scope TEXT NOT NULL,
                     provider_id TEXT NOT NULL,
                     model TEXT NOT NULL,
                     vault_id TEXT NOT NULL,
                     relative_path TEXT NOT NULL,
                     content_hash TEXT NOT NULL,
                     input_hash TEXT NOT NULL,
                     created_at TEXT NOT NULL,
                     PRIMARY KEY(workspace_scope, provider_id, model, vault_id, relative_path)
                 );

                 CREATE INDEX IF NOT EXISTS idx_note_neural_embedding_lookup
                   ON note_neural_embeddings(workspace_scope, provider_id, model, vault_id, content_hash);",
                "创建笔记神经嵌入绑定表",
            ),

            // 迁移 4: 创建神经嵌入索引状态表
            Migration::new(
                4,
                "CREATE TABLE IF NOT EXISTS neural_embedding_index_state (
                     workspace_scope TEXT NOT NULL,
                     vault_id TEXT NOT NULL,
                     provider_id TEXT NOT NULL,
                     model TEXT NOT NULL,
                     indexed_notes INTEGER NOT NULL,
                     updated_at TEXT NOT NULL,
                     PRIMARY KEY(workspace_scope, vault_id, provider_id, model)
                 );",
                "创建神经嵌入索引状态表",
            ),
        ]
    }

    fn config_schema(&self) -> Option<Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "max_search_results": {
                    "type": "integer",
                    "description": "最大搜索结果数",
                    "default": 100,
                    "minimum": 10,
                    "maximum": 1000
                },
                "enable_neural_search": {
                    "type": "boolean",
                    "description": "是否启用神经嵌入搜索",
                    "default": true
                },
                "neural_embedding_batch_size": {
                    "type": "integer",
                    "description": "神经嵌入批处理大小",
                    "default": 32,
                    "minimum": 1,
                    "maximum": 128
                },
                "slow_query_threshold_ms": {
                    "type": "integer",
                    "description": "慢查询阈值（毫秒）",
                    "default": 100,
                    "minimum": 10,
                    "maximum": 5000
                }
            }
        }))
    }

    fn health_check(&self) -> Result<(), String> {
        if !self.initialized {
            return Err("搜索引擎插件未初始化".to_string());
        }

        // TODO: 添加更多健康检查
        // - 检查搜索索引完整性
        // - 检查神经嵌入缓存状态
        // - 检查模型访问权限

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_metadata() {
        let plugin = SearchPlugin::new();
        assert_eq!(plugin.id(), "yunspire.search");
        assert_eq!(plugin.name(), "搜索引擎");
        assert_eq!(plugin.version(), "1.0.0");
    }

    #[test]
    fn test_plugin_capabilities() {
        let plugin = SearchPlugin::new();
        let caps = plugin.capabilities();
        assert_eq!(caps.len(), 3);
        assert!(caps.contains(&Capability::DatabaseRead));
        assert!(caps.contains(&Capability::DatabaseWrite));
        assert!(caps.contains(&Capability::ModelAccess));
    }

    #[test]
    fn test_plugin_commands() {
        let plugin = SearchPlugin::new();
        let commands = plugin.commands();
        assert_eq!(commands.len(), 3);
        assert_eq!(commands[0].name, "indexed_search");
        assert_eq!(commands[1].name, "get_neural_embedding_index_status");
        assert_eq!(commands[2].name, "rebuild_neural_embedding_index");
    }

    #[test]
    fn test_plugin_migrations() {
        let plugin = SearchPlugin::new();
        let migrations = plugin.migrations();
        assert_eq!(migrations.len(), 4);
        assert_eq!(migrations[0].version, 1);
        assert_eq!(migrations[1].version, 2);
        assert_eq!(migrations[2].version, 3);
        assert_eq!(migrations[3].version, 4);
    }

    #[test]
    fn test_plugin_config_schema() {
        let plugin = SearchPlugin::new();
        let schema = plugin.config_schema();
        assert!(schema.is_some());

        let schema = schema.unwrap();
        assert!(schema["properties"]["max_search_results"].is_object());
        assert!(schema["properties"]["enable_neural_search"].is_object());
    }

    #[test]
    fn test_plugin_lifecycle() {
        let mut plugin = SearchPlugin::new();
        assert!(!plugin.initialized);

        // 模拟加载（需要真实的 PluginContext）
        // plugin.on_load(&context).unwrap();
        // assert!(plugin.initialized);

        // 模拟卸载
        // plugin.on_unload().unwrap();
        // assert!(!plugin.initialized);
    }

    #[test]
    fn test_health_check_before_init() {
        let plugin = SearchPlugin::new();
        let result = plugin.health_check();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("未初始化"));
    }
}
