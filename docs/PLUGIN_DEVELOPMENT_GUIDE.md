# 云枢插件开发指南

**版本**: v1.0  
**更新时间**: 2026-08-15

---

## 📖 概述

云枢插件系统允许开发者以模块化的方式扩展云枢的功能。每个插件都是一个独立的 Rust 模块，实现统一的 `YunspirePlugin` trait。

---

## 🏗️ 插件架构

### 核心组件

```
core/
├── plugin.rs           # YunspirePlugin trait 定义
└── plugin_registry.rs  # 插件注册和管理

plugins/
├── example/            # 示例插件
├── search/             # 搜索插件（规划中）
└── tasks/              # 任务管理插件（规划中）
```

### 插件生命周期

```
1. 创建插件实例
   ↓
2. 注册到 PluginRegistry
   ↓
3. 依赖关系解析（拓扑排序）
   ↓
4. on_load() - 插件初始化
   ↓
5. 运行时（处理命令、数据库操作）
   ↓
6. on_unload() - 插件卸载
```

---

## 🚀 快速开始

### 1. 创建插件目录

```bash
mkdir -p src-tauri/src/plugins/my_plugin
touch src-tauri/src/plugins/my_plugin/mod.rs
```

### 2. 实现 YunspirePlugin Trait

```rust
use crate::core::plugin::{
    Capability, Command, Migration, PluginContext, YunspirePlugin
};

pub struct MyPlugin {
    // 插件状态
}

impl YunspirePlugin for MyPlugin {
    fn id(&self) -> &str {
        "yunspire.my_plugin"  // 唯一标识
    }

    fn name(&self) -> &str {
        "我的插件"
    }

    fn version(&self) -> &str {
        "1.0.0"
    }

    fn description(&self) -> &str {
        "这是我的第一个云枢插件"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::DatabaseRead,
            Capability::DatabaseWrite,
        ]
    }

    fn on_load(&mut self, context: &PluginContext) -> Result<(), String> {
        log::info!("插件加载中...");
        // 初始化逻辑
        Ok(())
    }

    fn on_unload(&mut self) -> Result<(), String> {
        log::info!("插件卸载中...");
        // 清理逻辑
        Ok(())
    }

    fn commands(&self) -> Vec<Command> {
        vec![
            Command::new("my_command", |params| {
                // 命令处理逻辑
                Ok(serde_json::json!({"success": true}))
            }),
        ]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration::new(
                1,
                "CREATE TABLE my_data (id TEXT PRIMARY KEY);",
                "创建数据表",
            ),
        ]
    }
}
```

### 3. 注册插件

在 `src-tauri/src/plugins/mod.rs` 中：

```rust
pub mod my_plugin;

pub use my_plugin::MyPlugin;
```

### 4. 编写测试

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_metadata() {
        let plugin = MyPlugin::new();
        assert_eq!(plugin.id(), "yunspire.my_plugin");
        assert_eq!(plugin.name(), "我的插件");
    }
}
```

### 5. 运行测试

```bash
cargo test --lib plugins::my_plugin::tests
```

---

## 📚 API 参考

### YunspirePlugin Trait

#### 必须实现的方法

| 方法 | 返回类型 | 描述 |
|------|---------|------|
| `id()` | `&str` | 插件唯一标识（如 "yunspire.search"） |
| `name()` | `&str` | 插件显示名称 |
| `version()` | `&str` | 插件版本（语义化版本） |
| `description()` | `&str` | 插件描述 |
| `capabilities()` | `Vec<Capability>` | 插件所需权限 |
| `on_load()` | `Result<(), String>` | 插件加载时调用 |
| `on_unload()` | `Result<(), String>` | 插件卸载时调用 |
| `commands()` | `Vec<Command>` | 注册的 Tauri 命令 |
| `migrations()` | `Vec<Migration>` | 数据库迁移脚本 |

#### 可选实现的方法

| 方法 | 默认行为 | 描述 |
|------|---------|------|
| `dependencies()` | `vec![]` | 依赖的其他插件 |
| `config_schema()` | `None` | 插件配置 Schema |
| `health_check()` | `Ok(())` | 健康检查 |

---

## 🔑 插件能力 (Capability)

```rust
pub enum Capability {
    DatabaseRead,      // 数据库读取
    DatabaseWrite,     // 数据库写入
    VaultRead,         // Vault 读取
    VaultWrite,        // Vault 写入
    Network,           // 网络访问
    Shell,             // Shell 执行
    ModelAccess,       // 模型访问
}
```

**最小权限原则**: 只请求插件实际需要的能力。

---

## 📦 命令 (Command)

### 命令定义

```rust
Command::new("command_name", |params: Value| -> Result<Value, String> {
    // 从参数中提取数据
    let name = params.get("name")
        .and_then(|v| v.as_str())
        .ok_or("缺少参数: name")?;

    // 处理逻辑
    let result = do_something(name);

    // 返回 JSON 结果
    Ok(serde_json::json!({
        "success": true,
        "data": result
    }))
})
```

### 前端调用

```typescript
// 调用插件命令
const result = await invoke('command_name', {
  name: 'Alice'
});

console.log(result); // { success: true, data: ... }
```

### 错误处理

```rust
Command::new("risky_operation", |params| {
    // 返回错误
    if params.is_null() {
        return Err("参数不能为空".to_string());
    }

    // ... 处理逻辑

    Ok(serde_json::json!({"success": true}))
})
```

---

## 🗄️ 数据库迁移 (Migration)

### 迁移定义

```rust
Migration::new(
    version,        // 迁移版本号（递增）
    sql,            // SQL 语句
    description,    // 迁移描述
)
```

### 迁移示例

```rust
fn migrations(&self) -> Vec<Migration> {
    vec![
        // 版本 1: 创建表
        Migration::new(
            1,
            "CREATE TABLE users (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE INDEX idx_users_name ON users(name);",
            "创建用户表",
        ),

        // 版本 2: 添加列
        Migration::new(
            2,
            "ALTER TABLE users ADD COLUMN email TEXT;",
            "添加邮箱列",
        ),

        // 版本 3: 创建新表
        Migration::new(
            3,
            "CREATE TABLE user_sessions (
                session_id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                FOREIGN KEY(user_id) REFERENCES users(id)
            );",
            "创建会话表",
        ),
    ]
}
```

### 迁移规则

1. **版本号递增**: 1, 2, 3, ...
2. **幂等性**: 使用 `IF NOT EXISTS`
3. **不可修改**: 已执行的迁移不能修改
4. **向后兼容**: 新迁移不破坏旧数据

---

## 🔗 插件依赖

### 声明依赖

```rust
fn dependencies(&self) -> Vec<String> {
    vec![
        "yunspire.search".to_string(),
        "yunspire.tasks".to_string(),
    ]
}
```

### 依赖解析

插件注册表会自动：
1. 检查依赖是否存在
2. 拓扑排序（按依赖关系）
3. 按顺序加载插件

### 循环依赖检测

```
插件 A 依赖 B
插件 B 依赖 A
↓
错误: 检测到循环依赖
```

---

## 🧪 测试最佳实践

### 1. 测试插件元数据

```rust
#[test]
fn test_plugin_metadata() {
    let plugin = MyPlugin::new();
    assert_eq!(plugin.id(), "yunspire.my_plugin");
    assert_eq!(plugin.name(), "我的插件");
    assert_eq!(plugin.version(), "1.0.0");
}
```

### 2. 测试命令

```rust
#[test]
fn test_my_command() {
    let plugin = MyPlugin::new();
    let commands = plugin.commands();
    let cmd = &commands[0];

    // 正常情况
    let result = (cmd.handler)(serde_json::json!({"name": "Alice"})).unwrap();
    assert_eq!(result["success"], true);

    // 错误情况
    let result = (cmd.handler)(serde_json::json!({}));
    assert!(result.is_err());
}
```

### 3. 测试迁移

```rust
#[test]
fn test_migrations() {
    let plugin = MyPlugin::new();
    let migrations = plugin.migrations();

    // 检查版本号递增
    for i in 1..migrations.len() {
        assert!(migrations[i].version > migrations[i-1].version);
    }
}
```

---

## 📋 完整示例

参考 `src-tauri/src/plugins/example/mod.rs`：

```rust
use crate::core::plugin::{Capability, Command, Migration, PluginContext, YunspirePlugin};

pub struct ExamplePlugin {
    initialized: bool,
}

impl ExamplePlugin {
    pub fn new() -> Self {
        Self { initialized: false }
    }
}

impl YunspirePlugin for ExamplePlugin {
    fn id(&self) -> &str {
        "yunspire.example"
    }

    fn name(&self) -> &str {
        "示例插件"
    }

    fn version(&self) -> &str {
        "1.0.0"
    }

    fn description(&self) -> &str {
        "这是一个示例插件"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability::DatabaseRead, Capability::DatabaseWrite]
    }

    fn on_load(&mut self, _context: &PluginContext) -> Result<(), String> {
        self.initialized = true;
        log::info!("示例插件已加载");
        Ok(())
    }

    fn on_unload(&mut self) -> Result<(), String> {
        self.initialized = false;
        log::info!("示例插件已卸载");
        Ok(())
    }

    fn commands(&self) -> Vec<Command> {
        vec![
            Command::new("example_hello", |params| {
                let name = params.get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("World");

                Ok(serde_json::json!({
                    "message": format!("Hello, {}!", name)
                }))
            }),
        ]
    }

    fn migrations(&self) -> Vec<Migration> {
        vec![
            Migration::new(
                1,
                "CREATE TABLE example_data (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL
                );",
                "创建示例表",
            ),
        ]
    }
}
```

---

## 🎯 下一步

1. **阅读架构文档**: [ARCHITECTURE_REFACTORING_ROADMAP.md](../ARCHITECTURE_REFACTORING_ROADMAP.md)
2. **查看示例插件**: `src-tauri/src/plugins/example/mod.rs`
3. **运行测试**: `cargo test --lib plugins::example::tests`
4. **开始开发**: 创建你的第一个插件！

---

## 📞 获取帮助

- **文档**: [docs/README.md](./README.md)
- **代码**: `src-tauri/src/core/plugin.rs`
- **示例**: `src-tauri/src/plugins/example/`

---

**Happy Coding! 🚀**
