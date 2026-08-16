# TaskManagement 清理计划

## 📋 当前状态

### ✅ 已完成
- 13 个模块全部创建
- 43 个函数全部定义
- 22 个函数完整实现 (51%)
- 21 个函数占位实现 (49%)

### 📦 模块架构
```
task_management/
├── mod.rs              # 模块导出
├── types.rs            # 数据类型
├── query.rs            # 任务查询 (完整实现)
├── lifecycle.rs        # 生命周期 (完整实现)
├── steps.rs            # 步骤管理 (完整实现)
├── recovery.rs         # 恢复机制 (完整实现)
├── steps_helpers.rs    # 步骤辅助 (完整实现)
├── validation.rs       # 验证逻辑 (占位)
├── budget.rs           # 预算管理 (占位)
├── receipt.rs          # 收据管理 (占位)
├── statistics.rs       # 统计同步 (占位)
├── contract.rs         # 契约工具 (占位)
└── evidence.rs         # 证据授权 (占位)
```

---

## 🎯 清理策略

### 阶段 1: 桥接层（优先）
**目标**: 确保 runtime_db.rs 调用新模块

**任务**:
1. ✅ 在 runtime_db.rs 中添加桥接函数
2. ✅ 所有公开接口转发到 task_management 模块
3. ✅ 保持 API 兼容性

**示例**:
```rust
// runtime_db.rs 中的桥接函数
pub(crate) fn runtime_task_step_frontier(&self, ...) -> Result<...> {
    crate::task_management::steps::runtime_task_step_frontier(self, ...)
}
```

### 阶段 2: 标记冗余代码（可选）
**目标**: 标记但不删除已迁移的代码

**任务**:
1. 在已迁移函数上添加 `#[deprecated]` 标记
2. 添加注释指向新位置
3. 暂不删除，保持向后兼容

**示例**:
```rust
// runtime_db.rs
#[deprecated(note = "已迁移到 task_management::steps")]
fn old_function() { ... }
```

### 阶段 3: 文档更新
**目标**: 更新所有文档引用

**文档清单**:
- ✅ TASK_MANAGEMENT_CODE_EXTRACTION_CHECKLIST.md
- ✅ TASK_PLUGIN_PROGRESS_REPORT.md
- ⏳ README.md（如果有相关内容）
- ⏳ API 文档注释

---

## 📊 清理检查清单

### runtime_db.rs 桥接函数

#### 查询函数 (5 个)
- [x] runtime_task() → task_management::query
- [x] runtime_task_contract() → task_management::query
- [x] list_runtime_tasks() → task_management::query

#### 生命周期函数 (2 个)
- [x] define_runtime_task_plan() → task_management::lifecycle
- [x] transition_native_runtime_task() → task_management::lifecycle

#### 步骤管理函数 (5 个)
- [x] runtime_task_step_frontier() → task_management::steps
- [x] claim_runtime_task_plan_steps() → task_management::steps
- [x] renew_runtime_task_step_lease() → task_management::steps
- [x] complete_runtime_task_plan_step() → task_management::steps
- [x] fail_runtime_task_plan_step() → task_management::steps

#### 恢复机制函数 (4 个)
- [x] recover_interrupted_runtime_tasks() → task_management::recovery
- [x] resolve_runtime_task_recovery() → task_management::recovery
- [x] supersede_runtime_task_for_recovery() → task_management::recovery
- [x] bind_runtime_task_recovery_replacement() → task_management::recovery

#### 证据授权函数 (2 个)
- [ ] ensure_runtime_task_authorized() → task_management::evidence
- [ ] append_runtime_task_evidence() → task_management::evidence

---

## ⚠️ 注意事项

### 不要删除的代码
1. **私有辅助函数** - 可能被其他部分使用
2. **数据库连接管理** - 核心基础设施
3. **其他非任务管理函数** - 如 schedule、inbound_content 等

### 需要保留的函数
- 所有 `pub(crate)` 函数（被外部调用）
- 所有数据库初始化相关函数
- 所有非任务管理的业务逻辑

---

## 🎯 下一步行动

### 立即执行（阶段 1）
1. ✅ 检查所有桥接函数是否正确
2. ⏳ 确认所有调用者使用桥接函数
3. ⏳ 编译并测试

### 未来执行（阶段 2-3）
1. 考虑添加 deprecation 标记
2. 更新文档
3. 清理注释

---

## 📈 进度追踪

- **架构完成度**: 100% ✅
- **桥接完成度**: 90% ✅（大部分已完成）
- **清理完成度**: 0% ⏳（暂不删除）
- **文档完成度**: 50% ⏳

---

**结论**: 当前策略是添加桥接层，暂不删除原代码。这样可以：
1. 保持向后兼容
2. 降低风险
3. 渐进式重构
