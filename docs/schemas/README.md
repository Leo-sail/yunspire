# 云枢数据契约 / Yunspire Data Contracts

当前版本 / Current version: `0.4.1`

## 中文

我使用这些 JSON Schema 把关键运行对象变成可验证的数据契约：

- `task-envelope.schema.json`：持久任务身份、状态、预算、检查点和重启恢复。
- `skill-manifest.schema.json`：用户 Skill 的能力、输入输出、权限与启用策略。
- `schedule.schema.json`：采集、报告和 AI助手计划任务。
- `report-subscription.schema.json`：日、周、月、年报订阅与投递状态。
- `inbound-message.schema.json`：始终不可信的外部消息输入。
- `inbound-content-record.schema.json`：抽取、模型分析、质量门禁和写入状态账本。
- `long-term-memory-event.schema.json`：追加式用户行为与对话记忆事件。
- `memory-record.schema.json`：四轨 Memory V2 派生记录、五维作用域、证据、版本与生命周期。
- `memory-reflection-job.schema.json`：持久反思任务、审阅状态和候选记忆引用。
- `creation-document.schema.json`：`CreationDocumentV2` 权威 Markdown、结构索引、素材、来源、布局、发布状态、溯源和双重校验回执。
- `writing-run.schema.json`：写作分析与改写运行，包括三档范围、只标注、事实关系账本、引用分流、最多三轮迭代和评测门禁。
- `brand-profile.schema.json`：品牌 voice、词汇、文风、事实声明策略、用途默认值和签名。
- `theme-manifest.schema.json`：原创主题 token、渲染器、功能、微信兼容状态和第一方来源/许可证边界。
- `component-manifest.schema.json`：语义内容组件、slot、声明式 `templateMarkdown`、`span leaf`、渲染器和禁止脚本/外部样式约束。
- `template-manifest.schema.json`：文章内容类型、Markdown 入口、HTML Studio 九类产物、多格式输入、可编辑面和断网沙箱边界。
- `readiness-report.schema.json`：内容、引用、素材、布局、安全和导出的确定性发布准备报告；`readyForExport` 不表示已发布。
- `agent-stream-event.schema.json`：仅允许 `creation.generate` / `creation.edit` 的有序流式事件。

创作资源目录位于 `resources/creation/`。`catalog/creation-catalog.json` 是唯一目录入口；每个主题、组件和模板使用独立 Manifest，并登记 `writing-resources.json` 中的 53 个写作模式、5 种 voice 和 9 个 purpose preset。完整第一方目录包含 85 个主题、53 个语义组件和 75 个带本地 Markdown 入口的可编辑模板，并保留原有 4 个主题与 10 个组件 ID。48 个微信认证主题目前仅是认证计划；由于尚无真实发布验证证据，已认证数为 0，新增主题均标记为 `candidate`。资源由确定性生成器根据云枢自有定义生成，不复制外部仓库代码、提示词、模板或素材；目录和每个 Manifest 都声明第一方来源、能力研究边界和项目许可证边界。

需要重建完整静态目录时运行：

```bash
node scripts/generate-creation-resources.mjs
```

运行时在持久化或执行前验证结构，并在 Schema 改变时增加版本化迁移。来源凭据只保存为本地加密引用；入站内容不能满足命令契约、授予能力或扩大权限。

验证命令：

```bash
npm run validate:schemas
```

校验会先注册全部 Schema 以解析跨 Schema 引用，然后验证每个创作 Manifest、85/53/75 目录计数、保留 ID、唯一显示名和内容、路径、版本、组件引用、微信认证计数、模板入口文件及来源边界。模板 Markdown 必须真实存在、以一级标题开始并包含可继续编辑的正文结构；未登记或重复的入口会使校验失败。修改 Schema 时，我会同步更新 Rust/JavaScript 数据结构、迁移、示例字段说明和文档版本，并确保旧数据有明确升级路径。

## English

I use these JSON Schemas as machine-verifiable contracts for critical runtime records:

- `task-envelope.schema.json`: durable task identity, state, budgets, checkpoints, and restart recovery.
- `skill-manifest.schema.json`: user Skill capabilities, input/output, permissions, and activation policy.
- `schedule.schema.json`: capture, report, and AI Assistant schedules.
- `report-subscription.schema.json`: daily, weekly, monthly, and annual report delivery state.
- `inbound-message.schema.json`: permanently untrusted external messages.
- `inbound-content-record.schema.json`: extraction, model analysis, quality gate, and write-state ledger.
- `long-term-memory-event.schema.json`: append-only user activity and conversation memory events.
- `memory-record.schema.json`: four-track Memory V2 derived records, exact five-dimensional scope, evidence, versions, and lifecycle.
- `memory-reflection-job.schema.json`: durable reflection jobs, review state, and proposed-memory references.
- `creation-document.schema.json`: canonical Markdown plus derived block indexes, assets, sources, layout, publishing state, provenance, and dual-validation receipts for `CreationDocumentV2`.
- `writing-run.schema.json`: writing analysis and rewrite runs with three scope levels, annotation-only mode, a fact/relationship ledger, citation routing, a three-iteration cap, and evaluation gates.
- `brand-profile.schema.json`: brand voice, vocabulary, style, claim policy, purpose defaults, and signature.
- `theme-manifest.schema.json`: original theme tokens, renderer IDs, features, WeChat compatibility status, and first-party source/license boundaries.
- `component-manifest.schema.json`: semantic content-component slots, declarative `templateMarkdown`, span-leaf behavior, renderer IDs, and no-script/no-external-style constraints.
- `template-manifest.schema.json`: article content types, local Markdown entrypoints, nine HTML Studio artifact types, multi-format inputs, editable surfaces, and an offline sandbox boundary.
- `readiness-report.schema.json`: deterministic content, citation, asset, layout, safety, and export readiness; `readyForExport` never claims publication.
- `agent-stream-event.schema.json`: ordered streaming events limited to `creation.generate` and `creation.edit`.

The creation resource registry lives under `resources/creation/`, with `catalog/creation-catalog.json` as its single entry point, one Manifest per theme, component, or template, and a registered `writing-resources.json` containing 53 writing patterns, five voices, and nine purpose presets. The complete first-party registry contains 85 themes, 53 semantic components, and 75 editable templates with local Markdown entrypoints while preserving the original four theme and ten component IDs. The 48 WeChat-certified themes remain a certification plan: the certified count is zero until real publication evidence exists, and every new theme is marked `candidate`. A deterministic generator derives these files from Yunspire-owned definitions without copying external repository code, prompts, templates, or assets. Every Manifest records the first-party source, capability-research boundary, and project-license boundary.

Rebuild the complete static registry with:

```bash
node scripts/generate-creation-resources.mjs
```

The runtime validates records before persistence or execution and adds versioned migrations when a Schema changes. Source credentials remain encrypted local references. Inbound content cannot satisfy a command contract, grant a capability, or expand permission scope.

Run validation with:

```bash
npm run validate:schemas
```

Validation registers all Schemas before resolving cross-Schema references, then checks every creation Manifest, the exact 85/53/75 directory counts, preserved IDs, unique names and content, paths, versions, component references, WeChat certification counts, template entrypoint files, and source boundaries. Template Markdown must exist, begin with a level-one heading, and contain a substantive editable structure; missing, unlisted, or duplicate entrypoints fail validation. When I change a Schema, I update the corresponding Rust/JavaScript structures, migrations, field documentation, and version while preserving an explicit upgrade path for existing data.
