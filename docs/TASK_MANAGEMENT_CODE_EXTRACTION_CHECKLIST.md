# runtime_db.rs 任务管理代码提取清单

**文件**: `src-tauri/src/runtime_db.rs`  
**总行数**: 19,206 行  
**目标**: 提取约 3000-4000 行任务管理代码到独立模块

---

## 📋 代码分类

### 1. 数据结构定义 (在文件顶部)

| 结构体/枚举 | 行号 | 说明 |
|------------|------|------|
| RuntimeTaskPlanStepRecord | 424 | 任务计划步骤记录 |
| RuntimeTaskRecovery | 485 | 任务恢复信息 |
| RuntimeTaskRecoveryReplacement | 503 | 任务恢复替换 |

**注意**: 大部分类型在 `task_runtime.rs` 中定义，这里只有少量补充类型。

---

### 2. 公开函数 (20 个)

#### 任务查询 (3 个)
| 函数 | 行号 | 功能 | 优先级 |
|------|------|------|--------|
| runtime_task | 4239-4253 | 查询单个任务 | 高 |
| runtime_task_contract | 4255-4269 | 查询任务契约 | 高 |
| list_runtime_tasks | 6195-6240 | 列出任务列表 | 高 |

#### 任务生命周期 (3 个)
| 函数 | 行号 | 功能 | 优先级 |
|------|------|------|--------|
| define_runtime_task_plan | 4366-4493 | 定义任务计划 | 高 |
| transition_native_runtime_task | 6242-6265 | 任务状态转换 | 高 |
| transition_native_runtime_task_with_trusted_execution_receipt | 6267-6292 | 带凭证的状态转换 | 中 |

#### 步骤管理 (7 个)
| 函数 | 行号 | 功能 | 优先级 |
|------|------|------|--------|
| runtime_task_step_frontier | 4495-4572 | 查询步骤前沿 | 高 |
| claim_runtime_task_plan_steps | 4574-4835 | 认领步骤 | 高 |
| renew_runtime_task_step_lease | 4837-4943 | 续期租约 | 高 |
| get_active_step_claims | 4945-5586 | 获取活跃认领 | 中 |
| complete_runtime_task_plan_step | 5588-5611 | 完成步骤 | 高 |
| fail_runtime_task_plan_step | 5613-5636 | 失败步骤 | 高 |
| list_runtime_task_step_receipts | 5638-5678 | 列出步骤收据 | 中 |

#### 任务证据 (1 个)
| 函数 | 行号 | 功能 | 优先级 |
|------|------|------|--------|
| append_runtime_task_evidence | 5680-6142 | 追加任务证据 | 中 |

#### 任务授权 (1 个)
| 函数 | 行号 | 功能 | 优先级 |
|------|------|------|--------|
| ensure_runtime_task_authorized | 6144-6193 | 确保任务已授权 | 中 |

#### 任务恢复 (4 个)
| 函数 | 行号 | 功能 | 优先级 |
|------|------|------|--------|
| recover_interrupted_runtime_tasks | 2760-3021 | 恢复中断任务 | 高 |
| resolve_runtime_task_recovery | 3023-3057 | 解决恢复 | 高 |
| supersede_runtime_task_for_recovery | 3059-3212 | 替代任务 | 高 |
| bind_runtime_task_recovery_replacement | 3214-3280 | 绑定替换 | 高 |

#### 其他 (1 个)
| 函数 | 行号 | 功能 | 优先级 |
|------|------|------|--------|
| claim_due_runtime_schedules | 2653-2758 | 认领到期的调度 | 低 (调度相关) |

**公开函数总行数**: 约 4000 行

---

### 3. 私有辅助函数 (约 20 个)

#### 任务读取 (3 个)
| 函数 | 行号 | 功能 |
|------|------|------|
| read_native_runtime_task | 11379-11394 | 读取原生任务 |
| read_runtime_task_contract | 10975-11356 | 读取任务契约 |
| map_native_runtime_task | 11358-11377 | 映射任务行 |

#### 任务计划 (5 个)
| 函数 | 行号 | 功能 |
|------|------|------|
| latest_runtime_task_plan_revision | 10029-10053 | 最新计划版本 |
| load_runtime_task_plan_step_records | 10055-10106 | 加载步骤记录 |
| insert_runtime_task_plan_revision | 10772-10845 | 插入计划版本 |
| runtime_task_plan_from_input | 10847-10879 | 从输入构建计划 |
| evaluate_runtime_task_completion | 10881-10940 | 评估任务完成 |

#### 步骤管理 (6 个)
| 函数 | 行号 | 功能 |
|------|------|------|
| latest_runtime_task_step_states | 10108-10151 | 最新步骤状态 |
| expire_runtime_task_step_claims | 10153-10262 | 过期步骤认领 |
| ensure_runtime_task_running_for_step_claim | 10264-10318 | 确保任务运行中 |
| read_runtime_task_step_receipt | 10320-10516 | 读取步骤收据 |
| cancel_runtime_task_step_claims | 10518-10770 | 取消步骤认领 |
| validate_runtime_task_step_command_binding_in_connection | 9728-9883 | 验证命令绑定 |

#### 任务验证 (2 个)
| 函数 | 行号 | 功能 |
|------|------|------|
| validate_runtime_task_step_child_authority | 9909-10027 | 验证子任务权限 |
| ensure_runtime_child_scope_subset | 9885-9907 | 确保子范围 |

#### 任务预算 (2 个)
| 函数 | 行号 | 功能 |
|------|------|------|
| read_runtime_task_execution_budget | 8795-8840 | 读取执行预算 |
| ensure_runtime_task_execution_budget | 8842-9291 | 确保执行预算 |

#### 任务证据 (1 个)
| 函数 | 行号 | 功能 |
|------|------|------|
| runtime_task_evidence_from_parts | 10942-10973 | 从部分构建证据 |

#### 任务统计 (1 个)
| 函数 | 行号 | 功能 |
|------|------|------|
| runtime_task_state_counts | 9293-9726 | 任务状态统计 |

#### 任务同步 (2 个)
| 函数 | 行号 | 功能 |
|------|------|------|
| sync_runtime_tasks | 14665-14913 | 同步任务 |
| sync_runtime_task_checkpoints | 14915-14971 | 同步任务检查点 |

**私有函数总行数**: 约 2500 行

---

## 📊 代码统计

| 类别 | 函数数 | 预估行数 |
|------|--------|----------|
| 数据结构 | 3 | ~100 |
| 公开函数 | 20 | ~4000 |
| 私有函数 | 20 | ~2500 |
| **总计** | **43** | **~6600** |

**注意**: 实际可能更多，因为还有一些嵌套的辅助逻辑。

---

## 🎯 提取策略

### 方案 A: 创建独立模块 (推荐)
**目标**: `src-tauri/src/task_management/mod.rs`

**步骤**:
1. 创建 `task_management/` 目录
2. 按功能拆分子模块:
   - `query.rs` - 任务查询
   - `lifecycle.rs` - 生命周期
   - `steps.rs` - 步骤管理
   - `recovery.rs` - 任务恢复
   - `evidence.rs` - 任务证据
   - `validation.rs` - 验证逻辑
   - `types.rs` - 数据结构
3. 逐个移动函数
4. 在 runtime_db.rs 中保留转发函数

### 方案 B: 直接集成到 TaskPlugin
**问题**: 
- TaskPlugin 当前的实现与 runtime_db.rs 不兼容
- 需要重新设计类型系统
- 风险太高

### 方案 C: 渐进式重构
1. **阶段 1**: 在 runtime_db.rs 中添加模块标记注释
2. **阶段 2**: 提取到独立模块
3. **阶段 3**: 集成到 TaskPlugin

---

## 🚀 执行计划

### 第一批：核心查询和生命周期 (优先级：高)
1. `runtime_task` (15 行)
2. `runtime_task_contract` (15 行)
3. `read_native_runtime_task` (16 行)
4. `read_runtime_task_contract` (382 行)
5. `map_native_runtime_task` (20 行)

**预估**: ~450 行，30 分钟

### 第二批：步骤管理核心 (优先级：高)
1. `runtime_task_step_frontier` (78 行)
2. `claim_runtime_task_plan_steps` (262 行)
3. `complete_runtime_task_plan_step` (24 行)
4. `fail_runtime_task_plan_step` (24 行)

**预估**: ~390 行，30 分钟

### 第三批：任务恢复 (优先级：高)
1. `recover_interrupted_runtime_tasks` (262 行)
2. `resolve_runtime_task_recovery` (35 行)
3. `supersede_runtime_task_for_recovery` (154 行)
4. `bind_runtime_task_recovery_replacement` (67 行)

**预估**: ~520 行，30 分钟

### 第四批：辅助函数 (优先级：中)
所有私有辅助函数

**预估**: ~2500 行，2 小时

---

## ⚠️ 风险和注意事项

1. **依赖关系复杂**: 函数之间相互调用，需要整体移动
2. **SQL 查询**: 大量直接的 SQL 操作，需要保持不变
3. **事务管理**: 很多函数使用事务，移动时要小心
4. **类型兼容**: 确保移动后类型引用正确
5. **测试覆盖**: 移动后需要确保所有测试通过

---

## ✅ 验收标准

1. ✅ 所有任务管理函数已移出 runtime_db.rs
2. ✅ runtime_db.rs 行数减少 6000+ 行
3. ✅ 所有测试通过
4. ✅ 编译无错误无警告
5. ✅ 功能保持不变（向后兼容）

---

**创建时间**: 2026-08-16  
**状态**: 规划完成，准备执行  
**预计完成时间**: 4-6 小时
