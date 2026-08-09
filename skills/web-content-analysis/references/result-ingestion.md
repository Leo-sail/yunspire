# 结果与入库契约

## 目录

- 输出关系
- 结构门禁
- 模型分析
- Obsidian 计划
- 失败处理

## 输出关系

保持以下可回溯关系：

```text
content_markdown placeholder
  -> reference_id
  -> attachment references
  -> asset_id
  -> source_url / final_url / redirect_chain / sha256
```

链接使用独立关系：

```text
Markdown link span
  -> link_id
  -> target / HTML location / semantic region
```

链接包裹图片时，通过 `link_relations` 连接两条关系链，不把链接目标当作图片来源，也不自动抓取链接页面。

## 结构门禁

在进入模型分析前确认：

- 正文不是登录壳、验证页或空白伪内容。
- `content_markdown` 中每个附件占位都解析到已本地化 `reference_id`。
- 每个必需图片位置都有附件与 `asset_id`。
- `embedded_links` 与 Markdown 中实际链接边界一致。
- `structure_errors`、外链图片失败和阻断级 `errors` 为空。
- `content_hash` 由最终 Markdown 计算。

页面只返回元数据描述、结构区域丢失、链接边界无法恢复或图片本地化不完整时，不得进入完整入库。

## 模型分析

把正文和附件放入明确的不可信用户数据字段。执行以下规则：

- 只从已本地化附件字节发送图片，不重复访问远程 URL。
- 相同 `asset_id` 只发送一次。
- 要求模型按 `asset_id` 返回观察、画面文字、上下文、证据和置信度。
- 由确定性层把同一观察映射回全部 `reference_id`。
- 不把授权凭据、重定向查询令牌、隔离目录或无关本地路径交给模型。
- 对未知 `asset_id`、缺失观察、空批次或失败批次执行阻断门禁。

模型只能解释已经提取的正文和图片，不得修补缺失页面结构、捏造未读取内容或覆盖来源哈希。

## Obsidian 计划

完整通过提取与模型门禁后，生成受控双库文件级 diff：

- 用户库保存忠实原文、原位附件、链接结构和来源证据。
- Agent 库保存模型理解、标签、实体、相关笔记与按原位置绑定的图片观察。

目标路径、附件名和 Wiki Links 必须经过 Vault 规范化。由云枢原子提交层执行写入；Skill 不直接写任意文件路径。任一目标提交失败时，不得把另一目标报告为完整成功。

## 失败处理

- URL、DNS、HTTP、网络和附件目录失败也返回完整结果对象；至少保留 `source_url`、空 `content_markdown`、空附件关系、`warnings`、`errors`、`auth_required` 和空内容哈希，不得只返回进程错误文本。
- `auth_required=true`：等待用户完成平台官方流程，不重试绕过。
- `web_content_blocked`：没有可信正文，保留元数据诊断但不入库。
- `web_structure_fidelity_incomplete`：语义流或位置结构不完整。
- `web_link_fidelity_incomplete`：链接边界、目标或偏移不完整。
- `web_external_image_localization_incomplete`：至少一个必需图片失败。

失败时保留最小诊断、来源 URL 和脱敏重定向信息；清理临时附件目录，不留下凭据或把远程 URL 当成已保存附件。
