# 云枢产品需求 / Yunspire Product Requirements

当前版本 / Current version: `0.1.1`

## 中文

### 1. 产品目标

我将云枢定位为中文、本地优先、以 Obsidian 为知识数据库的 macOS 与 Windows 桌面 Agent。用户通过 AI助手完成知识采集、整理、查询、创作、复盘、报告、Skill 调用和知识库维护，同时始终拥有自己的 Markdown、附件和 Vault。

我不在当前版本提供账户注册、登录或云端身份隔离。应用直接进入本机工作区；模型配置、任务和界面状态保存在本地数据库，API 密钥使用设备本地密钥加密。

### 2. 产品原则

- Obsidian Vault 是文档知识权威来源，SQLite 是结构化运行状态权威来源。
- 用户内容、网页、文件、图片、音视频和模型输出全部作为不可信数据处理。
- 普通聊天不写入 Obsidian；只有明确执行意图才创建本地任务。
- AI助手可以操作除设置外的产品能力，但不能直接获得文件或工具权限。
- 本地写入默认自动运行，仍必须完成范围校验、检查点、原子提交和操作日志。
- 设置只能由用户手动进入和修改。
- 任何状态都必须来自真实运行结果，不能预填演示任务或伪造成功。
- 用户指定 Vault 的忠实原文和 Agent 库的模型理解稿是两个关联交付物，不能用摘要替代原文，也不能把模型改写内容伪装成原文。

### 3. 信息架构

左侧导航固定为：

1. AI助手
2. 仪表盘
3. 采集
4. 搜索
5. 创作
6. 技能
7. 任务
8. 报告
9. 操作日志
10. 设置

左侧可以折叠，右侧工作区直接使用可用区域。每个页面在内容超过视口时必须独立纵向滚动，不能裁切内容，也不能因任务抽屉打开而改变主页面宽度。

不提供独立知识图谱页面。云枢通过笔记、标签、Wiki Links、嵌入和 Obsidian Graph 配置改善 Obsidian 原生图谱。

### 4. AI助手

AI助手是默认入口。它必须：

- 调用用户配置的真实模型理解自然语言，并提供正常对话能力。
- 支持拖入文件和图片，并把附件与用户文字作为同一次请求处理。
- 用户文件不设置单文件或单次选择总大小上限；桌面端必须分块传输和流式落盘，模型输入再按上下文能力分批。
- 图片第一次进入对话时必须由分析模型生成可持久化的摘要、画面文字、标签、实体、关键点、模型和时间记录。历史上下文默认只发送这份记录，不能重复发送原图。
- 用户通过文件名、序号或“刚才两张图片”等明确表达指定历史图片时，才允许重新读取对应原图进行进一步分析；当前窗口没有原图句柄时必须明确要求用户重新添加。
- 支持用户从内置 Emoji 表情包更换助手头像，名称、头像、语言和风格随本地工作区持久化。
- 为每个对话提供可编辑名称，并在侧栏显示名称。
- 输入 `/` 时向上展开真实命令候选；完整命令可直接执行。
- 支持 `/clear` 清空后续请求携带的对话上下文，不删除历史记录或知识。
- 在模型上下文接近配置上限前压缩，不按固定消息数量截断。
- 根据用户选定用途自动路由对话、分析和图片模型。
- 识别明确系统操作意图后，在原对话中持续运行到完成，不自动跳转设置或其他页面。
- 只向用户展示必要进度、结果、失败原因和下一步选择。
- 对可选择的下一步提供结构化选项，用户选择后恢复原任务。
- 用规范化富文本渲染标题、段落、列表、加粗、代码、链接和表格。

AI助手不得打开或修改设置，不得把用户内容提升为系统指令，不得绕过 Command Bus、Policy Engine、Task Runtime 或操作日志。

首次安装启动必须先显示一次统一工作授权，覆盖本地文件与媒体处理、Obsidian Vault 读写、已配置模型连接和用户主动发起的公开链接采集。授权决定必须保存在本机 SQLite；同一应用数据后续启动不得重复询问。未授权时不得扫描 Vault、连接模型或启动后台任务，只开放设置中的权限管理。该应用层授权不能替代或绕过 macOS、Windows 的系统级权限提示。

授权后显示版本化的 5 步引导，依次介绍 AI助手统一入口、Obsidian 本地知识数据库、多格式模型分析、定时采集与后台任务、本地优先与可恢复边界。完成或跳过后进入助手个性化设置；同一引导版本完成后不重复弹出。

### 5. Obsidian 与本地知识

云枢必须发现本机 Obsidian Vault，并支持用户选择当前查询范围。没有用户指定库时，首次启动初始化：

- `Agent 库`：系统 Skill、计划、报告、优化记录、追加式长期记忆，以及 `资料库/原文/` 下的结构化理解稿、逐图分析和知识关联。
- `个人库`：用户笔记、项目、资料、创作和归档内容；当用户没有指定其他目标库时，在 `资料库/原文/` 保存忠实原文，在 `资料库/附件/采集/` 保存原位附件。

Obsidian 管理能力包括读取、创建、更新、移动、重命名、Properties、标签、Wiki Links、附件、文件夹、Graph 配置、软删除和恢复。写入必须检查版本冲突并使用原子替换。

用户要求删除笔记、文件夹或 Vault 时，系统生成准确目标和影响，用户确认后移动到云枢回收区。永久物理删除必须由用户明确触发。

### 6. 采集

采集页只展示真实定时采集、运行步骤、历史和结果。创建、修改、暂停、恢复、立即运行、重试或删除采集任务必须通过 AI助手理解用户自然语言后完成，页面不提供绕过模型意图分析的手工创建器。

支持的输入范围：

- 公开网页、博客、公开社交分享页面和用户有权访问的来源。
- 文本、Markdown、PDF、Word、PowerPoint、Excel、图片、音频和视频。
- 用户选择的本地文件与文件夹。

处理要求：

1. 识别来源类型并保留原始来源。
2. 使用云枢第一方处理器抽取正文、结构、图片、音频和有价值的视频画面。
3. Word 必须保持正文段落、表格、页眉页脚、脚注尾注、批注、图片和链接的来源与位置关系；不能把图片统一移动到文末后再猜测上下文。
4. Excel 必须读取全部工作表及隐藏状态，先保留坐标单元格、公式、缓存值和图片锚点，再生成清洗 JSON；公式未由计算引擎重算时必须明确标记，不能把缓存值声称为实时计算结果。
5. PowerPoint 必须按真实页序保留文字、图片、表格、元素边界、层级、裁剪和版式/母版来源；空间近邻只能作为待模型验证的关系候选。
6. 网页正文图片、Markdown 图片语法或 OOXML 图片关系指向外部资源时，必须执行受控本地化：只接受公开 `http/https` 图片，逐跳校验 DNS 和重定向，拒绝私网、回环与链路本地地址，校验 MIME 和真实图片格式，流式暂存并计算 SHA-256。成功后用 `attachment://<reference_id>` 在原段落、Markdown 行列、单元格锚点或幻灯片元素位置回填，再由附件元数据映射到去重后的 `asset_id` 和真实路径。
7. 普通文件内网址必须保留显示文字、目标与所在段落、单元格或幻灯片位置，设置 `auto_open=false`、`auto_fetch=false`；只有用户明确要求采集时才建立独立任务。外链图片的专用本地化不能扩展为普通链接自动访问。
8. 内嵌和本地化图片按内容 `asset_id` 去重，每个出现位置保留独立 `reference_id`。每个唯一 `asset_id` 必须送入用户配置的分析模型一次并保存观察、画面文字、上下文、证据和置信度；确定性写入层再把逐图结果放回全部 `reference_id` 位置。原件不得因模型分析被压缩、转码或覆盖；仅在单次模型边界需要时生成隔离派生图，并以 `asset_id`、原图/派生 SHA-256、字节数和允许位置 ID 绑定，模型不能发明位置 ID。
9. 所有正文、图片、音频、视频和文件分析必须有用户配置模型参与。
10. 生成摘要、标签、主题、来源引用、Wiki Links、相关笔记和目标 Vault 建议；实体名称只作为笔记关联候选，不创建实体图谱。
11. 通过确定性质量门禁、路径和策略校验。必需外链图片下载、重定向、格式校验、哈希、暂存或原位映射失败时，必须显示具体原因并阻断完整入库，不能静默缺图或报告成功。Word 的必需 story/图片关系、Excel 的任一声明工作表或 Drawing、PowerPoint 的任一声明幻灯片或图片关系不能完整解析时，同样必须报告带位置证据的错误并阻断入库。
12. 用户指定 Vault（未指定时为个人库）保存忠实原文 Markdown、原位附件、完整结构 JSON 和来源证据；Agent 库 `资料库/原文/` 保存模型理解后的结构化 Markdown、逐图分析、标签、Wiki Links 和相关笔记。
13. 两份写入计划必须复用同一模型分析回执并作为同一跨 Vault 批次提交；任一目标失败不得把另一目标标记为完整成功。提交后同步索引、任务、对话和操作日志。
14. 双库 Markdown 与附件必须采用“完整规范化内容 SHA-256 目录 + 可读标题文件名”的稳定目标路径，使 Obsidian Graph 显示可读节点名称；同标题不同内容不得碰撞，采集批次不得覆盖任何已有目标。
15. 对规范化抽取内容生成持久哈希，跨任务和跨来源跳过已经通过门禁、写入中或已提交的重复内容，并显示原记录与跳过原因。

关键帧不设置固定数量上限；处理器按内容变化和信息价值提取，并受任务时间、存储和模型预算约束。

macOS 的 PDF、视频和语音链分别使用 PDFKit、AVFoundation/ImageIO 与 Speech；Windows 分别使用 Windows.Data.Pdf、Media Foundation/WIC 与本地 SAPI。Windows 安装包必须携带经过固定哈希校验的官方嵌入式 Python 运行时，不能要求最终用户另装 Python。Windows 缺少与所选 locale 精确匹配的离线 SAPI 语言包时，必须返回 `windows_sapi_language_unavailable`，不得回退默认错误语言或伪报转写成功。

云枢不得规避登录、Cookie、验证码、DRM、加密媒体或平台访问控制。需要授权时引导用户完成官方或合法流程，再处理用户有权访问或主动导出的内容。

### 7. 模型配置

- 支持增加和删除供应商。
- 同一 URL 与密钥可以配置多个模型；不同模型也可以来自不同供应商。
- 获取模型列表后，只在主要界面展示用户最终选定的用途模型。
- 用途至少包括对话、内容分析和图片生成。
- 图片意图必须调用图片模型，文本对话必须调用对话模型。
- 供应商错误、模型不存在、端点不兼容或返回为空时显示真实原因。
- 对话、分析、图片生成和图片编辑必须使用各自供应商的正确端点；每次请求记录模型、用途、Token、耗时、成本来源与错误，并支持取消长请求。
- API 配置只保存在本机，不使用注册登录系统或系统钥匙串。

### 8. 搜索与创作

搜索支持当前 Vault、显式跨 Vault、文件名、正文、Properties、标签、链接、来源、修改时间和内容类型。结果展示来源路径与匹配片段，并支持在云枢内只读查看或明确跳转 Obsidian。

创作支持 Markdown、Properties、标签、Wiki Links、来源引用和任意位置图片。保存前用户选择目标 Vault 与目录，系统执行冲突检查、检查点和原子写入。下拉框必须来自真实数据或明确的固定枚举，不能是无行为装饰。

### 9. Skills

系统内置 Skill 和处理程序在后台运行，不在技能页展示。技能页只展示用户创建的 Skill，支持创建、编辑、校验、试运行、启用、停用和删除。

每个 Skill 声明标识、版本、输入输出、模型用途、Vault 范围、网络目标、超时、重试、幂等、副作用和来源。第一方 Skill 与处理器必须由我独立设计实现，并通过来源校验；允许使用语言标准库、官方语言运行时和 macOS/Windows 系统框架，不复制或捆绑第三方采集器、解析器、下载器或开源模型。

### 10. 任务、报告与优化

任务支持 created、queued、running、awaiting_approval、paused、succeeded、failed 和 cancelled，并提供步骤、进度、预算、检查点、暂停、恢复、重试和失败原因。

报告包含日、周、月、年报，先保存到 Obsidian。后台优化分析任务成功率、用户纠正、回滚、成本、延迟、Skill 效果和知识健康；它只能生成版本化建议，由 AI助手在对话中提交用户审阅，确认后再执行。

后台优化必须以不可变证据游标增量读取，拒绝权限扩张，保存候选、评估和应用版本，并支持回滚到历史版本。长期记忆必须支持查询、指标、纠正、过期、替代和墓碑治理，不通过覆盖历史事件来“修正”用户记录。

### 11. 数据与隐私

- Vault、任务、计划、Skill、报告、会话、操作日志和配置均保存在本机。
- API 密钥使用 AES-256-GCM 加密，设备密钥只允许当前操作系统用户访问。
- 长期记忆记录必要行为与对话事件，不记录凭据或完整二进制附件。
- SQLite 使用事务、WAL、迁移、备份和完整性检查。
- 更新检查只读取项目稳定 Release；更新前必须建立 SQLite 在线备份和已连接 Vault 快照，回滚前建立当前状态安全点。当前版本不声称自动下载、静默安装、Apple 签名或公证。
- 外部连接器只能由用户在设置中配置规范化 HTTPS 地址与加密凭据；模型分析正文后仍需通过策略和用户确认才能发送。
- 仓库与发布包排除 Vault、数据库、密钥、日志、缓存、截图和本机路径。

### 12. 当前明确不包含

- 账户注册、登录、退出和跨账户数据隔离。
- 实体图谱、实体消歧、关系类型、多跳查询、向量索引和混合检索。
- 未经用户配置的外部消息发送或通用远程控制。
- 规避第三方平台访问控制的能力。

---

## English

### 1. Product objective

I define Yunspire as a Chinese-language, local-first macOS and Windows desktop Agent with Obsidian as its knowledge database. The AI Assistant helps users capture, organize, query, create, review, report, invoke Skills, and maintain knowledge while users retain ownership of their Markdown, attachments, and Vaults.

The current version has no registration, login, or cloud identity boundary. It opens directly into the local workspace. Model configuration, tasks, and interface state stay in SQLite, and API keys are encrypted with a device-local key.

### 2. Product principles

- Obsidian is authoritative for document knowledge; SQLite is authoritative for structured runtime state.
- User content, webpages, files, images, media, and model output remain untrusted data.
- Ordinary conversation does not write to Obsidian; explicit operational intent creates a local task.
- The Assistant may use product capabilities except Settings, but never receives direct file or tool access.
- Local writes run automatically within configured scope while preserving validation, checkpoints, atomic commit, and operation logs.
- Settings remain user-controlled.
- Every displayed state must come from a real execution result rather than demo or fabricated data.
- Source-faithful records in the selected Vault and model-interpreted records in the Agent Vault are separate, linked deliverables; a summary cannot replace the source and model rewriting cannot masquerade as source text.

### 3. Information architecture

The fixed navigation is AI Assistant, Dashboard, Capture, Search, Create, Skills, Tasks, Reports, Operation Log, and Settings. The sidebar is collapsible, the workspace uses the full available area, every content-heavy page scrolls vertically, and opening the task drawer must not resize or corrupt the main layout.

Yunspire has no standalone graph page. It improves Obsidian's native Graph through notes, tags, Wiki Links, embeds, and Graph configuration.

### 4. AI Assistant

The Assistant uses configured real models for conversation and intent analysis; accepts files and images with the prompt; supports editable conversation names; exposes real slash-command candidates; implements `/clear`; compresses context near the configured token limit instead of by message count; routes conversation, analysis, and image requests to user-selected models; keeps operational work in the same conversation; renders structured rich text; and resumes a task after a user selects a requested next action.

User files have no per-file or per-selection total size ceiling. The desktop client transfers them in bounded chunks and the native runtime writes them as streams before model-sized batching. A newly attached image is analyzed once into a persisted summary, visible-text, tag, entity, key-point, model, and timestamp record. Historical context sends that record rather than the original bytes. Only an explicit filename, ordinal, or multi-image reference may reload the corresponding originals for another visual pass. If an original is no longer available in the current window, the Assistant asks the user to add it again. The user can choose a built-in Emoji avatar, persisted with the Assistant name, language, and style.

The first installed launch requires one unified work authorization covering local files/media, Obsidian Vault access, configured model connections, and user-initiated public-link capture. Its decision is persisted in local SQLite and must not be requested again on later launches using the same application data. Before authorization, Vault scanning, model connections, and background work remain disabled; only permission management in Settings is available. This application-level decision never replaces or bypasses native macOS or Windows permissions.

After authorization, a versioned five-step onboarding flow covers the Assistant, Obsidian, multi-format model analysis, scheduled/background work, and local/recoverable boundaries. Completion or skip is persisted; Assistant personalization follows the guide.

It cannot open or modify Settings, elevate imported content, or bypass the Command Bus, Policy Engine, Task Runtime, and Operation Log.

### 5. Obsidian and local knowledge

Yunspire discovers local Vaults and lets users choose query scope. When no Vault is selected, the first run initializes an Agent Vault for system artifacts, append-only memory, and interpreted records under `资料库/原文/`, plus a Personal Vault for notes, projects, resources, writing, archives, faithful source records under `资料库/原文/`, and captured assets under `资料库/附件/采集/`.

The Obsidian capability covers read, create, update, move, rename, Properties, tags, Wiki Links, attachments, folders, Graph configuration, soft delete, and restore. Writes check conflicts and commit atomically. Confirmed deletion moves the target into Yunspire Trash; permanent physical deletion requires an explicit user action.

### 6. Capture

The Capture page displays only real scheduled captures, execution steps, history, and results. Creation and modification occur through model-analyzed Assistant requests rather than a manual form that bypasses intent analysis.

Supported inputs include public webpages, user-authorized sources, text, Markdown, PDF, Word, PowerPoint, Excel, images, audio, video, local files, and folders. First-party processors preserve provenance, extract content, normalize tabular data to JSON, involve a configured model in every analysis, produce summaries/tags/structure, pass deterministic gates, and atomically write related originals, attachments, and analysis results. A normalized extraction hash deduplicates ready, writing, or committed content across tasks and sources while recording the original record and skip reason. Valuable video frames have no fixed count limit but remain bounded by runtime, storage, and model budgets.

macOS uses PDFKit, AVFoundation/ImageIO, and Speech for local PDF/media/transcription work. Windows uses Windows.Data.Pdf, Media Foundation/WIC, and local SAPI. The Windows installer carries a pinned-hash official embeddable Python runtime, so end users do not install Python. If no installed offline SAPI recognizer exactly matches the requested locale, Yunspire returns `windows_sapi_language_unavailable` rather than falling back to the wrong language or reporting false success.

Office v2 extraction is position preserving. Word retains body/story order, tables, images, fields, links, sections, and supporting parts. Excel reads every worksheet, keeps coordinate cells and formulas before producing a cleaned JSON view, marks cached values as not recalculated, and anchors images to ranges and row/column context. PowerPoint follows real slide order and retains element geometry, layering, crop data, tables, and inherited layout/master provenance; spatial proximity remains a candidate rather than a semantic fact. Every parser emits `integrity.status/errors/checks`; any incomplete required story, worksheet, slide, image relationship, Drawing, or placement evidence is a blocking ingestion error, never a partial success.

External images in webpage body flow, Markdown image syntax, and OOXML image relationships use a dedicated localizer that accepts only public `http/https` images, resolves DNS and every redirect, rejects private/loopback/link-local destinations, verifies MIME and actual image format, streams into isolation, and hashes with SHA-256. A successful asset returns to the exact paragraph, Markdown line/column, Word position, Excel anchor, or PowerPoint element through an `attachment://<reference_id>` placeholder. Identical bytes deduplicate by `asset_id`; configured-model analysis runs once per unique asset, while the deterministic writer places the resulting observation, visible text, context, evidence, and confidence at every occurrence-level `reference_id`. Original bytes remain untouched; if a temporary model derivative is necessary, each request binds its asset ID, original/derivative SHA-256, byte lengths, and allowed reference IDs, validates the transmitted derivative, and releases it after its bounded batch. Dynamic disk, available-memory, decode, and request gates are safety conditions rather than product file-size limits.

Ordinary embedded URLs remain inert with `auto_open=false` and `auto_fetch=false` until the user explicitly requests a separate capture. A linked-image download, redirect, type, hash, staging, or placement failure blocks complete ingestion and reports its precise cause. The selected Vault, or Personal by default, receives source-faithful Markdown, in-place assets, full structure JSON, and provenance. The Agent Vault receives model-interpreted Markdown, per-image analysis, tags, Wiki Links, and related notes under `资料库/原文/`. Both writes and their assets use stable targets with the full normalized content SHA-256 as a directory and the readable title as the basename, so Obsidian Graph shows readable node names; they share one analysis receipt and commit as one cross-Vault batch. Equal titles with different content never collide, and capture batches cannot overwrite existing targets. Full structure and attachments are preserved while model requests are byte-bounded batches rather than file truncation. Entity graphs, vector indexes, and hybrid retrieval remain deferred.

Yunspire does not bypass login, cookies, CAPTCHA, DRM, encrypted media, or platform access control. It guides users through lawful authorization and processes only content they may access or have exported.

### 7. Models, search, creation, and Skills

Users may add or remove providers, assign multiple models behind one endpoint/key or across different providers, and select final models for conversation, analysis, and image generation. The primary UI shows selected models rather than every discovered model. Chat, analysis, image generation, and image editing use role-correct provider endpoints. Request usage records capture model, role, tokens, duration, cost source, and errors without credentials, and long-running requests can be cancelled. Errors expose the real provider, endpoint, or model cause.

Search covers current or explicit cross-Vault scope, filenames, content, Properties, tags, links, sources, timestamps, and content types. Results show provenance and support an in-app read-only viewer plus an explicit Obsidian launch action.

Creation supports Markdown, Properties, tags, Wiki Links, sources, and inline images with real target Vault/folder choices and atomic writes. System Skills remain hidden in the background; the Skills page manages only user-created Skills. Every Skill declares version, I/O, model role, Vault scope, network target, timeout, retry, idempotency, side effects, and origin.

### 8. Tasks, reports, optimization, and privacy

Tasks expose durable states, steps, progress, budgets, checkpoints, pause, resume, retry, and failure reasons. Daily, weekly, monthly, and annual reports are saved to Obsidian first. Background optimization uses immutable evidence cursors, rejects permission expansion, preserves candidate/evaluation/application versions, and supports rollback; the Assistant submits every change for user review. Long-term memory supports query, metrics, correction, expiry, replacement, and tombstone governance without silently rewriting history.

Vaults, tasks, schedules, Skills, reports, conversations, operation events, and configuration stay local. API keys use AES-256-GCM; long-term memory excludes credentials and complete binary attachments; SQLite uses transactions, WAL, migrations, backups, and integrity checks. External connectors require user-configured HTTPS endpoints, encrypted credentials, deterministic policy, and user confirmation. Update protection snapshots SQLite and connected Vaults before installation and creates another safety point before rollback; automatic download, signing, and notarization are not claimed. Source packages exclude Vaults, databases, keys, logs, caches, screenshots, and machine-specific paths.

### 9. Explicitly deferred

The current version does not include accounts, entity graphs, entity disambiguation, typed entity relations, multi-hop queries, vector indexes, hybrid retrieval, unconfigured external delivery, generic remote control, or bypasses for third-party access controls.
