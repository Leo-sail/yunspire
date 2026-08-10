# 更新记录 / Changelog

本文件只记录公开版本的实质变化，不记录本机验证数据或内部临时过程。

This file records material public-version changes only. It excludes local validation data and temporary internal work.

## 中文

### 0.4.2 - 2026-08-09

- 修复部分 OpenAI 兼容网关在内部转发 Responses 流时返回 HTTP 500、导致 AI助手直接失败的问题：云枢会识别 `responses stream error` / `response.failed`，不再重复已确认出现该错误的请求，并仅发送一次去除流式、结构化输出、采样和令牌限制参数的最小非流式兼容请求；普通瞬时服务端错误仍保留原有重试，模型响应受 2 MB 硬上限保护，同时拒绝失败事件前的残缺输出并脱敏错误中的 API 密钥。
- 修复 GitHub 创建草稿 Release 后列表接口短暂不可见导致发布中断的问题；预发布校验会在保持标签、提交、来源标记和唯一草稿约束的前提下进行有限等待重试。

### 0.4.0 - 2026-08-09

- 行为记录不再进入用户界面、长期记忆或 Obsidian；升级时会把旧行为记录移出 Vault，记忆页只展示已经确认并可召回的结构化内容。
- 便签改为直接本地保存，并增加一键整理：只处理尚未整理的灵感，按自然日合并写入一个 Obsidian Markdown 文件，已处理内容不会重复发送给 AI助手。
- 知识库默认按 Obsidian 文件夹浏览，输入搜索词后再展示匹配文件；“原生图谱”直接在云枢内读取 Markdown/Wiki Link 关系并渲染交互图谱，移除失效的外部打开按钮。
- 文章模板、排版主题和内容组件统一按分类浏览，主题与组件预览缩小约一半并重做密度、留白、边界和响应式布局。
- 工作台增加按时段变化的欢迎语、准确日期时间和“整理知识库”入口；继续工作、最近更新与知识库健康中的空白方形占位图标已移除。
- 303 个模型 Prompt 全部迁移为独立文件并纳入清单校验；5 个第一方 Skill 按专业工作流、边界、输入输出、失败恢复和参考资料重新定义。
- 清理 Storybook、测试源码、测试专用工作流、过时空操作和本机构建残留，统一生成不包含 Vault、数据库、密钥、日志、缓存、截图或机器路径的 macOS/Windows 纯净安装包。

### 0.3.0 - 2026-08-08

- 重构桌面端整体信息架构和视觉层级，统一总览、AI助手、采集、知识、创作、回望、操作日志与设置，并在常用桌面宽度下消除横向溢出。
- 建立版本绑定发布契约：应用版本、Git 标签、源码提交和源码树必须一致；macOS 与 Windows 安装包使用独立清单和 SHA-256，已有标签、Release 或安装包禁止覆盖。
- macOS DMG 改为 Apple Silicon/Intel 通用构建，Windows 保持 NSIS 安装方式并采用当前用户安装、禁止降级和静默 WebView2 引导；`0.3.0` 当前明确作为无签名版本发布，系统 Gatekeeper/SmartScreen 提示不属于应用可关闭的弹窗。
- 将 macOS PDF、媒体和语音辅助程序改为构建时编译并随应用打包，正式版本不再在用户设备上调用 `clang` 或触发命令行开发工具安装。
- 完成 Hermes 融合 P0 任务契约与调度闭环：原生版本化类型 DAG、确定性 `all_of` 完成契约、不可变计划/证据、frontier、原子 claim、lease/reclaim、硬预算、只读并行、副作用屏障、父子取消栅栏和 Rust 可信步骤回执。
- 完成 Hermes 融合 P1 能力编排主链路：AI助手使用 `capability-main -> verify-result` 计划，真实能力通过 `origin=runtime` 子命令执行；子命令绑定父任务/步骤/claim，不能复用模型凭证或扩大 capability、操作、Trace、Vault、路径、网络、声明范围和预算。
- 强化调度 occurrence：每个到期时间创建稳定 occurrence 与 wrapper 任务；只有核验真实 Command Bus/Policy 子任务绑定后才确认派发，历史 occurrence 不随日程删除。
- 完成 Hermes 融合 P1 反思与 Skill 记忆闭环：Skill 效果追加 `started/succeeded/failed/cancelled`，反思冻结效果快照并持久化 claim/lease/recovery；批准追加 `acceptance`，拒绝或重做追加 `correction`，候选、草稿和反馈按事务一致性更新。
- 增加 schema 37 任务步骤运行表、schema 38 反思与 Skill 效果表、schema 39 反思候选绑定；公共 IPC 拒绝 Renderer 伪造保留的 runtime/scheduler 证据。
- 增加 schema 40 任务恢复 replacement key 与唯一 replacement 任务绑定；自动恢复先封锁并取消旧父子任务和活动 claim，再创建和绑定新任务，同 key 重放幂等并拒绝绑定漂移。
- 增加 schema 41 不可变 Runtime 副作用重放账本；优化候选 create、evaluate、apply、rollback 和反思候选原子审批五类 handler 强制使用精确 Runtime 上下文，按 command、handler 与规范请求哈希返回原提交结果并拒绝参数替换，completion key 避免重放重复结算可信用量。
- 强化反思崩溃恢复：worker 先按反思 job 稳定 ID 复用原生候选，启动时从 `awaiting_review` job 与 `pending_review` candidate 恢复审阅草稿，不重新生成已持久化候选。
- 保留 Hermes 融合会话记忆边界：入队时冻结五维 MemoryScope，拒绝跨会话作用域或完成回执替换原请求作用域。
- 增加 schema 36 本地会话全文检索：SQLite FTS5 支持 ASCII/CJK 正文、严格工作区隔离、事务更新删除、旧版本回填和桌面端迟到查询失效。

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

### 0.4.2 - 2026-08-09

- Fixed Assistant failures when some OpenAI-compatible gateways returned HTTP 500 while forwarding a Responses stream. Yunspire now recognizes `responses stream error` / `response.failed`, does not repeat a request once that failure is identified, and sends one minimal non-streaming compatibility request without streaming, structured-output, sampling, or token-limit parameters. Ordinary transient server errors retain their existing retries, model responses have a hard 2 MB limit, partial output before a failure is rejected, and API keys are redacted from error details.
- Fixed release publication aborts caused by GitHub's draft Release list briefly lagging behind creation. Prepublication verification now applies a bounded visibility retry while preserving the tag, commit, provenance, and single-draft constraints.

### 0.4.0 - 2026-08-09

- Stopped exposing activity records in the UI, long-term memory, or Obsidian. Upgrades move legacy activity records outside the Vault, and memory surfaces show only confirmed recallable records.
- Changed sticky notes to save locally without Assistant processing and added one-click organization that handles only unresolved ideas and writes one daily Obsidian Markdown file without reprocessing completed items.
- Made the Knowledge page folder-first until a search is submitted. Native Graph now renders local Markdown and Wiki Link relationships interactively inside Yunspire, with the disabled external-open action removed.
- Unified article templates, typography themes, and content components under categorized browsing, while reducing theme/component previews by roughly half and refining density, spacing, borders, and responsive layout.
- Added a time-aware Workbench greeting with exact date/time and a Knowledge Maintenance entry, and removed blank square placeholders from Continue Working, Recent Updates, and Knowledge Health.
- Moved all 303 model prompts into standalone files with manifest validation and rewrote five first-party Skills with detailed workflows, boundaries, I/O contracts, failure recovery, and references.
- Removed Storybook, test source, test-only workflows, obsolete no-op code, and local build residue, producing clean macOS and Windows installers that exclude Vaults, databases, keys, logs, caches, screenshots, and machine-specific paths.

### 0.3.0 - 2026-08-08

- Reworked the desktop information architecture and visual hierarchy across Overview, Assistant, Capture, Knowledge, Creation, Reflection, Audit, and Settings, with overflow-free layouts at common desktop widths.
- Added a version-bound release contract: application version, Git tag, source commit, and source tree must agree; macOS and Windows installers carry separate manifests and SHA-256 digests, and existing tags, Releases, or assets are never overwritten.
- Changed macOS DMGs to universal Apple Silicon/Intel builds. Windows retains NSIS setup with current-user installation, downgrade prevention, and silent WebView2 bootstrap. Version `0.3.0` is explicitly published unsigned, so Gatekeeper and SmartScreen warnings remain outside application control.
- Moved macOS PDF, media, and speech helper compilation to build time and bundle the helpers with the app, so release builds no longer invoke `clang` or trigger Command Line Tools installation on user devices.
- Completed the Hermes-fusion P0 task-contract and scheduling loop: versioned typed DAGs, deterministic `all_of` completion contracts, immutable plans/evidence, frontier discovery, atomic claims, lease/reclaim, hard budgets, read-only fan-out, effectful barriers, parent-child cancellation fences, and Rust-generated trusted step receipts.
- Completed the Hermes-fusion P1 orchestration path: the Assistant uses a `capability-main -> verify-result` plan and executes real work through `origin=runtime` child commands bound to the parent task/step/claim. Children cannot reuse model authority or expand capability, operation, trace, Vault/path/network/declared scope, or budget.
- Strengthened native scheduling so every due timestamp creates a stable occurrence and wrapper task; dispatch completes only after Rust verifies real Command Bus/Policy child-task bindings, and schedule deletion retains occurrence history.
- Completed the Hermes-fusion P1 reflection and Skill-memory loop: Skill effects append `started/succeeded/failed/cancelled`, reflection freezes effect snapshots and supports durable claim/lease/recovery, approval appends `acceptance`, and rejection or revision appends `correction` transactionally.
- Added schema 37 task-step runtime tables, schema 38 reflection/Skill-effect tables, schema 39 reflection-candidate bindings, and public IPC rejection of forged runtime/scheduler evidence.
- Added schema 40 recovery replacement keys and unique replacement-task bindings; automatic recovery fences and cancels the old parent, children, and active claim before creating and binding a new task, makes identical-key replay idempotent, and rejects binding drift.
- Added the immutable schema 41 Runtime effect-mutation replay ledger. The five optimization handlers for candidate creation, evaluation, application, rollback, and atomic reflection approval now require exact Runtime context, return the original committed result by command, handler, and canonical request hash, reject parameter substitution, and use completion keys to avoid double-accounting trusted usage on replay.
- Strengthened reflection crash recovery so workers reuse native candidates identified by the stable reflection-job ID and startup reconstructs review drafts from `awaiting_review` jobs bound to `pending_review` candidates instead of regenerating persisted candidates.
- Retained the Hermes-fusion conversation-memory boundary: enqueue freezes the five-dimensional MemoryScope, and cross-session scope or completion-receipt substitution is rejected.
- Added schema 36 local conversation full-text search: SQLite FTS5 covers ASCII/CJK bodies with strict workspace isolation, transactional refresh/deletion, legacy backfill, and stale-query invalidation in the desktop UI.

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
