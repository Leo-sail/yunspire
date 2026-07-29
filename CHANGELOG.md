# 更新记录 / Changelog

本文件只记录公开版本的实质变化，不记录本机验证数据或内部临时过程。

This file records material public-version changes only. It excludes local validation data and temporary internal work.

## 中文

### 未发布

_暂无。_

### 0.1.2 - 2026-07-29

- AI 请求改为按对话先进先出排队、跨对话并行执行；取消令牌贯穿模型、分析和后续执行链，新建对话不再被其他对话阻塞。
- 长期记忆升级为四轨 Memory V2，增加五维作用域、证据与置信度、版本替代、过期和墓碑治理；反思草稿只有经用户批准后才可召回。
- 搜索增加面向中文的 CJK 字符对与标题、路径、标签、Wiki Link、时间等多信号词法排序，并保持到 Obsidian 原文的可打开回链。
- Command Bus 增加一次性执行票据，拒绝参数替换、并发重复提交和重放；跨 Vault 写入增加持久 manifest、崩溃恢复、冲突保护和目录同步。
- 增加云枢第一方受控深度研究 Skill，按计划、证据、矛盾、综合、引用和反思六阶段执行，并校验预算、取消、检查点、来源链与引用完整性。
- 增加 macOS/Windows 双平台质量门禁和未签名安装包发布流水线，并对版本、来源树、平台资源、校验和与安装产物执行发布审计。

### 0.1.1 - 2026-07-26

- 修复 AI助手处理消息时阻塞其他对话发送、Agent 库重复保存原文、搜索结果无法在 Obsidian 中打开，以及知识图谱文本乱码的问题。
- 本地文件和文件夹改为分块上传与流式暂存，不再设置单文件或单次选择总大小上限。
- 对话图片首次由分析模型建立可持久化视觉记录；普通历史只复用记录，明确引用历史图片时才重新读取原图。
- AI助手增加内置 Emoji 头像选择并随本地偏好持久化。
- 首次安装启动增加包含 5 个主要特点的版本化引导教学。
- Word、Excel 和 PowerPoint 升级为位置保真的 OOXML v2 抽取：保留附属部件、多工作表/公式、图片锚点、真实页序、元素边界和版式/母版来源。
- 文件内链接增加统一来源与安全策略；模型正文和多批结果按 UTF-8 字节边界完整分批及分层汇总，不再静默截断。

### 0.1.0 - 2026-07-21

- 建立以 Obsidian Vault 为知识权威、SQLite 为运行状态权威的本地桌面架构。
- 实现 AI助手对话、模型供应商、多用途模型路由和一次性意图回执。
- 实现 Vault 发现、搜索、读取、写入计划、原子提交、文件监听和 FTS。
- 实现采集抽取、模型分析、质量门禁、定时任务、报告和长期记忆投递。
- 增加 TXT、Markdown、PDF、Word、PowerPoint、Excel、图片与音视频的第一方抽取链路；Excel 先清洗为结构化 JSON。
- 增加跨任务、跨来源的规范化内容哈希去重，并区分“跳过重复”与解析或模型失败。
- 实现 Command Bus、Policy Engine、持久任务状态、检查点和操作日志。
- 实现笔记、文件夹、Properties、标签、Wiki Links、Graph 配置和回收区管理。
- 增加长期记忆治理、版本化后台优化、模型用量账本、请求取消和受控外部连接器。
- 增加稳定 Release 检查、SQLite/Vault 更新前保护点和本地回滚。
- 完成纯净发布边界、双语文档、架构图和 GitHub 协作模板。

## English

### Unreleased

_None._

### 0.1.2 - 2026-07-29

- Changed AI requests to per-conversation FIFO queues with cross-conversation concurrency; cancellation now spans model, analysis, and follow-on execution, while newly created conversations remain independent.
- Upgraded long-term memory to four-track Memory V2 with exact five-dimensional scope, evidence and confidence, version replacement, expiry, tombstones, and user approval before reflection drafts become recallable.
- Added Chinese-friendly CJK character-pair search with title, path, tag, Wiki Link, and time signals while preserving openable links to canonical Obsidian notes.
- Added single-use execution tickets that reject argument substitution, concurrent duplicate submission, and replay; cross-Vault writes now use durable manifests, crash recovery, conflict protection, and directory synchronization.
- Added Yunspire's first-party controlled Deep Research Skill with plan, evidence, contradiction, synthesis, citation, and reflection stages plus budget, cancellation, checkpoint, provenance, and citation validation.
- Added macOS and Windows quality/release workflows for unsigned installers with version, source-tree, platform-resource, checksum, and artifact audits.

### 0.1.1 - 2026-07-26

- Fixed Assistant request processing blocking sends in other conversations, duplicate source copies in the Agent vault, search results not opening in Obsidian, and garbled knowledge-graph labels.
- Changed local file and folder intake to chunked upload and streamed staging, with no per-file or per-selection total size ceiling.
- Added persisted first-pass visual memory for conversation images; ordinary history reuses the record and only explicit references reload originals.
- Added a built-in Emoji avatar picker persisted with Assistant preferences.
- Added a versioned five-feature onboarding flow on the first installed launch.
- Upgraded Word, Excel, and PowerPoint to position-preserving OOXML v2 extraction covering supporting parts, all worksheets/formulas, image anchors, real slide order, element geometry, and layout/master provenance.
- Added a unified provenance and safety policy for embedded links; model text and multi-batch results are now byte-aware batches and hierarchical consolidation without silent truncation.

### 0.1.0 - 2026-07-21

- Established a local desktop architecture with Obsidian Vaults authoritative for knowledge and SQLite authoritative for runtime state.
- Implemented AI Assistant conversation, model providers, role-based routing, and single-use intent receipts.
- Implemented Vault discovery, search, read, write planning, atomic commit, file watching, and FTS.
- Implemented capture extraction, model analysis, quality gates, schedules, reports, and long-term-memory delivery.
- Added first-party TXT, Markdown, PDF, Word, PowerPoint, Excel, image, and media extraction; Excel is cleaned into structured JSON before model analysis.
- Added normalized cross-task and cross-source content deduplication with an explicit skipped-duplicate result.
- Implemented the Command Bus, Policy Engine, durable task state, checkpoints, and operation events.
- Implemented note, folder, Properties, tag, Wiki Link, Graph configuration, and Trash management.
- Added long-term-memory governance, versioned background optimization, model usage records, request cancellation, and controlled external connectors.
- Added stable Release checks, pre-update SQLite/Vault protection points, and local rollback.
- Added a clean release boundary, bilingual documentation, architecture image, and GitHub collaboration templates.
