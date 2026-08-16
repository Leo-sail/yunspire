/// 任务契约和其他辅助函数
///
/// 从 runtime_db.rs 提取的契约读取等辅助函数

use crate::task_runtime::RuntimeTaskContractSnapshot;
use rusqlite::Connection;

/// 读取任务契约（占位实现）
/// 注意: 这是内部实现，公开接口在 query.rs
#[allow(dead_code)]
pub(crate) fn read_runtime_task_contract(
    _connection: &Connection,
    _workspace_scope: &str,
    _task_id: &str,
) -> Result<Option<RuntimeTaskContractSnapshot>, String> {
    // TODO: 从 runtime_db.rs 10975-11356 行提取实现
    // 这是一个大函数，约 382 行
    Ok(None)
}

/// 验证运行时标识符（已在 steps_helpers.rs 中实现）
/// 这里保留引用供文档完整性
#[allow(dead_code)]
pub(crate) fn valid_runtime_identifier_ref(value: &str, max: usize) -> bool {
    crate::task_management::steps_helpers::valid_runtime_identifier(value, max)
}

/// 验证任务状态（占位实现）
#[allow(dead_code)]
pub(crate) fn valid_runtime_task_state(state: &str) -> bool {
    // TODO: 添加状态验证逻辑
    matches!(
        state,
        "created"
            | "queued"
            | "running"
            | "awaiting_approval"
            | "succeeded"
            | "failed"
            | "cancelled"
    )
}

/// 规范化 JSON 字符串（占位实现）
#[allow(dead_code)]
pub(crate) fn canonical_runtime_json_string(
    _value: &serde_json::Value,
    _context: &str,
) -> Result<String, String> {
    // TODO: 实现规范化 JSON 序列化
    Ok(String::new())
}
