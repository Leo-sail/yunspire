pub mod types;

pub use types::{
    IndexedSearchResult, IndexedSearchSignals, NeuralEmbeddingIndexStatus,
    NeuralEmbeddingVaultIndexStatus,
};

// 将在后续实现中添加：
// - neural.rs: 神经嵌入相关函数
// - encoding.rs: 编码/解码/哈希函数
// - algorithm.rs: 搜索算法
// - plugin.rs: SearchPlugin 实现
