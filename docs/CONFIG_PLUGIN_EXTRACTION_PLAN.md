# ConfigPlugin 提取计划

**目标**: 将配置管理功能从 runtime_db.rs 提取到独立的 ConfigPlugin

---

## 📊 当前状态分析

### 已有配置相关代码

1. **DatabaseConfig** (src/database/config.rs)
   - ✅ 已经模块化
   - 113 行代码
   - 完整的验证逻辑
   - 预设配置（默认、高性能、低资源）

2. **runtime_settings 表**
   - workspace_scope
   - scheduler_enabled
   - updated_at

3. **配置使用场景**
   - 数据库连接池大小
   - 批处理大小
   - 查询超时
   - 慢查询阈值
   - 缓存配置

---

## 🎯 ConfigPlugin 设计

### 模块结构

```
plugins/config/
├── types.rs        - 配置数据结构
│   ├── DatabaseConfig (复用现有)
│   ├── RuntimeSettings
│   └── ConfigSchema
├── validation.rs   - 配置验证
│   ├── validate_config()
│   └── validate_schema()
├── storage.rs      - 配置持久化
│   ├── load_config()
│   ├── save_config()
│   └── load_runtime_settings()
├── migration.rs    - 配置迁移 (可选)
│   └── migrate_config()
└── plugin.rs       - ConfigPlugin 实现
    └── YunspirePlugin trait
```

### 核心功能

#### 1. 配置管理
- ✅ 读取配置
- ✅ 验证配置
- ✅ 保存配置
- ✅ 默认配置

#### 2. 运行时设置
- ✅ scheduler_enabled
- ✅ workspace 级别设置
- ✅ 持久化到数据库

#### 3. 配置预设
- ✅ 默认配置
- ✅ 高性能配置
- ✅ 低资源配置

---

## 📋 提取任务清单

### 阶段 1: 数据结构 (1 小时)

**任务**:
- [x] DatabaseConfig (已存在)
- [ ] RuntimeSettings 结构体
- [ ] ConfigSchema 定义
- [ ] 类型导出

**文件**: `types.rs`

### 阶段 2: 配置验证 (30 分钟)

**任务**:
- [x] DatabaseConfig::validate() (已存在)
- [ ] RuntimeSettings 验证
- [ ] 提取验证逻辑

**文件**: `validation.rs`

### 阶段 3: 配置存储 (1 小时)

**任务**:
- [ ] load_runtime_settings()
- [ ] save_runtime_settings()
- [ ] 数据库操作封装

**文件**: `storage.rs`

**从 runtime_db.rs 提取**:
- 行 1836: INSERT runtime_settings
- 行 2668: SELECT scheduler_enabled
- 行 9423: SELECT runtime_settings

### 阶段 4: Plugin 实现 (30 分钟)

**任务**:
- [ ] YunspirePlugin trait 实现
- [ ] 生命周期管理
- [ ] Tauri 命令注册
- [ ] 数据库迁移脚本

**文件**: `plugin.rs`

### 阶段 5: 测试 (1 小时)

**任务**:
- [ ] 单元测试
- [ ] 集成测试
- [ ] 配置验证测试

---

## 🔍 代码识别

### 需要提取的函数

#### runtime_db.rs 中的配置相关代码

```rust
// 行 1836: 初始化 runtime_settings
"INSERT INTO runtime_settings ..."

// 行 2668: 读取 scheduler_enabled
"SELECT scheduler_enabled FROM runtime_settings WHERE workspace_scope=?1"

// 行 9423: 读取完整设置
"SELECT scheduler_enabled, updated_at FROM runtime_settings WHERE workspace_scope=?1"

// 行 11813: 创建表
"CREATE TABLE IF NOT EXISTS runtime_settings (...)"
```

### 需要复用的代码

#### database/config.rs (已模块化) ✅

- DatabaseConfig 结构体
- validate() 方法
- 预设配置（default, high_performance, low_resource）

---

## 📊 预估工作量

| 阶段 | 任务 | 预估时间 | 难度 |
|------|------|---------|------|
| 1 | 数据结构 | 1h | 低 |
| 2 | 配置验证 | 0.5h | 低 |
| 3 | 配置存储 | 1h | 中 |
| 4 | Plugin 实现 | 0.5h | 低 |
| 5 | 测试 | 1h | 中 |
| **总计** | | **4h** | **低-中** |

**预计对话次数**: 1-2 次

---

## 🎯 成功标准

### 功能完整性
- [ ] 所有配置项可读写
- [ ] 验证逻辑完整
- [ ] 持久化正常工作
- [ ] 向后兼容

### 代码质量
- [ ] 编译通过
- [ ] 所有测试通过
- [ ] 代码覆盖率 > 80%
- [ ] 文档完整

### 架构设计
- [ ] 模块化清晰
- [ ] 接口设计合理
- [ ] 易于扩展
- [ ] 无循环依赖

---

## 📝 设计决策

### 决策 1: 复用 DatabaseConfig

**方案**: 保留 database/config.rs，ConfigPlugin 引用

**理由**:
- DatabaseConfig 已经模块化
- 验证逻辑完整
- 避免重复代码

### 决策 2: 配置存储位置

**方案**: runtime_settings 表 + DatabaseConfig 内存

**理由**:
- runtime_settings 存储运行时状态
- DatabaseConfig 作为初始化参数
- 分离关注点

### 决策 3: 配置迁移

**方案**: 暂不实现，作为预留功能

**理由**:
- 当前配置结构稳定
- 迁移需求不明确
- 可后续添加

---

## 🚀 实施策略

### 优先级 1: 基础功能

1. 数据结构定义
2. 基本的读写操作
3. Plugin 框架

### 优先级 2: 完善功能

1. 配置验证
2. 错误处理
3. 测试覆盖

### 优先级 3: 高级功能

1. 配置迁移（可选）
2. 配置热更新（可选）
3. 配置导入/导出（可选）

---

## 📦 依赖关系

### ConfigPlugin 依赖

- `database::DatabaseConfig` ✅
- `runtime_db::RuntimeDatabase`
- `core::YunspirePlugin`

### 被依赖

- `SearchPlugin` (使用配置)
- `TaskPlugin` (使用配置)
- 其他插件

---

## 🎊 预期成果

### 代码量预估

```
types.rs        ~100 行
validation.rs   ~80 行
storage.rs      ~150 行
plugin.rs       ~200 行
━━━━━━━━━━━━━━━━━━━
总计            ~530 行
```

### 测试预估

```
单元测试        ~8 个
集成测试        ~3 个
━━━━━━━━━━━━━━━━━━━
总计            ~11 个
```

---

## 📚 参考资料

- SearchPlugin 实现经验
- database/config.rs 现有代码
- PLUGIN_DEVELOPMENT_GUIDE.md

---

**下一步**: 开始实施阶段 1 - 数据结构定义
