use crate::db_pool::{DatabasePool, DatabasePoolConfig, PooledConnection};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// 增强的 RuntimeDatabase 包装器
pub struct EnhancedRuntimeDatabase {
    /// 数据库连接池
    pool: Arc<DatabasePool>,
    /// 数据库路径
    path: PathBuf,
    /// 旧的单连接（向后兼容）
    legacy_connection: Mutex<Connection>,
}

impl EnhancedRuntimeDatabase {
    /// 创建新的增强数据库实例
    pub fn new(path: PathBuf) -> Result<Self, String> {
        let pool_config = DatabasePoolConfig {
            max_connections: 8, // 增加到 8 个连接
            connection_timeout_secs: 30,
            idle_timeout_secs: 300,
            enable_wal: true,
            enable_foreign_keys: true,
        };

        let pool = DatabasePool::new(
            path.to_str()
                .ok_or("无效的数据库路径")?
                .to_string(),
            pool_config,
        )
        .map_err(|e| format!("创建数据库连接池失败: {}", e))?;

        // 保留旧的连接用于向后兼容
        let legacy_connection = Connection::open(&path)
            .map_err(|e| format!("打开数据库连接失败: {}", e))?;

        Ok(Self {
            pool: Arc::new(pool),
            path,
            legacy_connection: Mutex::new(legacy_connection),
        })
    }

    /// 获取池化连接
    pub fn get_connection(&self) -> Result<PooledConnection, String> {
        self.pool
            .get_connection()
            .map_err(|e| format!("获取数据库连接失败: {}", e))
    }

    /// 获取数据库路径
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// 获取连接池（用于共享）
    pub fn pool(&self) -> Arc<DatabasePool> {
        self.pool.clone()
    }

    /// 清理空闲连接
    pub fn cleanup_idle_connections(&self) {
        self.pool.cleanup_idle_connections();
    }

    /// 向后兼容：获取旧的单连接
    #[allow(dead_code)]
    pub fn legacy_connection(&self) -> &Mutex<Connection> {
        &self.legacy_connection
    }
}

/// 数据库迁移管理器
pub struct DatabaseMigrationManager {
    connection: Connection,
    current_version: i64,
}

impl DatabaseMigrationManager {
    pub fn new(path: &PathBuf) -> Result<Self, String> {
        let connection = Connection::open(path)
            .map_err(|e| format!("打开数据库失败: {}", e))?;

        let current_version = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap_or(0);

        Ok(Self {
            connection,
            current_version,
        })
    }

    /// 执行迁移
    pub fn migrate(&mut self, target_version: i64) -> Result<(), String> {
        if self.current_version >= target_version {
            return Ok(());
        }

        eprintln!(
            "开始数据库迁移: v{} -> v{}",
            self.current_version, target_version
        );

        // 这里可以根据版本号执行不同的迁移脚本
        // 示例：
        // for version in (self.current_version + 1)..=target_version {
        //     self.migrate_to_version(version)?;
        // }

        self.connection
            .execute(&format!("PRAGMA user_version = {}", target_version), [])
            .map_err(|e| format!("更新 schema 版本失败: {}", e))?;

        self.current_version = target_version;
        eprintln!("数据库迁移完成: v{}", target_version);

        Ok(())
    }

    /// 迁移到特定版本
    #[allow(dead_code)]
    fn migrate_to_version(&mut self, version: i64) -> Result<(), String> {
        eprintln!("迁移到版本 {}", version);

        // 根据版本号执行特定的 SQL
        match version {
            46 => {
                // v46: 添加性能监控相关表
                self.connection
                    .execute_batch(
                        "
                        CREATE TABLE IF NOT EXISTS performance_metrics (
                            id INTEGER PRIMARY KEY AUTOINCREMENT,
                            operation TEXT NOT NULL,
                            duration_ms INTEGER NOT NULL,
                            timestamp TEXT NOT NULL,
                            details TEXT
                        );
                        CREATE INDEX IF NOT EXISTS idx_performance_timestamp
                            ON performance_metrics(timestamp);
                        ",
                    )
                    .map_err(|e| format!("v46 迁移失败: {}", e))?;
            }
            _ => {
                return Err(format!("未知的迁移版本: {}", version));
            }
        }

        Ok(())
    }

    /// 获取当前版本
    pub fn current_version(&self) -> i64 {
        self.current_version
    }
}

/// 数据库事务辅助工具
pub struct TransactionHelper;

impl TransactionHelper {
    /// 在事务中执行操作
    pub fn in_transaction<F, T>(connection: &mut Connection, f: F) -> Result<T, String>
    where
        F: FnOnce(&rusqlite::Transaction) -> Result<T, String>,
    {
        let tx = connection
            .transaction()
            .map_err(|e| format!("开启事务失败: {}", e))?;

        let result = f(&tx)?;

        tx.commit()
            .map_err(|e| format!("提交事务失败: {}", e))?;

        Ok(result)
    }

    /// 带重试的事务执行
    pub fn in_transaction_with_retry<F, T>(
        connection: &mut Connection,
        max_retries: u32,
        f: F,
    ) -> Result<T, String>
    where
        F: Fn(&rusqlite::Transaction) -> Result<T, String>,
    {
        let mut attempt = 0;

        loop {
            match Self::in_transaction(connection, &f) {
                Ok(result) => return Ok(result),
                Err(e) if attempt < max_retries && e.contains("database is locked") => {
                    attempt += 1;
                    eprintln!("事务重试 {}/{}: {}", attempt, max_retries, e);
                    std::thread::sleep(std::time::Duration::from_millis(100 * attempt as u64));
                }
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_enhanced_database_creation() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        let db = EnhancedRuntimeDatabase::new(db_path.clone());
        assert!(db.is_ok());
    }

    #[test]
    fn test_get_pooled_connection() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        let db = EnhancedRuntimeDatabase::new(db_path).unwrap();
        let conn = db.get_connection();
        assert!(conn.is_ok());
    }

    #[test]
    fn test_migration_manager() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        // 创建数据库
        Connection::open(&db_path).unwrap();

        let mut manager = DatabaseMigrationManager::new(&db_path).unwrap();
        assert_eq!(manager.current_version(), 0);

        // 迁移到版本 46
        let result = manager.migrate(46);
        assert!(result.is_ok());
        assert_eq!(manager.current_version(), 46);
    }
}
