use serde::Serialize;

/// 索引搜索结果
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedSearchResult {
    pub vault_id: String,
    pub relative_path: String,
    pub title: String,
    pub excerpt: String,
    pub modified_at: String,
    pub score: f64,
    pub tags: Vec<String>,
    pub wiki_links: Vec<String>,
    pub source_kind: String,
    pub ranking_signals: IndexedSearchSignals,
}

/// 搜索排序信号
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedSearchSignals {
    pub lexical_rank: Option<usize>,
    pub vector_rank: Option<usize>,
    pub neural_rank: Option<usize>,
    pub local_vector_rank: Option<usize>,
    pub lexical_rrf: f64,
    pub vector_rrf: f64,
    pub neural_rrf: f64,
    pub local_vector_rrf: f64,
    pub vector_similarity: Option<f64>,
    pub neural_similarity: Option<f64>,
    pub local_vector_similarity: Option<f64>,
    pub title_path_bonus: f64,
    pub relation_bonus: f64,
    pub recency_bonus: f64,
    pub vector_kind: String,
    pub embedding_provider: Option<String>,
    pub embedding_model: Option<String>,
    pub embedding_index_state: Option<String>,
}

/// 搜索候选（内部使用）
#[derive(Clone)]
pub(crate) struct IndexedSearchCandidate {
    pub vault_id: String,
    pub relative_path: String,
    pub title: String,
    pub excerpt: String,
    pub modified_at: String,
    pub lexical_score: Option<f64>,
    pub vector_similarity: Option<f64>,
    pub tags: Vec<String>,
    pub wiki_links: Vec<String>,
}

/// 神经搜索上下文
#[derive(Clone, Debug)]
pub(crate) struct NeuralSearchContext {
    pub workspace_scope: String,
    pub provider_id: String,
    pub provider: String,
    pub model: String,
    pub query_vector: Vec<f32>,
    pub index_state: String,
}

/// 神经嵌入笔记输入
#[derive(Clone, Debug)]
pub(crate) struct NeuralEmbeddingNoteInput {
    pub vault_id: String,
    pub relative_path: String,
    pub content_hash: String,
    pub input_hash: String,
    pub input: String,
}

/// Vault 神经嵌入索引状态
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NeuralEmbeddingVaultIndexStatus {
    pub vault_id: String,
    pub state: String,
    pub total_notes: i64,
    pub indexed_notes: i64,
    pub pending_notes: i64,
    pub last_error: Option<String>,
    pub updated_at: Option<String>,
}

/// 神经嵌入索引状态
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NeuralEmbeddingIndexStatus {
    pub workspace_scope: String,
    pub vault_id: Option<String>,
    pub configured: bool,
    pub provider_id: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub state: String,
    pub total_notes: i64,
    pub indexed_notes: i64,
    pub pending_notes: i64,
    pub cache_entries: i64,
    pub last_error: Option<String>,
    pub updated_at: Option<String>,
    pub vaults: Vec<NeuralEmbeddingVaultIndexStatus>,
}

/// 神经嵌入刷新结果（内部使用）
#[derive(Default)]
pub(crate) struct NeuralEmbeddingRefreshOutcome {
    pub loaded_notes: usize,
    pub indexed_notes: usize,
    pub error: Option<String>,
}
