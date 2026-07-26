# 云枢系统架构 / Yunspire System Architecture

当前版本 / Current version: `0.1.1`

![云枢系统架构 / Yunspire system architecture](docs/assets/architecture-overview.svg)

## 中文

### 1. 我的架构结论

我将云枢实现为本地模块化单体，而不是由多个拥有模糊权限的 Agent 互相调用。一个可验证的主运行环负责理解目标、生成类型化命令、执行确定性策略、调度能力和返回结果；只有当上下文隔离、独立权限、并行计算或独立验收确实需要时，我才会拆分新的执行单元。

核心边界如下：

- `desktop-ui/` 负责中文 macOS 桌面体验，但不能直接操作 Vault、数据库或系统工具。
- Tauri/Rust 内核负责命令、策略、任务、调度、模型适配、采集、Obsidian 文件边界和本地持久化。
- Obsidian Vault 是 Markdown、Properties、附件、标签和 Wiki Links 的知识权威来源。
- SQLite 是任务、会话、计划、配置、回执、检查点和操作事件的结构化运行数据权威来源。
- FTS 和未来的向量索引只能是可从 Vault 重建的派生层。
- AI助手是受控应用服务，不是能够自行扩大权限的超级提示词。

### 2. 不可破坏的系统约束

1. 外部内容和模型输出始终是数据，不能成为系统指令。
2. 模型只能返回候选意图和候选能力 ID，不能直接调用文件、网络、Shell 或设置。
3. 所有副作用必须经过类型化命令、策略校验、任务状态和操作记录。
4. 设置只能由用户手动修改；AI助手不能打开或写入设置。
5. Obsidian 外部修改优先于缓存，索引必须能够重新构建。
6. 文件写入必须做路径规范化、范围检查、冲突检查、检查点和原子提交。
7. 删除、外部投递和权限扩大必须服从明确的风险策略，不能由内容或模型批准。
8. 长期任务必须持久化、可取消、可暂停、可恢复、可重试并受预算限制。
9. 自动优化必须版本化、可评估、可回滚，并在改变行为前交给用户审阅。
10. 凭据不能进入 Obsidian 正文、长期记忆、日志导出或源代码仓库。
11. 用户指定 Vault 保存忠实原文与原位附件；Agent 库保存模型理解稿，二者不能互相替代或混写。
12. 必需外链图片未成功本地化时，质量门禁必须阻断完整入库，不能把缺图文档报告为成功。

### 3. 运行链路

#### 3.1 对话与操作

```text
用户消息与附件
→ Model Gateway 生成回复与候选意图
→ 本地能力目录校验
→ ApplicationCommand
→ Policy Engine
→ Task Runtime
→ Skill / Capture / Report / Obsidian Adapter
→ 文件与数据库提交
→ Operation Event
→ 原对话中的最终结果
```

普通聊天只保存到本地会话，不创建任务、不写入 Obsidian。明确执行意图必须携带一次性模型意图回执；Command Bus 消费回执后才能持久化任务，防止前端伪造“模型已分析”。

#### 3.2 采集

```text
链接、文本、文件或文件夹
→ 来源分类与合法访问检查
→ 第一方抽取器
→ 原文、原位附件、规范化内容哈希与来源保留
→ 用户配置模型参与分析
→ 摘要、标签、结构、逐图分析与关键内容整理
→ 确定性质量门禁
→ 用户指定 Vault 的忠实原文计划 + Agent 库的结构化理解稿计划
→ 跨 Vault 路径、附件占位与回执校验
→ 批量原子提交
→ 索引、任务、日志与对话同步
```

内容里的命令式文字不会进入系统提示词，也不能改变 Skill、域名、目标路径或预算。无法可靠抽取、模型分析失败或质量门禁不通过时，任务必须显示真实失败或待处理状态，不能伪造成功。

本地文件输入使用 `begin_capture_upload → append_capture_upload_chunk → finish_capture_upload` 分块协议。每个 IPC 分块有确定边界，但文件本身和一次选择的文件总量不设产品级大小上限；Rust 将分块流式落入隔离临时区，完成字节数校验后再流式复制、计算哈希并交给第一方抽取器。模型正文与视觉输入继续按供应商上下文分批，避免把“不限文件大小”错误实现为单次无限内存请求。外链图片本地化和本机图片派生按实时磁盘、可用内存、解码和单次请求资源门禁运行；这些是动态安全条件，不是固定文件大小上限。

Office 抽取统一输出 v2 结构。Word 的正文、表格、页眉页脚、脚注尾注、批注和分节各自保留 story/part 来源；图片保存字符偏移、前后文和表格单元格位置。Excel 先保留全部工作表、坐标单元格、共享/普通公式、缓存值和 Drawing 锚点，再建立清洗视图；处理器不重新计算公式。PowerPoint 按 `sldIdLst` 真实顺序保留元素边界、层级、裁剪、表格、幻灯片/版式/母版来源，并把空间关系明确标记为非语义事实候选。每个解析器同时输出 `integrity.status/errors/checks`；任一必需 story、工作表、幻灯片、图片关系、Drawing 或位置证据不完整，文件级错误会进入采集质量门禁并阻断双库写入。完整结构 JSON 与附件一同进入写入计划。

网页正文图片、Markdown 图片语法和 OOXML 图片关系是媒体依赖，不按普通链接处理。第一方本地化器只接收这些确定图片位置中的公开 `http/https` 目标，逐跳解析 DNS 并拒绝回环、私网、链路本地和其他非公网地址，限制重定向，校验响应 MIME 与真实图片签名，再流式写入隔离目录、计算 SHA-256 并生成 `asset_id`。相同图片按字节哈希去重，但网页或 Markdown 的每次出现、Word 字符位置、Excel 锚点和 PowerPoint 元素都保留独立 `reference_id`；文件夹导入会先按来源文件稳定命名空间化位置 ID，再全局去重并只物化一份附件。`attachment://<reference_id>` 本地附件占位在原位置进入忠实 Markdown，Obsidian Adapter 再映射到真实路径。下载、重定向、类型、哈希、暂存或原位映射失败会留下结构化原因，并阻断双库入库。

普通网页或文件内链接始终是 `untrusted_data`。抽取器保存显示文字、关系目标和段落/单元格/幻灯片来源，但设置 `auto_open=false`、`auto_fetch=false`；只有用户明确提出“采集文件内链接”时，AI助手才把选定目标转换为新的类型化采集命令。确定图片位置的窄范围本地化不会改变这条普通链接策略。

模型分析必须覆盖全部本地化图片。相同字节只按内容级 `asset_id` 送模型一次，`image_observations` 保存观察、画面文字、上下文、证据和置信度；确定性写入层再根据附件元数据把观察展开到每个位置级 `reference_id`。原件永远不因模型分析而改写；必要时在隔离区生成临时 JPEG 派生图，并以 `asset_id`、原件/派生 SHA-256、原件/派生字节数和允许的 `reference_id` 形成结构化 binding。原生侧校验派生实际字节与哈希，模型返回的未知位置标识会被拒绝或规范化；前端只在一个模型批次内保留派生图，提交后释放。用户指定 Vault（未指定时为个人库）的 `资料库/原文/` 保存未被模型重写的忠实 Markdown、原位附件与来源证据；Agent 库的 `资料库/原文/` 保存模型形成的结构化理解稿、原位逐图分析、标签、Wiki Links 与相关笔记。两份文件与附件使用“完整内容 SHA-256 目录 + 可读标题文件名”的稳定目标路径，使 Obsidian Graph 显示可读节点名称；它们复用同一模型分析回执并作为同一批次提交，同标题不同内容不会碰撞，采集批次不允许覆盖已有目标。实体名称只用于候选笔记匹配，当前不建立实体图谱、向量索引或混合检索。

对话图片走独立的视觉记忆链：首次图片只发送给分析模型一次，生成摘要、OCR/画面文字、标签、实体、关键点、模型 ID 和时间并随消息持久化。普通历史上下文只携带这份分析记录，不包含 Data URL。引用解析器只有在用户明确使用文件名、图片序号或多图指代表达时，才从当前会话原图句柄重新生成视觉输入并更新进一步分析记录。

采集账本以规范化抽取内容哈希做跨任务、跨来源去重，同时独立保存来源类型和来源引用。已提交、写入中或已通过质量门禁的相同内容不会再次写入；系统记录原记录 ID 和“已跳过重复”结果，而不是误报模型或解析失败。

#### 3.3 定时任务与报告

用户通过 AI助手表达周期、时区、来源、目标和失败策略。模型负责理解自然语言，本地调度器负责保存确定的计划、领取到期任务、控制重叠、补跑、重试和幂等。日报、周报、月报和年报先保存到 Obsidian；没有配置外部连接时只进入本地待投递状态。

### 4. 模块职责

#### 4.1 体验层

- AI助手：对话、附件、命令候选、任务进度和最终结果。
- 仪表盘：Vault、任务、采集、知识变化和健康摘要。
- 采集：定时采集定义、历史、步骤和结果，只允许 AI助手创建或修改。
- 搜索：当前 Vault 与显式跨库搜索、来源片段和只读笔记查看。
- 创作：Markdown、属性、标签、Wiki Links、引用和附件编辑。
- 技能：只显示用户创建的 Skill；系统 Skill 后台注册。
- 任务、报告、操作日志：运行状态、周期成果和审计证据。
- 设置：用户手动控制 Vault、模型、权限、自动化和外观。

#### 4.2 Command Bus

`ApplicationCommand` 至少包含：

- 命令 ID、来源和类型。
- 自然语言意图与注册能力 ID。
- 操作、参数、目标 Vault 和相对路径。
- 网络目标和声明范围。
- 步骤、运行时间、工具、Token 和费用预算。
- 幂等键、追踪 ID 和模型意图回执。

Command Bus 不执行任意模型文本，只接收通过反序列化和枚举校验的结构化字段。

#### 4.3 Policy Engine

策略返回 `allow`、`deny`、`require_approval` 或 `allow_with_reduced_scope`。当前确定性检查覆盖：

- 标识符、负载大小和参数结构。
- 相对路径穿越、绝对路径和空路径。
- 允许的 HTTPS/本地网络目标。
- Vault 写入范围、操作类别和预算上限。
- 外链图片本地化的确定位置、公网地址、逐跳重定向和内容类型边界。
- AI助手设置写入禁令。
- 删除、外部投递与其他高风险副作用。

模型永远不能产生最终授权决定。

#### 4.4 Task Runtime

任务状态为：

```text
created → queued → running → succeeded
                     ↘ paused → queued
                     ↘ awaiting_approval → queued/running
                     ↘ failed → queued
created/queued/running/paused/failed → cancelled
```

运行时保存进度、当前步骤、预算、检查点、错误、重试次数和 trace ID。终态不能被重新启动；重试必须生成合法状态迁移，并保持幂等。

#### 4.5 Model Gateway

模型配置支持多个供应商和多个用途选择。网关兼容常见 JSON、文本和 SSE 响应布局，但只把结果作为候选数据。每次意图分析生成短期一次性回执；图像请求必须路由到用户指定的图片模型。API 密钥使用设备本地密钥加密后存入 SQLite，不使用账户系统或 macOS 钥匙串。

每次模型请求记录供应商、模型、用途、状态、Token、耗时、成本来源和错误，但不记录 API 密钥。取消操作使用请求 ID 触发本地取消标记；对话、分析、图片生成和图片编辑使用各自端点，不能串用。

#### 4.6 Skill 与采集能力

系统 Skill 必须带第一方来源声明、清晰输入输出、能力范围和可验证脚本。用户创建的 Skill 单独展示和管理。处理程序可以使用语言标准库与 macOS 系统框架，但不能复制或捆绑第三方采集器、解析器、下载器或开源语音模型。

#### 4.7 Obsidian Adapter

Adapter 负责：

- 从 Obsidian 配置和用户明确路径发现 Vault。
- 创建默认 `Agent 库` 与 `个人库`。
- 将忠实原文与原位附件写入用户指定 Vault（默认个人库）。
- 将结构化理解稿、逐图分析和 Obsidian 原生知识关联写入 Agent 库。
- 读取、创建、更新、移动、重命名、软删除和恢复。
- 管理 Properties、标签、Wiki Links、附件、文件夹和 Graph 配置。
- 生成 diff、检查冲突、创建检查点并原子写入。
- 监听外部文件变化并增量重建索引。
- 将长期记忆事件追加到 Agent 库，并保存投递回执。

删除笔记、文件夹或 Vault 时先生成删除计划并要求用户确认，确认后移动到云枢回收区。永久物理删除必须由明确的用户操作触发。

#### 4.8 外部连接器与更新保护

外部连接器只能由用户在设置中创建，地址必须是规范化 HTTPS 目标，凭据使用本地设备密钥加密。AI助手可以在模型确认真实正文和连接器类型后发起候选投递，但 Policy Engine 仍将外部发送视为高风险副作用并要求用户确认。

更新模块只查询项目 GitHub 仓库的稳定 Release。安装前保护点由 SQLite 在线备份和所有已连接 Vault 的文件快照组成；符号链接不会跟随。回滚前再次建立当前状态安全保护点，然后只覆盖旧快照中存在的文件，不删除更新后新增文件。`0.1.1` 不包含自动下载安装、Apple 签名或公证。

### 5. 数据模型

#### 5.1 Obsidian 文档层

保存知识正文、Properties、标签、附件、Wiki Links、来源、报告和人类可读 Skill/模板。外部内容保留来源 URL 或本地来源引用、采集时间、内容哈希和处理状态。用户指定 Vault 保存来源忠实层；Agent 库保存模型理解层。图片资产使用内容级 `asset_id` 去重，图片出现位置使用 `reference_id` 保真，理解层的逐图分析必须同时引用二者。

#### 5.2 SQLite 运行层

保存工作区快照、模型供应商、会话、消息、任务、任务步骤、计划、采集记录、分析回执、受管资源、操作事件、FTS、长期记忆投递状态和迁移版本。SQLite 使用事务、WAL、完整性检查和备份。

#### 5.3 长期记忆

长期记忆记录用户在云枢中的必要行为和对话事件，采用追加式事件结构。写入分为暂存、Vault 原子写入、数据库提交三个阶段；崩溃后可重放未完成事件。密钥、Cookie、令牌、完整二进制附件和无关系统隐私不得进入长期记忆。

长期记忆支持查询、指标、纠正、过期、替代和墓碑治理；历史事件保持追加式，不通过静默覆盖改写。后台优化使用不可变证据游标读取任务、纠正、回滚和知识健康信号，候选必须通过权限扩张检查和确定性评估，由用户审阅后才能成为新版本。

### 6. 可靠性与安全

- 原子写入：同目录临时文件、刷新、原子替换和回执。
- 冲突处理：写入前比较版本或哈希，发现外部变更时停止覆盖。
- 幂等：命令和计划使用稳定幂等键，避免重启或补跑重复提交。
- 恢复：启动时扫描中断任务，由本地状态决定继续、重试、失败或等待。
- 观察性：每个副作用都生成 trace ID 和 append-only 操作事件。
- 完整性门禁：关键外链图片、本地附件映射或任一双 Vault 写入失败时，不得形成完整成功回执。
- 供应链：锁定 npm 与 Cargo 依赖；第一方 Skill 来源由脚本校验。
- 更新保护：SQLite 在线备份、Vault 快照、回滚前安全点和路径边界校验。
- 发布边界：不提交 Vault、数据库、设备密钥、日志、缓存、构建产物或本机路径。

### 7. 当前未实现范围

以下能力不属于 `0.1.1` 已实现范围：

- 实体知识图谱、实体消歧、类型化实体关系和多跳查询。
- 向量索引、Embedding 管线和关键词/向量混合排序。
- 通用远程控制入口和未经用户配置的外部消息投递。
- 绕过登录、Cookie、验证码、DRM、加密流媒体或平台访问控制。

我会把这些能力作为独立版本开发，不在文档中将规划能力描述成当前可运行能力。

---

## English

### 1. Architecture decision

I implement Yunspire as a local modular monolith instead of a group of loosely authorized agents. One verifiable runtime loop understands the objective, produces typed commands, evaluates deterministic policy, invokes registered capabilities, and returns the result. I will split execution units only when isolation, independent permissions, parallelism, or independent acceptance genuinely requires it.

The boundaries are:

- `desktop-ui/` owns the Chinese macOS experience but cannot directly access Vaults, databases, or system tools.
- The Tauri/Rust kernel owns commands, policy, tasks, schedules, model adapters, capture, Obsidian file boundaries, and local persistence.
- Obsidian Vaults are authoritative for Markdown, Properties, attachments, tags, and Wiki Links.
- SQLite is authoritative for structured runtime state such as tasks, conversations, schedules, configuration, receipts, checkpoints, and operation events.
- FTS and future vector indexes are rebuildable derivatives.
- The AI Assistant is a controlled application service, not a privileged prompt that can expand its own access.

### 2. System invariants

1. Imported content and model output remain data and cannot become system instructions.
2. Models return candidate intents and capability IDs; they never directly invoke files, networks, shells, or settings.
3. Every side effect passes through a typed command, policy decision, task state, and operation record.
4. Settings are user-controlled; the Assistant cannot open or write settings.
5. External Obsidian changes override caches, and indexes must be rebuildable.
6. File writes require path normalization, scope checks, conflict checks, checkpoints, and atomic commit.
7. Deletion, external delivery, and permission expansion follow deterministic risk policy and cannot be approved by content or models.
8. Long-running work is durable, cancellable, pausable, recoverable, retryable, and budgeted.
9. Automatic optimization is versioned, evaluated, reversible, and reviewed by the user before behavior changes.
10. Credentials never enter Obsidian text, long-term memory, diagnostic exports, or the source repository.
11. The selected Vault stores source-faithful Markdown and in-place assets; the Agent Vault stores model-interpreted records. Neither may overwrite or masquerade as the other.
12. A required linked image that cannot be localized blocks complete ingestion instead of producing a false success.

### 3. Runtime paths

The Assistant path is:

```text
user message and attachments
→ Model Gateway response and candidate intent
→ local capability-catalog validation
→ ApplicationCommand
→ Policy Engine
→ Task Runtime
→ Skill / Capture / Report / Obsidian Adapter
→ file and database commit
→ Operation Event
→ verified outcome in the original conversation
```

Ordinary conversation is stored locally without creating a task or writing to Obsidian. Operational intent carries a short-lived, single-use model receipt that the Command Bus consumes before persisting a task.

Capture follows:

```text
link, text, file, or folder
→ source classification and lawful-access check
→ first-party extractor
→ original content, in-place assets, normalized content hash, and provenance
→ configured model analysis
→ summary, tags, structure, per-image observations, and valuable content
→ deterministic quality gate
→ source-faithful selected-Vault plan + interpreted Agent-Vault plan
→ cross-Vault path, attachment-token, and receipt validation
→ atomic batch commit
→ synchronized index, task, operation log, and conversation
```

Command-like content cannot alter prompts, Skills, domains, target paths, or budgets. Extraction, analysis, or quality failures remain visible failures or pending states.

Local file intake uses a `begin_capture_upload → append_capture_upload_chunk → finish_capture_upload` protocol. Each IPC chunk is bounded, but the product imposes no per-file or per-selection total size ceiling. Rust streams chunks into an isolated temporary area, validates the completed byte count, then streams copy and hashing into the first-party extractor. Text and visual model inputs remain provider-sized batches; unlimited file intake never becomes an unbounded in-memory model request. Linked-image localization and local analysis derivatives use live disk, available-memory, decode, and request gates; these are dynamic runtime safeguards rather than fixed product file-size limits.

Office extraction uses position-preserving v2 structures. Word retains ordered body blocks, tables, headers, footers, footnotes, endnotes, comments, sections, positioned images, and field/relationship links. Excel retains every worksheet, coordinate cell, ordinary/shared formula, cached-not-recalculated value, cleaned view, and drawing anchor. PowerPoint follows real `sldIdLst` order and retains element bounds, z-order, crop data, tables, and slide/layout/master provenance; spatial relationships remain non-semantic candidates. Each parser emits `integrity.status/errors/checks`; an incomplete required story, worksheet, slide, image relationship, Drawing, or placement becomes a file-level blocking error. Full structure JSON and deduplicated attachments join the same write plan.

Webpage body images, Markdown image syntax, and OOXML image relationships are media dependencies rather than ordinary links. The first-party localizer accepts only public `http/https` targets from those deterministic image positions. It resolves DNS at every hop, rejects loopback, private, link-local, and other non-public destinations, limits redirects, verifies response MIME and actual image signatures, streams into isolation, hashes with SHA-256, and emits a stable `asset_id`. Identical bytes deduplicate while every web or Markdown occurrence, Word text position, Excel anchor, and PowerPoint element retains an occurrence-level `reference_id`. Faithful Markdown keeps an `attachment://<reference_id>` placeholder at that exact position; the Obsidian Adapter resolves it to the deduplicated asset and final path. Download, redirect, type, hash, staging, or placement failures retain evidence and block both Vault writes.

Ordinary embedded links remain `untrusted_data` with `auto_open=false` and `auto_fetch=false`. Their visible text, target, and paragraph/cell/slide provenance are preserved, and only an explicit request creates a new typed capture command. Deterministic image localization does not broaden this ordinary-link policy.

Every localized image enters configured-model analysis once per unique `asset_id`. Its observation, visible text, context, evidence, and confidence are reused at every occurrence-level `reference_id` by the deterministic writer. Original bytes are never rewritten for analysis. When needed, a temporary JPEG derivative is created in isolation and structurally bound by asset ID, original/derivative SHA-256, original/derivative byte lengths, and allowed reference IDs. Native code validates the transmitted bytes and normalizes or rejects model-invented placement IDs; the UI retains derivatives for one bounded request batch and then releases them. `资料库/原文/` in the selected Vault, or Personal Vault by default, receives source-faithful Markdown, in-place assets, and provenance. The same path in the Agent Vault receives model-interpreted Markdown with in-place image analysis, tags, Wiki Links, and related notes. Both plans and their assets use stable targets with the full normalized content SHA-256 as a directory and the readable title as the basename, so Obsidian Graph shows readable node names; they share one analysis receipt and commit as one batch. Equal titles with different content cannot collide, and a capture batch cannot overwrite an existing target. Entity names may suggest existing notes, but entity graphs, vector indexes, and hybrid retrieval remain disabled.

Conversation images use a separate visual-memory path. A new image is sent to the analysis model once and persisted with summary, visible text, tags, entities, key points, model ID, and timestamp. Ordinary history carries this record without a Data URL. The original is reloaded only when the user explicitly references a filename, image ordinal, or a specific group such as the previous two images.

The capture ledger deduplicates normalized extracted content across tasks and sources while preserving source type and reference separately. Matching content already ready, writing, or committed is recorded as an explicit skipped duplicate with the original record ID.

### 4. Components

The Experience layer provides Assistant, Dashboard, Capture, Search, Create, Skills, Tasks, Reports, Operation Log, and user-controlled Settings. The Command Bus accepts typed `ApplicationCommand` records containing identity, origin, intent, capability, operation, parameters, Vault, relative paths, network targets, declared scope, budgets, idempotency, trace ID, and model receipt.

The Policy Engine returns `allow`, `deny`, `require_approval`, or `allow_with_reduced_scope`. It validates payload size, identifiers, path traversal, network targets, Vault scope, operation category, budgets, settings restrictions, and high-risk side effects. The model never makes the final authorization decision.

The Task Runtime persists the state machine, progress, step, budget, checkpoint, error, retry count, and trace ID. Terminal states cannot restart, and retries must preserve idempotency.

The Model Gateway supports multiple providers and role assignments. It accepts common JSON, text, and SSE layouts but treats every result as candidate data. Intent analysis generates a short-lived receipt; image requests route only to a user-selected image model. API keys are encrypted with a device-local key in SQLite. Model usage records include provider, model, role, state, tokens, duration, cost source, and error without storing credentials. Chat, analysis, image generation, and image editing keep separate endpoints and support request-ID cancellation.

System Skills carry first-party provenance, explicit input/output, scoped capabilities, and verifiable scripts. User Skills are displayed and managed separately. Processors may use language standard libraries and macOS frameworks but do not copy or bundle third-party collectors, parsers, downloaders, or open-source speech models.

The Obsidian Adapter discovers Vaults, initializes the default Agent and Personal Vaults, writes source-faithful records and in-place assets to the selected Vault, writes interpreted records and native Obsidian links to the Agent Vault, manages notes/folders/properties/tags/links/attachments/Graph configuration, creates diffs and checkpoints, commits batches atomically, watches external changes, rebuilds indexes, and appends long-term-memory events. Deletion first creates a plan and moves confirmed targets to Yunspire Trash; permanent physical deletion requires an explicit user action.

User-created external connectors accept normalized HTTPS endpoints and device-key-encrypted credentials. Model-analyzed delivery still passes deterministic policy and user confirmation. The updater checks only stable project Releases, creates online SQLite and connected-Vault snapshots, skips symlinks, creates a safety point before rollback, and never deletes files added after a snapshot. Version `0.1.1` does not claim automatic download, signing, or notarization.

### 5. Data and reliability

Obsidian stores human-readable knowledge, sources, reports, and reusable artifacts. The selected Vault is the source-faithful layer; the Agent Vault is the model-interpreted layer. Content-level `asset_id` values deduplicate image bytes, occurrence-level `reference_id` values preserve placement, and Agent image observations bind both. SQLite stores workspace snapshots, providers, conversations, tasks, steps, schedules, capture records, receipts, managed resources, operation events, FTS, memory-delivery state, and migrations.

Long-term memory is an append-only event stream for necessary user activity and conversation history. Delivery is staged, atomically written to the Agent Vault, then committed in SQLite so interrupted writes can be replayed. Query, metrics, correction, expiry, replacement, and tombstone governance preserve history without silent overwrite. Background optimization reads immutable evidence cursors, rejects permission expansion, and applies only evaluated, user-reviewed, reversible versions. Secrets, tokens, full binary attachments, and unrelated system privacy are excluded.

Reliability controls include same-directory temporary files, flush and atomic replacement, version/hash conflict checks, stable idempotency keys, startup recovery, trace IDs, append-only operation events, linked-image completeness gates, cross-Vault batch integrity, locked dependencies, first-party Skill provenance checks, and a release boundary that excludes local data and generated output.

### 6. Deferred scope

Version `0.1.1` does not claim entity graphs, entity disambiguation, typed entity relations, multi-hop queries, vector indexes, embedding pipelines, hybrid ranking, generic remote control, unconfigured external delivery, or bypasses for login, CAPTCHA, DRM, encrypted media, or platform access controls. I will develop these as explicit future versions rather than describe planned behavior as current capability.
