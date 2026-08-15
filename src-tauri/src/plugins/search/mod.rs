pub mod encoding;
pub mod types;

pub use types::{
    IndexedSearchResult, IndexedSearchSignals, NeuralEmbeddingIndexStatus,
    NeuralEmbeddingVaultIndexStatus,
};

// 内部使用
pub(crate) use encoding::{
    decode_neural_embedding, encode_neural_embedding, neural_embedding_input_hash,
    neural_embedding_state_priority, neural_note_embedding_input,
    normalize_neural_embedding, normalize_neural_embedding_vault_id,
};

// 将在后续实现中添加：
// - neural.rs: 神经嵌入数据库操作
// - algorithm.rs: 搜索算法
// - plugin.rs: SearchPlugin 实现
