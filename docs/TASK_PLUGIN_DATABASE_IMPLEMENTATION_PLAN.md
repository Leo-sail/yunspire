# TaskPlugin 数据库实现计划

**日期**: 2024-08-15
**状态**: 规划中
**优先级**: 高

---

## 概述

TaskPlugin 的核心功能接口已 100% 完成，包含完整的类型系统、验证逻辑和集成测试。现需实现数据库操作层，将所有标记为 TODO 的函数连接到实际的 SQLite 数据库。

---

## 当前状态

### ✅ 已完成
- 8 个模块的接口设计
- 67 个测试（接口和验证层）
- 4 个数据库迁移脚本
- 完整的错误处理

### ⏳ 待实现
- **20 个 TODO 函数**需要实现数据库操作
- 分布在 5 个模块中
- 预计工作量：800-1000 行代码

---

## TODO 清单

### 1. lifecycle.rs (3 个 TODO)

```rust
// TODO 1: create_task() - 完整的任务创建逻辑
// 需要：INSERT INTO runtime_tasks

// TODO 2: transition_task_state() - 实际的数据库更新
// 需要：UPDATE runtime_tasks SET state = ?, updated_at = ? WHERE task_id = ?

// TODO 3: get_task_state() - 实际的数据库查询
// 需要：SELECT state FROM runtime_tasks WHERE task_id = ?
```

**预计**: ~100 行代码

---

### 2. steps.rs (6 个 TODO)

```rust
// TODO 1: claim_steps() - 步骤认领逻辑
// 需要：
// - 查询可认领的步骤（无依赖或依赖已完成）
// - INSERT INTO runtime_step_leases
// - UPDATE runtime_task_steps SET state = 'claimed'

// TODO 2: renew_step_lease() - 租约续期逻辑
// 需要：
// - UPDATE runtime_step_leases SET expires_at = ?, renewal_count = renewal_count + 1

// TODO 3: complete_step() - 步骤完成逻辑
// 需要：
// - UPDATE runtime_task_steps SET state = 'completed', result = ?
// - DELETE FROM runtime_step_leases

// TODO 4: fail_step() - 步骤失败逻辑
// 需要：
// - UPDATE runtime_task_steps SET state = 'failed', error = ?
// - DELETE FROM runtime_step_leases

// TODO 5: release_step_lease() - 租约释放逻辑
// 需要：
// - DELETE FROM runtime_step_leases WHERE step_id = ?

// TODO 6: get_step_frontier() - 前沿查询逻辑
// 需要：
// - 复杂查询：查找所有依赖已完成的未执行步骤
```

**预计**: ~250 行代码

---

### 3. recovery.rs (5 个 TODO)

```rust
// TODO 1: recover_interrupted_tasks() - 恢复查询逻辑
// 需要：
// - SELECT FROM runtime_task_recovery
// - JOIN runtime_tasks

// TODO 2: resolve_recovery() - 恢复执行逻辑
// 需要：
// - 根据恢复策略执行不同操作
// - UPDATE runtime_tasks
// - DELETE FROM runtime_task_recovery

// TODO 3: supersede_task() - 任务替代逻辑
// 需要：
// - INSERT INTO runtime_task_recovery
// - 设置 recommendation = 'supersede'

// TODO 4: bind_recovery_replacement() - 绑定逻辑
// 需要：
// - UPDATE runtime_task_recovery
// - 设置 replacement_task_id

// TODO 5: get_task_recovery() - 查询逻辑
// 需要：
// - SELECT FROM runtime_task_recovery WHERE task_id = ?
```

**预计**: ~200 行代码

---

### 4. storage.rs (5 个 TODO)

```rust
// TODO 1: load_task() - 加载任务
// 需要：
// - SELECT FROM runtime_tasks WHERE task_id = ?
// - 构造 RuntimeTask 对象

// TODO 2: save_task() - 保存任务
// 需要：
// - INSERT OR REPLACE INTO runtime_tasks
// - 可能需要保存相关步骤

// TODO 3: list_tasks() - 列出任务
// 需要：
// - SELECT FROM runtime_tasks WHERE workspace_scope = ?
// - 应用过滤器（states, task_kinds, created_after, created_before）
// - 应用分页（LIMIT, OFFSET）

// TODO 4: task_statistics() - 统计查询
// 需要：
// - SELECT COUNT(*) ... GROUP BY state
// - SELECT COUNT(*) ... GROUP BY task_kind

// TODO 5: delete_task() - 删除操作
// 需要：
// - 事务中删除：
//   - runtime_step_leases (通过 step_id)
//   - runtime_task_steps (通过 task_id)
//   - runtime_task_recovery (通过 task_id)
//   - runtime_tasks (通过 task_id)
```

**预计**: ~250 行代码

---

### 5. plugin.rs (1 个 TODO)

```rust
// TODO 1: health_check() - 添加更多健康检查
// 需要：
// - 检查数据库表结构
// - 检查过期租约
// - 检查中断任务
```

**预计**: ~50 行代码

---

## 实现策略

### 阶段 1: 基础存储操作 (storage.rs)
**优先级**: 最高  
**原因**: 其他模块依赖基础的 CRUD 操作

1. `save_task()` - 保存任务到数据库
2. `load_task()` - 从数据库加载任务
3. `list_tasks()` - 列出任务（带过滤和分页）
4. `task_statistics()` - 任务统计
5. `delete_task()` - 删除任务（级联删除）

**测试**: 为每个函数添加实际的数据库测试

---

### 阶段 2: 生命周期操作 (lifecycle.rs)
**优先级**: 高  
**原因**: 任务状态管理是核心功能

1. `create_task()` - 创建任务（调用 storage::save_task）
2. `transition_task_state()` - 更新任务状态
3. `get_task_state()` - 查询任务状态

**测试**: 完整的生命周期流程测试

---

### 阶段 3: 步骤管理操作 (steps.rs)
**优先级**: 高  
**原因**: 步骤执行是任务系统的关键

1. `claim_steps()` - 认领步骤（插入租约）
2. `complete_step()` - 完成步骤
3. `fail_step()` - 失败步骤
4. `renew_step_lease()` - 续期租约
5. `release_step_lease()` - 释放租约
6. `get_step_frontier()` - 查询可执行步骤

**测试**: 并发步骤执行测试

---

### 阶段 4: 恢复机制操作 (recovery.rs)
**优先级**: 中  
**原因**: 恢复是增强功能，可以后置

1. `recover_interrupted_tasks()` - 查询中断任务
2. `supersede_task()` - 创建替代任务
3. `bind_recovery_replacement()` - 绑定替代
4. `resolve_recovery()` - 执行恢复
5. `get_task_recovery()` - 查询恢复信息

**测试**: 恢复场景测试

---

### 阶段 5: 健康检查 (plugin.rs)
**优先级**: 低  
**原因**: 监控功能，最后实现

1. `health_check()` - 完整的健康检查

**测试**: 健康检查测试

---

## 技术要点

### 1. 数据库连接
```rust
use crate::runtime_db::RuntimeDatabase;

// RuntimeDatabase 已经提供了 SQLite 连接
// 需要使用 rusqlite API 进行查询
```

### 2. 事务管理
```rust
// 对于复杂操作（如删除任务），使用事务
let tx = database.connection.transaction()?;
// ... 多个操作
tx.commit()?;
```

### 3. 错误处理
```rust
// 统一的错误转换
.map_err(|e| StorageError::DatabaseError(e.to_string()))?
```

### 4. JSON 序列化
```rust
// payload 和 result 是 JSON 类型
let payload_json = serde_json::to_string(&task.payload)?;
let payload: Value = serde_json::from_str(&payload_str)?;
```

### 5. 时间戳
```rust
// 使用 RFC3339 格式
let now = chrono::Utc::now().to_rfc3339();
```

---

## 测试策略

### 单元测试
- 每个数据库函数都需要单元测试
- 使用内存数据库 (`:memory:`) 进行测试
- 验证 CRUD 操作的正确性

### 集成测试
- 测试跨模块的数据库操作
- 测试事务的正确性
- 测试并发场景

### 边界测试
- 空数据库
- 大量数据
- 并发写入
- 事务回滚

---

## 预计工作量

| 阶段 | 模块 | TODO 数 | 预计行数 | 预计时间 |
|------|------|---------|----------|----------|
| 1 | storage.rs | 5 | 250 | 2h |
| 2 | lifecycle.rs | 3 | 100 | 1h |
| 3 | steps.rs | 6 | 250 | 2h |
| 4 | recovery.rs | 5 | 200 | 1.5h |
| 5 | plugin.rs | 1 | 50 | 0.5h |
| **总计** | **5 模块** | **20** | **850** | **7h** |

---

## 成功标准

### 功能完整性
- [ ] 所有 20 个 TODO 函数实现完成
- [ ] 所有数据库操作通过测试
- [ ] 事务正确性验证
- [ ] 错误处理完整

### 代码质量
- [ ] 无编译错误
- [ ] 无 clippy 警告
- [ ] 代码覆盖率 > 80%
- [ ] 所有测试通过

### 性能要求
- [ ] 单次查询 < 10ms
- [ ] 批量操作 < 100ms
- [ ] 并发操作无死锁
- [ ] 内存使用合理

---

## 依赖关系

```
阶段 1 (storage.rs)
    ↓
阶段 2 (lifecycle.rs)
    ↓
阶段 3 (steps.rs)
    ↓
阶段 4 (recovery.rs)
    ↓
阶段 5 (plugin.rs)
```

**说明**: 各阶段有依赖关系，需按顺序实施。

---

## 风险和挑战

### 1. 数据库模式匹配
- **风险**: 迁移脚本定义的表结构可能需要调整
- **缓解**: 先验证迁移脚本，确保所有字段存在

### 2. 并发控制
- **风险**: 租约机制的并发竞争
- **缓解**: 使用数据库事务和唯一约束

### 3. 性能优化
- **风险**: 复杂查询（如 get_step_frontier）可能较慢
- **缓解**: 添加合适的索引，使用 EXPLAIN 分析

### 4. 事务管理
- **风险**: 嵌套事务或长事务可能导致死锁
- **缓解**: 保持事务简短，避免嵌套

---

## 下一步行动

1. ✅ 完成集成测试（已完成）
2. ⏳ **开始阶段 1**: 实现 storage.rs 的数据库操作
3. ⏳ 验证数据库迁移脚本
4. ⏳ 实现其余阶段
5. ⏳ 性能测试和优化

---

**准备就绪，可以开始实现数据库操作层！** 🚀
