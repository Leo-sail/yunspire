# 云枢产品需求 / Yunspire Product Requirements

当前版本 / Current version: `0.3.0`

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

顶部导航固定为四个长期空间：

1. 工作台
2. 知识
3. 创作
4. 成长中心

工作台是默认启动空间，恢复最近阅读、创作、采集和待判断变化，并承载常用工作入口。知识库承载检索、只读阅读、Properties、标签、Wiki Links、相关笔记和知识维护；创作承载 Grounded 文稿、来源、候选、版本、资源和导出；成长中心承载报告、长期记忆、反思审阅和成长版本。

搜索/命令、采集和“问云枢”组成全局动作区。工作台主工作区提供完整 AI 会话、采集运行与定时任务、后台任务抽屉、Skills、操作记录和设置入口，不把运行治理提升为一级空间。内容超过视口时页面必须纵向滚动，任务抽屉不得改变主工作区宽度。当前只验收 macOS 与 Windows 桌面布局，不提供移动端专用导航、组件或响应式验收。

云枢不在自身界面重绘或替代知识图谱。知识库提供“原生图谱”入口，选择 Vault 后直接唤起 Obsidian 原生 Graph View；图谱的渲染、筛选、局部图、反向链接、节点操作和全部交互继续由 Obsidian 原生界面承担。云枢只负责 Vault 选择、入口和安全边界。

### 4. AI助手

AI助手是贯穿全局的上下文协助层；完整会话中心由工作台进入。它必须：

- 调用用户配置的真实模型理解自然语言，并提供正常对话能力。
- 支持拖入文件和图片，并把附件与用户文字作为同一次请求处理。
- 用户文件不设置单文件或单次选择总大小上限；桌面端必须分块传输和流式落盘，模型输入再按上下文能力分批。
- 图片第一次进入对话时必须由分析模型生成可持久化的摘要、画面文字、标签、实体、关键点、模型和时间记录。历史上下文默认只发送这份记录，不能重复发送原图。
- 用户通过文件名、序号或“刚才两张图片”等明确表达指定历史图片时，才允许重新读取对应原图进行进一步分析；当前窗口没有原图句柄时必须明确要求用户重新添加。
- 支持用户从内置 Lucide 图标中更换助手图标，名称、图标、语言和风格随本地工作区持久化。
- 为每个对话提供可编辑名称，并在完整会话工作区的会话列表显示名称。
- 输入 `/` 时向上展开真实命令候选；完整命令可直接执行。
- 支持 `/clear` 清空后续请求携带的对话上下文，不删除历史记录或知识。
- 在模型上下文接近配置上限前压缩，不按固定消息数量截断。
- 根据用户选定用途自动路由对话、分析和图片模型。
- 识别明确系统操作意图后，在原对话中持续运行到完成，不自动跳转设置或其他页面。
- 只向用户展示必要进度、结果、失败原因和下一步选择。
- 对可选择的下一步提供结构化选项，用户选择后恢复原任务。
- 用规范化富文本渲染标题、段落、列表、加粗、代码、链接和表格。
- 同一对话中的请求按先进先出顺序执行，不同对话拥有独立队列并可并行；创建新对话时不受其他对话运行状态限制。
- 取消令牌贯穿模型请求、内容分析和后续执行；观察到取消后不得继续发起网络、模型或写入调用。
- 明确操作意图的回执只能兑换一次与规范参数、调用方和有效期绑定的执行票据；参数替换、并发重复提交和重放必须被拒绝。

AI助手不得打开或修改设置，不得把用户内容提升为系统指令，不得绕过 Command Bus、Policy Engine、Task Runtime 或操作日志。

首次安装启动必须先显示一次统一工作授权，覆盖本地文件与媒体处理、Obsidian Vault 读写、已配置模型连接和用户主动发起的公开链接采集。授权决定必须保存在本机 SQLite；同一应用数据后续启动不得重复询问。未授权时不得扫描 Vault、连接模型或启动后台任务，只开放设置中的权限管理。该应用层授权不能替代或绕过 macOS、Windows 的系统级权限提示。

授权后显示版本化的 3 步引导，依次介绍从本机知识开始、从上次位置继续，以及在当前对象旁使用 AI助手。完成或跳过后进入助手个性化设置；同一引导版本完成后不重复弹出。

### 5. Obsidian 与本地知识

云枢必须发现本机 Obsidian Vault，并支持用户选择当前查询范围。没有用户指定库时，首次启动初始化：

- `Agent 库`：默认建立 `知识库`、`原子库`、`资料库`、`收件箱`、`画像` 和 `长期记忆/行为记录`；资料库内部的来源分类按实际内容、用户选择或 AI 判断按需建立，不预置公众号、抖音、小红书等平台目录。
- `个人库`：默认建立复盘报告、随想、项目和 `创作成品`；创作成品内部分类由用户选择或由 AI 根据内容判断后按需建立。

Obsidian 管理能力包括读取、创建、更新、移动、重命名、Properties、标签、Wiki Links、附件、文件夹、Graph 配置、软删除和恢复；知识库的图谱显示与交互必须通过 Obsidian 原生 Graph View 完成，云枢不得另建一套图谱画布。写入必须检查版本冲突并使用原子替换。

用户要求删除笔记、文件夹或 Vault 时，系统生成准确目标和影响，用户确认后移动到云枢回收区。永久物理删除必须由用户明确触发。

### 6. 采集

全局“采集”动作打开直接采集工作区，用户可以提交链接、文本、本地文件或文件夹，并查看真实处理阶段、取消和最终结果。定时采集的创建、修改、暂停、恢复、立即运行、重试或删除继续通过 AI助手理解自然语言后完成；采集工作区展示真实计划、运行历史、步骤和结果。

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

搜索支持当前 Vault、显式跨 Vault、文件名、正文、Properties、标签、链接、来源、修改时间和内容类型。中文词法索引使用 CJK 字符对；本地特征向量从字符、词项、标题、路径、标签和 Wiki Link 确定性生成。用户明确同意后可启用外部神经 Embedding；默认关闭，查询只发送搜索词，笔记索引只发送标题、相对路径、标签、Wiki Links 和最多 24,000 个规范化正文字符，不发送 Properties 或 Vault ID。词法、本地向量和可用的神经向量候选使用标准 RRF `1 / (60 + rank)` 融合，并返回各路名次、相似度和标题/路径/关系/时间信号。未同意、未配置、向量缺失或损坏不得阻断 FTS 与本地向量。结果必须携带 Vault ID 与规范相对路径，展示来源片段，并支持在云枢内只读查看或明确跳转 Obsidian 权威原文。

AI助手的对话搜索必须覆盖标题、元数据和已持久化正文。正文索引完全位于本地 SQLite，支持 ASCII 与 CJK 词项，严格过滤当前 `workspace_scope`；查询最长 512 字符、返回最多 100 条。`workspace_messages` 保持事实来源，FTS 只能作为可回填投影，并与消息更新、单条删除和整会话删除处于同一事务。桌面端必须忽略迟到查询结果；无原生运行时时保留标题与元数据降级。

创作使用 `CreationDocumentV2` 管理 Canonical Markdown、派生块、图片与文件资产、跨 Vault 来源、布局、发布状态、provenance、Validation Receipt 和仅用于导出的 Readiness。编辑器必须保留表格、代码围栏、Properties、标签、Wiki Links、来源引用和任意位置图片，并保护同名标题不覆盖已有草稿。单篇文章正文、文章内图片数量和文章总字节数不设置产品级固定上限；桌面端以耐久资产、分块传输、严格单事件和按模型上下文分批处理控制资源。保存到 Vault 时，最终 Markdown 先通过 `uploadDurableText` 分块暂存，再由 `prepare_note_write_from_durable_asset` 流式校验 UTF-8、行数和 SHA-256，并与图片作为同一可回滚原子批次提交。4 MiB 只决定使用完整逐行 diff 还是包含前后字节数、行数与完整 SHA-256 的有界 diff 预览，不得拒绝、截断或缩写大文稿。

`creation.generate` 与 `creation.edit` 必须统一使用原生 WritingRun。SQLite 持久化 WritingRun、严格 Creation Agent Stream 事件、稳定 checkpoint 和逐请求 usage 账本；同一文稿只允许一个活动运行。供应商流式正文由原生层解析为带 `providerSequence` 的真实 `contentDelta`，前端拒绝重复、倒序、跳号、错误通道和身份不匹配事件，并要求所有增量拼接结果与最终模型回执逐字一致，不能用最终全文伪造流。AI 生成、全文优化、面板选区改写及浮动选区“润色/改写/仿写/自定义”都先生成候选，支持模型请求取消、WritingRun 取消和重启恢复；恢复从原生基础文稿、checkpoint 与完整事件序列重放，并重新校验 document ID、revision、输入哈希和候选哈希。每个 `creation.generate`、`creation.edit`、grounding 核验和品牌评测请求记录 provider、model、Token、耗时、估算成本、状态与错误，并按 WritingRun 聚合。候选通过确定性门禁后进入差异审核，用户接受前正文不得变化。

候选接受必须在原生事务中再次检查基础 revision、输入哈希、候选 checkpoint 哈希、Creation Validation 和逐块 grounding。跨 Vault `SourceRef` 持久化 `vaultId`、`relativePath`、正文/摘录 SHA-256、可复核摘录和正文块关系；任一来源身份、哈希、引文或 `supported` 结论失效都必须阻止接受。图片原件通过耐久资产注册表恢复，当前 `CreationDocument.assets` Manifest 决定该 revision 的图片集合；Vault 提交成功后真实路径、内容哈希、`localized` 状态与时间回写文稿，失败或拒绝执行清理/回滚。自定义主题、组件和模板进入原生 Manifest 版本库，支持校验、列表、版本查看、归档与恢复。`BrandProfile` 支持创建/更新、批准、归档、删除、文稿绑定/解绑与评测。PDF、PNG、JPEG 以逐页渲染方式导出并及时释放页面画布，多页 PNG/JPEG 输出带 Manifest 的 ZIP；所有复制和导出继续受 Readiness 门禁约束，并记录格式、页数、字节数、目标和 SHA-256，不声称已发布到外部平台。

### 9. Skills

系统内置 Skill 和处理程序只在后台运行，不进入用户 Skill 列表。用户从 AI助手创建、安装、修改、查询和运行 Skill；工作台中的 Skills 面板展示用户 Skill 的真实状态、路由依据、权限、运行数据和版本历史，并提供启用、停用、退役与历史恢复。第三方 Skill 安装必须由 AI助手发起并经过同一治理链；当前只允许用户明确提供的 GitHub `SKILL.md`，禁止克隆仓库、执行脚本、下载其他文件或继承外部能力声明，且必须记录规范化来源 URL、修订和内容哈希。AI 创建和修改只生成候选版本；第三方安装在用户明确确认后立即执行确定性安全评估，全部检查通过时自动记录该安装确认对应的批准并默认启用，失败时保持被拒绝、禁用且不可路由，不得要求用户再次批准。

每个 Skill 声明标识、版本、输入输出、模型用途、Vault 范围、网络目标、超时、重试、幂等、副作用和来源。运行请求必须冻结 `version` 与 `payloadHash`；原生执行器在调用真实聊天模型前后重新校验启用状态、确定性评估、用户批准、输入/输出 Schema，并写入开始、成功、失败或取消审计。Skill 指令与声明能力只允许内容转换，不得获得文件、网络、Shell、设置或其他系统副作用。第一方 Skill 与处理器必须由我独立设计实现，并通过来源校验；允许使用语言标准库、官方语言运行时和 macOS/Windows 系统框架，不复制或捆绑第三方采集器、解析器、下载器或开源模型。

每次执行必须追加结构化效果：execution/request/task/trace、Skill version、输入哈希、可选输出哈希、`started/succeeded/failed/cancelled` outcome、warning、error 和时间。效果与其 `acceptance`/`correction` 反馈必须不可更新、不可删除，并能按 Skill、request、task、trace 与 outcome 查询。Renderer 可以查询普通效果，但不能伪造原生任务保留证据。

第一方受控深度研究 Skill 必须按计划、证据收集、矛盾核对、综合、引用和反思六阶段运行。每阶段执行策略、预算、取消和检查点校验；每个可核验主张必须回溯到证据、来源及内容哈希。研究 Skill 不得直接写 Vault、修改设置或扩大网络范围。

### 10. 任务、报告与优化

任务支持 created、queued、running、awaiting_approval、paused、succeeded、failed 和 cancelled，并提供步骤、进度、预算、检查点、暂停、恢复、重试和失败原因。

新原生任务可绑定 schema `1.0` 的版本化类型 DAG。步骤必须使用注册类型并通过依赖、环、参数和容量校验；计划首次进入执行后不得换版。完成契约当前只支持 `all_of`，每项要求声明证据类型和最小数量，所有当前版本要求满足前不得写入 `succeeded`。证据必须规范化、哈希并追加保存；同一 ID 只允许相同内容幂等重放。客户端快照不得覆盖契约任务，恢复建议必须按当前计划版本重新核验并同步原生终态。

运行时必须从依赖成功的 frontier 原子领取步骤，记录 worker、attempt、lease 和 cancellation fence，并预留 step、tool call、runtime、Token 与 cost 预算。依赖就绪的 `read_only` 步骤可以并行领取；活动 `effectful` 步骤必须形成屏障，副作用步骤一次只领取一个。lease 过期必须释放预留并写 `expired` 回执；父任务取消必须释放预留、写 `cancelled` 回执、递增 fence、拒绝迟到结果并递归取消绑定子任务。

能力步骤必须以 `origin=runtime` 子命令执行。子命令必须精确绑定 runtime task、plan revision、step 和 claim，不得携带新计划、复用模型凭证或伪装成直接用户/系统维护命令。Rust 必须验证 capability、operation、Trace、Vault、相对路径、网络目标、声明范围和预算均未超出父任务及父步骤授权，并在绑定子任务成功后才允许步骤成功。成功回执必须由 Rust 在同一事务转为 `runtime.step_receipt` 完成证据；公共 IPC 不得接受 Renderer 伪造 `runtime.*`、`schedule.dispatch_ack` 或 runtime/scheduler 来源。

AI助手主执行路径必须真实接入 `capability-main -> verify-result`：先领取并执行能力步骤，再领取验证步骤，最后重读当前完成契约后才结算父任务。当前产品 planner 只生成这条两步路径；底层只读 fan-out 不构成通用多 Agent planner，验证回执也不能替代各业务域的语义质量门禁。

每个到期时间必须生成稳定 occurrence，并在原生事务中创建 wrapper 任务、`schedule_dispatch` 步骤、完成契约和 Trace。同一日程的 occurrence 必须串行处理。只有 Rust 核验至少一个真实子任务已由 Command Bus 与 Policy 接受，且参数绑定到该 occurrence、wrapper、日程类型和计划时间后，才能记录派发完成证据；跳过、无匹配、内部失败或 Renderer 自声明不得满足契约。删除日程不得抹除历史 occurrence。

报告包含日、周、月、年报，先保存到 Obsidian。后台优化分析任务成功率、用户纠正、回滚、成本、延迟、Skill 效果和知识健康；它只能生成版本化建议，由 AI助手在对话中提交用户审阅，确认后再执行。

后台优化必须以不可变证据游标增量读取，拒绝权限扩张，保存候选、评估和应用版本，并支持回滚到历史版本。长期记忆派生层分为 `user_episode`、`user_profile`、`agent_case`、`agent_skill` 四轨，按 user、agent、app、project、session 五维精确隔离，并保存证据、置信度、版本、替代、过期和墓碑。

反思任务必须冻结可重放 source snapshot/hash、来源文档、终态 Skill 效果和指标，支持 queued/running/awaiting_review/completed/failed/cancelled、原子 claim、claim token、5 至 900 秒 lease、过期回收、启动恢复、续期与取消。worker 只能从冻结快照生成不可召回草稿。绑定优化候选时，job、candidate 和 proposal memory 必须建立不可替换关联；批准必须在同一事务激活记忆、应用候选、完成 job/binding 并给来源效果追加 `acceptance`；拒绝或重做必须墓碑化草稿、supersede 绑定并追加 `correction`，其中重做重新进入 queued。系统不得通过覆盖历史事件来“修正”记录。

当前桌面主流程尚未周期性调用步骤 lease 或反思 lease 续期；超长单步和超长模型反思必须在现有 lease 内完成，否则由运行时回收并重试。浏览器模式不作为 Tauri IPC、SQLite 事务或原生权限的验收证据。

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
- 实体图谱、实体消歧、关系类型、多跳查询、远程向量数据库和学习式语义重排。神经 Embedding 已限于明确同意、可重建缓存和本地降级的检索链路。
- 未经用户配置的外部消息发送或通用远程控制。
- 规避第三方平台访问控制的能力。
- 移动端专用导航、组件和响应式验收；当前产品只面向 macOS 与 Windows 桌面应用。

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

The top navigation has four durable spaces: Workbench, Knowledge Base, Creation, and Growth Center. Workbench is the default start space and resumes reading, writing, capture, and pending decisions. Knowledge Base owns retrieval and reading; Creation owns grounded documents, sources, candidates, versions, resources, and export; Growth Center owns reports, long-term memory, reflection review, and growth versions.

Search/commands, Capture, and Ask Yunspire form the global action area. The Workbench exposes full AI conversations, capture runs and schedules, the background-task drawer, Skills, Operation Log, and Settings without promoting runtime governance into primary navigation. Content-heavy pages scroll vertically and the task drawer must not resize the main workspace. Acceptance covers the macOS and Windows desktop layout only; Yunspire has no mobile-specific navigation, components, or responsive acceptance target.

Yunspire does not redraw or replace the knowledge graph. The Knowledge surface provides a native-graph action that selects a Vault and opens Obsidian's native Graph View; rendering, filters, local graph, backlinks, node operations, and all graph interactions remain in Obsidian. Yunspire owns only the Vault selection, launch path, and safety boundary.

### 4. AI Assistant

The Assistant is a contextual layer available across the product; its full conversation center is opened from the Workbench. It uses configured real models for conversation and intent analysis; accepts files and images with the prompt; supports editable conversation names; exposes real slash-command candidates; implements `/clear`; compresses context near the configured token limit instead of by message count; routes conversation, analysis, and image requests to user-selected models; keeps operational work in the same conversation; renders structured rich text; and resumes a task after a user selects a requested next action. Requests execute FIFO within one conversation and concurrently across conversations, so a running request never disables a new conversation. Cancellation spans model, analysis, and follow-on execution. A model receipt can mint only one execution ticket bound to canonical arguments, caller, and expiry; substitution, concurrent duplicate submission, and replay fail closed.

User files have no per-file or per-selection total size ceiling. The desktop client transfers them in bounded chunks and the native runtime writes them as streams before model-sized batching. A newly attached image is analyzed once into a persisted summary, visible-text, tag, entity, key-point, model, and timestamp record. Historical context sends that record rather than the original bytes. Only an explicit filename, ordinal, or multi-image reference may reload the corresponding originals for another visual pass. If an original is no longer available in the current window, the Assistant asks the user to add it again. The user can choose a built-in Lucide icon, persisted with the Assistant name, language, and style.

The first installed launch requires one unified work authorization covering local files/media, Obsidian Vault access, configured model connections, and user-initiated public-link capture. Its decision is persisted in local SQLite and must not be requested again on later launches using the same application data. Before authorization, Vault scanning, model connections, and background work remain disabled; only permission management in Settings is available. This application-level decision never replaces or bypasses native macOS or Windows permissions.

After authorization, a versioned three-step onboarding flow covers starting from local knowledge, resuming the last context, and using the Assistant beside the current object. Completion or skip is persisted; Assistant personalization follows the guide.

It cannot open or modify Settings, elevate imported content, or bypass the Command Bus, Policy Engine, Task Runtime, and Operation Log.

### 5. Obsidian and local knowledge

Yunspire discovers local Vaults and lets users choose query scope. When no Vault is selected, the first run initializes an Agent Vault with `知识库`, `原子库`, `资料库`, `收件箱`, `画像`, and `长期记忆/行为记录`; source categories inside `资料库` are created only when justified by actual content, user choice, or an AI classification. It also initializes a Personal Vault with reports, ideas, projects, and `创作成品`, whose internal categories are chosen by the user or inferred by AI when requested.

The Obsidian capability covers read, create, update, move, rename, Properties, tags, Wiki Links, attachments, folders, Graph configuration, soft delete, and restore. Knowledge-graph rendering and interaction must stay in Obsidian's native Graph View; Yunspire does not create a second graph canvas. Writes check conflicts and commit atomically. Confirmed deletion moves the target into Yunspire Trash; permanent physical deletion requires an explicit user action.

### 6. Capture

The global Capture action opens a direct capture workspace where users can submit links, text, local files, or folders and inspect real processing stages, cancellation, and final outcomes. Scheduled-capture creation, modification, pause, resume, immediate run, retry, and deletion continue through model-analyzed Assistant requests; the workspace displays the resulting schedules, execution steps, history, and outcomes.

Supported inputs include public webpages, user-authorized sources, text, Markdown, PDF, Word, PowerPoint, Excel, images, audio, video, local files, and folders. First-party processors preserve provenance, extract content, normalize tabular data to JSON, involve a configured model in every analysis, produce summaries/tags/structure, pass deterministic gates, and atomically write related originals, attachments, and analysis results. A normalized extraction hash deduplicates ready, writing, or committed content across tasks and sources while recording the original record and skip reason. Valuable video frames have no fixed count limit but remain bounded by runtime, storage, and model budgets.

macOS uses PDFKit, AVFoundation/ImageIO, and Speech for local PDF/media/transcription work. Windows uses Windows.Data.Pdf, Media Foundation/WIC, and local SAPI. The Windows installer carries a pinned-hash official embeddable Python runtime, so end users do not install Python. If no installed offline SAPI recognizer exactly matches the requested locale, Yunspire returns `windows_sapi_language_unavailable` rather than falling back to the wrong language or reporting false success.

Office v2 extraction is position preserving. Word retains body/story order, tables, images, fields, links, sections, and supporting parts. Excel reads every worksheet, keeps coordinate cells and formulas before producing a cleaned JSON view, marks cached values as not recalculated, and anchors images to ranges and row/column context. PowerPoint follows real slide order and retains element geometry, layering, crop data, tables, and inherited layout/master provenance; spatial proximity remains a candidate rather than a semantic fact. Every parser emits `integrity.status/errors/checks`; any incomplete required story, worksheet, slide, image relationship, Drawing, or placement evidence is a blocking ingestion error, never a partial success.

External images in webpage body flow, Markdown image syntax, and OOXML image relationships use a dedicated localizer that accepts only public `http/https` images, resolves DNS and every redirect, rejects private/loopback/link-local destinations, verifies MIME and actual image format, streams into isolation, and hashes with SHA-256. A successful asset returns to the exact paragraph, Markdown line/column, Word position, Excel anchor, or PowerPoint element through an `attachment://<reference_id>` placeholder. Identical bytes deduplicate by `asset_id`; configured-model analysis runs once per unique asset, while the deterministic writer places the resulting observation, visible text, context, evidence, and confidence at every occurrence-level `reference_id`. Original bytes remain untouched; if a temporary model derivative is necessary, each request binds its asset ID, original/derivative SHA-256, byte lengths, and allowed reference IDs, validates the transmitted derivative, and releases it after its bounded batch. Dynamic disk, available-memory, decode, and request gates are safety conditions rather than product file-size limits.

Ordinary embedded URLs remain inert with `auto_open=false` and `auto_fetch=false` until the user explicitly requests a separate capture. A linked-image download, redirect, type, hash, staging, or placement failure blocks complete ingestion and reports its precise cause. The selected Vault, or Personal by default, receives source-faithful Markdown, in-place assets, full structure JSON, and provenance. The Agent Vault receives model-interpreted Markdown, per-image analysis, tags, Wiki Links, and related notes under `资料库/原文/`. Both writes and their assets use stable targets with the full normalized content SHA-256 as a directory and the readable title as the basename, so Obsidian Graph shows readable node names; they share one analysis receipt and commit as one cross-Vault batch. Equal titles with different content never collide, and capture batches cannot overwrite existing targets. Full structure and attachments are preserved while model requests are byte-bounded batches rather than file truncation. Entity graphs remain deferred; search maintains rebuildable local feature vectors and RRF hybrid ranking.

Yunspire does not bypass login, cookies, CAPTCHA, DRM, encrypted media, or platform access control. It guides users through lawful authorization and processes only content they may access or have exported.

### 7. Models, search, creation, and Skills

Users may add or remove providers, assign multiple models behind one endpoint/key or across different providers, and select final models for conversation, analysis, and image generation. The primary UI shows selected models rather than every discovered model. Chat, analysis, image generation, and image editing use role-correct provider endpoints. Request usage records capture model, role, tokens, duration, cost source, and errors without credentials, and long-running requests can be cancelled. Errors expose the real provider, endpoint, or model cause.

Search covers current or explicit cross-Vault scope, filenames, content, Properties, tags, links, sources, timestamps, and content types. Chinese lexical search uses CJK character pairs, while a deterministic local feature vector uses characters, terms, titles, paths, tags, and Wiki Links. With explicit consent, an external neural-embedding index is also available. It is off by default: query requests send the search text, and note indexing sends title, relative path, tags, Wiki Links, and at most 24,000 normalized body characters, but not Properties or Vault IDs. Lexical, local-vector, and available neural-vector ranks are fused with standard `1 / (60 + rank)` RRF. Missing consent, configuration, or valid vectors never disables FTS and local vectors. Results retain local Vault IDs and canonical relative paths, show provenance, and support an in-app read-only viewer plus an explicit Obsidian launch action.

Assistant conversation search must cover titles, metadata, and persisted bodies. Body indexing stays entirely in local SQLite, supports ASCII and CJK terms, and strictly filters the current `workspace_scope`; queries are capped at 512 characters and 100 results. `workspace_messages` remains authoritative, while FTS is a rebuildable projection refreshed transactionally with updates, individual deletes, and whole-conversation deletes. The desktop must ignore late query results and retain title/metadata fallback when the native runtime is unavailable.

Creation uses `CreationDocumentV2` for canonical Markdown, derived blocks, image and file assets, cross-Vault sources, layout, publishing state, provenance, validation receipts, and export-only readiness. The editor preserves tables, code fences, Properties, tags, Wiki Links, citations, and images at arbitrary positions, while title collision handling prevents an existing draft from being overwritten. An article body, its image count, and its total byte size have no product-level fixed ceiling. The desktop runtime controls resource use through durable assets, chunked transfer, strict per-event bounds, and model-context-sized batches. When saving to a Vault, final Markdown is staged in chunks through `uploadDurableText`, then `prepare_note_write_from_durable_asset` validates UTF-8, line counts, and SHA-256 as streams before committing the note and images in one recoverable atomic batch. The 4 MiB threshold only switches between a full line diff and a bounded preview containing before/after byte counts, line counts, and complete SHA-256 values; it never rejects, truncates, or summarizes a large article.

Both `creation.generate` and `creation.edit` use the native WritingRun runtime. SQLite persists WritingRuns, strictly ordered Creation Agent Stream events, stable checkpoints, and per-request usage ledgers, with only one active run per document. Native provider streaming is decoded into real `contentDelta` events carrying `providerSequence`; the front end rejects duplicate, reversed, gapped, wrong-channel, or identity-mismatched events and requires their concatenated text to match the final model receipt byte for byte. A final full response may never be substituted for a missing stream. AI generation, full-text optimization, panel selection edits, and the floating Polish/Rewrite/Imitate/Custom actions all create review candidates first. Model requests and WritingRuns can be cancelled, and restart recovery replays the native base document, checkpoint, and complete event sequence after revalidating document ID, revision, input hash, and candidate hash. Every `creation.generate`, `creation.edit`, grounding-verification, and brand-evaluation request records provider, model, tokens, duration, estimated cost, state, and error and aggregates them by WritingRun. Passing candidates enter an explicit diff review and cannot change the current body before acceptance.

Candidate acceptance runs as a native transaction that rechecks the base revision, input hash, checkpointed candidate hash, Creation validation, and block-level grounding. A cross-Vault `SourceRef` persists `vaultId`, `relativePath`, body and excerpt SHA-256 values, a reviewable excerpt, and its block relation; any stale source identity, hash, quotation, or `supported` verdict blocks acceptance. Original images recover through the durable-asset registry, while the current `CreationDocument.assets` manifest defines the image set for that revision. A successful Vault commit writes the real path, content hash, `localized` state, and timestamp back to the document; rejection or failure cleans up or rolls back staged data. Custom themes, components, and templates use the native Manifest revision registry with validation, listing, version history, archive, and restore. `BrandProfile` supports create/update, approval, archive, delete, document bind/unbind, and evaluation. PDF, PNG, and JPEG exports render one page at a time and release each canvas promptly; multi-page PNG/JPEG output is a ZIP with a Manifest. Copy and export remain subject to Readiness, and receipts record format, page count, byte length, target, and SHA-256 without claiming external publication.

System Skills remain hidden in the background and never appear in the user Skill list. Users create, install, update, query, and run Skills through the Assistant; the Workbench Skills panel exposes user-Skill status, routing evidence, permissions, run data, version history, activation, retirement, and restoration. Third-party installation accepts only a user-supplied GitHub `SKILL.md`, never clones the repository, executes scripts, downloads other files, or inherits external capability declarations, and records the normalized source URL, revision, and content hash. Creation and update produce candidates; after the user explicitly confirms a third-party installation, Yunspire immediately runs deterministic safety evaluation, automatically records the installation confirmation as approval and enables the version by default only when every check passes, while failed versions remain rejected, disabled, and unroutable without a second approval prompt. Every run freezes `version` and `payloadHash`; the native executor rechecks enabled state, deterministic evaluation, recorded approval, and input/output schemas before and after the model call, and records started/succeeded/failed/cancelled audit events. Skill instructions and declared capabilities are content-conversion data only and never grant file, network, Shell, Settings, or other side effects. The first-party controlled Deep Research Skill runs plan, evidence collection, contradiction review, synthesis, citations, and reflection under policy, budget, cancellation, checkpoint, and provenance controls without direct Vault writes or permission expansion.

Each run must append structured effects containing execution/request/task/trace identity, Skill version, input hash, optional output hash, outcome, warnings, error, and timestamps. Effects and their `acceptance`/`correction` links are immutable and queryable by Skill, request, task, trace, and outcome. The Renderer may query ordinary effects but cannot forge native-runtime reserved task evidence.

### 8. Tasks, reports, optimization, and privacy

Tasks expose durable states, steps, progress, budgets, checkpoints, pause, resume, retry, and failure reasons. New native tasks may bind a versioned schema `1.0` typed DAG. Execution locks the plan revision, and deterministic `all_of` completion requirements block `succeeded` until enough current-revision, canonical, hashed evidence exists. Identical evidence is idempotent; a reused ID cannot change content, and client snapshots cannot overwrite contract-owned tasks. Recovery revalidates the current contract before changing the native terminal state.

The runtime atomically claims dependency-ready frontier steps with worker, attempt, lease, cancellation fence, and reserved step/tool-call/runtime/token/cost budgets. Read-only frontier steps may fan out; an active effectful step is a barrier and effectful work is serialized. Lease expiry releases reservations and appends an `expired` receipt. Parent cancellation releases reservations, appends `cancelled` receipts, advances the fence, rejects late settlement, and recursively cancels bound children.

A capability step executes through an `origin=runtime` child command exactly bound to runtime task, plan revision, step, and claim. It cannot carry a new plan, reuse a model receipt, or masquerade as direct-user/system-maintenance authority. Rust verifies that capability, operation, trace, Vault, relative paths, network targets, declared scope, and budget do not exceed parent/step authorization, and requires the bound child to succeed before the step can succeed. Rust converts successful receipts into reserved `runtime.step_receipt` completion evidence in the same transaction; public IPC rejects forged `runtime.*`, `schedule.dispatch_ack`, runtime-source, and scheduler-source evidence.

The Assistant product path must execute `capability-main -> verify-result`: claim and execute capability work, claim verification, and reread the current contract before settling the parent. The product planner currently emits this two-step path. Low-level read-only fan-out is not a general multi-Agent planner, and receipt verification does not replace domain-specific semantic quality gates.

Every scheduled timestamp has a stable occurrence with a native wrapper task, `schedule_dispatch` step, completion contract, and trace. Occurrences for one schedule run serially. Rust records the dispatch acknowledgement only after verifying at least one real child task accepted by the Command Bus and Policy Engine and bound to the exact occurrence, wrapper, schedule kind, and scheduled time. Skips, no-ops, internal failures, and Renderer-only assertions cannot satisfy completion, and deleting a schedule does not erase occurrence history. Daily, weekly, monthly, and annual reports are saved to Obsidian first. Background optimization uses immutable evidence cursors, rejects permission expansion, preserves candidate/evaluation/application versions, and supports rollback; the Assistant submits every change for user review. Memory V2 separates `user_episode`, `user_profile`, `agent_case`, and `agent_skill`, requires exact user/agent/app/project/session scope, retains evidence, confidence, versions, replacement, expiry, and tombstones.

Reflection jobs freeze a replayable source snapshot/hash, source documents, terminal Skill effects, metrics, and evidence cursor. They support queued/running/awaiting_review/completed/failed/cancelled, atomic claim tokens, 5-to-900-second leases, expiry reclaim, startup recovery, renewal, and cancellation. Workers replay only the frozen source and first create non-recallable drafts. A bound optimization must atomically activate memory, apply the candidate, complete the job/binding, and append `acceptance`; reject/revise tombstones the draft, supersedes the binding, appends `correction`, and revision requeues the job. The current desktop flow does not periodically renew task-step or reflection leases, so exceptionally long single steps/reflections must finish within their lease or be reclaimed and retried. Browser mode is not acceptance evidence for Tauri IPC, SQLite transactions, or native permissions.

Vaults, tasks, schedules, Skills, reports, conversations, operation events, and configuration stay local. API keys use AES-256-GCM; long-term memory excludes credentials and complete binary attachments; SQLite uses transactions, WAL, migrations, backups, and integrity checks. External connectors require user-configured HTTPS endpoints, encrypted credentials, deterministic policy, and user confirmation. Update protection snapshots SQLite and connected Vaults before installation and creates another safety point before rollback. Release manifests must explicitly record whether macOS and Windows installers are signed; version 0.3.0 is currently an unsigned release, so Gatekeeper and SmartScreen warnings remain an operating-system boundary. Automatic in-app download is not claimed. Source packages exclude Vaults, databases, keys, logs, caches, screenshots, and machine-specific paths.

### 9. Explicitly deferred

Version 0.3.0 does not include accounts, entity graphs, entity disambiguation, typed entity relations, multi-hop queries, remote vector databases, learned semantic reranking, unconfigured external delivery, generic remote control, bypasses for third-party access controls, or mobile-specific navigation/components/responsive acceptance. Neural embeddings are limited to the explicit-consent, provider-backed, locally cached retrieval path described above.
