use std::sync::Arc;
use std::time::Instant;

/// 查询性能追踪器
pub struct QueryTracker {
    query_name: String,
    start_time: Instant,
    monitor: Option<Arc<crate::performance_monitor::PerformanceMonitor>>,
}

impl QueryTracker {
    /// 创建新的查询追踪器
    pub fn new(query_name: String) -> Self {
        Self {
            query_name,
            start_time: Instant::now(),
            monitor: None,
        }
    }

    /// 带性能监控的追踪器
    pub fn with_monitor(
        query_name: String,
        monitor: Arc<crate::performance_monitor::PerformanceMonitor>,
    ) -> Self {
        Self {
            query_name,
            start_time: Instant::now(),
            monitor: Some(monitor),
        }
    }

    /// 手动结束追踪
    pub fn finish(self) -> u64 {
        let duration_ms = self.start_time.elapsed().as_millis() as u64;
        if let Some(monitor) = &self.monitor {
            monitor.record_operation(self.query_name.clone(), duration_ms, None);
        }
        duration_ms
    }
}

impl Drop for QueryTracker {
    fn drop(&mut self) {
        let duration_ms = self.start_time.elapsed().as_millis() as u64;
        if let Some(monitor) = &self.monitor {
            monitor.record_operation(self.query_name.clone(), duration_ms, None);
        }

        // 慢查询日志（阈值 100ms）
        if duration_ms > 100 {
            eprintln!(
                "[SLOW QUERY] {} took {}ms",
                self.query_name, duration_ms
            );
        }
    }
}

/// 数据库操作宏：自动追踪性能
#[macro_export]
macro_rules! tracked_query {
    ($name:expr, $query:expr) => {{
        let _tracker = $crate::db_instrumentation::QueryTracker::new($name.to_string());
        $query
    }};
}

/// 批量操作优化器
pub struct BatchOperationOptimizer {
    batch_size: usize,
    _operations_count: usize,
}

impl BatchOperationOptimizer {
    pub fn new(batch_size: usize) -> Self {
        Self {
            batch_size,
            _operations_count: 0,
        }
    }

    /// 获取批量大小
    pub fn batch_size(&self) -> usize {
        self.batch_size
    }
}

/// 数据库健康检查
pub struct DatabaseHealthCheck {
    db_path: String,
}

impl DatabaseHealthCheck {
    pub fn new(db_path: String) -> Self {
        Self { db_path }
    }

    /// 检查数据库连接
    pub fn check_connection(&self) -> Result<(), String> {
        use rusqlite::Connection;

        let conn = Connection::open(&self.db_path)
            .map_err(|e| format!("数据库连接失败: {}", e))?;

        conn.execute_batch("SELECT 1")
            .map_err(|e| format!("数据库查询失败: {}", e))?;

        Ok(())
    }

    /// 检查数据库完整性
    pub fn check_integrity(&self) -> Result<bool, String> {
        use rusqlite::Connection;

        let conn = Connection::open(&self.db_path)
            .map_err(|e| format!("数据库连接失败: {}", e))?;

        let result: String = conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(|e| format!("完整性检查失败: {}", e))?;

        Ok(result == "ok")
    }

    /// 获取数据库统计信息
    pub fn get_statistics(&self) -> Result<DatabaseStatistics, String> {
        use rusqlite::Connection;

        let conn = Connection::open(&self.db_path)
            .map_err(|e| format!("数据库连接失败: {}", e))?;

        let page_count: i64 = conn
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .unwrap_or(0);

        let page_size: i64 = conn
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .unwrap_or(4096);

        let freelist_count: i64 = conn
            .query_row("PRAGMA freelist_count", [], |row| row.get(0))
            .unwrap_or(0);

        let database_size_bytes = page_count * page_size;
        let free_space_bytes = freelist_count * page_size;
        let used_space_bytes = database_size_bytes - free_space_bytes;

        Ok(DatabaseStatistics {
            database_size_bytes: database_size_bytes as u64,
            used_space_bytes: used_space_bytes as u64,
            free_space_bytes: free_space_bytes as u64,
            page_count: page_count as u64,
            page_size: page_size as u64,
            fragmentation_ratio: if database_size_bytes > 0 {
                (free_space_bytes as f64 / database_size_bytes as f64) * 100.0
            } else {
                0.0
            },
        })
    }
}

/// 数据库统计信息
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseStatistics {
    pub database_size_bytes: u64,
    pub used_space_bytes: u64,
    pub free_space_bytes: u64,
    pub page_count: u64,
    pub page_size: u64,
    pub fragmentation_ratio: f64,
}

/// 数据库维护操作
pub struct DatabaseMaintenance {
    db_path: String,
}

impl DatabaseMaintenance {
    pub fn new(db_path: String) -> Self {
        Self { db_path }
    }

    /// 优化数据库（VACUUM）
    pub fn optimize(&self) -> Result<(), String> {
        use rusqlite::Connection;

        let conn = Connection::open(&self.db_path)
            .map_err(|e| format!("数据库连接失败: {}", e))?;

        conn.execute_batch("VACUUM")
            .map_err(|e| format!("VACUUM 失败: {}", e))?;

        Ok(())
    }

    /// 分析数据库统计信息
    pub fn analyze(&self) -> Result<(), String> {
        use rusqlite::Connection;

        let conn = Connection::open(&self.db_path)
            .map_err(|e| format!("数据库连接失败: {}", e))?;

        conn.execute_batch("ANALYZE")
            .map_err(|e| format!("ANALYZE 失败: {}", e))?;

        Ok(())
    }

    /// 重建索引
    pub fn reindex(&self) -> Result<(), String> {
        use rusqlite::Connection;

        let conn = Connection::open(&self.db_path)
            .map_err(|e| format!("数据库连接失败: {}", e))?;

        conn.execute_batch("REINDEX")
            .map_err(|e| format!("REINDEX 失败: {}", e))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_tracker() {
        let tracker = QueryTracker::new("test_query".to_string());
        std::thread::sleep(std::time::Duration::from_millis(50));
        let duration = tracker.finish();
        assert!(duration >= 50);
    }

    #[test]
    fn test_batch_optimizer() {
        let optimizer = BatchOperationOptimizer::new(10);
        assert_eq!(optimizer.batch_size(), 10);
    }
}
