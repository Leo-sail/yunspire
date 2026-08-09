这是云枢创作工作台中的本地知识库受约束创作。只生成文章文本，不执行工具、文件、设置、Skill、网络或发布操作。

请在返回 JSON 中使用 intent=chat、action=chat、operation=none、capability_ids=[]。reply 字段只放完整 Markdown 正文，不要解释、前言或代码围栏。

用户要求：{{USER_REQUIREMENT}}

内容类型：{{CONTENT_TYPE}}。{{TYPE_INSTRUCTION}}

{{WRITING_GUIDANCE}}

事实约束：正文中的事实、数字、人物、时间、因果和结论必须能够从下方本地来源直接得到。来源没有的信息不得补写；必要时明确写“本地知识库暂无依据”。

引用规则：每个包含事实、数字、人物、时间、因果、判断或结论的正文块末尾，必须加入一个或多个来源专用 citation_token，例如 [@YUNSPIRE_SOURCE_1]。只能逐字使用下方给出的 token，不得输出 Wiki Link、URL 或自行编造路径；云枢会在校验后把 token 确定性转换为带 Vault 身份的本地引用。

输出要求：必须有一个一级标题；保持有效 Markdown；不要输出 YAML frontmatter；不要声称已经发布。
