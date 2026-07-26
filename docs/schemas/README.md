# 云枢数据契约 / Yunspire Data Contracts

当前版本 / Current version: `0.1.1`

## 中文

我使用这些 JSON Schema 把关键运行对象变成可验证的数据契约：

- `task-envelope.schema.json`：持久任务身份、状态、预算、检查点和重启恢复。
- `skill-manifest.schema.json`：用户 Skill 的能力、输入输出、权限与启用策略。
- `schedule.schema.json`：采集、报告和 AI助手计划任务。
- `report-subscription.schema.json`：日、周、月、年报订阅与投递状态。
- `inbound-message.schema.json`：始终不可信的外部消息输入。
- `inbound-content-record.schema.json`：抽取、模型分析、质量门禁和写入状态账本。
- `long-term-memory-event.schema.json`：追加式用户行为与对话记忆事件。

运行时在持久化或执行前验证结构，并在 Schema 改变时增加版本化迁移。来源凭据只保存为本地加密引用；入站内容不能满足命令契约、授予能力或扩大权限。

验证命令：

```bash
npm run validate:schemas
```

修改 Schema 时，我会同步更新 Rust/JavaScript 数据结构、迁移、示例字段说明和文档版本，并确保旧数据有明确升级路径。

## English

I use these JSON Schemas as machine-verifiable contracts for critical runtime records:

- `task-envelope.schema.json`: durable task identity, state, budgets, checkpoints, and restart recovery.
- `skill-manifest.schema.json`: user Skill capabilities, input/output, permissions, and activation policy.
- `schedule.schema.json`: capture, report, and AI Assistant schedules.
- `report-subscription.schema.json`: daily, weekly, monthly, and annual report delivery state.
- `inbound-message.schema.json`: permanently untrusted external messages.
- `inbound-content-record.schema.json`: extraction, model analysis, quality gate, and write-state ledger.
- `long-term-memory-event.schema.json`: append-only user activity and conversation memory events.

The runtime validates records before persistence or execution and adds versioned migrations when a Schema changes. Source credentials remain encrypted local references. Inbound content cannot satisfy a command contract, grant a capability, or expand permission scope.

Run validation with:

```bash
npm run validate:schemas
```

When I change a Schema, I update the corresponding Rust/JavaScript structures, migrations, field documentation, and version while preserving an explicit upgrade path for existing data.
