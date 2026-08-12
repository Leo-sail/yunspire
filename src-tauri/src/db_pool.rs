use rusqlite::{Connection, Result as SqliteResult};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// 数据库连接池配置
#[derive(Clone, Debug)]
pub struct DatabasePoolConfig {
    /// 最大连接数
    pub max_connections: usize,
    /// 连接超时时间（秒）
    pub connection_timeout_secs: u64,
    /// 空闲连接保持时间（秒）
    pub idle_timeout_secs: u64,
    /// 是否启用预写日志（WAL）模式
    pub enable_wal: bool,
    /// 是否启用外键约束
    pub enable_foreign_keys: bool,
}

impl Default for DatabasePoolConfig {
    fn default() -> Self {
        Self {
            max_connections: 4,
            connection_timeout_secs: 30,
            idle_timeout_secs: 300,
            enable_wal: true,
            enable_foreign_keys: true,
        }
    }
}

/// 数据库连接包装器
pub struct DatabaseConnection {
    connection: Connection,
    last_used: std::time::Instant,
}

impl DatabaseConnection {
    fn new(connection: Connection) -> Self {
        Self {
            connection,
            last_used: std::time::Instant::now(),
        }
    }

    fn touch(&mut self) {
        self.last_used = std::time::Instant::now();
    }

    fn is_idle(&self, idle_timeout: Duration) -> bool {
        self.last_used.elapsed() > idle_timeout
    }
}

/// 简化版数据库连接池
pub struct DatabasePool {
    connections: Arc<Mutex<Vec<DatabaseConnection>>>,
    config: DatabasePoolConfig,
    db_path: String,
}

impl DatabasePool {
    /// 创建新的数据库连接池
    pub fn new(db_path: String, config: DatabasePoolConfig) -> SqliteResult<Self> {
        let pool = Self {
            connections: Arc::new(Mutex::new(Vec::new())),
            config,
            db_path,
        };

        // 预创建一个连接
        let conn = pool.create_connection()?;
        pool.connections.lock().unwrap().push(conn);

        Ok(pool)
    }

    /// 创建新连接
    fn create_connection(&self) -> SqliteResult<DatabaseConnection> {
        let connection = Connection::open(&self.db_path)?;

        // 配置连接
        if self.config.enable_wal {
            connection.execute_batch("PRAGMA journal_mode=WAL")?;
        }

        if self.config.enable_foreign_keys {
            connection.execute_batch("PRAGMA foreign_keys=ON")?;
        }

        // 性能优化设置
        connection.execute_batch(
            "PRAGMA synchronous=NORMAL;
             PRAGMA cache_size=-64000;
             PRAGMA temp_store=MEMORY;
             PRAGMA mmap_size=30000000000;",
        )?;

        Ok(DatabaseConnection::new(connection))
    }

    /// 获取连接
    pub fn get_connection(&self) -> SqliteResult<PooledConnection> {
        let mut connections = self.connections.lock().unwrap();

        // 查找可用连接
        if let Some(mut conn) = connections.pop() {
            conn.touch();
            return Ok(PooledConnection {
                connection: Some(conn),
                pool: self.connections.clone(),
            });
        }

        // 创建新连接（如果未达到上限）
        if connections.len() < self.config.max_connections {
            drop(connections);
            let conn = self.create_connection()?;
            return Ok(PooledConnection {
                connection: Some(conn),
                pool: self.connections.clone(),
            });
        }

        // 等待可用连接（简化版：直接创建临时连接）
        drop(connections);
        let conn = self.create_connection()?;
        Ok(PooledConnection {
            connection: Some(conn),
            pool: self.connections.clone(),
        })
    }

    /// 清理空闲连接
    pub fn cleanup_idle_connections(&self) {
        let idle_timeout = Duration::from_secs(self.config.idle_timeout_secs);
        let mut connections = self.connections.lock().unwrap();
        connections.retain(|conn| !conn.is_idle(idle_timeout));
    }
}

/// 池化连接（RAII 模式）
pub struct PooledConnection {
    connection: Option<DatabaseConnection>,
    pool: Arc<Mutex<Vec<DatabaseConnection>>>,
}

impl PooledConnection {
    /// 获取底层连接的可变引用
    pub fn as_mut(&mut self) -> &mut Connection {
        &mut self.connection.as_mut().unwrap().connection
    }

    /// 执行查询并自动处理错误
    pub fn execute<P>(&mut self, sql: &str, params: P) -> SqliteResult<usize>
    where
        P: rusqlite::Params,
    {
        self.as_mut().execute(sql, params)
    }

    /// 执行查询并返回单行结果
    pub fn query_row<T, P, F>(&mut self, sql: &str, params: P, f: F) -> SqliteResult<T>
    where
        P: rusqlite::Params,
        F: FnOnce(&rusqlite::Row<'_>) -> SqliteResult<T>,
    {
        self.as_mut().query_row(sql, params, f)
    }
}

impl Drop for PooledConnection {
    fn drop(&mut self) {
        if let Some(mut conn) = self.connection.take() {
            conn.touch();
            if let Ok(mut pool) = self.pool.lock() {
                pool.push(conn);
            }
        }
    }
}

/// 数据库事务包装器
pub struct DatabaseTransaction<'a> {
    transaction: Option<rusqlite::Transaction<'a>>,
}

impl<'a> DatabaseTransaction<'a> {
    pub fn new(connection: &'a mut Connection) -> SqliteResult<Self> {
        let transaction = connection.transaction()?;
        Ok(Self {
            transaction: Some(transaction),
        })
    }

    /// 提交事务
    pub fn commit(mut self) -> SqliteResult<()> {
        if let Some(tx) = self.transaction.take() {
            tx.commit()?;
        }
        Ok(())
    }

    /// 回滚事务
    pub fn rollback(mut self) -> SqliteResult<()> {
        if let Some(tx) = self.transaction.take() {
            tx.rollback()?;
        }
        Ok(())
    }

    /// 获取事务引用
    pub fn as_ref(&self) -> &rusqlite::Transaction<'a> {
        self.transaction.as_ref().unwrap()
    }
}

impl<'a> Drop for DatabaseTransaction<'a> {
    fn drop(&mut self) {
        // 如果未显式提交或回滚，自动回滚
        if let Some(tx) = self.transaction.take() {
            let _ = tx.rollback();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_database_pool_creation() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let config = DatabasePoolConfig::default();

        let pool = DatabasePool::new(db_path.to_str().unwrap().to_string(), config);
        assert!(pool.is_ok());
    }

    #[test]
    fn test_get_connection() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let config = DatabasePoolConfig::default();

        let pool = DatabasePool::new(db_path.to_str().unwrap().to_string(), config).unwrap();
        let conn = pool.get_connection();
        assert!(conn.is_ok());
    }

    #[test]
    fn test_connection_reuse() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let config = DatabasePoolConfig::default();

        let pool = DatabasePool::new(db_path.to_str().unwrap().to_string(), config).unwrap();

        {
            let _conn1 = pool.get_connection().unwrap();
        }

        let conn2 = pool.get_connection().unwrap();
        assert!(pool.connections.lock().unwrap().is_empty());
    }
}
