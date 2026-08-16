use std::time::Instant;

/// 查询性能分析器（自动追踪慢查询）
pub struct QueryProfiler {
    operation: String,
    start: Instant,
    slow_threshold_ms: u64,
}

impl QueryProfiler {
    /// 创建新的查询分析器
    pub fn new(operation: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
            start: Instant::now(),
            slow_threshold_ms: 100, // 默认 100ms
        }
    }

    /// 设置慢查询阈值
    pub fn with_threshold(mut self, ms: u64) -> Self {
        self.slow_threshold_ms = ms;
        self
    }

    /// 手动完成并返回耗时
    pub fn finish(self) -> u64 {
        let duration_ms = self.start.elapsed().as_millis() as u64;
        self.record_if_slow(duration_ms);
        duration_ms
    }

    fn record_if_slow(&self, duration_ms: u64) {
        if duration_ms > self.slow_threshold_ms {
            log::warn!(
                "🐌 慢查询检测: {} 耗时 {}ms (阈值 {}ms)",
                self.operation,
                duration_ms,
                self.slow_threshold_ms
            );

            // 记录到全局性能监控
            if let Some(monitor) = crate::performance_monitor::global_monitor() {
                monitor.record_slow_query(
                    self.operation.clone(),
                    duration_ms,
                    None,
                );
            }
        }
    }
}

impl Drop for QueryProfiler {
    /// 析构时自动记录性能数据
    fn drop(&mut self) {
        let duration_ms = self.start.elapsed().as_millis() as u64;
        self.record_if_slow(duration_ms);
    }
}

/// 简化的宏：自动创建查询分析器
#[macro_export]
macro_rules! profile_query {
    ($operation:expr) => {
        $crate::database::profiling::QueryProfiler::new($operation)
    };
    ($operation:expr, $threshold_ms:expr) => {
        $crate::database::profiling::QueryProfiler::new($operation)
            .with_threshold($threshold_ms)
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_profiler_fast_query() {
        let profiler = QueryProfiler::new("fast_query").with_threshold(100);
        thread::sleep(Duration::from_millis(10));
        let duration = profiler.finish();
        assert!(duration >= 10 && duration < 50);
    }

    #[test]
    fn test_profiler_slow_query() {
        let profiler = QueryProfiler::new("slow_query").with_threshold(50);
        thread::sleep(Duration::from_millis(60));
        let duration = profiler.finish();
        assert!(duration >= 60);
    }

    #[test]
    fn test_profiler_drop() {
        let _profiler = QueryProfiler::new("drop_test").with_threshold(10);
        thread::sleep(Duration::from_millis(5));
        // profiler 会在这里自动 drop 并记录
    }
}
