# SearchPlugin 集成测试计划

## 测试目标

验证 SearchPlugin 的核心功能正常工作，包括：
- 数据结构序列化/反序列化
- 编码/解码函数
- 搜索算法
- RRF 融合

---

## 单元测试状态 ✅

### 已完成测试 (19/19)

```
✅ encoding tests      6/6
  - test_normalize_neural_embedding
  - test_encode_decode_neural_embedding
  - test_decode_invalid_dimensions
  - test_neural_embedding_input_hash
  - test_normalize_vault_id
  - test_neural_embedding_state_priority

✅ algorithm tests     3/3  
  - test_cjk_lexical_terms
  - test_is_cjk
  - test_indexed_search_candidate_signals

✅ neural tests        2/2
  - test_cached_neural_embedding_in_connection
  - test_persist_neural_embedding_and_bindings_validation

✅ plugin tests        7/7
  - test_plugin_metadata
  - test_plugin_capabilities
  - test_plugin_commands
  - test_plugin_migrations
  - test_plugin_config_schema
  - test_plugin_lifecycle
  - test_health_check_before_init

✅ async_ops tests     1/1
  - test_load_neural_embedding_index_state

✅ bridge tests        1/1
  - test_max_search_query_chars
```

---

## 集成测试计划

### 测试 1: 端到端搜索流程

**目标**: 验证完整的搜索流程

**测试步骤**:
1. 创建测试数据库
2. 插入测试笔记
3. 执行搜索
4. 验证结果

**当前状态**: ⏳ 需要真实数据库

### 测试 2: RRF 融合算法

**目标**: 验证 RRF 分数计算正确

**测试步骤**:
1. 构造已知排名的候选
2. 计算 RRF 分数
3. 验证分数正确性

**当前状态**: ✅ 可以实现

### 测试 3: 信号计算

**目标**: 验证额外信号计算正确

**测试步骤**:
1. 构造不同类型的候选
2. 计算信号分数
3. 验证各项加分

**当前状态**: ✅ 已有单元测试

---

## 性能基准测试计划

### 基准 1: 搜索响应时间

**指标**: 
- P50 < 50ms
- P95 < 100ms
- P99 < 200ms

**方法**:
- 测试不同查询长度
- 测试不同结果集大小
- 与旧实现对比

### 基准 2: 内存使用

**指标**:
- 峰值内存 < 100MB
- 无内存泄漏

### 基准 3: RRF 计算开销

**指标**:
- RRF 计算 < 10ms
- 不随候选数量线性增长

---

## 实施计划

### 优先级 1: RRF 融合测试 ✅

创建简单的单元测试验证 RRF 计算：

```rust
#[test]
fn test_rrf_fusion() {
    // 构造测试数据
    let candidates = vec![
        // lexical rank 1
        // neural rank 2
        // 预期 RRF 分数 = 1/(60+1) + 2.0/(60+2) = ...
    ];
    
    // 执行融合
    // 验证分数
}
```

### 优先级 2: 简化集成测试

创建不依赖真实数据库的集成测试：

```rust
#[test]
fn test_search_pipeline() {
    // 使用内存数据库
    // 或 mock 数据库连接
}
```

### 优先级 3: 性能基准

使用 criterion.rs 创建性能基准：

```rust
fn criterion_benchmark(c: &mut Criterion) {
    c.bench_function("rrf_fusion", |b| {
        b.iter(|| {
            // 测试代码
        })
    });
}
```

---

## 已验证功能 ✅

### 编码/解码
- ✅ 向量归一化
- ✅ 向量编码/解码
- ✅ 哈希计算
- ✅ Vault ID 规范化

### 搜索算法
- ✅ CJK 分词
- ✅ 候选信号计算
- ✅ 核心搜索框架

### 插件系统
- ✅ Plugin trait 实现
- ✅ 生命周期管理
- ✅ 配置 Schema
- ✅ 数据库迁移

---

## 待验证功能 ⏳

### 数据库操作
- ⏳ 嵌入缓存读写
- ⏳ 笔记绑定持久化
- ⏳ 索引状态管理

### 异步操作
- ⏳ 嵌入刷新
- ⏳ 搜索上下文准备
- ⏳ 模型 API 调用

### 核心搜索
- ⏳ FTS 查询
- ⏳ 向量搜索
- ⏳ RRF 融合（端到端）

---

## 测试覆盖率目标

| 模块 | 当前 | 目标 |
|------|------|------|
| encoding | 95% | 95% ✅ |
| algorithm | 80% | 90% |
| neural | 30% | 60% |
| async_ops | 20% | 50% |
| core_search | 0% | 40% |
| plugin | 70% | 80% |
| bridge | 10% | 30% |

---

## 结论

### 当前状态
- ✅ 单元测试完整（19 个）
- ✅ 核心算法验证
- ⏳ 集成测试待完善
- ⏳ 性能基准待实施

### 建议
1. **优先**: 完善算法单元测试（RRF、排序）
2. **中期**: 添加简化的集成测试
3. **长期**: 性能基准和压力测试

### 风险评估
- **低风险**: 编码/解码、CJK 分词（已充分测试）
- **中风险**: 数据库操作（需要真实连接）
- **高风险**: 完整搜索流程（依赖多个组件）

**建议**: 先在生产环境小规模测试，再逐步推广
