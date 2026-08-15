# TaskPlugin 提取计划

**目标**: 将任务管理功能从 runtime_db.rs 提取到独立的 TaskPlugin

---

## 📊 代码分析

### 已识别的任务相关函数（~30 个）

#### 核心任务管理
1. `runtime_task()` - 获取任务详情
2. `runtime_task_contract()` - 获取任务契约
3. `define_runtime_task_plan()` - 定义任务计划
4. `list_runtime_tasks()` - 列出任务

#### 任务恢复
5. `recover_interrupted_runtime_tasks()` - 恢复中断的任务
6. `resolve_runtime_task_recovery()` - 解决任务恢复
7. `supersede_runtime_task_for_recovery()` - 替代任务用于恢复
8. `bind_runtime_task_recovery_replacement()` - 绑定恢复替换

#### 任务步骤管理
9. `runtime_task_step_frontier()` - 任务步骤前沿
10. `claim_runtime_task_plan_steps()` - 认领任务步骤
11. `renew_runtime_task_step_lease()` - 续租任务步骤
12. `complete_runtime_task_plan_step()` - 完成任务步骤
13. `fail_runtime_task_plan_step()` - 失败任务步骤
14. `list_runtime_task_step_receipts()` - 列出任务步骤回执

#### 任务执行
15. `transition_native_runtime_task()` - 转换原生任务状态
16. `transition_native_runtime_task_with_trusted_execution_receipt()` - 带可信执行回执转换
17. `append_runtime_task_evidence()` - 追加任务证据
18. `ensure_runtime_task_authorized()` - 确保任务授权

#### 辅助函数（内部）
19. `valid_runtime_task_state()` - 验证任务状态
20. `read_runtime_task_execution_budget()` - 读取执行预算
21. `ensure_runtime_task_execution_budget()` - 确保执行预算
22. `runtime_task_state_counts()` - 任务状态统计
23. `runtime_read_only_tasks_output()` - 只读任务输出
24. `validate_runtime_task_step_command_binding_in_connection()` - 验证命令绑定
25. `validate_runtime_task_step_child_authority()` - 验证子任务权限
26. `latest_runtime_task_plan_revision()` - 最新计划版本
27. `load_runtime_task_plan_step_records()` - 加载步骤记录
28. `latest_runtime_task_step_states()` - 最新步骤状态
29. `expire_runtime_task_step_claims()` - 过期步骤认领
30. `ensure_runtime_task_running_for_step_claim()` - 确保任务运行中

### 数据结构（~8 个）

1. `RuntimeTaskRecovery` - 任务恢复
2. `RuntimeTaskRecoveryReplacement` - 恢复替换
3. `ScheduleOccurrenceTask` - 调度任务
4. `RuntimeTaskPlanStepRecord` - 任务步骤记录
5. `RuntimeTaskStepCommandBinding` - 步骤命令绑定
6. (待发现更多...)

---

## 🎯 TaskPlugin 设计

### 模块结构

```
plugins/task/
├── types.rs            - 任务数据结构
│   ├── RuntimeTask
│   ├── RuntimeTaskContract
│   ├── RuntimeTaskPlan
│   ├── RuntimeTaskStep
│   ├── RuntimeTaskRecovery
│   └── TaskState (枚举)
├── validation.rs       - 任务验证
│   ├── validate_task_state()
│   ├── validate_task_plan()
│   └── validate_step_binding()
├── lifecycle.rs        - 任务生命周期
│   ├── create_task()
│   ├── transition_task()
│   ├── complete_task()
│   └── fail_task()
├── steps.rs            - 任务步骤管理
│   ├── claim_step()
│   ├── renew_lease()
│   ├── complete_step()
│   └── fail_step()
├── recovery.rs         - 任务恢复
│   ├── recover_interrupted_tasks()
│   ├── resolve_recovery()
│   └── supersede_task()
├── storage.rs          - 任务持久化
│   ├── load_task()
│   ├── save_task()
│   └── list_tasks()
└── plugin.rs           - TaskPlugin 实现
    └── YunspirePlugin trait
```

### 核心功能域

#### 1. 任务生命周期
- 创建任务
- 状态转换
- 完成/失败
- 任务授权

#### 2. 任务步骤
- 步骤认领
- 租约续期
- 步骤执行
- 步骤回执

#### 3. 任务恢复
- 中断恢复
- 任务替代
- 恢复绑定

#### 4. 任务查询
- 获取任务详情
- 列出任务
- 任务统计
- 状态查询

---

## 📋 提取任务清单

### 阶段 1: 数据结构定义 (2 小时)

**任务**:
- [ ] RuntimeTask 结构体
- [ ] RuntimeTaskContract 结构体
- [ ] RuntimeTaskPlan 结构体
- [ ] RuntimeTaskStep 结构体
- [ ] RuntimeTaskRecovery 结构体
- [ ] TaskState 枚举
- [ ] 其他辅助类型

**文件**: `types.rs` (~200 行)

**从 runtime_db.rs 提取**:
- 行 485-503: RuntimeTaskRecovery
- 行 503-510: RuntimeTaskRecoveryReplacement
- 行 406-422: ScheduleOccurrenceTask
- 行 424-483: RuntimeTaskPlanStepRecord

### 阶段 2: 任务验证 (1 小时)

**任务**:
- [ ] 状态验证
- [ ] 计划验证
- [ ] 命令绑定验证
- [ ] 权限验证

**文件**: `validation.rs` (~150 行)

**从 runtime_db.rs 提取**:
- 行 8659: valid_runtime_task_state()
- 行 9728: validate_runtime_task_step_command_binding_in_connection()
- 行 9909: validate_runtime_task_step_child_authority()

### 阶段 3: 任务生命周期 (2 小时)

**任务**:
- [ ] 任务创建
- [ ] 状态转换
- [ ] 任务完成
- [ ] 任务失败

**文件**: `lifecycle.rs` (~300 行)

**从 runtime_db.rs 提取**:
- 行 4239: runtime_task()
- 行 4255: runtime_task_contract()
- 行 6242: transition_native_runtime_task()
- 行 6267: transition_native_runtime_task_with_trusted_execution_receipt()

### 阶段 4: 任务步骤管理 (2 小时)

**任务**:
- [ ] 步骤认领
- [ ] 租约续期
- [ ] 步骤完成
- [ ] 步骤失败

**文件**: `steps.rs` (~350 行)

**从 runtime_db.rs 提取**:
- 行 4495: runtime_task_step_frontier()
- 行 4574: claim_runtime_task_plan_steps()
- 行 4837: renew_runtime_task_step_lease()
- 行 5588: complete_runtime_task_plan_step()
- 行 5613: fail_runtime_task_plan_step()

### 阶段 5: 任务恢复 (1.5 小时)

**任务**:
- [ ] 恢复中断任务
- [ ] 解决恢复
- [ ] 任务替代

**文件**: `recovery.rs` (~250 行)

**从 runtime_db.rs 提取**:
- 行 2760: recover_interrupted_runtime_tasks()
- 行 3023: resolve_runtime_task_recovery()
- 行 3059: supersede_runtime_task_for_recovery()
- 行 3214: bind_runtime_task_recovery_replacement()

### 阶段 6: 任务存储 (1.5 小时)

**任务**:
- [ ] 加载任务
- [ ] 保存任务
- [ ] 列出任务
- [ ] 任务统计

**文件**: `storage.rs` (~200 行)

**从 runtime_db.rs 提取**:
- 行 6195: list_runtime_tasks()
- 行 4366: define_runtime_task_plan()
- 行 9293: runtime_task_state_counts()

### 阶段 7: Plugin 实现 (1 小时)

**任务**:
- [ ] YunspirePlugin trait 实现
- [ ] Tauri 命令注册
- [ ] 数据库迁移
- [ ] 健康检查

**文件**: `plugin.rs` (~250 行)

### 阶段 8: 测试 (2 小时)

**任务**:
- [ ] 单元测试 (~20 个)
- [ ] 集成测试
- [ ] 边界测试

---

## 📊 预估工作量

| 阶段 | 任务 | 预估时间 | 难度 | 代码量 |
|------|------|---------|------|--------|
| 1 | 数据结构 | 2h | 中 | ~200 行 |
| 2 | 任务验证 | 1h | 低 | ~150 行 |
| 3 | 生命周期 | 2h | 高 | ~300 行 |
| 4 | 步骤管理 | 2h | 高 | ~350 行 |
| 5 | 任务恢复 | 1.5h | 中 | ~250 行 |
| 6 | 任务存储 | 1.5h | 中 | ~200 行 |
| 7 | Plugin 实现 | 1h | 低 | ~250 行 |
| 8 | 测试 | 2h | 中 | ~300 行 |
| **总计** | | **13h** | | **~2,000 行** |

**预计对话次数**: 3-4 次

---

## 🚨 复杂度评估

### 高复杂度部分

1. **任务生命周期**
   - 状态机复杂
   - 多种转换路径
   - 需要事务支持

2. **任务步骤管理**
   - 租约机制
   - 并发认领
   - 步骤依赖

3. **任务恢复**
   - 异常处理
   - 状态一致性
   - 回滚机制

### 中复杂度部分

4. **数据结构**
   - 多层嵌套
   - 关系复杂

5. **任务存储**
   - 多表查询
   - 事务处理

### 低复杂度部分

6. **任务验证**
   - 简单验证
   - 枚举检查

7. **Plugin 框架**
   - 标准模式

---

## 🎯 成功标准

### 功能完整性
- [ ] 所有核心任务函数可用
- [ ] 状态转换正确
- [ ] 步骤管理稳定
- [ ] 恢复机制可靠
- [ ] 向后兼容

### 代码质量
- [ ] 编译通过
- [ ] 所有测试通过
- [ ] 代码覆盖率 > 70%
- [ ] 文档完整

### 架构设计
- [ ] 模块化清晰
- [ ] 职责分离
- [ ] 易于扩展
- [ ] 无循环依赖

---

## 📝 设计决策

### 决策 1: 模块划分

**方案**: 按功能域划分（生命周期、步骤、恢复）

**理由**:
- 职责清晰
- 易于理解
- 便于测试

### 决策 2: 状态机实现

**方案**: 集中式状态转换函数

**理由**:
- 状态转换逻辑集中
- 易于验证正确性
- 避免状态不一致

### 决策 3: 租约机制

**方案**: 保留在 TaskPlugin 内部

**理由**:
- 任务步骤专用
- 简化接口
- 避免过度抽象

---

## 🚀 实施策略

### 优先级 1: 核心功能

1. 数据结构定义
2. 基本生命周期
3. Plugin 框架

### 优先级 2: 完整功能

1. 步骤管理
2. 任务恢复
3. 任务存储

### 优先级 3: 增强功能

1. 性能优化
2. 边界处理
3. 监控指标

---

## 📦 依赖关系

### TaskPlugin 依赖

- `runtime_db::RuntimeDatabase`
- `core::YunspirePlugin`
- `chrono` (时间处理)

### 被依赖

- 前端任务管理界面
- 调度器
- 其他需要任务的插件

---

## 🎊 预期成果

### 代码量预估

```
types.rs        ~200 行
validation.rs   ~150 行
lifecycle.rs    ~300 行
steps.rs        ~350 行
recovery.rs     ~250 行
storage.rs      ~200 行
plugin.rs       ~250 行
tests           ~300 行
━━━━━━━━━━━━━━━━━━━━
总计            ~2,000 行
```

### 测试预估

```
单元测试        ~20 个
集成测试        ~5 个
━━━━━━━━━━━━━━━━━━━━
总计            ~25 个
```

---

**下一步**: 开始实施阶段 1 - 数据结构定义
