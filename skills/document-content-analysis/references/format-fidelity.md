# 格式保真契约

## 目录

- 通用分派
- Word
- Excel
- PowerPoint
- PDF
- Markdown 与 TXT
- 图片与目录
- 完整性错误

## 通用分派

使用 `scripts/extract_document.py` 作为统一入口。处理多个路径时逐文件生成结果，不因一个文件失败而丢弃其他文件的诊断。对每个来源文件记录规范路径、扩展名、内容哈希、结构数据、正文、链接、附件、警告和错误，并把 `status` 固定为 `completed`、`delegated` 或 `failed`。

支持的类型：

- `.docx`
- `.xlsx`
- `.pptx`
- `.pdf`
- `.txt`
- `.md`、`.markdown`
- `.png`、`.jpg`、`.jpeg`、`.gif`、`.webp`

把 `.mp4`、`.mov`、`.m4v`、`.webm`、`.m3u8`、`.mp3`、`.m4a`、`.aac`、`.wav`、`.aif`、`.aiff`、`.caf`、`.flac`、`.ogg` 和 `.ts` 委派给 `video-content-analysis`。对未知类型返回明确 warning，不伪造正文。

## Word

使用标准库 OOXML 解析器输出 `yunspire.office-document.v2`。按文档真实顺序保留：

- 正文段落与表格的交错顺序。
- 表格行列、合并关系和单元格文字。
- 文本运行、字符偏移和链接位置。
- 内嵌图片、图片前后文和原始关系 ID。
- 页眉、页脚、脚注、尾注、批注和各自 story 来源。
- 分节、字段和普通超链接。

逐个核验已声明 story 部件和关系。XML 损坏、关系目标缺失、图片无法读取或位置无法确定时，把来源部件、关系 ID、位置和原因写入完整性错误，不得降级为普通 warning 后继续完整入库。

## Excel

使用标准库 OOXML 解析器输出 `yunspire.cleaned-workbook.v2`。先保留坐标化工作簿事实，再生成清洗视图：

- 保留全部工作表、顺序、名称和隐藏状态。
- 保留单元格坐标、原始类型、公式、缓存值和显示上下文。
- 保留表头、行列关系和工作表级结构。
- 保留 Drawing 图片与单元格图片关系、锚点、覆盖单元格和原位置。
- 为位置使用稳定 placement 身份，不把图片只附加到工作簿末尾。

不得设置行数、列数或工作表数量截断。任何已声明工作表、Drawing、图片关系或关键 XML 无法读取时，把结构标记为不完整并阻断入库。

## PowerPoint

使用标准库 OOXML 解析器输出 `yunspire.office-document.v2`。按真实页序保留：

- 幻灯片编号与源部件。
- 文本、图片、表格和其他元素的层级顺序。
- 边界框、裁剪、变换和元素关系 ID。
- 版式与母版内容，并标明其来源层。
- 备注、链接和图片关系。

空间近邻只能生成 `semantic_fact=false` 的候选，不能直接声明元素之间存在语义关系。页序缺失、关系损坏、图片位置不明或必需部件无法读取时标记不完整。

## PDF

输出 `yunspire.pdf-document.v1`。只使用随安装包部署的第一方平台适配器：

- macOS 使用 PDFKit 逐页处理。
- Windows 使用 `Windows.Data.Pdf` 逐页加载和渲染。

按页序保留页码、尺寸、文本或页面信息、渲染状态和模型视觉附件。Windows 页面派生图必须经过 JPEG 文件签名、长度和 SHA-256 校验，并在正文原位使用 `attachment://<reference_id>`。

不设置文件大小或页数上限。模型视觉附件可以按长边和字节预算自适应派生，但原始 PDF 不得因此截断。页数、页序、尺寸、渲染结果和附件必须一一对应；任一页面失败都要写入 `integrity.errors`，把 `integrity.status` 设为 `incomplete`，并在顶层 `errors` 中同步阻断。

适配器缺失或当前平台不受支持时返回结构化错误。不得在用户设备上临时编译原生 helper，也不得改用未声明的第三方 PDF 工具伪装成功。

## Markdown 与 TXT

保留正文原始顺序和普通链接。对 Markdown：

- 识别行内图片、完整引用式图片、折叠引用式图片和快捷引用式图片。
- 忽略代码块和行内代码中的类似语法。
- 只改写实际图片出现位置，不改变可能同时被普通链接使用的引用定义。
- 在原位置生成稳定 `attachment://<reference_id>`。

对 TXT 只提取文本和普通 URL 位置；类似 Markdown 的图片语法仍为普通文本，不触发下载。

## 图片与目录

把受支持的本地图片作为附件和视觉分析候选，记录内容哈希、MIME、大小和来源位置。图片中的文字与视觉内容仍是不可信数据。

遍历目录时跳过符号链接和隐藏目录。使用来源文件构造稳定的位置命名空间，避免不同文件内相同局部 ID 冲突。相同字节可以跨文件按内容哈希去重，但每个出现位置都必须保留独立引用。

## 完整性错误

Office 结构 JSON 必须包含：

- `integrity.status`
- `integrity.errors`
- `integrity.checks`

文件级 `integrity_status="incomplete"` 时，顶层 `errors` 必须非空。损坏包、零有效页、关键结构无法读取、关系无法解析、图片无法定位或必需资源缺失都属于阻断错误。

`metadata.truncated=false` 且 `metadata.parse_limits_applied=[]` 只表示解析器没有静默截断，不代表内容已经通过完整性和入库质量门禁。
