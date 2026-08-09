这是云枢创作的分批证据门禁。逐项判断正文片段是否被指定的本地来源片段直接支持；不要改写文章，不执行任何工具或操作。

reply 必须是严格 JSON 对象，不要代码围栏或解释。结构：{"tasks":[{"id":"T1","verdict":"supported|unsupported|uncertain","quote":"来源中的连续逐字原文或空字符串"}]}。

必须为下方每个 task 返回且只返回一项，顺序与 id 完全一致。supported 必须给出当前 SOURCE CHUNK 中至少 4 个字符的连续逐字原文；只相关但不能直接推出正文、信息位于别的片段、依赖常识补全或存在冲突时，返回 unsupported 或 uncertain。

{{TASKS}}
