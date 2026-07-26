# 参与云枢 / Contributing to Yunspire

## 中文

感谢你关注云枢。我维护这个项目的产品边界、架构一致性和最终合并决定。提交 Issue 或 Pull Request 前，请先阅读 [README](README.md)、[系统架构](ARCHITECTURE.md)、[产品需求](docs/PRODUCT_REQUIREMENTS.md) 和 [LICENSE](LICENSE)。

### 适合提交的内容

- 可复现的功能缺陷、崩溃、数据一致性或界面问题。
- 不扩大权限、不削弱本地优先原则的改进建议。
- 与当前实现一致的文档、翻译和无障碍修正。
- 能说明输入、预期结果、实际结果和验证方式的代码改动。

实体知识图谱、向量索引、混合检索、账户系统和通用远程控制目前不在开放开发范围，除非我先在产品计划中确认。

### 开发流程

```bash
npm ci
npm run verify
npm run native:clippy
```

提交代码时请：

1. 保持 Obsidian Vault 与 SQLite 的权威边界。
2. 把导入内容和模型输出视为不可信数据。
3. 让所有副作用经过 Command Bus、Policy Engine、Task Runtime 和操作日志。
4. 不提交 Vault、数据库、密钥、日志、缓存、截图、构建产物或本机路径。
5. 同步更新中英双语文档和统一版本字段。
6. 说明验证命令和手工运行结果，不使用演示数据代替真实持久化。
7. 不添加规避登录、Cookie、验证码、DRM 或平台访问控制的实现。

### Pull Request

一个 Pull Request 只解决一个清晰问题。描述中应包含：目标、变更范围、数据影响、风险、验证证据和回滚方式。代码接受不代表授予商业权利；所有使用仍受 [LICENSE](LICENSE) 约束。

## English

Thank you for your interest in Yunspire. I maintain the product boundary, architecture consistency, and final merge decisions. Before opening an Issue or Pull Request, read the [README](README.md), [Architecture](ARCHITECTURE.md), [Product Requirements](docs/PRODUCT_REQUIREMENTS.md), and [LICENSE](LICENSE).

Appropriate contributions include reproducible defects, local-first improvements that do not expand permissions, documentation/accessibility corrections, and focused code changes with clear inputs, expected results, actual results, and verification. Entity graphs, vector indexes, hybrid retrieval, accounts, and generic remote control are out of scope unless I first add them to the product plan.

Run:

```bash
npm ci
npm run verify
npm run native:clippy
```

Preserve the Obsidian/SQLite authority boundary, treat imported content and model output as untrusted, route side effects through the deterministic runtime, exclude all local data and generated output, update both Chinese and English documentation, report real verification, and do not add access-control bypasses.

Keep each Pull Request focused. Describe the objective, scope, data impact, risk, verification evidence, and rollback. Acceptance of code does not grant commercial rights; all use remains subject to the [LICENSE](LICENSE).
