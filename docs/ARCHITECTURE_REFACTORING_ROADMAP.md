# 云枢架构重构与优化路线图

**版本**: v1.0  
**创建时间**: 2026-08-15  
**目标**: 将云枢重构为真正的插件化框架

---

## 🎯 愿景

**理想架构**: 云枢应该是一个轻量级框架，所有功能以模块化插件形式存在，用户可以根据需要添加、替换、扩展任何功能插件。

---

## 🔴 当前问题诊断

### 严重架构问题

#### 1. 巨型文件问题（屎山代码）

| 文件 | 行数 | 函数数 | 问题 |
|------|------|--------|------|
| `runtime_db.rs` | 18,528 | 163 | 🔴 **巨型上帝类** - 混合了所有数据库操作 |
| `model_provider.rs` | 5,554 | - | 🟡 单文件过大 |
| `obsidian.rs` | 5,106 | - | 🟡 单文件过大 |
| `skill_lifecycle.rs` | 3,375 | - | 🟡 可接受，但可优化 |
| `capture_pipeline.rs` | 3,028 | - | 🟡 可接受 |

**runtime_db.rs 包含的功能**（应该拆分）:
- 任务管理（RuntimeTask）
- 消息管理（WorkspaceMessage）
- 搜索系统（indexed_search, neural_embedding）
- 长期记忆（long_term_memory）
- 优化系统（optimization_profile）
- 报告管理（report_records）
- 创作资源（creation_resources）
- 调度系统（runtime_schedules）
- 备份恢复（database_backup）
- 入站内容（inbound_content）
- Vault 索引（vault_index）
- ... 还有更多

#### 2. 单一职责原则严重违反

```
问题：一个类负责太多事情
后果：
- 维护困难
- 测试困难  
- 修改风险高
- 合并冲突频繁
- 新人无法理解
```

#### 3. 插件系统与核心功能割裂

**现状**:
- ✅ Skill 系统已实现（`skill_lifecycle.rs`）
- ✅ 支持动态安装、版本控制、权限管理
- ❌ **但核心功能不可插件化**
- ❌ Skills 只是"附加功能"，不是架构基础

---

## ✅ 已完成的基础工作

### 1. 性能监控系统 ✅

**状态**: 100% 完成
- 82 个性能监控点全覆盖
- RuntimeDatabase 78 个方法
- 模块级函数 4 个
- 慢查询检测（默认 100ms）
- 生产级可观测性

### 2. 安全基础设施 ✅

**状态**: 已完成
- IPv6 SSRF 防护
- Worktree 污染修复
- 网络目标安全验证

### 3. 任务稳定性 ✅

**状态**: 已集成
- Lease 心跳续期机制
- 后台守护线程
- 防止超长任务回收

### 4. 数据库基础设施 ✅

**状态**: 已完成
- 统一错误处理（`database/error.rs`）
- DatabaseConfig 配置管理
- QueryProfiler RAII 模式

---

## 🏗️ 重构目标架构

### 核心设计原则

1. **插件优先**: 所有功能都是插件，核心框架只负责插件管理
2. **清晰边界**: 每个模块 < 1000 行，职责单一
3. **可替换性**: 用户可以替换任何插件
4. **向后兼容**: 重构过程中保持现有功能正常运行

### 目标架构图

```
yunspire/
├── src-tauri/src/
│   ├── core/                          # 核心框架（轻量）
│   │   ├── mod.rs
│   │   ├── app.rs                    # 应用生命周期
│   │   ├── database.rs               # 只负责连接池
│   │   ├── plugin_loader.rs          # 插件加载器
│   │   └── plugin_registry.rs        # 插件注册表
│   │
│   ├── plugins/                       # 内置插件（核心功能）
│   │   ├── search/                   # 搜索插件
│   │   │   ├── mod.rs
│   │   │   ├── plugin.rs            # 实现 Plugin trait
│   │   │   ├── indexed_search.rs
│   │   │   ├── neural_search.rs
│   │   │   └── migrations.sql
│   │   │
│   │   ├── tasks/                    # 任务管理插件
│   │   │   ├── mod.rs
│   │   │   ├── plugin.rs
│   │   │   ├── task_manager.rs
│   │   │   ├── scheduler.rs
│   │   │   └── migrations.sql
│   │   │
│   │   ├── messages/                 # 消息管理插件
│   │   │   ├── mod.rs
│   │   │   ├── plugin.rs
│   │   │   ├── message_store.rs
│   │   │   └── migrations.sql
│   │   │
│   │   ├── memory/                   # 长期记忆插件
│   │   │   ├── mod.rs
│   │   │   ├── plugin.rs
│   │   │   ├── long_term_memory.rs
│   │   │   └── migrations.sql
│   │   │
│   │   ├── optimization/             # 优化系统插件
│   │   │   ├── mod.rs
│   │   │   ├── plugin.rs
│   │   │   ├── optimizer.rs
│   │   │   └── migrations.sql
│   │   │
│   │   ├── reports/                  # 报告管理插件
│   │   │   ├── mod.rs
│   │   │   ├── plugin.rs
│   │   │   ├── report_manager.rs
│   │   │   └── migrations.sql
│   │   │
│   │   ├── vault/                    # Vault 索引插件
│   │   │   ├── mod.rs
│   │   │   ├── plugin.rs
│   │   │   ├── vault_indexer.rs
│   │   │   └── migrations.sql
│   │   │
│   │   ├── capture/                  # 采集管道插件
│   │   │   ├── mod.rs
│   │   │   ├── plugin.rs
│   │   │   ├── capture_pipeline.rs
│   │   │   └── graceful_degradation.rs
│   │   │
│   │   ├── skills/                   # Skill 系统插件
│   │   │   ├── mod.rs
│   │   │   ├── plugin.rs
│   │   │   └── skill_lifecycle.rs
│   │   │
│   │   └── creation/                 # 创作系统插件
│   │       ├── mod.rs
│   │       ├── plugin.rs
│   │       └── creation_runtime.rs
│   │
│   ├── api/                          # 保留现有模块（逐步迁移）
│   │   ├── model_provider.rs        # 模型提供商
│   │   ├── obsidian.rs              # Obsidian 集成
│   │   ├── policy.rs                # 安全策略
│   │   └── ...
│   │
│   └── lib.rs                        # 主入口
│
└── ~/.yunspire/plugins/              # 用户插件目录（未来）
    ├── custom-search/
    ├── advanced-analytics/
    └── export-tools/
```

---

## 🔧 统一插件接口

### 核心 Trait 定义

```rust
// src-tauri/src/core/plugin.rs

use serde_json::Value;
use rusqlite::Connection;

/// 插件能力标识
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capability {
    DatabaseRead,
    DatabaseWrite,
    VaultRead,
    VaultWrite,
    Network,
    Shell,
    ModelAccess,
}

/// 插件上下文（插件间通信）
pub struct PluginContext {
    pub app_handle: tauri::AppHandle,
    pub database: Arc<Mutex<Connection>>,
    pub config: PluginConfig,
}

/// Tauri 命令定义
pub struct Command {
    pub name: String,
    pub handler: Box<dyn Fn(Value) -> Result<Value, String> + Send + Sync>,
}

/// 数据库迁移定义
pub struct Migration {
    pub version: i64,
    pub sql: String,
}

/// 云枢插件 Trait（所有插件必须实现）
pub trait YunspirePlugin: Send + Sync {
    /// 插件唯一标识
    fn id(&self) -> &str;
    
    /// 插件名称
    fn name(&self) -> &str;
    
    /// 插件版本
    fn version(&self) -> &str;
    
    /// 插件描述
    fn description(&self) -> &str;
    
    /// 所需能力
    fn capabilities(&self) -> Vec<Capability>;
    
    /// 依赖的其他插件
    fn dependencies(&self) -> Vec<String> {
        vec![]
    }
    
    /// 插件加载时调用
    fn on_load(&mut self, context: &PluginContext) -> Result<(), String>;
    
    /// 插件卸载时调用
    fn on_unload(&mut self) -> Result<(), String>;
    
    /// 注册 Tauri 命令
    fn commands(&self) -> Vec<Command>;
    
    /// 数据库迁移脚本
    fn migrations(&self) -> Vec<Migration>;
    
    /// 插件配置 Schema
    fn config_schema(&self) -> Option<Value> {
        None
    }
}

/// 插件注册表
pub struct PluginRegistry {
    plugins: HashMap<String, Box<dyn YunspirePlugin>>,
    load_order: Vec<String>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self {
            plugins: HashMap::new(),
            load_order: Vec::new(),
        }
    }
    
    /// 注册插件
    pub fn register(&mut self, plugin: Box<dyn YunspirePlugin>) -> Result<(), String> {
        let id = plugin.id().to_string();
        
        // 检查依赖
        for dep in plugin.dependencies() {
            if !self.plugins.contains_key(&dep) {
                return Err(format!("插件 {} 依赖的插件 {} 未注册", id, dep));
            }
        }
        
        self.plugins.insert(id.clone(), plugin);
        self.load_order.push(id);
        Ok(())
    }
    
    /// 获取所有命令
    pub fn get_commands(&self) -> Vec<Command> {
        self.plugins.values()
            .flat_map(|p| p.commands())
            .collect()
    }
    
    /// 按顺序加载所有插件
    pub fn load_all(&mut self, context: &PluginContext) -> Result<(), String> {
        for id in &self.load_order {
            if let Some(plugin) = self.plugins.get_mut(id) {
                plugin.on_load(context)?;
                log::info!("插件已加载: {}", plugin.name());
            }
        }
        Ok(())
    }
}
```

### 插件实现示例

```rust
// src-tauri/src/plugins/search/plugin.rs

use crate::core::plugin::{YunspirePlugin, Capability, Command, Migration, PluginContext};

pub struct SearchPlugin {
    // 插件状态
}

impl YunspirePlugin for SearchPlugin {
    fn id(&self) -> &str {
        "yunspire.search"
    }
    
    fn name(&self) -> &str {
        "搜索引擎"
    }
    
    fn version(&self) -> &str {
        "1.0.0"
    }
    
    fn description(&self) -> &str {
        "提供全文搜索、语义搜索和神经嵌入搜索功能"
    }
    
    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::DatabaseRead, Capability::DatabaseWrite]
    }
    
    fn on_load(&mut self, context: &PluginContext) -> Result<(), String> {
        log::info!("搜索插件初始化");
        // 初始化搜索索引等
        Ok(())
    }
    
    fn on_unload(&mut self) -> Result<(), String> {
        log::info!("搜索插件卸载");
        Ok(())
    }
    
    fn commands(&self) -> Vec<Command> {
        vec![
            Command {
                name: "indexed_search".to_string(),
                handler: Box::new(|params| {
                    // 搜索实现
                    Ok(serde_json::json!({"results": []}))
                }),
            },
            Command {
                name: "rebuild_search_index".to_string(),
                handler: Box::new(|_| {
                    // 重建索引
                    Ok(serde_json::json!({"success": true}))
                }),
            },
        ]
    }
    
    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration {
                version: 1,
                sql: include_str!("migrations/001_initial.sql").to_string(),
            },
        ]
    }
}
```

---

## 📋 重构实施计划

### 阶段 0: 准备工作 (1周)

**Week 1: 建立基础设施**
- [ ] 创建 `src-tauri/src/core/` 目录
- [ ] 定义 `YunspirePlugin` trait
- [ ] 实现 `PluginRegistry`
- [ ] 编写插件开发文档

**交付物**:
- `core/plugin.rs` - 插件接口定义
- `core/plugin_registry.rs` - 插件注册表
- `docs/PLUGIN_DEVELOPMENT_GUIDE.md` - 插件开发指南

---

### 阶段 1: 拆解 runtime_db.rs (6周)

#### Week 1-2: 搜索模块独立

**任务**:
- [ ] 创建 `plugins/search/` 目录
- [ ] 提取 `indexed_search` 函数
- [ ] 提取 `neural_embedding` 相关函数
- [ ] 提取 `get_neural_embedding_index_status`
- [ ] 提取 `rebuild_neural_embedding_index`
- [ ] 实现 `SearchPlugin` trait
- [ ] 编写迁移脚本（从 runtime_db.rs 提取 schema）
- [ ] 保持旧接口向后兼容

**从 runtime_db.rs 提取的函数** (~1500 行):
```rust
// 搜索相关
- indexed_search()
- get_neural_embedding_index_status()
- rebuild_neural_embedding_index()
- refresh_neural_note_embeddings()
- batch_create_neural_note_embeddings()
- compute_rrf_scores()
- 相关辅助函数
```

**向后兼容桥接**:
```rust
// runtime_db.rs 保留旧接口
impl RuntimeDatabase {
    pub fn indexed_search(...) -> Result<...> {
        let search_plugin = get_plugin::<SearchPlugin>("yunspire.search")?;
        search_plugin.indexed_search(...)
    }
}
```

#### Week 3-4: 任务管理模块独立

**任务**:
- [ ] 创建 `plugins/tasks/` 目录
- [ ] 提取 `RuntimeTask` 相关函数
- [ ] 提取 `runtime_schedules` 相关函数
- [ ] 实现 `TaskPlugin` trait
- [ ] 编写迁移脚本
- [ ] 保持向后兼容

**从 runtime_db.rs 提取的函数** (~2000 行):
```rust
// 任务管理
- list_runtime_tasks()
- get_runtime_task()
- transition_native_runtime_task()
- claim_runtime_task_plan_steps()
- complete_runtime_task_plan_step()
- fail_runtime_task_plan_step()
- renew_runtime_task_step_lease()
- 调度相关函数
- 相关辅助函数
```

#### Week 5-6: 消息和内存模块独立

**任务 A: 消息插件**
- [ ] 创建 `plugins/messages/` 目录
- [ ] 提取 `workspace_messages` 相关函数
- [ ] 实现 `MessagePlugin` trait

**从 runtime_db.rs 提取的函数** (~800 行):
```rust
// 消息管理
- upsert_workspace_messages_page()
- list_workspace_messages_page()
- search_workspace_messages()
- delete_workspace_messages()
- delete_workspace_conversation_messages()
```

**任务 B: 记忆插件**
- [ ] 创建 `plugins/memory/` 目录
- [ ] 提取 `long_term_memory` 相关函数
- [ ] 实现 `MemoryPlugin` trait

**从 runtime_db.rs 提取的函数** (~1000 行):
```rust
// 长期记忆
- query_long_term_memory()
- govern_long_term_memory()
- export_long_term_memory()
- long_term_memory_metrics()
- append_long_term_memory_event()
```

---

### 阶段 2: 剩余模块拆分 (4周)

#### Week 7-8: 优化、报告、Vault 插件

**Week 7: 优化系统插件**
- [ ] 创建 `plugins/optimization/`
- [ ] 提取优化相关函数 (~1000 行)
- [ ] 实现 `OptimizationPlugin` trait

**Week 8: 报告和 Vault 插件**
- [ ] 创建 `plugins/reports/`
- [ ] 提取报告管理函数 (~600 行)
- [ ] 创建 `plugins/vault/`
- [ ] 提取 Vault 索引函数 (~800 行)

#### Week 9-10: 剩余小模块

**创作资源插件**:
- [ ] 创建 `plugins/creation_resources/`
- [ ] 提取创作资源函数 (~400 行)

**入站内容插件**:
- [ ] 创建 `plugins/inbound/`
- [ ] 提取入站内容函数 (~300 行)

**备份恢复插件**:
- [ ] 创建 `plugins/backup/`
- [ ] 提取备份恢复函数 (~500 行)

---

### 阶段 3: 集成与优化 (3周)

#### Week 11: 插件系统集成

**任务**:
- [ ] 修改 `lib.rs` 使用 `PluginRegistry`
- [ ] 动态命令注册
- [ ] 插件依赖解析
- [ ] 插件加载顺序优化

**新的 lib.rs 结构**:
```rust
// lib.rs
use crate::core::plugin_registry::PluginRegistry;

pub fn run() {
    // 创建插件注册表
    let mut registry = PluginRegistry::new();
    
    // 注册内置插件
    registry.register(Box::new(SearchPlugin::new()))?;
    registry.register(Box::new(TaskPlugin::new()))?;
    registry.register(Box::new(MessagePlugin::new()))?;
    // ... 其他插件
    
    // 加载所有插件
    let context = PluginContext { ... };
    registry.load_all(&context)?;
    
    // 动态生成命令处理器
    let commands = registry.get_commands();
    
    tauri::Builder::default()
        .invoke_handler(generate_dynamic_handler(commands))
        .run(tauri::generate_context!())
        .expect("启动失败");
}
```

#### Week 12: 测试与文档

**任务**:
- [ ] 每个插件编写单元测试
- [ ] 集成测试覆盖关键路径
- [ ] 性能基准测试
- [ ] 更新所有文档

#### Week 13: 代码审查与优化

**任务**:
- [ ] 代码审查所有插件
- [ ] 性能优化瓶颈
- [ ] 内存泄漏检查
- [ ] 最终集成测试

---

### 阶段 4: 用户扩展支持 (3周)

#### Week 14: 插件沙箱

**任务**:
- [ ] 设计插件沙箱机制
- [ ] 权限验证系统
- [ ] 资源限制（CPU、内存、网络）

#### Week 15: 动态加载

**任务**:
- [ ] 实现 `.so`/`.dylib`/`.dll` 加载
- [ ] 插件热重载
- [ ] 插件更新机制

#### Week 16: 插件市场

**任务**:
- [ ] 插件规范文档
- [ ] 插件签名验证
- [ ] 插件市场 API 设计

---

## 🔀 与功能增强计划融合

### 同步进行的功能增强

在重构过程中，功能增强以**插件形式**实施：

#### Sprint 1 (Week 1-2): 核心体验改进插件

**1. 采集优雅降级** (进行中)
- 实施位置: `plugins/capture/graceful_degradation.rs`
- 集成到 `CapturePlugin`
- 引入 `PartialSuccess` 状态
- 工时: 4-6 小时

**2. 搜索匹配解释** (代码已完成)
- 实施位置: `plugins/search/match_reasons.rs`
- 集成到 `SearchPlugin`
- 调用现有 `MatchReasonAnalyzer`
- 工时: 3-4 小时

#### Sprint 2 (Week 3-4): 日常使用优化

**3. 创作流程简化**
- 实施位置: `plugins/creation/mode.rs`
- 快速/专业模式切换
- 工时: 4-6 小时

**4. 知识库健康度仪表盘**
- 实施位置: `plugins/knowledge_health/`
- 新建独立插件
- 工时: 6-8 小时

#### Sprint 3 (Week 5-7): 智能机制

**5. AI 执行计划透明化**
- 实施位置: `plugins/ai_transparency/`
- 新建独立插件
- 工时: 10-12 小时

**6. 多层内容指纹去重**
- 实施位置: `plugins/deduplication/`
- 新建独立插件
- 工时: 8-10 小时

#### Sprint 4 (Week 8-10): 量化效果

**7. 使用效果量化指标**
- 实施位置: `plugins/metrics/`
- 新建独立插件
- 工时: 8-10 小时

---

## 📊 进度跟踪指标

### 重构进度指标

| 指标 | 当前 | 目标 | 说明 |
|------|------|------|------|
| runtime_db.rs 行数 | 18,528 | < 500 | 只保留连接管理 |
| 插件数量 | 0 | 15+ | 所有核心功能插件化 |
| 最大文件行数 | 18,528 | < 1,000 | 每个文件可维护 |
| 模块职责清晰度 | 20% | 100% | 单一职责原则 |
| 用户可扩展性 | 0% | 100% | 支持第三方插件 |

### 质量指标

| 指标 | 当前 | 目标 |
|------|------|------|
| 单元测试覆盖率 | ~30% | > 80% |
| 集成测试覆盖率 | ~20% | > 70% |
| 编译警告数 | 7 | 0 |
| Clippy 警告数 | 0 | 0 |
| 文档覆盖率 | ~40% | > 90% |

---

## 🎯 里程碑

### M1: 插件基础设施 (Week 1)
- ✅ `YunspirePlugin` trait 定义完成
- ✅ `PluginRegistry` 实现完成
- ✅ 插件开发文档完成

### M2: 搜索插件独立 (Week 2)
- ✅ `SearchPlugin` 完成
- ✅ 搜索功能测试通过
- ✅ 性能无退化

### M3: 任务管理插件独立 (Week 4)
- ✅ `TaskPlugin` 完成
- ✅ 任务功能测试通过
- ✅ Lease 续期集成

### M4: 核心模块拆分完成 (Week 10)
- ✅ runtime_db.rs < 500 行
- ✅ 所有插件测试通过
- ✅ 性能基准达标

### M5: 插件系统集成 (Week 11)
- ✅ 动态命令注册
- ✅ 所有功能正常
- ✅ 向后兼容

### M6: 用户扩展支持 (Week 16)
- ✅ 第三方插件加载
- ✅ 插件市场上线
- ✅ 文档完善

---

## 🚨 风险管理

### 高风险项

#### 1. 数据迁移风险

**风险**: 拆分数据库 schema 可能导致数据丢失

**缓解措施**:
- 每个插件提供独立迁移脚本
- 自动备份机制
- 回滚脚本
- 金丝雀测试

#### 2. 性能退化风险

**风险**: 插件间通信开销可能影响性能

**缓解措施**:
- 性能基准测试
- 关键路径优化
- 零拷贝数据传递
- 异步 IO

#### 3. 向后兼容性风险

**风险**: 重构可能破坏现有 API

**缓解措施**:
- 保持旧 API 接口
- 内部委托到新插件
- 完整集成测试
- 版本控制

### 中等风险项

#### 4. 开发周期风险

**风险**: 16 周时间可能不够

**缓解措施**:
- 按阶段交付
- 每周 review
- 优先级动态调整

#### 5. 技术债务风险

**风险**: 重构不彻底，遗留问题

**缓解措施**:
- Code review
- 技术债务跟踪
- 定期重构

---

## 📚 文档规划

### 开发文档

- [x] 架构重构路线图（本文档）
- [ ] 插件开发指南
- [ ] API 参考文档
- [ ] 迁移指南

### 用户文档

- [ ] 插件安装指南
- [ ] 插件配置手册
- [ ] 常见问题 FAQ
- [ ] 最佳实践

### 内部文档

- [ ] 设计决策记录（ADR）
- [ ] 性能优化记录
- [ ] 测试策略文档

---

## ✅ 成功标准

### 技术标准

1. **代码质量**
   - [ ] runtime_db.rs < 500 行
   - [ ] 所有模块 < 1,000 行
   - [ ] 0 Clippy 警告
   - [ ] 测试覆盖率 > 80%

2. **性能标准**
   - [ ] 搜索性能 < 100ms (p95)
   - [ ] 启动时间 < 2s
   - [ ] 内存占用 < 500MB

3. **架构标准**
   - [ ] 15+ 插件
   - [ ] 插件依赖关系清晰
   - [ ] 支持第三方插件

### 用户体验标准

1. **功能完整性**
   - [ ] 所有现有功能正常
   - [ ] 7 个新功能上线
   - [ ] 向后兼容

2. **可扩展性**
   - [ ] 用户可安装第三方插件
   - [ ] 插件配置简单
   - [ ] 文档完善

---

## 🔄 持续改进

### 重构完成后

**Phase 1** (Week 17-20): 稳定性
- 修复用户反馈的问题
- 性能优化
- 文档补充

**Phase 2** (Week 21-24): 生态建设
- 官方插件库扩充
- 第三方插件支持
- 社区建设

**Phase 3** (Week 25+): 持续演进
- 插件 API v2
- 更多能力开放
- 跨平台支持

---

## 📞 总结

### 核心目标

1. **拆解屎山**: 将 18,528 行的 `runtime_db.rs` 拆分为 15+ 个清晰的插件
2. **真正插件化**: 所有功能都是插件，用户可替换/扩展
3. **保持稳定**: 重构过程中保持现有功能正常运行
4. **融合增强**: 7 个功能增强以插件形式实施

### 时间线

- **Week 0-1**: 准备工作
- **Week 1-10**: 核心重构（runtime_db.rs 拆分）
- **Week 11-13**: 集成与测试
- **Week 14-16**: 用户扩展支持
- **Week 17+**: 持续改进

### 资源投入

- **全职开发**: 1 人
- **Code Review**: 1 人（兼职）
- **测试**: 自动化 + 手工
- **总工时**: ~640 小时（16 周 × 40 小时）

---

**下一步**: 开始阶段 0 - 建立插件基础设施

**负责人**: Claude (Opus 5)  
**状态**: 规划完成，等待启动
