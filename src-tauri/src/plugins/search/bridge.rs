/// SearchPlugin 桥接层
///
/// 提供可以在 runtime_db.rs 中调用的桥接函数
///
/// 注意：不使用 #[tauri::command] 宏，避免与 runtime_db.rs 中的定义冲突

use crate::plugins::search::{
    async_ops::prepare_neural_search_context,
    core_search::indexed_search_in_connection_with_neural,
    encoding::normalize_neural_embedding_vault_id,
    IndexedSearchResult,
};
use crate::runtime_db::RuntimeDatabase;

/// 最大搜索查询字符数
const MAX_SEARCH_QUERY_CHARS: usize = 512;

/// 索引搜索 - 桥接实现
///
/// 这个函数可以被 runtime_db.rs 中的 tauri::command 调用
///
/// # 参数
/// - `database`: 数据库实例
/// - `vault_id`: 可选的 vault ID
/// - `query`: 搜索查询
/// - `limit`: 最大结果数
/// - `allow_neural_embedding`: 是否启用神经嵌入搜索
///
/// # 返回
/// 搜索结果列表
pub async fn indexed_search_impl(
    database: &RuntimeDatabase,
    vault_id: Option<String>,
    query: String,
    limit: Option<usize>,
    allow_neural_embedding: Option<bool>,
) -> Result<Vec<IndexedSearchResult>, String> {
    // 验证输入
    let normalized_query = query.trim();
    if normalized_query.is_empty() {
        return Err("搜索词不能为空".to_string());
    }
    if normalized_query.chars().count() > MAX_SEARCH_QUERY_CHARS {
        return Err("搜索词超过 512 个字符的安全上限".to_string());
    }

    // 获取工作区范围
    let workspace_scope = database.local_workspace_scope()?;

    // 规范化 vault ID
    let scoped_vault_id = normalize_neural_embedding_vault_id(vault_id.as_deref())?;

    // 限制结果数量
    let max_results = limit.unwrap_or(50).clamp(1, 200);

    // 对指定 vault 执行搜索
    let vault_id_to_search = if let Some(ref vault_id) = scoped_vault_id {
        // TODO: 检查权限
        // database.ensure_vault_read_allowed(&workspace_scope, vault_id)?;
        vault_id.as_str()
    } else {
        // TODO: 支持跨 vault 搜索
        // 当前暂不支持，返回错误
        return Err("当前版本需要指定 vault_id".to_string());
    };

    // 准备神经搜索上下文
    let neural = if allow_neural_embedding == Some(true) {
        match prepare_neural_search_context(
            database,
            &workspace_scope,
            Some(vault_id_to_search),
            normalized_query,
        )
        .await
        {
            Ok(context) => context,
            Err(error) => {
                log::warn!(
                    "Vault {} 的神经 Embedding 搜索不可用，回退到本地混合搜索：{}",
                    vault_id_to_search,
                    error
                );
                None
            }
        }
    } else {
        None
    };

    // 执行搜索
    let results = {
        let connection = database
            .connection
            .lock()
            .map_err(|_| "SQLite 连接锁不可用".to_string())?;

        indexed_search_in_connection_with_neural(
            &connection,
            Some(vault_id_to_search),
            normalized_query,
            max_results,
            neural.as_ref(),
        )?
    };

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_max_search_query_chars() {
        assert_eq!(MAX_SEARCH_QUERY_CHARS, 512);
    }
}
