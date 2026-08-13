/// 数据库配置（替代硬编码常量）
#[derive(Clone, Debug)]
pub struct DatabaseConfig {
    /// 最大快照记录数
    pub max_snapshot_records: usize,
    /// 最大记录字节数
    pub max_record_bytes: usize,
    /// Vault 索引批处理大小
    pub vault_index_batch_size: usize,
    /// 连接池大小
    pub connection_pool_size: usize,
    /// 查询超时时间（毫秒）
    pub query_timeout_ms: u64,
    /// 慢查询阈值（毫秒）
    pub slow_query_threshold_ms: u64,
    /// 缓存容量
    pub cache_capacity: usize,
    /// 最大嵌入向量输入字符数
    pub max_embedding_input_chars: usize,
    /// 神经嵌入批处理大小
    pub neural_embedding_batch_size: usize,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            max_snapshot_records: 10_000,
            max_record_bytes: 2 * 1024 * 1024,
            vault_index_batch_size: 32,
            connection_pool_size: 8,
            query_timeout_ms: 30_000,
            slow_query_threshold_ms: 100,
            cache_capacity: 1000,
            max_embedding_input_chars: 24_000,
            neural_embedding_batch_size: 32,
        }
    }
}

impl DatabaseConfig {
    /// 创建高性能配置（适用于高端机器）
    pub fn high_performance() -> Self {
        Self {
            connection_pool_size: 16,
            vault_index_batch_size: 64,
            cache_capacity: 5000,
            neural_embedding_batch_size: 64,
            ..Default::default()
        }
    }

    /// 创建低资源配置（适用于低端机器或节能模式）
    pub fn low_resource() -> Self {
        Self {
            connection_pool_size: 4,
            vault_index_batch_size: 16,
            cache_capacity: 500,
            neural_embedding_batch_size: 16,
            ..Default::default()
        }
    }

    /// 验证配置合理性
    pub fn validate(&self) -> Result<(), String> {
        if self.connection_pool_size == 0 {
            return Err("连接池大小必须大于 0".to_string());
        }
        if self.vault_index_batch_size == 0 {
            return Err("批处理大小必须大于 0".to_string());
        }
        if self.slow_query_threshold_ms == 0 {
            return Err("慢查询阈值必须大于 0".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = DatabaseConfig::default();
        assert_eq!(config.connection_pool_size, 8);
        assert_eq!(config.vault_index_batch_size, 32);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_high_performance_config() {
        let config = DatabaseConfig::high_performance();
        assert_eq!(config.connection_pool_size, 16);
        assert_eq!(config.vault_index_batch_size, 64);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_low_resource_config() {
        let config = DatabaseConfig::low_resource();
        assert_eq!(config.connection_pool_size, 4);
        assert_eq!(config.vault_index_batch_size, 16);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_invalid_config() {
        let mut config = DatabaseConfig::default();
        config.connection_pool_size = 0;
        assert!(config.validate().is_err());
    }
}
