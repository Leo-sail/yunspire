# 云枢 Yunspire

<p align="center">
  <img src="desktop-ui/assets/brand/yunspire-lockup.jpg" alt="云枢 Yunspire" width="420" />
</p>

<p align="center">
  <strong>让知识流动，让成长发生。 / Let knowledge flow and growth follow.</strong>
</p>

<p align="center">
  <a href="#中文">中文</a> · <a href="#english">English</a>
</p>

---

## 中文

### 产品说明

云枢是我独立设计并实现的中文跨平台桌面 Agent 系统，支持 macOS 与 Windows。我用本地 Obsidian Vault 保存可长期拥有、阅读和迁移的知识，用 SQLite 保存明确的运行状态，再通过 AI助手把对话、采集、搜索、创作、定时任务、报告、Skill 调用和知识库维护连接成一个可追踪的本地工作流。

我设计云枢不是为了增加另一个封闭的聊天窗口，而是为了让模型在清晰的权限边界内完成真实工作：理解目标、选择能力、执行任务、验证结果，并把有价值的成果整理回用户自己的 Obsidian 知识库。

当前版本为 `0.1.1`，面向 macOS 13+ 与 Windows 10/11 x64。实体知识图谱、实体消歧、多跳查询、向量索引和混合检索暂未开发；我会在确认数据模型和产品边界后再单独实现这些能力。

### 主要特点

- **Obsidian 是知识权威来源**：Markdown、Properties、标签、附件和 Wiki Links 始终保存在用户自己的 Vault 中。
- **AI助手统一调度**：普通对话只进入本地会话；明确操作意图才会转换为受控命令并运行到完成。
- **图片只读一次、按需复用**：图片首次进入对话时由分析模型生成摘要、画面文字、标签和关键点；后续默认只携带分析记录，用户明确点名历史图片时才重新读取对应原图。
- **不限用户文件大小**：文件和文件夹通过分块通道进入本地隔离区，不设置单文件或一次选择的总大小上限；模型请求按供应商边界继续分批处理。
- **真实模型路由**：支持多个供应商和多个模型，并按对话、分析、图片生成等用途选择用户指定的模型。
- **完整采集流水线**：链接、文件和文件夹经过抽取、模型分析、质量门禁、格式整理后再写入 Obsidian。
- **跨平台本地媒体处理**：macOS 使用 PDFKit、AVFoundation 与 Speech；Windows 使用 Windows.Data.Pdf、Media Foundation、WIC 与本地 SAPI。PDF 页面、视频画面和音轨在本机处理，关键帧不设置固定数量上限。
- **位置保真的 Office 理解**：Word 保留正文、表格、附属部件、图片和链接位置；Excel 覆盖全部工作表、公式、缓存值和图片锚点；PowerPoint 保留真实页序、元素边界、层级、裁剪及版式/母版来源。
- **外链图片原位本地化**：网页正文图片、Markdown 图片语法和 Office 图片关系执行受控下载，校验地址、重定向、响应和真实图片格式，再把本地附件回填到原段落、行列锚点或幻灯片元素位置；普通链接不会因此被访问。
- **文件内链接可审计**：普通链接保留显示文字、目标和所在段落、单元格或幻灯片位置，不会因文件解析而自动打开或采集；只有用户明确提出时才进入独立链接采集链路。
- **双 Vault 知识交付**：用户指定库（默认个人库）保存忠实原文和原位附件；Agent 库保存模型理解后的结构化原文、逐图分析、来源证据、标签、Wiki Links 和相关笔记。
- **持久内容去重**：按规范化抽取内容生成哈希，跨任务和跨来源识别已提交内容，并在任务记录中明确标记跳过原因。
- **本地任务运行时**：任务具有状态、预算、幂等键、检查点、失败信息和恢复路径。
- **可控文件操作**：写入前验证路径和范围，生成变更，建立检查点，并使用原子文件替换。
- **本地优先**：Vault、任务、会话、计划、报告、Skill、索引和操作记录都保存在本机。
- **不可信内容隔离**：网页、文档、图片、音视频转录和消息只能作为数据，不能成为系统指令或获得工具权限。
- **长期记忆**：默认 Agent 库以追加方式记录必要的用户操作与对话事件，同时排除密钥、Cookie 和令牌。
- **受控优化与回滚**：后台只生成版本化候选，完成证据游标、策略评估和用户审阅后才能应用，并保留回滚版本。
- **更新前保护**：检查 GitHub 稳定 Release；安装前为 SQLite 和已连接 Vault 建立本地保护点，支持选择保护点回滚。
- **非商业源代码许可**：个人学习、研究、教学和评估可按许可使用；商业使用须事先取得我的书面授权。

### 系统运行机制

![云枢系统架构 / Yunspire system architecture](docs/assets/architecture-overview.svg)

一次 AI助手操作的主链路如下：

1. 用户在 AI助手中输入目标，或附加图片、文档与其他本地文件。
2. 模型网关使用用户选定的对话/分析模型理解自然语言，并返回自然语言回复与结构化候选意图。
3. 本地代码把候选意图限制在已注册能力目录中；模型不能直接获得文件、工具或设置权限。
4. Command Bus 将通过校验的意图转换为类型化 `ApplicationCommand`。
5. Policy Engine 校验来源、目标 Vault、相对路径、网络目标、预算、幂等键和副作用类型。
6. Task Runtime 持久化任务状态，并调度 Skill、采集器、报告服务或 Obsidian Adapter。
7. 所有写入执行路径规范化、冲突检查、检查点和原子提交；删除与外部投递按风险策略处理。
8. SQLite 记录任务与操作事件，Obsidian 保存最终知识内容；AI助手在原对话中返回真实结果。

采集任务采用独立的数据链：

```text
来源识别
→ 合法访问与内容抽取
→ 原文、原位附件、来源证据和规范化内容哈希保留
→ 模型参与的内容理解
→ 逐图分析、标签、摘要、结构与关联整理
→ 确定性质量和策略门禁
→ 忠实原文写入用户指定库，结构化理解稿写入 Agent 库
→ 跨 Vault 批量原子写入
→ 索引、任务、操作日志和对话结果同步
```

网页正文 `<img>`、Markdown 行内/引用式图片以及 OOXML 图片关系中的外链图片都会先转为本地附件；外链图片只允许公开 `http/https` 地址，并逐次校验 DNS、重定向、私网地址、MIME 与实际格式。图片按内容哈希去重，但每个原始位置分别保留 `reference_id`；Word 使用字符位置引用，Excel 使用 Drawing 锚点，PowerPoint 使用元素标识。文件夹导入会为不同文件建立稳定的位置命名空间，相同图片只物化和送模一次。原位 `attachment://<reference_id>` 由 Obsidian Adapter 在写入时映射到真实附件。任一必需图片无法本地化时，质量门禁阻断双库入库并显示具体失败，不会保存一份看似完整的原文。普通网页和文件链接仍只记录，不因解析而访问。

同一采集批次生成两个相关但职责不同的知识文件：用户指定 Vault（未指定时为个人库）保存未被模型改写的忠实原文、原位图片和来源证据；Agent 库的 `资料库/原文/` 保存分析模型形成的结构化理解稿。文件和附件采用“完整内容 SHA-256 目录 + 可读标题文件名”的稳定目标路径，使 Obsidian Graph 显示可读节点名称；同标题不同内容会生成不同目标，采集写入也拒绝覆盖任何已有文件。相同图片只按 `asset_id` 送模型理解一次，写入层再把这份逐图观察放回每个 `reference_id` 对应的位置，并用标签、Wiki Links 与相关笔记建立 Obsidian 原生关联。当前版本不把实体名称升级为实体图谱，也不启用向量索引或混合检索。

Excel 公式值只有缓存证据，处理器不会伪装成已经重新计算。Office 的任一必需 story、工作表、幻灯片、图片关系、Drawing 或位置证据损坏，都会带上文件和部件证据进入阻断错误，不会以“部分成功”入库。文件大小不设产品上限，解析内容和视觉资料按单次模型请求字节边界完整分批，再逐层汇总。

图片原件始终按原始字节写入 Vault。只有模型请求边界需要时，云枢才在隔离临时区生成 JPEG 分析派生物；每次请求都绑定 `asset_id`、原件 SHA-256、派生 SHA-256、原始/派生字节数和允许的 `reference_id`，模型不能自行改写位置标识。前端逐批准备、发送并释放派生图，不会因图片数量增加而先累积全量 Data URL。动态磁盘、可用内存、解码和请求边界是运行时安全门禁，不是对用户文件设置的产品大小上限。

### 架构边界

| 层 | 我的实现 | 权威数据 |
| --- | --- | --- |
| 体验层 | `desktop-ui/` 中的中文桌面界面 | 用户当前交互状态 |
| 控制层 | Command Bus、Policy Engine、Task Runtime、Scheduler | SQLite 中的任务与策略回执 |
| 能力层 | 模型网关、第一方 Skill、采集流水线、报告服务 | 版本化能力声明与执行结果 |
| 知识层 | Obsidian Adapter、文件监听、FTS | Obsidian Vault 中的 Markdown 与附件 |
| 运行数据层 | SQLite/WAL、检查点、操作事件 | 明确的结构化运行数据 |

我把 Obsidian Vault 与 SQLite 的职责严格分开：Vault 是文档知识的权威来源；SQLite 是任务、会话、计划、模型配置、回执和操作状态的权威来源；FTS 和未来的向量索引只是可重建的查询加速层。

详细设计见 [系统架构](ARCHITECTURE.md) 与 [产品需求](docs/PRODUCT_REQUIREMENTS.md)。

### 页面与工作区

- **AI助手**：对话、附件输入、命令、计划、后台执行状态和优化审阅。
- **仪表盘**：当前 Vault、任务、定时采集、知识变化和系统健康概览。
- **采集**：只展示由 AI助手创建或修改的真实定时采集及运行历史。
- **搜索**：按当前 Vault 或明确的跨库范围查询，并在应用内只读查看笔记。
- **创作**：创建和编辑 Markdown、Wiki Link、标签、来源引用及附件。
- **技能**：只展示用户创建的 Skill；第一方系统 Skill 在后台注册运行。
- **任务**：展示持久任务的状态、进度、预算、检查点、暂停、恢复与重试。
- **报告**：生成和管理日、周、月、年报及本地订阅。
- **操作日志**：追踪模型、任务、文件、策略、投递、优化与回滚事件。
- **设置**：由用户手动配置 Vault、模型、外部连接器、更新保护、权限、自动化和界面偏好。

### 环境要求

- macOS 13 或更高版本，或 Windows 10/11 x64
- Node.js `20.19+` 或 `22.12+`
- Rust `1.88.0+`
- macOS 构建需要 Xcode Command Line Tools / Apple clang
- Windows 构建需要 Visual Studio 2022 Build Tools、MSVC x64 与 Windows SDK
- Obsidian 桌面应用

网页、文档和媒体能力可能需要用户配置可用的模型 API；仓库不包含 API 密钥、Cookie、账户数据或用户 Vault。

### 安装与运行

```bash
git clone https://github.com/Leo-sail/yunspire.git
cd yunspire
npm ci
npm run verify
npm run tauri:dev
```

构建未签名的 macOS Debug 应用：

```bash
npm run tauri:build:debug
```

在 Windows x64 构建未签名 NSIS 安装器：

```powershell
npx tauri build --bundles nsis --no-sign --ci
```

Windows 构建会下载 Python 官方 3.13.7 x64 嵌入式运行时，核验固定 SHA-256 和官方发布校验值后再随安装包部署；用户无需单独安装 Python。生产签名、公证和分发证书不包含在仓库中，需要由构建者分别按照 Apple 或 Microsoft 的分发流程配置。

### 首次使用

1. 第一次打开已安装的云枢时，先确认统一工作授权，范围包括本地文件和媒体处理、Obsidian Vault 读写、已配置模型连接，以及用户主动发起的公开链接采集。决定保存在本机 SQLite；同一安装数据后续启动不会重复询问。
2. 授权后展示 5 步引导：AI助手、Obsidian、本地内容分析、定时任务和本地安全边界。
3. 选择 AI助手名称、内置 Emoji 头像、回复语言和风格；这些偏好可随时从对话菜单修改。
4. 云枢读取本机 Obsidian 配置并发现已有 Vault；如果用户没有指定 Vault，则初始化 `Agent 库` 和 `个人库`。个人库承载忠实原文与原位附件，Agent 库承载结构化理解稿、逐图分析和知识关联。
5. 在“设置 → API 配置”中添加供应商，获取真实模型列表，并为对话、分析或图片用途选择模型；回到 AI助手后可在任务和操作日志页面核对真实结果。

统一工作授权不代替操作系统权限。macOS 或 Windows 仍可能在首次使用文件、麦克风、屏幕等受系统保护的能力时显示原生权限提示；云枢不会绕过这些提示。Windows 本地转写要求安装与所选语言匹配的离线 SAPI 识别器，缺失时会返回明确错误而不会改用错误语言或伪报成功。

API 密钥由本机设备密钥使用 AES-256-GCM 加密后保存在应用数据目录内的 SQLite 数据库中。云枢不使用注册登录系统，也不会把密钥写入 Obsidian、长期记忆或仓库。

### 使用技巧

- 先在左下角确认当前查询 Vault，再提出“统计、搜索、总结”类问题，可减少跨库歧义。
- 在 AI助手中同时发送目标和附件，例如“阅读这个 PDF，提取核心论点并保存到个人库/研究”。
- 处理 Word、Excel 或 PowerPoint 时，可要求 AI助手说明图片对应的段落、行列/工作表或幻灯片元素。Office 图片关系指向的外部图片会受控下载为本地附件并回填原位；普通网址仍只作为来源数据保留，明确说“继续采集文档内链接”后才会建立独立网页采集任务。
- 图片首次分析后会显示“已记录”。普通追问不会再次上传原图；需要更细分析时可说“进一步分析第 2 张图”“比较上面两张图片”或直接输入图片文件名。
- 输入 `/` 查看可用命令；使用 `/clear` 清空当前对话上下文，但不会删除已保存的 Obsidian 知识或操作日志。
- 创建定时采集时写清来源、频率、时区、目标 Vault 和失败处理方式。
- 删除笔记、文件夹或 Vault 前检查差异与目标路径；确认后文件先进入云枢回收区，再按产品流程处理物理删除。
- 将系统设置保留给用户手动修改；在对话中让 AI助手执行知识、任务、采集、报告和 Skill 工作。
- 采集结果不完整时先查看任务步骤和操作日志，不要仅以对话文字判断是否真正写入。
- 定期备份 Obsidian Vault 和云枢 SQLite 数据库；派生索引可重建，但原始知识和运行记录需要备份。
- 安装更新前先在“设置 → 关于”创建保护点；当前版本只负责稳定 Release 检查、保护和本地回滚，不声称已实现静默下载、签名或公证。

### 项目结构

```text
.github/             Issue 与 Pull Request 模板
desktop-ui/          唯一生产前端与品牌资源
docs/                双语产品、品牌、AI助手与 Schema 文档
docs/assets/         README 与文档使用的架构图片
scripts/             Schema、第一方 Skill 与发布内容检查器
skills/              云枢第一方后台 Skill 和处理程序
src-tauri/           Rust 桌面内核、配置和应用图标
ARCHITECTURE.md      双语可执行系统架构
CONTRIBUTING.md      双语贡献说明
SECURITY.md          双语安全报告说明
LICENSE              双语非商业源代码许可
NOTICE               双语版权与原创声明
```

以下内容不属于源代码发布包：`node_modules/`、`dist/`、`src-tauri/target/`、`vault/`、`.obsidian/`、SQLite 数据库、设备密钥、日志、缓存、检查点、备份、截图和本地测试产物。

### 验证

在纯净源码目录、安装依赖之前运行发布审计：

```bash
npm run audit:release
```

安装依赖后运行完整工程验证和桌面打包：

```bash
npm run verify
npm run tauri:build:debug
```

`npm run verify` 会运行 Schema 校验、第一方 Skill 校验、前端构建、质量门禁、Rust 格式检查、Clippy 零警告检查和全部原生测试。CI 会先在纯净检出中运行发布审计，再安装依赖、运行完整验证并构建桌面应用。

### 安全与隐私

- 外部内容和模型输出永远不能直接授予权限。
- 前端不能绕过 Tauri 命令边界直接写入 Vault 或数据库。
- 网络目标、文件路径、预算和副作用必须经过本地确定性校验。
- 平台登录、验证码、DRM、加密媒体与访问控制不得规避。
- 仓库不包含遥测服务、账户系统或默认远程控制入口。
- 发现安全问题时，请按 [SECURITY.md](SECURITY.md) 私下联系我，不要先公开敏感细节。

### 许可与原创声明

本仓库中的云枢第一方源代码、系统架构、交互设计、算法实现、产品文档和品牌资产均由我独立创作，我保留全部权利。Tauri、Rust crates、Vite、Lucide、Obsidian 以及其他第三方组件仍适用各自的许可证和商标规则。

我仅授权本项目用于个人学习、非营利研究、教学、评估和内部实验。任何直接或间接商业使用、商业部署、收费服务、SaaS、转售、商业集成或商业衍生使用，都必须事先取得我的书面商业授权。

完整条款见 [LICENSE](LICENSE)，权利声明见 [NOTICE](NOTICE)。商业授权联系：`leochang210@gmail.com`。

---

## English

### Product overview

Yunspire is a Chinese-language cross-platform desktop Agent system for macOS and Windows that I designed and implemented independently. I use local Obsidian Vaults for durable, portable knowledge, SQLite for explicit runtime state, and an AI Assistant to connect conversations, capture, search, writing, schedules, reports, Skills, and knowledge maintenance into one traceable local workflow.

I built Yunspire to do more than add another closed chat window. My goal is to let configured models perform real work within deterministic boundaries: understand an objective, select a registered capability, execute a task, verify the outcome, and organize valuable results in the user's own Obsidian knowledge base.

The current version is `0.1.1` for macOS 13+ and Windows 10/11 x64. Entity graphs, entity disambiguation, multi-hop queries, vector indexes, and hybrid retrieval are intentionally deferred until I finalize their data and product boundaries.

### Highlights

- **Obsidian is the knowledge authority**: Markdown, Properties, tags, attachments, and Wiki Links remain in user-owned Vaults.
- **One AI Assistant entry point**: ordinary conversation stays local; only explicit operational intent becomes a controlled command.
- **Analyze images once, reuse on demand**: the analysis model records a summary, visible text, tags, and key points when an image first enters a conversation. Later turns use that record unless the user explicitly names one or more historical images.
- **No user-file size ceiling**: files and folders enter a local isolation area through bounded IPC chunks. Yunspire does not impose a per-file or per-selection total size cap, while provider requests remain safely batched.
- **Real model routing**: multiple providers and models can be assigned to conversation, analysis, and image generation roles.
- **End-to-end capture**: links, files, and folders pass through extraction, model analysis, quality gates, formatting, and atomic Vault writes.
- **Cross-platform local media processing**: macOS uses PDFKit, AVFoundation, and Speech; Windows uses Windows.Data.Pdf, Media Foundation, WIC, and local SAPI. PDF pages, video frames, and audio tracks are processed locally, with no fixed key-frame count ceiling.
- **Position-preserving Office analysis**: Word keeps ordered stories, tables, images, and links; Excel covers every worksheet, formula, cached value, and drawing anchor; PowerPoint keeps real slide order, element bounds, layers, crop data, and layout/master provenance.
- **Controlled Office linked-image localization**: only OOXML image relationships trigger a guarded download. Address, redirects, response type, and actual image format are validated before the local asset is restored to its original paragraph, cell anchor, or slide element.
- **Auditable embedded links**: ordinary links preserve display text, target, and paragraph/cell/slide provenance. File parsing never opens or captures them; an explicit user request creates a separate link-capture task.
- **Dual-Vault knowledge delivery**: the selected Vault, or Personal Vault by default, receives faithful source Markdown and in-place assets; the Agent Vault receives model-interpreted source Markdown, per-image analysis, provenance, tags, Wiki Links, and related notes.
- **Persistent content deduplication**: normalized extraction hashes identify committed content across tasks and sources, with an explicit skipped-duplicate outcome.
- **Durable local runtime**: tasks carry states, budgets, idempotency keys, checkpoints, errors, and recovery paths.
- **Controlled file changes**: every write validates its path and scope, creates a checkpoint, and commits atomically.
- **Local-first storage**: Vaults, tasks, conversations, schedules, reports, Skills, indexes, and operation events stay on the Mac.
- **Untrusted-content isolation**: imported text and media remain data and cannot become system instructions or grant tool access.
- **Long-term memory**: the default Agent Vault receives append-only user activity and conversation events while excluding secrets.
- **Governed optimization and rollback**: background candidates are versioned, evidence-bound, evaluated, user-reviewed, and reversible.
- **Pre-update protection**: Yunspire checks stable GitHub Releases, snapshots SQLite and connected Vaults before installation, and can restore a selected protection point.
- **Non-commercial source license**: personal, research, teaching, and evaluation use is licensed; commercial use requires my prior written authorization.

### How it works

![Yunspire system architecture](docs/assets/architecture-overview.svg)

The primary AI Assistant path is:

1. The user enters an objective and may attach images, documents, or other local files.
2. The Model Gateway uses the selected model to return a natural-language response and a structured candidate intent.
3. Local code restricts that intent to the registered capability catalog; the model never receives direct file, tool, or settings permissions.
4. The Command Bus converts the validated intent into a typed `ApplicationCommand`.
5. The Policy Engine validates origin, Vault, relative paths, network targets, budgets, idempotency, and side-effect category.
6. The Task Runtime persists state and invokes a Skill, capture adapter, report service, or Obsidian Adapter.
7. Writes perform path normalization, conflict checks, checkpoints, and atomic replacement; destructive or external actions follow risk policy.
8. SQLite stores runtime events, Obsidian stores final knowledge, and the Assistant returns the verified outcome in the same conversation.

Capture uses a dedicated data pipeline:

```text
source classification
→ lawful access and extraction
→ original content, in-place assets, provenance, and normalized content hash
→ model-assisted understanding
→ per-image observations, tags, summary, structure, and links
→ deterministic quality and policy gates
→ faithful source to the selected Vault and interpreted record to the Agent Vault
→ atomic cross-Vault batch write
→ synchronized index, task, operation-log, and conversation result
```

External images in webpage body flow, Markdown image syntax, and OOXML image relationships become local assets before ingestion. The independently designed localization path accepts only public `http/https` targets and validates DNS, every redirect, private-address exclusions, MIME, and actual image format before streaming and hashing the file. Identical bytes share one `asset_id`, while every occurrence keeps its own `reference_id`, including Word character positions, Excel Drawing anchors, and PowerPoint elements. Ordinary links remain inert. The Obsidian Adapter resolves in-place `attachment://<reference_id>` placeholders at commit time; any required linked-image failure blocks both Vault writes and remains visible.

The selected Vault, or Personal Vault by default, receives source-faithful Markdown, in-place assets, and provenance. `Agent 库/资料库/原文/` receives a model-interpreted record whose image observations bind both the deduplicated `asset_id` and occurrence-level `reference_id`, followed by tags, Wiki Links, and related-note connections. Notes and assets use stable targets with the full content SHA-256 as a directory and the readable title as the basename, so Obsidian Graph shows readable node names; equal titles with different content resolve to different targets, and capture writes never overwrite an existing file. Any incomplete Office story, worksheet, slide, image relationship, Drawing, or placement evidence becomes a blocking error rather than a partial success. Original image bytes are never modified for analysis: a temporary JPEG derivative is used only when needed and every model request binds the asset ID, original SHA-256, derivative SHA-256, byte lengths, and allowed reference IDs. The UI prepares, submits, and releases these derivatives batch by batch. Dynamic disk, memory, decode, and request gates are runtime safeguards, not product file-size limits. This does not enable an entity graph, vector index, or hybrid retrieval. Excel cached formula values remain labeled as not recalculated. Files have no product-size ceiling; extracted text and visuals are completely batched by per-request byte boundaries and then hierarchically consolidated.

### Architecture boundaries

| Layer | My implementation | Authoritative data |
| --- | --- | --- |
| Experience | Chinese desktop UI in `desktop-ui/` | Current user interaction state |
| Control | Command Bus, Policy Engine, Task Runtime, Scheduler | Task and policy receipts in SQLite |
| Capability | Model Gateway, first-party Skills, capture pipeline, reports | Versioned capability definitions and outcomes |
| Knowledge | Obsidian Adapter, file watcher, FTS | Markdown and attachments in Obsidian Vaults |
| Runtime data | SQLite/WAL, checkpoints, operation events | Explicit structured runtime state |

I keep Obsidian and SQLite responsibilities separate. Vaults are authoritative for document knowledge; SQLite is authoritative for tasks, conversations, schedules, model configuration, receipts, and operation state; FTS and future vector indexes are rebuildable accelerators.

See [System Architecture](ARCHITECTURE.md) and [Product Requirements](docs/PRODUCT_REQUIREMENTS.md) for the detailed design.

### Workspaces

- **AI Assistant**: conversation, attachments, commands, execution state, and optimization review.
- **Dashboard**: Vault, tasks, scheduled capture, knowledge changes, and health.
- **Capture**: real scheduled captures and execution history created or modified through the Assistant.
- **Search**: current-Vault or explicit cross-Vault queries with an in-app read-only note viewer.
- **Create**: Markdown, Wiki Links, tags, sources, and attachments.
- **Skills**: user-created Skills only; first-party system Skills run in the background.
- **Tasks**: durable state, progress, budgets, checkpoints, pause, resume, and retry.
- **Reports**: daily, weekly, monthly, and annual reports and subscriptions.
- **Operation Log**: model, task, file, policy, delivery, optimization, and rollback events.
- **Settings**: user-controlled Vault, model, external connector, update protection, permission, automation, and appearance settings.

### Requirements and setup

- macOS 13 or later, or Windows 10/11 x64
- Node.js `20.19+` or `22.12+`
- Rust `1.88.0+`
- Xcode Command Line Tools / Apple clang for macOS builds
- Visual Studio 2022 Build Tools, MSVC x64, and the Windows SDK for Windows builds
- Obsidian desktop

```bash
git clone https://github.com/Leo-sail/yunspire.git
cd yunspire
npm ci
npm run verify
npm run tauri:dev
```

Build an unsigned macOS Debug application:

```bash
npm run tauri:build:debug
```

Build an unsigned Windows x64 NSIS installer on Windows:

```powershell
npx tauri build --bundles nsis --no-sign --ci
```

The Windows build downloads the official Python 3.13.7 x64 embeddable runtime, verifies a pinned SHA-256 plus the official release checksum, and packages it with Yunspire; end users do not install Python separately. Apple/Microsoft signing, notarization, and distribution credentials are not part of this repository.

### First run

1. On the first installed launch, approve Yunspire's unified work authorization for local files/media, Obsidian Vault access, configured model connections, and user-initiated public-link capture. The decision is stored in local SQLite and is not requested again on later launches using the same application data.
2. After authorization, Yunspire opens the five-step introduction covering the Assistant, Obsidian, local content analysis, scheduled work, and local safety boundaries.
3. Choose the Assistant name, a built-in Emoji avatar, response language, and style. These preferences remain editable from the conversation menu.
4. Yunspire discovers local Vaults and initializes `Agent 库` and `个人库` when no Vault was selected. Personal holds source-faithful records and in-place assets; Agent holds interpreted records, image observations, and links.
5. Add providers and role-specific models under Settings, then use Tasks and Operation Log to verify real execution outcomes.

This application-level authorization does not replace operating-system permissions. macOS or Windows may still show native prompts the first time a protected file, microphone, screen, or similar capability is used. Windows local transcription requires an installed offline SAPI recognizer matching the requested locale; if it is absent, Yunspire returns a structured error instead of using the wrong language or reporting false success.

API keys are encrypted with AES-256-GCM using a device-local key and stored in the application-data SQLite database. Yunspire has no registration/login system and never writes credentials into Obsidian, long-term memory, or the repository.

### Practical tips

- Confirm the active Vault before asking for counts, searches, or summaries.
- Send an objective together with attachments, for example: “Read this PDF, extract the main arguments, and save them under Personal/Research.”
- For Word, Excel, or PowerPoint, linked image resources are localized and restored to their source positions. Ordinary embedded URLs remain inert until you explicitly request a separate webpage capture.
- After an image shows as recorded, ordinary follow-ups reuse its analysis without resending the original. Say “analyze image 2 further,” “compare the previous two images,” or use the exact filename to request a new deep visual pass.
- Type `/` to discover commands. `/clear` resets conversation context without deleting Vault knowledge or operation history.
- For scheduled capture, specify source, frequency, time zone, target Vault, and failure behavior.
- Review the exact target path before confirming a note, folder, or Vault deletion.
- Keep Settings user-controlled; use the Assistant for knowledge, tasks, capture, reports, and Skill operations.
- Inspect task steps and operation events when capture is incomplete instead of relying on chat text alone.
- Back up both Obsidian Vaults and the Yunspire SQLite database. Derived indexes are rebuildable; source knowledge and runtime history are not.
- Create a protection point under Settings → About before updating. Version `0.1.1` checks stable Releases and provides local protection/rollback; it does not claim silent download, signing, or notarization.

### Repository layout

```text
.github/             bilingual Issue and Pull Request templates
desktop-ui/          production frontend and brand assets
docs/                bilingual product, brand, Assistant, and Schema docs
docs/assets/         architecture images used by README and docs
scripts/             Schema, first-party Skill, and release audits
skills/              Yunspire first-party background Skills and processors
src-tauri/           Rust desktop kernel, configuration, and app icons
ARCHITECTURE.md      bilingual executable system architecture
CONTRIBUTING.md      bilingual contribution guide
SECURITY.md          bilingual security reporting guide
LICENSE              bilingual non-commercial source license
NOTICE               bilingual copyright and authorship notice
```

The source package excludes `node_modules/`, `dist/`, `src-tauri/target/`, `vault/`, `.obsidian/`, SQLite databases, device keys, logs, caches, checkpoints, backups, screenshots, and local test artifacts.

### Verification

Run the release audit in a clean source checkout before installing dependencies:

```bash
npm run audit:release
```

After dependency installation, run full engineering verification and the desktop bundle build:

```bash
npm run verify
npm run tauri:build:debug
```

`npm run verify` runs Schema validation, first-party Skill validation, the frontend build, quality gates, Rust formatting, zero-warning Clippy, and all native tests. CI audits a clean checkout first, then installs dependencies, runs full verification, and builds the desktop application.

### Security, authorship, and license

Imported content and model output cannot grant permissions. The frontend cannot bypass Tauri commands to write directly to Vaults or SQLite. Local deterministic code validates network targets, file paths, budgets, and side effects. Yunspire does not bypass logins, CAPTCHA, DRM, encrypted media, or platform access controls.

I independently created the Yunspire first-party source code, architecture, interaction design, algorithms, documentation, and brand assets in this repository. Third-party components such as Tauri, Rust crates, Vite, Lucide, and Obsidian remain governed by their own licenses and trademark rules.

I license this project only for personal study, nonprofit research, teaching, evaluation, and internal experimentation. Any direct or indirect commercial use, deployment, paid service, SaaS, resale, integration, or commercial derivative requires my prior written authorization.

See [LICENSE](LICENSE), [NOTICE](NOTICE), [CONTRIBUTING.md](CONTRIBUTING.md), and [SECURITY.md](SECURITY.md). Commercial licensing contact: `leochang210@gmail.com`.
