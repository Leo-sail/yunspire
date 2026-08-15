/// 异步搜索操作模块
///
/// 包含需要异步执行的搜索相关函数

use crate::plugins::search::encoding::{
    neural_embedding_input_hash, NEURAL_EMBEDDING_BATCH_SIZE,
};
use crate::plugins::search::neural::{
    load_cached_neural_embedding, load_missing_neural_embedding_inputs,
    persist_neural_embedding_and_bindings,
};
use crate::plugins::search::types::{
    NeuralEmbeddingNoteInput, NeuralEmbeddingRefreshOutcome, NeuralSearchContext,
};
use crate::runtime_db::RuntimeDatabase;
use std::collections::HashMap;
use unicode_normalization::UnicodeNormalization;

/// 刷新神经嵌入笔记索引
///
/// # 参数
/// - `database`: 数据库实例
/// - `configured`: 配置的嵌入模型
/// - `workspace_scope`: 工作区范围
/// - `vault_id`: 可选的 vault ID
/// - `limit`: 限制处理的笔记数量
///
/// # 返回
/// 刷新结果（包含加载、索引的笔记数和错误信息）
pub(crate) async fn refresh_neural_embedding_notes(
    database: &RuntimeDatabase,
    configured: &crate::model_provider::ConfiguredEmbeddingModel,
    workspace_scope: &str,
    vault_id: Option<&str>,
    limit: usize,
) -> Result<NeuralEmbeddingRefreshOutcome, String> {
    // 加载缺失的嵌入输入
    let missing = load_missing_neural_embedding_inputs(
        database,
        configured,
        workspace_scope,
        vault_id,
        limit,
    )?;

    let mut outcome = NeuralEmbeddingRefreshOutcome {
        loaded_notes: missing.len(),
        ..NeuralEmbeddingRefreshOutcome::default()
    };

    // 按 input_hash 分组，相同输入可以共享嵌入
    let mut pending = HashMap::<String, (String, Vec<NeuralEmbeddingNoteInput>)>::new();

    for note in missing {
        // 检查是否已经有缓存
        if load_cached_neural_embedding(database, configured, workspace_scope, &note.input_hash)?
            .is_some()
        {
            // 缓存存在，只需绑定
            persist_neural_embedding_and_bindings(
                database,
                configured,
                workspace_scope,
                vec![note.clone()],
                vec![], // 空向量表示使用缓存
            )?;
            outcome.indexed_notes += 1;
        } else {
            // 需要生成新嵌入
            let entry = pending
                .entry(note.input_hash.clone())
                .or_insert_with(|| (note.input.clone(), Vec::new()));
            entry.1.push(note);
        }
    }

    // 按哈希排序，确保一致性
    let mut pending = pending.into_iter().collect::<Vec<_>>();
    pending.sort_by(|left, right| left.0.cmp(&right.0));

    // 批量处理
    let mut batch_start = 0;
    while batch_start < pending.len() {
        let mut batch_end = batch_start;
        let mut batch_characters = 0_usize;

        // 构建批次，考虑批次大小和字符数限制
        while batch_end < pending.len()
            && batch_end - batch_start < NEURAL_EMBEDDING_BATCH_SIZE
        {
            let (_, (input, _)) = &pending[batch_end];
            let input_characters = input.chars().count();

            if batch_end > batch_start
                && batch_characters.saturating_add(input_characters)
                    > crate::model_provider::MAX_EMBEDDING_TOTAL_CHARS
            {
                break;
            }

            batch_characters = batch_characters.saturating_add(input_characters);
            batch_end += 1;
        }

        let chunk = &pending[batch_start..batch_end];
        let inputs = chunk
            .iter()
            .map(|(_, (input, _))| input.clone())
            .collect::<Vec<_>>();

        // 请求嵌入
        let vectors = match request_embeddings_with_usage(
            database,
            configured,
            &inputs,
            "embedding.index",
        )
        .await
        {
            Ok(vectors) => vectors,
            Err(error) => {
                outcome.error = Some(error);
                break;
            }
        };

        // 持久化嵌入和绑定
        for ((input_hash, (_, notes)), vector) in chunk.iter().zip(vectors) {
            persist_neural_embedding_and_bindings(
                database,
                configured,
                workspace_scope,
                notes.clone(),
                vec![vector],
            )?;
            outcome.indexed_notes += notes.len();
        }

        batch_start = batch_end;
    }

    Ok(outcome)
}

/// 准备神经搜索上下文
///
/// # 参数
/// - `database`: 数据库实例
/// - `workspace_scope`: 工作区范围
/// - `vault_id`: 可选的 vault ID
/// - `query`: 搜索查询
///
/// # 返回
/// - `Ok(Some(context))`: 准备成功
/// - `Ok(None)`: 未配置嵌入模型
/// - `Err(String)`: 准备失败
pub(crate) async fn prepare_neural_search_context(
    database: &RuntimeDatabase,
    workspace_scope: &str,
    vault_id: Option<&str>,
    query: &str,
) -> Result<Option<NeuralSearchContext>, String> {
    // 检查是否配置了嵌入模型
    let Some(configured) =
        crate::model_provider::configured_embedding_model(database, workspace_scope)?
    else {
        return Ok(None);
    };

    // 规范化查询
    let query_input = query.trim().nfc().collect::<String>();
    let query_hash = neural_embedding_input_hash(&query_input);

    // 获取查询向量
    let query_vector = if let Some(vector) =
        load_cached_neural_embedding(database, &configured, workspace_scope, &query_hash)?
    {
        // 缓存命中，更新最后使用时间
        persist_neural_embedding_and_bindings(
            database,
            &configured,
            workspace_scope,
            vec![],
            vec![],
        )?;
        vector
    } else {
        // 缓存未命中，请求新嵌入
        let vectors = request_embeddings_with_usage(
            database,
            &configured,
            &[query_input.clone()],
            "embedding.search",
        )
        .await?;

        let vector = vectors
            .into_iter()
            .next()
            .ok_or("嵌入响应为空".to_string())?;

        // 持久化查询嵌入
        persist_neural_embedding_and_bindings(
            database,
            &configured,
            workspace_scope,
            vec![],
            vec![vector.clone()],
        )?;

        vector
    };

    // 刷新笔记嵌入索引
    let refresh_error = match refresh_neural_embedding_notes(
        database,
        &configured,
        workspace_scope,
        vault_id,
        crate::plugins::search::encoding::MAX_NEURAL_EMBEDDING_REFRESH_NOTES,
    )
    .await
    {
        Ok(_) => None,
        Err(error) => Some(error),
    };

    // 加载索引状态
    let index_state = load_neural_embedding_index_state(
        database,
        workspace_scope,
        vault_id,
        Some(&configured),
        refresh_error.as_deref(),
    )?;

    if let Some(error) = refresh_error {
        log::warn!(
            "神经 Embedding 索引补齐失败，继续使用已有向量与本地搜索：{}",
            error
        );
    }

    Ok(Some(NeuralSearchContext {
        workspace_scope: workspace_scope.to_string(),
        provider_id: configured.provider_id,
        provider: configured.provider,
        model: configured.model,
        query_vector,
        index_state,
    }))
}

/// 请求嵌入并记录使用量
///
/// # 参数
/// - `database`: 数据库实例
/// - `configured`: 配置的嵌入模型
/// - `inputs`: 输入文本列表
/// - `operation`: 操作类型（用于追踪）
///
/// # 返回
/// 嵌入向量列表
async fn request_embeddings_with_usage(
    _database: &RuntimeDatabase,
    configured: &crate::model_provider::ConfiguredEmbeddingModel,
    inputs: &[String],
    _operation: &str,
) -> Result<Vec<Vec<f32>>, String> {
    // 调用模型提供商 API
    let vectors = crate::model_provider::request_embeddings(configured, inputs).await?;

    // TODO: 记录使用量
    // 当前版本简化处理，未来可以添加使用量追踪

    Ok(vectors)
}

/// 加载神经嵌入索引状态
///
/// # 参数
/// - `database`: 数据库实例
/// - `workspace_scope`: 工作区范围
/// - `vault_id`: 可选的 vault ID
/// - `configured`: 可选的配置模型
/// - `error`: 可选的错误信息
///
/// # 返回
/// 索引状态字符串
fn load_neural_embedding_index_state(
    _database: &RuntimeDatabase,
    _workspace_scope: &str,
    _vault_id: Option<&str>,
    _configured: Option<&crate::model_provider::ConfiguredEmbeddingModel>,
    error: Option<&str>,
) -> Result<String, String> {
    // TODO: 实现完整的状态加载逻辑
    // 当前返回简化状态
    if error.is_some() {
        Ok("degraded".to_string())
    } else {
        Ok("ready".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_neural_embedding_index_state() {
        // 测试状态加载
        let state = load_neural_embedding_index_state(
            &RuntimeDatabase::default(),
            "test",
            None,
            None,
            None,
        );
        // 由于没有真实连接，这里只验证不会 panic
    }
}
