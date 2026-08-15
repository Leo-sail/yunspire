pub mod algorithm;
pub mod encoding;
pub mod neural;
pub mod types;

pub use types::{
    IndexedSearchResult, IndexedSearchSignals, NeuralEmbeddingIndexStatus,
    NeuralEmbeddingVaultIndexStatus,
};

// 内部使用
pub(crate) use algorithm::{
    cjk_lexical_terms, indexed_search_candidate_signals, indexed_search_in_connection_with_neural,
};

pub(crate) use encoding::{
    decode_neural_embedding, encode_neural_embedding, neural_embedding_input_hash,
    neural_embedding_state_priority, neural_note_embedding_input,
    normalize_neural_embedding, normalize_neural_embedding_vault_id,
};

pub(crate) use neural::{
    cached_neural_embedding_in_connection, load_cached_neural_embedding,
    load_missing_neural_embedding_inputs, persist_neural_embedding_and_bindings,
    update_neural_embedding_index_state,
};

// 将在后续实现中添加：
// - plugin.rs: SearchPlugin 实现
