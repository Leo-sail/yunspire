# SearchPlugin 向后兼容桥接计划

**目标**: 让 SearchPlugin 完全接管搜索功能，同时保持 runtime_db.rs 中的旧 API 向后兼容

---

## 📋 当前状态

### SearchPlugin 已完成
- ✅ 数据结构 (types.rs)
- ✅ 编码/解码 (encoding.rs)
- ✅ 神经嵌入操作 (neural.rs)
- ✅ 搜索算法 (algorithm.rs)
- ✅ Plugin 实现 (plugin.rs)
- ✅ 18/18 测试通过

### runtime_db.rs 当前状态
- 🔴 3 个公开搜索函数（行 18956-19100）
  - `indexed_search()` - 主搜索入口
  - `get_neural_embedding_index_status()` - 状态查询
  - `rebuild_neural_embedding_index()` - 索引重建
- 🔴 ~15 个内部辅助函数
- 🔴 所有前端调用都指向 runtime_db.rs

---

## 🎯 桥接策略

### 阶段 1: SearchPlugin 完善 (本次)

**目标**: 实现完整的搜索功能

#### 1.1 实现完整的 indexed_search
- [ ] 从 runtime_db.rs 迁移完整逻辑（~287 行）
- [ ] 实现 RRF 融合算法
- [ ] 实现向量相似度搜索
- [ ] 完整的排序和去重

#### 1.2 实现索引状态管理
- [ ] `get_neural_embedding_index_status_inner()`
- [ ] Vault 状态聚合
- [ ] 错误状态处理

#### 1.3 实现索引重建
- [ ] `rebuild_neural_embedding_index_inner()`
- [ ] `refresh_neural_embedding_notes()`
- [ ] `prepare_neural_search_context()`
- [ ] 批量处理逻辑

### 阶段 2: Tauri 命令桥接

**目标**: 让 SearchPlugin 的命令可以从前端调用

#### 2.1 在 lib.rs 中注册 SearchPlugin
```rust
// src-tauri/src/lib.rs
use plugins::SearchPlugin;

fn setup_plugins(app: &AppHandle) -> Result<(), String> {
    let mut registry = PluginRegistry::new();
    registry.register(Box::new(SearchPlugin::new()))?;
    registry.load_all(&plugin_context)?;
    // 存储 registry 到全局状态
    app.manage(registry);
    Ok(())
}
```

#### 2.2 创建 Tauri 命令包装器
```rust
// src-tauri/src/lib.rs
#[tauri::command]
pub async fn indexed_search(
    database: State<'_, RuntimeDatabase>,
    registry: State<'_, PluginRegistry>,
    vault_id: Option<String>,
    query: String,
    max_results: Option<usize>,
) -> Result<Vec<IndexedSearchResult>, String> {
    // 委托给 SearchPlugin
    let plugin = registry.get_plugin("yunspire.search")?;
    plugin.indexed_search(&database, vault_id, query, max_results).await
}
```

### 阶段 3: 向后兼容层

**目标**: 保持 runtime_db.rs 中的旧 API 不变

#### 3.1 保留 runtime_db.rs 中的公开函数
```rust
// runtime_db.rs (保留旧 API)
#[tauri::command]
pub async fn indexed_search(
    database: State<'_, RuntimeDatabase>,
    vault_id: Option<String>,
    query: String,
    max_results: Option<usize>,
) -> Result<Vec<IndexedSearchResult>, String> {
    // 委托给 SearchPlugin
    crate::plugins::search::SearchPlugin::indexed_search_static(
        &database,
        vault_id,
        query,
        max_results
    ).await
}
```

#### 3.2 逐步迁移内部调用
- [ ] 识别所有调用 `indexed_search()` 的位置
- [ ] 逐步替换为 SearchPlugin 调用
- [ ] 保持功能不变

### 阶段 4: 集成测试

**目标**: 确保功能和性能正常

#### 4.1 端到端测试
- [ ] 基础搜索测试
- [ ] 神经嵌入搜索测试
- [ ] 混合搜索测试
- [ ] 边界情况测试

#### 4.2 性能基准测试
- [ ] 搜索响应时间（目标 <100ms）
- [ ] 内存使用
- [ ] 数据库查询次数
- [ ] 与旧实现对比

#### 4.3 兼容性测试
- [ ] 前端调用正常
- [ ] 搜索结果一致
- [ ] 错误处理正确

---

## 📝 实施步骤（本次会话）

### Step 1: 完善 SearchPlugin 核心函数

**优先级**: 🔥 高

1. 在 `algorithm.rs` 实现完整的 `indexed_search_in_connection_with_neural()`
   - 从 runtime_db.rs:18664-18950 迁移
   - ~287 行代码
   
2. 在 `neural.rs` 添加缺失的函数
   - `get_neural_embedding_index_status_inner()`
   - `rebuild_neural_embedding_index_inner()`
   - `refresh_neural_embedding_notes()`
   - `prepare_neural_search_context()`

3. 在 `plugin.rs` 实现真实的 Tauri 命令
   - 替换占位实现
   - 连接到核心函数

### Step 2: 创建测试

**优先级**: 🔥 高

1. 单元测试
   - 测试每个迁移的函数
   
2. 集成测试
   - 端到端搜索流程
   - 神经嵌入索引流程

### Step 3: 性能验证

**优先级**: 🟡 中

1. 基准测试
   - 与旧实现对比
   - 确保性能无退化

---

## ⏱️ 时间估算

| 任务 | 预估时间 | 难度 |
|------|---------|------|
| 迁移 indexed_search | 2-3 小时 | 🔴 高 |
| 迁移索引管理函数 | 1-2 小时 | 🟡 中 |
| 实现 Tauri 命令 | 1 小时 | 🟢 低 |
| 编写测试 | 1-2 小时 | 🟡 中 |
| 性能验证 | 1 小时 | 🟢 低 |
| **总计** | **6-9 小时** | |

预计需要 **2-3 次对话**完成

---

## 🚨 风险和注意事项

1. **依赖关系**: indexed_search 依赖很多内部函数，需要一并迁移
2. **性能**: 确保迁移后性能不降低
3. **兼容性**: 确保搜索结果与旧实现一致
4. **错误处理**: 所有边界情况都要正确处理

---

## ✅ 成功标准

- [ ] SearchPlugin 的 3 个命令完全可用
- [ ] 所有测试通过（包括新增的集成测试）
- [ ] 性能与旧实现相当或更好
- [ ] 前端调用无需修改
- [ ] runtime_db.rs 中的旧 API 仍然工作

---

**下一步**: 开始实施 Step 1 - 迁移核心搜索函数
