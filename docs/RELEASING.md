# 发布流程 / Release Process

## 中文

### 不可变发布身份

`package.json` 的 `version` 是应用发布版本的权威来源。`package-lock.json`、Tauri、Cargo、应用界面、辅助程序和公开文档中的版本都是受校验的镜像，不得独立决定版本。

每个正式版本必须满足以下恒等关系：

```text
package version 0.3.0
= tag v0.3.0
= tag 剥离后的 commit
= 工作流检出的 HEAD
= macOS manifest.sourceCommit
= Windows manifest.sourceCommit
```

两个平台的 `sourceTree` 也必须完全相同。发布标签必须位于 `main`，不得移动、复用或删除旧标签。

### 源码预检

版本提交合入 `main` 后，在触发发布分支前运行：

```bash
npm ci
npm run verify
node scripts/verify-release-artifact.mjs source --require-windows true
git diff --check
```

发布入口是与版本严格对应的 `release/vX.Y.Z` 分支。该分支必须指向远端 `main` 的当前 HEAD；连接器不需要直接创建标签或 Release。推送前先执行源码身份校验：

```bash
node scripts/verify-release-version.mjs source \
  --source-commit "$(git rev-parse HEAD)" \
  --require-clean true

git push origin HEAD:refs/heads/release/v0.3.0
```

`release/v*` 推送启动正式工作流。其 prepare 作业必须在创建任何远端对象前运行：

```bash
node scripts/verify-release-version.mjs prepare \
  --repository "$GITHUB_REPOSITORY" \
  --release-branch "$GITHUB_REF_NAME" \
  --source-commit "$SOURCE_COMMIT"
```

该门禁要求分支后缀等于 `v${package.version}`、检出 SHA 等于指定 SHA 且等于远端 `main` HEAD，并要求同名 tag 与 Release 均不存在。Release 缺失检查必须使用具备 `contents: write` 的工作流令牌分页枚举调用者可见的全部 Release，再按 `tag_name` 精确匹配；该权限用于读取 draft 可见性，门禁步骤本身不执行写操作。不能使用只返回已公开版本的 `/releases/tags/{tag}` 接口判断 draft 是否存在。通过后，工作流才创建精确指向 `SOURCE_COMMIT` 的标签和未公开 draft Release。创建 Release 时仍使用 `--verify-tag`，禁止 GitHub 隐式选择其他提交。

该工作流只发布稳定版本并设置为 Latest；包含预发布后缀（例如 `-beta.1`）的版本会在 prepare 和 prepublish 阶段被拒绝，不能误标为稳定版。

### 同一提交构建

发布工作流先输出唯一的 `source_commit` 与 `source_tree`。macOS 和 Windows 作业必须按该完整 commit SHA 检出源码，不能各自检出分支尖端，也不能上传本机或其他工作流生成的安装包。

每个平台在最终签名、公证或 Authenticode 处理完成后生成安装包清单。清单至少包含：

- `version` 与 `tag`
- `sourceCommit` 与 `sourceTree`
- 平台和架构
- 最终文件名、字节数与 SHA-256
- 显式且可核验的签名模式（`signed` 与 `signingMode`）
- GitHub 工作流运行标识

发布前必须同时校验两个清单：

```bash
node scripts/verify-release-version.mjs manifests \
  --tag v0.3.0 \
  --source-commit "$SOURCE_COMMIT" \
  --source-tree "$SOURCE_TREE" \
  --manifest artifacts/macos/Yunspire_0.3.0_macOS-universal_unsigned.dmg.manifest.json \
  --manifest artifacts/windows/Yunspire_0.3.0_Windows-x64_unsigned-setup.exe.manifest.json
```

校验器要求清单恰好覆盖 macOS 与 Windows，版本、标签、commit 和 tree 完全相同，并要求两个平台使用一致的签名模式。每份清单必须同时包含布尔值 `signed` 和与其一致的 `signingMode`；当前 `v0.3.0` 使用 `signed: false` 与 `signingMode: "unsigned"`。未来签名版本可以额外传入 `--require-signed true` 作为强制门禁。

### 签名状态与安装边界

用户已选择当前 `v0.3.0` 以未签名形式发布，因此工作流会显式使用 `--no-sign`，文件名包含 `unsigned`，清单也必须如实记录未签名状态。该选择存在无法消除的系统行为：

- macOS Gatekeeper 可能阻止首次打开，并要求用户右键打开或进入“隐私与安全性”授权。
- Windows SmartScreen 可能显示“未知发布者”或额外确认。
- 因为缺少 Apple Developer ID、公证和可信 Authenticode，当前版本不能保证普通安装和首次启动完全没有系统提示，也不能承诺用户无需进入系统设置。
- 麦克风、语音识别、屏幕等受保护能力的必要权限提示与安装包签名无关，不能也不应绕过。

未来正式签名发布时，macOS 应签名应用、嵌套辅助程序和 DMG，完成 Apple 公证与 stapling；Windows 应签名主程序、辅助程序和 NSIS 安装器，并验证 Authenticode 状态为 `Valid`。签名版本不得复用或覆盖当前未签名 Release，而应使用新的版本号和标签。

### 禁止覆盖与发布

prepare 门禁已经在创建前证明 tag 与 Release 均不存在。完成双平台构建、校验和及清单核验后，在把 draft 公开为正式 Release 前还必须运行：

```bash
RUN_PROVENANCE="yunspire-release-run:$GITHUB_RUN_ID:$GITHUB_RUN_ATTEMPT:$SOURCE_COMMIT"
node scripts/verify-release-version.mjs prepublish \
  --repository "$GITHUB_REPOSITORY" \
  --tag "$RELEASE_TAG" \
  --source-commit "$SOURCE_COMMIT" \
  --run-provenance "$RUN_PROVENANCE"
```

该门禁要求远端 `main` 仍严格指向本次构建 commit，远端标签必须是直接指向该 commit 且携带本次 run provenance 的 annotated tag，并要求同名 GitHub Release 恰好只有一个，且仍是目标为该 commit、正文携带同一 provenance 的未公开稳定草稿。第一次通过后必须锁定其数值 Release ID；资产上传、公开为 Latest、最终核验和失败清理都只能操作这个 ID。上传完成后必须在公开 Release 的命令紧前再次运行该门禁并核对 ID 未变化。工作流不得使用 `--clobber`，也不得“更新”一个已经公开的版本。

仓库设置必须在正式发布前由管理员启用 GitHub **Immutable Releases**；Actions 自带令牌没有读取该管理设置的权限，因此触发发布前必须人工确认，发布后再以 Release 对象的 `immutable: true` 作为结果门禁。创建草稿时必须使用 `--verify-tag`，稳定版本必须通过 Release API 显式设置 `make_latest: "true"`。若发布失败，自动恢复只分页定位并按数值 ID 核验本次 run 的未公开草稿与 annotated tag，二者都保留并报警，待人工复核后处理；客户端检查后绝不自动执行删除。

用户选择某个版本时，应进入 `/releases/tag/vX.Y.Z` 并下载文件名中带有同一版本号的资产。只有“获取当前稳定版”的入口可以使用 `/releases/latest`。旧版本 Release 保持独立，不重定向到其他版本资产。

`.github/workflows/windows-installer.yml` 只用正式发布配置执行 Windows CI 构建、静默安装和启动验证，不上传可下载的安装器。该配置固定为当前用户安装、无语言选择器、无独立许可页，并把完整 WebView2 离线安装程序静默内置，因此用户安装时不依赖额外下载；面向用户分发的 Windows 安装包只能来自与版本标签绑定的正式 GitHub Release。

发布后运行：

```bash
gh release verify v0.3.0 --repo Leo-sail/yunspire
gh release view v0.3.0 --repo Leo-sail/yunspire
gh release view --repo Leo-sail/yunspire --json tagName,isDraft,isPrerelease
```

还应从最终 Release 重新下载 macOS 与 Windows 安装包，核对 SHA-256，并在干净系统中完成真实安装和启动验证。

## English

### Immutable release identity

The `version` field in `package.json` is the authority for the application release version. Versions in `package-lock.json`, Tauri, Cargo, the UI, helper programs, and public documentation are validated mirrors and cannot independently select a release version.

Every stable release must preserve this identity:

```text
package version 0.3.0
= tag v0.3.0
= peeled tag commit
= checked-out workflow HEAD
= macOS manifest.sourceCommit
= Windows manifest.sourceCommit
```

Both platform manifests must also contain the same `sourceTree`. The release tag must be on `main`; an old tag must never be moved, reused, or deleted.

### Source preflight

After the version commit reaches `main`, run these checks before triggering the release branch:

```bash
npm ci
npm run verify
node scripts/verify-release-artifact.mjs source --require-windows true
git diff --check
```

The release entry point is a version-bound `release/vX.Y.Z` branch. It must point to the current remote `main` HEAD; the connector does not need permission to create tags or Releases directly. Validate source identity before pushing it:

```bash
node scripts/verify-release-version.mjs source \
  --source-commit "$(git rev-parse HEAD)" \
  --require-clean true

git push origin HEAD:refs/heads/release/v0.3.0
```

The `release/v*` push starts the production workflow. Its prepare job runs before creating any remote object:

```bash
node scripts/verify-release-version.mjs prepare \
  --repository "$GITHUB_REPOSITORY" \
  --release-branch "$GITHUB_REF_NAME" \
  --source-commit "$SOURCE_COMMIT"
```

This gate requires the branch suffix to equal `v${package.version}`, the checked-out SHA to equal both the supplied SHA and the remote `main` HEAD, and the matching tag and Release to be absent. Release absence must be checked with a workflow token granted `contents: write`, paginating every Release visible to the authenticated caller and filtering exact `tag_name` matches; that permission is needed for draft visibility while the gate itself remains read-only. The published-only `/releases/tags/{tag}` endpoint cannot prove that a draft is absent. Only then does the workflow create an exact tag at `SOURCE_COMMIT` and an unpublished draft Release. Release creation still uses `--verify-tag`, preventing GitHub from selecting another commit implicitly.

This workflow publishes stable versions only and marks them Latest. A version with a prerelease suffix such as `-beta.1` is rejected by both prepare and prepublish gates instead of being mislabeled as stable.

### Build both platforms from one commit

The release workflow first emits one `source_commit` and `source_tree`. The macOS and Windows jobs must check out that full commit SHA. They cannot independently check out a moving branch tip or upload installers produced locally or by another workflow.

Each platform creates its artifact manifest only after final signing, notarization, or Authenticode processing. A manifest includes at least:

- `version` and `tag`
- `sourceCommit` and `sourceTree`
- platform and architecture
- final filename, byte length, and SHA-256
- explicit, verifiable signing mode (`signed` and `signingMode`)
- GitHub workflow run identity

Before publication, validate both manifests together:

```bash
node scripts/verify-release-version.mjs manifests \
  --tag v0.3.0 \
  --source-commit "$SOURCE_COMMIT" \
  --source-tree "$SOURCE_TREE" \
  --manifest artifacts/macos/Yunspire_0.3.0_macOS-universal_unsigned.dmg.manifest.json \
  --manifest artifacts/windows/Yunspire_0.3.0_Windows-x64_unsigned-setup.exe.manifest.json
```

The verifier requires exactly one macOS and one Windows manifest, identical release identity, and one consistent signing mode. Every manifest contains both a boolean `signed` field and a matching `signingMode`. The current `v0.3.0` release uses `signed: false` and `signingMode: "unsigned"`. A future signed release can add `--require-signed true` as a strict gate.

### Signing status and installation boundary

The user selected an unsigned `v0.3.0` release. The workflow therefore uses `--no-sign` explicitly, includes `unsigned` in filenames, and records the unsigned state truthfully. This choice has unavoidable operating-system consequences:

- macOS Gatekeeper may block first launch and require a right-click Open action or approval under Privacy & Security.
- Windows SmartScreen may show an unknown-publisher warning or another confirmation.
- Without Apple Developer ID signing, notarization, and trusted Authenticode, this release cannot guarantee a prompt-free install or promise that users will never need a security-settings override.
- Necessary permission prompts for protected microphone, speech, screen, or similar capabilities are separate from package signing and cannot or should not be bypassed.

A future signed release signs the macOS application, nested helpers, and DMG, completes notarization and stapling, and signs the Windows application, helpers, and NSIS installer with Authenticode status `Valid`. It uses a new version and tag rather than replacing this unsigned Release.

### No overwrite and publication

The prepare gate proves that the tag and Release are absent before creation. After both platform builds, checksums, and manifests pass, run this second gate before publishing the draft:

```bash
RUN_PROVENANCE="yunspire-release-run:$GITHUB_RUN_ID:$GITHUB_RUN_ATTEMPT:$SOURCE_COMMIT"
node scripts/verify-release-version.mjs prepublish \
  --repository "$GITHUB_REPOSITORY" \
  --tag "$RELEASE_TAG" \
  --source-commit "$SOURCE_COMMIT" \
  --run-provenance "$RUN_PROVENANCE"
```

This gate requires remote `main` to still point exactly to the built commit, requires the remote tag to be an annotated tag pointing directly to that commit and carrying this run's provenance, and requires exactly one matching GitHub Release to remain an unpublished stable draft targeting the commit with the same provenance in its body. The first successful gate locks the numeric Release ID; asset upload, publication as Latest, final verification, and failure cleanup may operate only on that ID. Run the gate again immediately before publication and require the ID to remain unchanged. The workflow never uses `--clobber` and never updates an already published version.

An administrator must enable GitHub **Immutable Releases** before production publication. The built-in Actions token cannot read that administration setting, so it is confirmed manually before the trigger and enforced after publication by requiring the Release object to report `immutable: true`. Draft creation uses `--verify-tag`, and stable publication sends `make_latest: "true"` through the Release API. If publication fails, automation only paginates and validates this run's draft by numeric ID together with its annotated tag; both are preserved and reported for manual review, with no automatic deletion after client-side checks.

When a user selects a version, direct them to `/releases/tag/vX.Y.Z` and an asset whose filename contains that same version. Only a generic current-stable download entry may use `/releases/latest`. Historical Releases remain independent and never redirect to another version's assets.

`.github/workflows/windows-installer.yml` uses the production release configuration only for Windows CI build, silent-install, and startup verification; it uploads no downloadable installer. That configuration is fixed to current-user installation with no language selector or separate license page, and it silently embeds the complete offline WebView2 installer so setup needs no additional download. The only user-facing Windows installer comes from the formal GitHub Release bound to its version tag.

After publication, run:

```bash
gh release verify v0.3.0 --repo Leo-sail/yunspire
gh release view v0.3.0 --repo Leo-sail/yunspire
gh release view --repo Leo-sail/yunspire --json tagName,isDraft,isPrerelease
```

Finally, download both installers from the published Release, verify SHA-256, and perform real installation and launch checks on clean macOS and Windows systems.
