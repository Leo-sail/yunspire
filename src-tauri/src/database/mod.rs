pub mod config;
pub mod error;
pub mod profiling;

// 重新导出常用类型
pub use config::DatabaseConfig;
pub use error::{DatabaseError, DatabaseResult};
pub use profiling::QueryProfiler;

// 初始化数据库基础设施
pub fn init_database_infrastructure() {
    // 初始化全局性能监控器
    crate::performance_monitor::init_global_monitor(100);

    log::info!("数据库基础设施初始化完成");
}
