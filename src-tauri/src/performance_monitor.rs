use rusqlite::Connection;
use serde::Serialize;
use std::time::Instant;

/// 性能监控记录
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceMetrics {
    pub operation: String,
    pub duration_ms: u64,
    pub timestamp: String,
    pub details: Option<String>,
}

/// SQL 查询性能
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryPerformance {
    pub query_pattern: String,
    pub execution_count: usize,
    pub total_duration_ms: u64,
    pub avg_duration_ms: f64,
    pub max_duration_ms: u64,
    pub min_duration_ms: u64,
}

/// 慢查询日志
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlowQueryLog {
    pub query: String,
    pub duration_ms: u64,
    pub timestamp: String,
    pub params: Option<String>,
}

/// 性能报告
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceReport {
    pub total_queries: usize,
    pub slow_queries_count: usize,
    pub avg_query_time_ms: f64,
    pub p95_query_time_ms: u64,
    pub p99_query_time_ms: u64,
    pub top_slow_queries: Vec<SlowQueryLog>,
    pub query_performance: Vec<QueryPerformance>,
}

use std::sync::{Arc, Mutex};

/// 性能监控器
pub struct PerformanceMonitor {
    metrics: Arc<Mutex<Vec<PerformanceMetrics>>>,
    slow_queries: Arc<Mutex<Vec<SlowQueryLog>>>,
    slow_threshold_ms: u64,
}

impl PerformanceMonitor {
    pub fn new(slow_threshold_ms: u64) -> Self {
        Self {
            metrics: Arc::new(Mutex::new(Vec::new())),
            slow_queries: Arc::new(Mutex::new(Vec::new())),
            slow_threshold_ms,
        }
    }

    /// 记录操作性能
    pub fn record_operation(&self, operation: String, duration_ms: u64, details: Option<String>) {
        let timestamp = chrono::Utc::now().to_rfc3339();
        let metric = PerformanceMetrics {
            operation,
            duration_ms,
            timestamp,
            details,
        };

        if let Ok(mut metrics) = self.metrics.lock() {
            metrics.push(metric);

            // 保留最近 1000 条记录
            if metrics.len() > 1000 {
                metrics.drain(0..100);
            }
        }
    }

    /// 记录慢查询
    #[allow(dead_code)]
    pub fn record_slow_query(&self, query: String, duration_ms: u64, params: Option<String>) {
        if duration_ms < self.slow_threshold_ms {
            return;
        }

        let timestamp = chrono::Utc::now().to_rfc3339();
        let log = SlowQueryLog {
            query,
            duration_ms,
            timestamp,
            params,
        };

        if let Ok(mut slow_queries) = self.slow_queries.lock() {
            slow_queries.push(log);

            // 保留最近 100 条慢查询
            if slow_queries.len() > 100 {
                slow_queries.drain(0..10);
            }
        }
    }

    /// 生成性能报告
    pub fn generate_report(&self) -> PerformanceReport {
        let metrics = self.metrics.lock().unwrap();
        let slow_queries = self.slow_queries.lock().unwrap();

        if metrics.is_empty() {
            return PerformanceReport {
                total_queries: 0,
                slow_queries_count: 0,
                avg_query_time_ms: 0.0,
                p95_query_time_ms: 0,
                p99_query_time_ms: 0,
                top_slow_queries: vec![],
                query_performance: vec![],
            };
        }

        // 计算统计数据
        let total_queries = metrics.len();
        let slow_queries_count = slow_queries.len();

        let total_duration: u64 = metrics.iter().map(|m| m.duration_ms).sum();
        let avg_query_time_ms = total_duration as f64 / total_queries as f64;

        // 计算百分位数
        let mut durations: Vec<u64> = metrics.iter().map(|m| m.duration_ms).collect();
        durations.sort_unstable();

        let p95_index = (total_queries as f64 * 0.95) as usize;
        let p99_index = (total_queries as f64 * 0.99) as usize;

        let p95_query_time_ms = durations.get(p95_index).copied().unwrap_or(0);
        let p99_query_time_ms = durations.get(p99_index).copied().unwrap_or(0);

        // Top 10 慢查询
        let mut top_slow_queries = slow_queries.clone();
        top_slow_queries.sort_by(|a, b| b.duration_ms.cmp(&a.duration_ms));
        top_slow_queries.truncate(10);

        // 按查询模式聚合
        let mut query_stats: std::collections::HashMap<String, Vec<u64>> =
            std::collections::HashMap::new();

        for metric in metrics.iter() {
            query_stats
                .entry(metric.operation.clone())
                .or_default()
                .push(metric.duration_ms);
        }

        let mut query_performance: Vec<QueryPerformance> = query_stats
            .into_iter()
            .map(|(pattern, durations)| {
                let execution_count = durations.len();
                let total_duration_ms: u64 = durations.iter().sum();
                let avg_duration_ms = total_duration_ms as f64 / execution_count as f64;
                let max_duration_ms = *durations.iter().max().unwrap_or(&0);
                let min_duration_ms = *durations.iter().min().unwrap_or(&0);

                QueryPerformance {
                    query_pattern: pattern,
                    execution_count,
                    total_duration_ms,
                    avg_duration_ms,
                    max_duration_ms,
                    min_duration_ms,
                }
            })
            .collect();

        // 按总时间排序
        query_performance.sort_by(|a, b| b.total_duration_ms.cmp(&a.total_duration_ms));

        PerformanceReport {
            total_queries,
            slow_queries_count,
            avg_query_time_ms,
            p95_query_time_ms,
            p99_query_time_ms,
            top_slow_queries,
            query_performance,
        }
    }

    /// 清空监控数据
    pub fn clear(&self) {
        if let Ok(mut metrics) = self.metrics.lock() {
            metrics.clear();
        }
        if let Ok(mut slow_queries) = self.slow_queries.lock() {
            slow_queries.clear();
        }
    }
}

/// 计时器（RAII 模式）
#[allow(dead_code)]
pub struct Timer {
    operation: String,
    start: Instant,
    monitor: Arc<PerformanceMonitor>,
}

impl Timer {
    #[allow(dead_code)]
    pub fn new(operation: String, monitor: Arc<PerformanceMonitor>) -> Self {
        Self {
            operation,
            start: Instant::now(),
            monitor,
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        let duration_ms = self.start.elapsed().as_millis() as u64;
        self.monitor
            .record_operation(self.operation.clone(), duration_ms, None);
    }
}

use std::sync::OnceLock;

// 全局性能监控器（慢查询阈值 100ms）
static GLOBAL_MONITOR: OnceLock<Arc<PerformanceMonitor>> = OnceLock::new();

fn get_global_monitor() -> &'static Arc<PerformanceMonitor> {
    GLOBAL_MONITOR.get_or_init(|| Arc::new(PerformanceMonitor::new(100)))
}

/// 获取性能报告
#[tauri::command]
pub fn get_performance_report() -> Result<PerformanceReport, String> {
    Ok(get_global_monitor().generate_report())
}

/// 清空性能监控数据
#[tauri::command]
pub fn clear_performance_metrics() -> Result<(), String> {
    get_global_monitor().clear();
    Ok(())
}

/// 启用 SQLite 查询日志
#[allow(dead_code)]
pub fn enable_sqlite_profiling(_connection: &Connection) -> Result<(), rusqlite::Error> {
    // SQLite 查询日志需要在应用层通过包装查询调用实现
    // 生产环境建议使用 GLOBAL_MONITOR 记录查询性能
    Ok(())
}

/// 内存使用统计
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryStats {
    pub allocated_bytes: usize,
    pub resident_bytes: usize,
    pub metadata_bytes: usize,
}

/// 获取内存使用情况（简化版）
#[tauri::command]
pub fn get_memory_stats() -> Result<MemoryStats, String> {
    // 实际实现需要使用 jemalloc 或其他内存分配器的统计 API
    // 这里提供一个简化的版本
    #[cfg(target_os = "linux")]
    {
        use std::fs;
        if let Ok(status) = fs::read_to_string("/proc/self/status") {
            let mut resident_kb = 0;
            for line in status.lines() {
                if line.starts_with("VmRSS:") {
                    if let Some(value) = line.split_whitespace().nth(1) {
                        resident_kb = value.parse().unwrap_or(0);
                        break;
                    }
                }
            }
            return Ok(MemoryStats {
                allocated_bytes: 0,
                resident_bytes: resident_kb * 1024,
                metadata_bytes: 0,
            });
        }
    }

    // 其他平台或解析失败时返回占位数据
    Ok(MemoryStats {
        allocated_bytes: 0,
        resident_bytes: 0,
        metadata_bytes: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_performance_monitor() {
        let monitor = PerformanceMonitor::new(50);

        monitor.record_operation("test_query".to_string(), 30, None);
        monitor.record_operation("test_query".to_string(), 60, None);
        monitor.record_operation("slow_query".to_string(), 150, None);

        let report = monitor.generate_report();
        assert_eq!(report.total_queries, 3);
        assert!(report.avg_query_time_ms > 0.0);
    }

    #[test]
    fn test_slow_query_logging() {
        let monitor = PerformanceMonitor::new(50);

        monitor.record_slow_query("SELECT * FROM large_table".to_string(), 120, None);
        monitor.record_slow_query("SELECT * FROM users".to_string(), 40, None); // 不应记录

        let slow_queries = monitor.slow_queries.lock().unwrap();
        assert_eq!(slow_queries.len(), 1);
    }

    #[test]
    fn test_percentile_calculation() {
        let monitor = PerformanceMonitor::new(100);

        for i in 1..=100 {
            monitor.record_operation("query".to_string(), i, None);
        }

        let report = monitor.generate_report();
        // P95 = 95th element in sorted array (0-indexed: 94)
        assert!(report.p95_query_time_ms >= 94 && report.p95_query_time_ms <= 96);
        assert!(report.p99_query_time_ms >= 98 && report.p99_query_time_ms <= 100);
    }
}
