# 提取与本地化契约

## 目录

- 请求安全
- 一次性授权
- 正文提取
- 链接保真
- 图片发现
- 图片本地化
- 安全边界

## 请求安全

对页面和每张图片的每次请求：

- 只允许 HTTP 或 HTTPS。
- 拒绝 URL 中的用户名和密码。
- 规范化 IDN 主机、端口、路径和查询。
- 解析 DNS 并拒绝非公网地址。
- 把连接固定到已校验地址，同时为 HTTPS 保留正确 SNI 和证书校验。
- 逐跳验证重定向，限制跳数，禁止 HTTPS 降级为 HTTP。
- 移除未授权的 Cookie、Authorization、Host 和 Connection 头。

页面响应必须使用可处理的身份编码。超过 8 MB 页面响应安全边界时失败，不截断后继续解析。

## 一次性授权

只接受用户主动提供的临时 Cookie 或平台官方 Bearer 令牌。通过标准输入传入：

```json
{
  "allowed_hosts": ["example.com"],
  "headers": {
    "Cookie": "temporary-value",
    "Authorization": "Bearer temporary-value"
  }
}
```

只向 `allowed_hosts` 中的精确主机发送敏感头。跨域页面或图片重定向自动剥离授权。任务完成、失败或取消后销毁授权，不写入磁盘、日志、警告、附件元数据或模型输入。

401、403、验证码、风控页、登录壳或空壳内容应返回 `auth_required=true` 和明确错误。不得破解验证或把页面描述伪装为正文。

## 正文提取

解析 HTML 标题、Open Graph、JSON-LD 和语义内容流。执行以下规则：

- 忽略 `script`、`style`、`noscript`、`svg`、导航和页面级站点外壳。
- 优先保留 `article` 或 `main` 内的内容。
- 保留语义流内部的 `header` 和 `footer`，包括标题、导语、hero 图、署名和脚注。
- 保持段落、标题、列表、表格、引用、链接和图片的来源顺序。
- 只有 HTML 内容流为空时才使用 JSON-LD `articleBody`。
- 只有原文与结构化正文都不可用时，才把元数据描述作为明确标注的降级结果；该结果不得通过完整入库门禁。

把 Open Graph 或 JSON-LD 中不在正文流内的附图放入“来源附图”附录，并记录来源类型；不得假装它们位于正文中的特定段落。

## 链接保真

对正文中的每个 `<a href>` 记录：

- 稳定 `link_id`。
- 显示文字和规范化目标。
- HTML 行列、语义区域和 Markdown 偏移。
- `auto_open=false`、`auto_fetch=false`。
- 是否需要用户另行创建采集任务。

提取阶段不访问普通链接。链接包裹图片时，让链接记录和图片 `reference_id` 双向关联，图片仍只走图片本地化流程。边界标记缺失、链接目标无法恢复或 Markdown 偏移不一致时写入 `structure_errors` 并阻断入库。

## 图片发现

识别正文流中的 `<img>`、惰性加载属性、`srcset` 和受支持的结构化附图。为每次出现建立独立位置记录，包含：

- `reference_id`、顺序和来源类型。
- 原始与解析后的 URL。
- 替代文字、标题、前后文和语义区域。
- HTML 与 Markdown 位置。
- 包裹它的链接身份。

不得把所有图片统一追加到正文末尾，也不得因为字节相同就合并位置身份。

## 图片本地化

对每张公开 HTTP/HTTPS 图片：

1. 应用与页面相同的公网地址、重定向、授权和 TLS 边界。
2. 流式写入隔离目录并持续检查磁盘余量。
3. 校验声明长度、响应 MIME 和真实文件签名。
4. 计算 SHA-256，并按内容生成 `asset_id`。
5. 对相同字节只物化一份附件，把所有 `reference_id` 追加到附件的 `references`。
6. 把正文原位置改写为 `attachment://<reference_id>`。

单图响应上限为 128 MB，单页全部图片累计响应上限为 1 GB。任一必需图片失败或越界时，保留失败详情并产生 `web_external_image_localization_incomplete`；不得返回部分图片后继续完整入库。

## 安全边界

结果必须明确报告：

- 页面响应 8 MB。
- 单图响应 128 MB。
- 单页图片总量 1 GB。
- 越界行为为 `block_without_partial_write`。

这些是安全失败边界，不是静默截断策略。图片数量不设上限，但总字节、磁盘余量和每个响应都必须通过校验。
