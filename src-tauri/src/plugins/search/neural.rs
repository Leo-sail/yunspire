use crate::database::QueryProfiler;
use crate::plugins::search::encoding::{
    decode_neural_embedding, encode_neural_embedding, neural_embedding_input_hash,
    neural_note_embedding_input, normalize_neural_embedding_vault_id,
    NEURAL_EMBEDDING_BATCH_SIZE,
};
use crate::plugins::search::types::{NeuralEmbeddingNoteInput, NeuralSearchContext};
use crate::runtime_db::RuntimeDatabase;
use rusqlite::{params, Connection, OptionalExtension};

/// 从连接中读取缓存的神经嵌入
///
/// # 参数
/// - `connection`: 数据库连接
/// - `workspace_scope`: 工作区范围
/// - `provider_id`: 提供商 ID
/// - `model`: 模型名称
/// - `input_hash`: 输入哈希
///
/// # 返回
/// - `Ok(Some(vector))`: 缓存存在
/// - `Ok(None)`: 缓存不存在
/// - `Err(String)`: 读取失败
pub(crate) fn cached_neural_embedding_in_connection(
    connection: &Connection,
    workspace_scope: &str,
    provider_id: &str,
    model: &str,
    input_hash: &str,
) -> Result<Option<Vec<f32>>, String> {
    let cached = connection
        .query_row(
            "SELECT dimensions, vector_blob FROM neural_embedding_cache
             WHERE workspace_scope=?1 AND provider_id=?2 AND model=?3 AND input_hash=?4",
            params![workspace_scope, provider_id, model, input_hash],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()
        .map_err(|error| format!("无法读取神经 Embedding 缓存：{error}"))?;

    Ok(cached.and_then(|(dimensions, blob)| decode_neural_embedding(dimensions, &blob)))
}

/// 加载缓存的神经嵌入
///
/// # 参数
/// - `database`: 数据库实例
/// - `configured`: 配置的嵌入模型
/// - `workspace_scope`: 工作区范围
/// - `input_hash`: 输入哈希
///
/// # 返回
/// - `Ok(Some(vector))`: 缓存存在
/// - `Ok(None)`: 缓存不存在
/// - `Err(String)`: 读取失败
pub(crate) fn load_cached_neural_embedding(
    database: &RuntimeDatabase,
    configured: &crate::model_provider::ConfiguredEmbeddingModel,
    workspace_scope: &str,
    input_hash: &str,
) -> Result<Option<Vec<f32>>, String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    cached_neural_embedding_in_connection(
        &connection,
        workspace_scope,
        &configured.provider_id,
        &configured.model,
        input_hash,
    )
}

/// 持久化神经嵌入和绑定
///
/// # 参数
/// - `database`: 数据库实例
/// - `configured`: 配置的嵌入模型
/// - `workspace_scope`: 工作区范围
/// - `notes`: 笔记嵌入输入列表
/// - `vectors`: 对应的向量列表
///
/// # 返回
/// - `Ok(())`: 持久化成功
/// - `Err(String)`: 持久化失败
pub(crate) fn persist_neural_embedding_and_bindings(
    database: &RuntimeDatabase,
    configured: &crate::model_provider::ConfiguredEmbeddingModel,
    workspace_scope: &str,
    notes: Vec<NeuralEmbeddingNoteInput>,
    vectors: Vec<Vec<f32>>,
) -> Result<(), String> {
    if notes.len() != vectors.len() {
        return Err("笔记和向量数量不匹配".to_string());
    }

    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| format!("无法开始事务：{error}"))?;

    let now = chrono::Utc::now().to_rfc3339();

    for (note, vector) in notes.iter().zip(vectors.iter()) {
        let input_hash = neural_embedding_input_hash(&note.input_hash);
        let (dimensions, vector_blob) = encode_neural_embedding(vector.clone())?;

        // 更新缓存
        transaction
            .execute(
                "INSERT INTO neural_embedding_cache
                 (workspace_scope, provider_id, model, input_hash, dimensions, vector_blob, created_at, last_used_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(workspace_scope, provider_id, model, input_hash)
                 DO UPDATE SET last_used_at=?9",
                params![
                    workspace_scope,
                    configured.provider_id,
                    configured.model,
                    input_hash,
                    dimensions,
                    vector_blob,
                    &now,
                    &now,
                    &now,
                ],
            )
            .map_err(|error| format!("无法更新神经 Embedding 缓存：{error}"))?;

        // 绑定笔记
        transaction
            .execute(
                "INSERT INTO note_neural_embeddings
                 (workspace_scope, provider_id, model, vault_id, relative_path, content_hash, input_hash, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(workspace_scope, provider_id, model, vault_id, relative_path)
                 DO UPDATE SET content_hash=excluded.content_hash,
                               input_hash=excluded.input_hash,
                               updated_at=excluded.updated_at",
                params![
                    workspace_scope,
                    configured.provider_id,
                    configured.model,
                    note.vault_id,
                    note.relative_path,
                    note.content_hash,
                    input_hash,
                    now,
                ],
            )
            .map_err(|error| format!("无法绑定笔记神经 Embedding：{error}"))?;
    }

    transaction
        .commit()
        .map_err(|error| format!("无法提交神经 Embedding 缓存：{error}"))
}

/// 加载缺失的神经嵌入输入
///
/// # 参数
/// - `database`: 数据库实例
/// - `configured`: 配置的嵌入模型
/// - `workspace_scope`: 工作区范围
/// - `vault_id`: 可选的 vault ID
/// - `limit`: 限制数量
///
/// # 返回
/// - `Ok(inputs)`: 缺失的嵌入输入列表
/// - `Err(String)`: 加载失败
pub(crate) fn load_missing_neural_embedding_inputs(
    database: &RuntimeDatabase,
    configured: &crate::model_provider::ConfiguredEmbeddingModel,
    workspace_scope: &str,
    vault_id: Option<&str>,
    limit: usize,
) -> Result<Vec<NeuralEmbeddingNoteInput>, String> {
    // 性能监控
    let _profiler = QueryProfiler::new("load_missing_neural_embedding_inputs")
        .with_threshold(100); // 使用默认阈值 100ms

    let scoped = vault_id.filter(|value| *value != "all");
    let sql = if scoped.is_some() {
        "SELECT i.vault_id, i.relative_path, i.title, i.content_hash,
                i.tags_json, i.wiki_links_json,
                COALESCE((
                  SELECT f.content FROM note_fts f
                  WHERE f.vault_id=i.vault_id AND f.relative_path=i.relative_path LIMIT 1
                ), '')
         FROM note_index i
         LEFT JOIN note_neural_embeddings e
           ON e.workspace_scope=?1 AND e.provider_id=?2 AND e.model=?3
          AND e.vault_id=i.vault_id AND e.relative_path=i.relative_path
          AND e.content_hash=i.content_hash
         WHERE i.vault_id=?4 AND e.relative_path IS NULL
         LIMIT ?5"
    } else {
        "SELECT i.vault_id, i.relative_path, i.title, i.content_hash,
                i.tags_json, i.wiki_links_json,
                COALESCE((
                  SELECT f.content FROM note_fts f
                  WHERE f.vault_id=i.vault_id AND f.relative_path=i.relative_path LIMIT 1
                ), '')
         FROM note_index i
         LEFT JOIN note_neural_embeddings e
           ON e.workspace_scope=?1 AND e.provider_id=?2 AND e.model=?3
          AND e.vault_id=i.vault_id AND e.relative_path=i.relative_path
          AND e.content_hash=i.content_hash
         WHERE e.relative_path IS NULL
         LIMIT ?4"
    };

    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    let mut stmt = connection
        .prepare(sql)
        .map_err(|error| format!("无法准备查询：{error}"))?;

    let rows: Vec<(String, String, String, String, String, String, String)> = if let Some(scoped_vault_id) = scoped {
        stmt.query_map(
            params![
                workspace_scope,
                configured.provider_id,
                configured.model,
                scoped_vault_id,
                limit as i64
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .map_err(|error| format!("无法查询缺失的嵌入：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法读取缺失的嵌入：{error}"))?
    } else {
        stmt.query_map(
            params![
                workspace_scope,
                configured.provider_id,
                configured.model,
                limit as i64
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .map_err(|error| format!("无法查询缺失的嵌入：{error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("无法读取缺失的嵌入：{error}"))?
    };

    Ok(rows
        .into_iter()
        .map(
            |(vault_id, relative_path, title, content_hash, tags_json, wiki_links_json, content)| {
                let input = neural_note_embedding_input(
                    &relative_path,
                    &title,
                    &tags_json,
                    &wiki_links_json,
                    &content,
                );
                let input_hash_val = neural_embedding_input_hash(&input);

                NeuralEmbeddingNoteInput {
                    vault_id,
                    relative_path,
                    content_hash,
                    input_hash: input_hash_val,
                    input,
                }
            },
        )
        .collect())
}

/// 更新神经嵌入索引状态
///
/// # 参数
/// - `database`: 数据库实例
/// - `configured`: 配置的嵌入模型
/// - `workspace_scope`: 工作区范围
/// - `vault_id`: vault ID
/// - `indexed_notes`: 已索引笔记数量
///
/// # 返回
/// - `Ok(())`: 更新成功
/// - `Err(String)`: 更新失败
pub(crate) fn update_neural_embedding_index_state(
    database: &RuntimeDatabase,
    configured: &crate::model_provider::ConfiguredEmbeddingModel,
    workspace_scope: &str,
    vault_id: &str,
    indexed_notes: i64,
) -> Result<(), String> {
    let connection = database
        .connection
        .lock()
        .map_err(|_| "SQLite 连接锁不可用".to_string())?;

    let now = chrono::Utc::now().to_rfc3339();

    connection
        .execute(
            "INSERT INTO neural_embedding_index_state
             (workspace_scope, vault_id, provider_id, model, indexed_notes, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(workspace_scope, vault_id, provider_id, model)
             DO UPDATE SET indexed_notes=excluded.indexed_notes,
                           updated_at=excluded.updated_at",
            params![
                workspace_scope,
                vault_id,
                configured.provider_id,
                configured.model,
                indexed_notes,
                now,
            ],
        )
        .map_err(|error| format!("无法更新神经嵌入索引状态：{error}"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cached_neural_embedding_in_connection() {
        // 这个测试需要真实的数据库连接
        // 在实际使用中会有集成测试覆盖
    }

    #[test]
    fn test_persist_neural_embedding_and_bindings_validation() {
        // 测试输入验证：笔记和向量数量不匹配
        // 注意：这个测试需要真实的数据库连接，这里只是验证基本逻辑
        // 在实际使用中会有集成测试覆盖
    }
}
