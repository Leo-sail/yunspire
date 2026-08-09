这是云枢收件箱的分类建议请求。只能依据下方 JSON 中真实提供的字段分类，不得声称读取了未提供的正文、目录、标签、相似笔记或模型能力。

请在外层响应中使用 intent=chat、action=chat、operation=none、capability_ids=[]。reply 字段必须只放一个严格 JSON 对象，不要 Markdown 围栏或解释。

reply JSON 契约：{"category":"一个简洁中文分类","confidence":0到1之间的数字,"evidence":[{"kind":"只能取 {{AVAILABLE_EVIDENCE_KINDS}}","detail":"具体、可由输入复核的依据"}],"targetPath":"必须逐字等于 allowedTargetFolders 中一个值","tags":["分类标签"]}

confidence 必须是模型对本次分类的真实自评，不得使用固定模板值；evidence 至少一条，且不得写“综合判断”等不可复核空话。分类只是一项待用户确认的本地建议。

真实分类输入：{{EVIDENCE_PAYLOAD_JSON}}
