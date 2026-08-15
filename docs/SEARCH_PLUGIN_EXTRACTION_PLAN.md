# SearchPlugin 提取计划

**目标**: 从 runtime_db.rs 提取所有搜索相关代码到独立的 SearchPlugin

---

## 📋 待提取的函数清单

### Tauri 命令（3个）

1. `indexed_search()` - 行 18984
   - 主要搜索接口
   - 支持本地搜索 + 神经嵌入搜索

2. `get_neural_embedding_index_status()` - 行 18956
   - 查询神经嵌入索引状态

3. `rebuild_neural_embedding_index()` - 行 18964
   - 重建神经嵌入索引

### 核心搜索函数

4. `indexed_search_in_connection_with_neural()` - 需定位
   - 在连接中执行搜索（带神经嵌入）

5. `indexed_search_candidate_signals()` - 行 17249
   - 搜索候选信号计算

6. `prepare_neural_search_context()` - 需定位
   - 准备神经搜索上下文

### 神经嵌入相关函数

7. `cached_neural_embedding_in_connection()` - 行 17293
   - 从连接读取缓存的嵌入

8. `load_cached_neural_embedding()` - 行 17312
   - 加载缓存的嵌入

9. `persist_neural_embedding_and_bindings()` - 行 17331
   - 持久化嵌入和绑定

10. `load_missing_neural_embedding_inputs()` - 行 17414
    - 加载缺失的嵌入输入

11. `normalize_neural_embedding()` - 行 15412
    - 归一化嵌入向量

12. `encode_neural_embedding()` - 行 15430
    - 编码嵌入向量

13. `decode_neural_embedding()` - 行 15440
    - 解码嵌入向量

14. `neural_embedding_input_hash()` - 行 15461
    - 计算嵌入输入哈希

15. `get_neural_embedding_index_status_inner()` - 需定位
    - 内部状态查询

16. `rebuild_neural_embedding_index_inner()` - 需定位
    - 内部索引重建

17. `normalize_neural_embedding_vault_id()` - 需定位
    - 规范化 vault ID

### 辅助函数

18. `cjk_lexical_terms()` - 需定位
    - CJK 分词

19. `compute_rrf_scores()` - 需定位
    - 计算 RRF 分数

20. `runtime_read_only_search_output()` - 行 9224
    - 只读搜索输出

---

## 📊 数据库 Schema

### 表结构

```sql
-- neural_embedding_cache
CREATE TABLE neural_embedding_cache (
    workspace_scope TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    model TEXT NOT NULL,
    input_hash TEXT NOT NULL,
    dimensions INTEGER NOT NULL,
    vector_blob BLOB NOT NULL,
    created_at TEXT NOT NULL,
    last_used_at TEXT NOT NULL,
    PRIMARY KEY(workspace_scope, provider_id, model, input_hash)
);

-- note_neural_embeddings
CREATE TABLE note_neural_embeddings (
    workspace_scope TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    model TEXT NOT NULL,
    vault_id TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    input_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY(workspace_scope, provider_id, model, vault_id, relative_path)
);

CREATE INDEX idx_note_neural_embedding_lookup
  ON note_neural_embeddings(workspace_scope, provider_id, model, vault_id, content_hash);

-- neural_embedding_index_state
CREATE TABLE neural_embedding_index_state (
    workspace_scope TEXT NOT NULL,
    vault_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    model TEXT NOT NULL,
    indexed_notes INTEGER NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(workspace_scope, vault_id, provider_id, model)
);
```

### 相关常量

```rust
const LOCAL_FEATURE_VECTOR_VERSION: i64 = 1;
const LOCAL_FEATURE_VECTOR_DIMENSIONS: usize = 384;
const MAX_LOCAL_VECTOR_CONTENT_CHARS: usize = 250_000;
const MIN_LOCAL_VECTOR_SIMILARITY: f64 = 0.025;
const MIN_NEURAL_EMBEDDING_SIMILARITY: f64 = 0.1;
const MAX_NEURAL_EMBEDDING_INPUT_CHARS: usize = 24_000;
const NEURAL_EMBEDDING_BATCH_SIZE: usize = 32;
const MAX_NEURAL_EMBEDDING_REFRESH_NOTES: usize = 64;
const NEURAL_RRF_WEIGHT: f64 = 2.0;
const LOCAL_VECTOR_RRF_WEIGHT_WITH_NEURAL: f64 = 0.5;
const RRF_K: f64 = 60.0;
```

---

## 🗂️ 数据结构

```rust
pub struct IndexedSearchResult {
    vault_id: String,
    relative_path: String,
    title: String,
    score: f64,
    snippet: String,
    modified_at: String,
    // ... 其他字段
}

pub struct NeuralEmbeddingIndexStatus {
    // 索引状态信息
}

struct NeuralSearchContext {
    // 神经搜索上下文
}
```

---

## 📝 实施步骤

### Step 1: 创建插件目录和基础结构

```bash
mkdir -p src-tauri/src/plugins/search
touch src-tauri/src/plugins/search/mod.rs
touch src-tauri/src/plugins/search/plugin.rs
touch src-tauri/src/plugins/search/neural.rs
touch src-tauri/src/plugins/search/index.rs
```

### Step 2: 定义数据结构

将所有搜索相关的数据结构移动到新模块

### Step 3: 提取核心函数

按以下顺序提取：
1. 辅助函数（编码/解码/哈希）
2. 数据库操作函数
3. 搜索算法函数
4. Tauri 命令函数

### Step 4: 实现 SearchPlugin

```rust
pub struct SearchPlugin {
    // 插件状态
}

impl YunspirePlugin for SearchPlugin {
    fn id(&self) -> &str { "yunspire.search" }
    // ... 实现其他方法
}
```

### Step 5: 数据库迁移

将 schema 提取到迁移脚本

### Step 6: 保持向后兼容

在 runtime_db.rs 中保留旧 API，委托给 SearchPlugin

```rust
// runtime_db.rs (保留向后兼容)
pub async fn indexed_search(...) -> Result<...> {
    let search_plugin = get_search_plugin()?;
    search_plugin.indexed_search(...).await
}
```

### Step 7: 测试

- 单元测试
- 集成测试
- 性能基准测试

---

## ⚠️ 注意事项

1. **依赖关系**: 搜索模块可能依赖 vault、model_provider 等
2. **性能**: 确保提取后性能无退化
3. **向后兼容**: 必须保持现有 API 不变
4. **错误处理**: 统一错误处理方式

---

## 📊 预估

- **代码行数**: ~1,500 行
- **函数数量**: ~20 个
- **工时**: 10-15 小时
- **完成后 runtime_db.rs**: 18,528 → ~17,000 行

---

**下一步**: 开始实施 Step 1 - 创建插件目录和基础结构
