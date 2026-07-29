# 发布流程 / Release Process

## 中文

### 发布边界

仓库发布工作流生成两个经过验证的未签名安装包：

- macOS 通用 DMG，包含 `arm64` 与 `x86_64`。
- Windows x64 NSIS 安装程序。

安装包、校验和与构建清单只作为 GitHub Actions Artifact 或 GitHub Release Asset 保存，不提交到 Git。`installers/`、`artifacts/`、`release/` 和 Tauri 的 `target/` 输出均为本机生成内容。

### 版本准备

发布标签必须与以下位置的版本完全一致：

- `package.json`
- `package-lock.json` 的顶层版本和根包版本
- `src-tauri/Cargo.toml`
- `src-tauri/tauri.conf.json`
- `desktop-ui/index.html`
- `desktop-ui/app.js`
- `skills/video-content-analysis/scripts/yunspire_speech_info.plist`
- `CHANGELOG.md` 中对应的发布节

先更新版本，例如：

```bash
npm version 0.1.2 --no-git-tag-version
```

再更新其余版本位置和中英文更新记录，然后执行：

```bash
npm ci
npm run verify
node scripts/verify-release-artifact.mjs source --tag v0.1.2 --require-windows true
git diff --check
```

### 自动构建与发布

推送匹配版本的标签会启动 `Release Installers` 工作流，并且只有两个平台都通过后才会创建或更新 GitHub Release：

```bash
git push origin main
git tag -a v0.1.2 -m "Yunspire 0.1.2"
git push origin v0.1.2
```

也可以在 GitHub Actions 中手动运行该工作流。`publish=false` 只生成保留 14 天的 Actions Artifact；`publish=true` 只允许从 `main` 运行，并会发布 GitHub Release。若同名标签已指向其他提交，工作流会立即失败，避免覆盖历史版本。

macOS 作业会验证 DMG、挂载应用、核对应用版本并确认两个 CPU 架构。Windows 作业会静默安装 NSIS 包、核对已安装版本、启动应用八秒、卸载并清理测试目录。两个作业都会生成独立的 SHA-256 文件和 JSON 构建清单。

构建前会从锁定的 Cargo/npm 元数据和已安装包内许可文件生成第三方许可汇总。缺少独立许可文件的依赖只能通过精确版本与锁文件哈希审查清单。两个平台的安装后检查都必须确认并核对 `legal/LICENSE`、`legal/NOTICE` 和 `legal/THIRD_PARTY_NOTICES.txt`；Windows 还会核对 CPython `LICENSE.txt` 与官方嵌入式运行时的固定 SHA-256。

发布门禁还要求 Windows 嵌入式 Python、文档与图片解析器、媒体与语音解析器以及平台资源配置全部存在，并执行各解析器的真实冒烟测试。缺少这些资源时不会降级发布一个功能不完整的安装包。

### 签名状态

当前自动化明确使用 Tauri 的 `--no-sign`，产物文件名包含 `unsigned`，Release 说明也会提示 Gatekeeper 与 SmartScreen 风险。正式对外分发前仍需额外配置：

- Apple Developer ID Application 证书、Team ID 和 Apple 公证凭据。
- Windows 代码签名证书或受信任的云签名服务。
- 对应的 GitHub Actions 加密 Secrets，以及工作流中的证书导入、签名和签名验证步骤。

在这些凭据缺失时，不应把未签名产物描述为已签名或已公证版本。

## English

### Release boundary

The repository release workflow produces two verified, unsigned installers:

- A universal macOS DMG containing both `arm64` and `x86_64`.
- A Windows x64 NSIS installer.

Installers, checksums, and build manifests are stored only as GitHub Actions artifacts or GitHub Release assets. They are never committed to Git. `installers/`, `artifacts/`, `release/`, and Tauri `target/` outputs are local generated content.

### Version preparation

The release tag must exactly match the versions in:

- `package.json`
- The top-level and root-package entries in `package-lock.json`
- `src-tauri/Cargo.toml`
- `src-tauri/tauri.conf.json`
- `desktop-ui/index.html`
- `desktop-ui/app.js`
- `skills/video-content-analysis/scripts/yunspire_speech_info.plist`
- The matching release section in `CHANGELOG.md`

Start the version update with, for example:

```bash
npm version 0.1.2 --no-git-tag-version
```

Update the remaining version locations and both changelog languages, then run:

```bash
npm ci
npm run verify
node scripts/verify-release-artifact.mjs source --tag v0.1.2 --require-windows true
git diff --check
```

### Automated builds and publication

Pushing the matching version tag starts the `Release Installers` workflow. A GitHub Release is created or updated only after both platforms pass:

```bash
git push origin main
git tag -a v0.1.2 -m "Yunspire 0.1.2"
git push origin v0.1.2
```

The workflow can also be dispatched manually in GitHub Actions. `publish=false` creates only 14-day Actions artifacts. `publish=true` is accepted only from `main` and publishes the GitHub Release. If an existing tag points to another commit, the workflow fails before replacing any release history.

The macOS job verifies and mounts the DMG, checks the app version, and confirms both CPU architectures. The Windows job silently installs the NSIS package, verifies the installed version, starts the app for eight seconds, uninstalls it, and cleans its test directory. Both jobs generate a separate SHA-256 file and JSON build manifest.

Before bundling, the build generates third-party notices from locked Cargo/npm metadata and the license files shipped by installed packages. A dependency without a separate license file is accepted only through an exact reviewed version and lock-integrity hash. Post-install checks on both platforms verify `legal/LICENSE`, `legal/NOTICE`, and `legal/THIRD_PARTY_NOTICES.txt`; Windows also checks the CPython `LICENSE.txt` against the pinned official embedded-runtime SHA-256.

The release gate also requires the embedded Windows Python runtime, document and image helpers, media and speech helpers, and platform resource configuration. It runs real smoke tests for those helpers and refuses to publish a feature-incomplete fallback installer when any resource is absent.

### Signing status

The current automation deliberately passes Tauri `--no-sign`, includes `unsigned` in asset names, and warns about Gatekeeper and SmartScreen in the release notes. Production distribution still requires:

- An Apple Developer ID Application certificate, Team ID, and Apple notarization credentials.
- A Windows code-signing certificate or trusted cloud-signing service.
- Corresponding encrypted GitHub Actions secrets plus certificate import, signing, and signature-verification workflow steps.

Until those credentials exist, unsigned artifacts must not be described as signed or notarized releases.
