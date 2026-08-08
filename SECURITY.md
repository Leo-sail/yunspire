# 安全策略 / Security Policy

## 中文

### 支持版本

我当前只为仓库默认分支上的最新版本提供安全修复。当前版本为 `0.3.0`。

### 私下报告漏洞

请不要先公开可能导致 Vault 数据泄露、凭据暴露、任意文件访问、权限绕过、远程执行或数据破坏的问题。请发送邮件至 `leochang210@gmail.com`，标题使用“Yunspire Security”。

报告请包含：

- 受影响版本、操作系统及其版本。
- 前置条件、最小复现步骤和实际影响。
- 相关日志或截图，但不要包含真实密钥和私人知识内容。
- 你已经尝试的缓解方式。
- 是否已在其他位置公开。

我会先确认收到，再评估影响、修复范围和披露时间。未经我的明确确认，请不要访问不属于你的数据，也不要进行破坏性、持久化或社交工程测试。

### 安全边界

云枢的设计边界包括：本地优先、内容与指令隔离、模型无直接权限、一次性执行票据、确定性策略、Vault 范围校验、持久跨库提交、原子写入、加密密钥和操作事件。第三方模型供应商、Obsidian、macOS、Windows 与外部网站仍适用各自的安全和服务边界。

## English

### Supported version

I currently provide security fixes only for the latest version on the repository's default branch. The current version is `0.3.0`.

### Private vulnerability reporting

Do not first disclose an issue that may expose Vault data or credentials, allow arbitrary file access, bypass policy, execute code remotely, or destroy data. Email `leochang210@gmail.com` with the subject “Yunspire Security”.

Include the affected Yunspire, operating system and version, prerequisites, minimal reproduction, impact, sanitized evidence, attempted mitigations, and whether the issue is already public. Do not include real secrets or private knowledge content.

I will acknowledge the report and evaluate impact, remediation, and disclosure timing. Without explicit authorization, do not access data that is not yours or perform destructive, persistent, or social-engineering testing.

Yunspire's security boundary includes local-first storage, content/instruction isolation, no direct model permissions, single-use execution tickets, deterministic policy, Vault scope validation, durable cross-Vault commits, atomic writes, encrypted keys, and operation events. External model providers, Obsidian, macOS, Windows, and websites retain their own security and service boundaries.
