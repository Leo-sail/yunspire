import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  appendFile,
  open,
  readFile,
  stat,
} from 'node:fs/promises';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const execFileAsync = promisify(execFile);
export const repositoryRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const releaseVersionPattern = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/u;
const commitPattern = /^[0-9a-f]{40}$/u;
const versionCapture = '([0-9]+\\.[0-9]+\\.[0-9]+(?:-[0-9A-Za-z.-]+)?)';

function extractVersion(text, pattern) {
  return text.match(pattern)?.[1] || null;
}

function plistStringValue(text, key) {
  const pattern = new RegExp(`<key>${escapeRegExp(key)}</key>\\s*<string>([^<]*)</string>`, 'u');
  return text.match(pattern)?.[1] || null;
}

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/gu, '\\$&');
}

function normalizedCommit(value, label) {
  const commit = String(value || '').trim().toLowerCase();
  if (!commitPattern.test(commit)) throw new Error(`${label} is not a full Git commit SHA: ${value || 'missing'}`);
  return commit;
}

async function tauriVersion(root, configuredVersion) {
  if (releaseVersionPattern.test(configuredVersion || '')) return configuredVersion;
  if (!configuredVersion || typeof configuredVersion !== 'string') return configuredVersion;
  const packagePath = path.resolve(root, 'src-tauri', configuredVersion);
  const packageJson = JSON.parse(await readFile(packagePath, 'utf8'));
  return packageJson.version;
}

export async function readReleaseVersion(root = repositoryRoot) {
  const [packageJson, packageLock, tauriConfig, cargoToml, cargoLock, changelog] = await Promise.all([
    readFile(path.join(root, 'package.json'), 'utf8').then(JSON.parse),
    readFile(path.join(root, 'package-lock.json'), 'utf8').then(JSON.parse),
    readFile(path.join(root, 'src-tauri/tauri.conf.json'), 'utf8').then(JSON.parse),
    readFile(path.join(root, 'src-tauri/Cargo.toml'), 'utf8'),
    readFile(path.join(root, 'src-tauri/Cargo.lock'), 'utf8'),
    readFile(path.join(root, 'CHANGELOG.md'), 'utf8'),
  ]);
  const version = packageJson.version;
  if (!releaseVersionPattern.test(version || '')) throw new Error(`unsupported release version: ${version || 'missing'}`);

  const cargoVersion = cargoToml.match(/^version\s*=\s*"([^"]+)"/mu)?.[1];
  const cargoLockVersion = cargoLock.match(/\[\[package\]\]\r?\nname = "yunspire"\r?\nversion = "([^"]+)"/mu)?.[1];
  const versions = {
    'package.json': version,
    'package-lock.json': packageLock.version,
    'package-lock.json root package': packageLock.packages?.['']?.version,
    'src-tauri/tauri.conf.json': await tauriVersion(root, tauriConfig.version),
    'src-tauri/Cargo.toml': cargoVersion,
    'src-tauri/Cargo.lock yunspire package': cargoLockVersion,
  };
  const mismatches = Object.entries(versions).filter(([, value]) => value !== version);
  if (mismatches.length) throw new Error(`release version mismatch: ${JSON.stringify(versions)}`);
  if (!new RegExp(`^### ${escapeRegExp(version)} (?:-|$)`, 'mu').test(changelog)) {
    throw new Error(`CHANGELOG.md has no release section for ${version}`);
  }

  const [desktopIndex, desktopApp, runtimeBundle, creationGenerator, speechHelperPlist,
    readme, security, aiInstructions, brandGuide, memoryV2, productRequirements,
    schemaDocs, issueTemplate, architectureSvg, releasingGuide] = await Promise.all([
    readFile(path.join(root, 'desktop-ui/index.html'), 'utf8'),
    readFile(path.join(root, 'desktop-ui/app.js'), 'utf8'),
    readFile(path.join(root, 'resources/creation/catalog/runtime-bundle.json'), 'utf8').then(JSON.parse),
    readFile(path.join(root, 'scripts/generate-creation-resources.mjs'), 'utf8'),
    readFile(path.join(root, 'skills/video-content-analysis/scripts/yunspire_speech_info.plist'), 'utf8'),
    readFile(path.join(root, 'README.md'), 'utf8'),
    readFile(path.join(root, 'SECURITY.md'), 'utf8'),
    readFile(path.join(root, 'docs/AI_ASSISTANT_INSTRUCTIONS.md'), 'utf8'),
    readFile(path.join(root, 'docs/BRAND_GUIDE.md'), 'utf8'),
    readFile(path.join(root, 'docs/MEMORY_V2.md'), 'utf8'),
    readFile(path.join(root, 'docs/PRODUCT_REQUIREMENTS.md'), 'utf8'),
    readFile(path.join(root, 'docs/schemas/README.md'), 'utf8'),
    readFile(path.join(root, '.github/ISSUE_TEMPLATE/bug_report.yml'), 'utf8'),
    readFile(path.join(root, 'docs/assets/architecture-overview.svg'), 'utf8'),
    readFile(path.join(root, 'docs/RELEASING.md'), 'utf8'),
  ]);
  const mirrorVersions = {
    'desktop-ui/index.html': extractVersion(desktopIndex, new RegExp(`版本\\s+${versionCapture}\\s+·`, 'u')),
    'desktop-ui/app.js diagnostics': extractVersion(desktopApp, new RegExp(`Yunspire Desktop\\s+${versionCapture}`, 'u')),
    'resources/creation/catalog/runtime-bundle.json runtimeVersion': runtimeBundle.runtimeVersion,
    'scripts/generate-creation-resources.mjs RUNTIME_VERSION': extractVersion(
      creationGenerator,
      new RegExp(`^const\\s+RUNTIME_VERSION\\s*=\\s*["']${versionCapture}["']\\s*;`, 'mu'),
    ),
    'skills/video-content-analysis/scripts/yunspire_speech_info.plist CFBundleShortVersionString': plistStringValue(
      speechHelperPlist,
      'CFBundleShortVersionString',
    ),
    'README.md Chinese current version': extractVersion(readme, new RegExp(`^当前版本为\\s*\\x60${versionCapture}\\x60`, 'mu')),
    'README.md English current version': extractVersion(readme, new RegExp(`^The current version is\\s*\\x60${versionCapture}\\x60`, 'mu')),
    'SECURITY.md Chinese current version': extractVersion(security, new RegExp(`当前版本为\\s*\\x60${versionCapture}\\x60`, 'mu')),
    'SECURITY.md English current version': extractVersion(security, new RegExp(`The current version is\\s*\\x60${versionCapture}\\x60`, 'mu')),
    'docs/AI_ASSISTANT_INSTRUCTIONS.md header': extractVersion(aiInstructions, new RegExp(`^当前版本 / Current version:\\s*\\x60${versionCapture}\\x60`, 'mu')),
    'docs/BRAND_GUIDE.md header': extractVersion(brandGuide, new RegExp(`^当前版本 / Current version:\\s*\\x60${versionCapture}\\x60`, 'mu')),
    'docs/MEMORY_V2.md header': extractVersion(memoryV2, new RegExp(`^Current Yunspire version:\\s*\\x60${versionCapture}\\x60`, 'mu')),
    'docs/PRODUCT_REQUIREMENTS.md header': extractVersion(productRequirements, new RegExp(`^当前版本 / Current version:\\s*\\x60${versionCapture}\\x60`, 'mu')),
    'docs/schemas/README.md header': extractVersion(schemaDocs, new RegExp(`^当前版本 / Current version:\\s*\\x60${versionCapture}\\x60`, 'mu')),
    '.github/ISSUE_TEMPLATE/bug_report.yml version placeholder': extractVersion(issueTemplate, new RegExp(`^\\s*placeholder:\\s*${versionCapture}\\s*$`, 'mu')),
    'docs/assets/architecture-overview.svg version label': extractVersion(architectureSvg, new RegExp(`architecture\\s*·\\s*v${versionCapture}`, 'u')),
    'docs/RELEASING.md package identity example': extractVersion(releasingGuide, new RegExp(`^package version\\s+${versionCapture}\\s*$`, 'mu')),
    'docs/RELEASING.md tag identity example': extractVersion(releasingGuide, new RegExp(`^= tag\\s+v${versionCapture}\\s*$`, 'mu')),
  };
  const mirrorMismatches = Object.entries(mirrorVersions)
    .filter(([, mirrorVersion]) => mirrorVersion !== version);
  if (mirrorMismatches.length) {
    throw new Error(`release version mirror mismatch: ${JSON.stringify({ expected: version, mirrors: mirrorVersions })}`);
  }
  return version;
}

export function releaseTag(version) {
  return `v${version}`;
}

export async function gitRevision(revision, root = repositoryRoot) {
  const { stdout } = await execFileAsync('git', ['rev-parse', '--verify', revision], {
    cwd: root,
    encoding: 'utf8',
  });
  return normalizedCommit(stdout, `Git revision ${revision}`);
}

async function requireCleanSource(root) {
  const { stdout } = await execFileAsync('git', ['status', '--porcelain=v1', '--untracked-files=all'], {
    cwd: root,
    encoding: 'utf8',
  });
  const changes = stdout.trim();
  if (changes) {
    throw new Error(`release source contains uncommitted or untracked files:\n${changes.split(/\r?\n/u).slice(0, 20).join('\n')}`);
  }
}

export async function validateSourceIdentity({
  root = repositoryRoot,
  suppliedTag = null,
  expectedCommit = null,
  requireTag = false,
  requireClean = false,
} = {}) {
  if (requireClean) await requireCleanSource(root);
  const version = await readReleaseVersion(root);
  const expectedTag = releaseTag(version);
  const eventTag = process.env.GITHUB_REF_TYPE === 'tag' ? process.env.GITHUB_REF_NAME?.trim() : null;
  const effectiveTag = suppliedTag?.trim() || eventTag || null;
  if (effectiveTag && effectiveTag !== expectedTag) {
    throw new Error(`release tag ${effectiveTag} does not match source version ${expectedTag}`);
  }
  if (requireTag && !effectiveTag) throw new Error(`release tag is required; expected ${expectedTag}`);

  const sourceCommit = await gitRevision('HEAD^{commit}', root);
  const sourceTree = await gitRevision('HEAD^{tree}', root);
  const requestedCommit = expectedCommit
    || process.env.YUNSPIRE_RELEASE_SOURCE_SHA
    || (process.env.GITHUB_ACTIONS === 'true' ? process.env.GITHUB_SHA : null);
  if (requestedCommit) {
    const normalizedExpected = normalizedCommit(requestedCommit, 'expected source commit');
    if (normalizedExpected !== sourceCommit) {
      throw new Error(`checked-out source commit ${sourceCommit} does not match expected source commit ${normalizedExpected}`);
    }
  }

  let tagCommit = null;
  if (effectiveTag) {
    try {
      tagCommit = await gitRevision(`refs/tags/${expectedTag}^{commit}`, root);
    } catch (error) {
      throw new Error(`release tag ${expectedTag} is missing or cannot be peeled to a commit: ${error.message}`);
    }
    if (tagCommit !== sourceCommit) {
      throw new Error(`release tag ${expectedTag} peels to ${tagCommit}, not checked-out source ${sourceCommit}`);
    }
  }

  return {
    version,
    tag: expectedTag,
    sourceCommit,
    sourceTree,
    tagCommit,
  };
}

function githubHeaders() {
  const headers = {
    Accept: 'application/vnd.github+json',
    'User-Agent': 'Yunspire-Release-Integrity',
    'X-GitHub-Api-Version': '2022-11-28',
  };
  const token = process.env.GITHUB_TOKEN || process.env.GH_TOKEN;
  if (token) headers.Authorization = `Bearer ${token}`;
  return headers;
}

async function githubRequest(repository, endpoint, { allowNotFound = false } = {}) {
  const apiRoot = (process.env.GITHUB_API_URL || 'https://api.github.com').replace(/\/$/u, '');
  const response = await fetch(`${apiRoot}/repos/${repository}${endpoint}`, { headers: githubHeaders() });
  if (allowNotFound && response.status === 404) return { response, body: null };
  const body = await response.json().catch(() => null);
  if (!response.ok) {
    throw new Error(`GitHub API ${endpoint} returned HTTP ${response.status}: ${body?.message || 'unknown error'}`);
  }
  return { response, body };
}

async function remoteAnnotatedTag(repository, tag) {
  const { body: reference } = await githubRequest(repository, `/git/ref/tags/${encodeURIComponent(tag)}`);
  if (reference?.object?.type !== 'tag') {
    throw new Error(`remote tag ${tag} must be the annotated tag created by this workflow`);
  }
  const tagObjectSha = normalizedCommit(reference.object.sha, `remote tag object ${tag}`);
  const { body: tagObject } = await githubRequest(repository, `/git/tags/${tagObjectSha}`);
  if (tagObject?.object?.type !== 'commit') {
    throw new Error(`remote annotated tag ${tag} does not point directly to a commit`);
  }
  return {
    commit: normalizedCommit(tagObject.object.sha, `remote tag ${tag} commit`),
    message: String(tagObject.message || ''),
    tagObjectSha,
  };
}

async function remoteBranchCommit(repository, branch) {
  const { body: reference } = await githubRequest(repository, `/git/ref/heads/${encodeURIComponent(branch)}`);
  if (reference?.object?.type !== 'commit') throw new Error(`remote branch ${branch} does not resolve to a commit`);
  return normalizedCommit(reference.object.sha, `remote branch ${branch} commit`);
}

export async function verifyPrepare({
  repository,
  releaseBranch,
  expectedCommit,
  root = repositoryRoot,
} = {}) {
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/u.test(repository || '')) {
    throw new Error(`invalid GitHub repository: ${repository || 'missing'}`);
  }
  const identity = await validateSourceIdentity({ root, expectedCommit, requireClean: true });
  if (identity.version.includes('-')) {
    throw new Error(`stable release workflow does not accept prerelease version ${identity.version}`);
  }
  const expectedBranch = `release/${identity.tag}`;
  const branch = releaseBranch?.trim()
    || (process.env.GITHUB_REF_TYPE === 'branch' ? process.env.GITHUB_REF_NAME?.trim() : null);
  if (branch !== expectedBranch) {
    throw new Error(`release branch ${branch || 'missing'} does not match source version ${expectedBranch}`);
  }
  const mainCommit = await remoteBranchCommit(repository, 'main');
  if (mainCommit !== identity.sourceCommit) {
    throw new Error(`release source ${identity.sourceCommit} is not the remote main HEAD ${mainCommit}`);
  }
  const { response: tagResponse } = await githubRequest(
    repository,
    `/git/ref/tags/${encodeURIComponent(identity.tag)}`,
    { allowNotFound: true },
  );
  if (tagResponse.status !== 404) {
    throw new Error(`remote tag ${identity.tag} already exists; refusing to move or reuse it`);
  }
  const { response: releaseResponse } = await githubRequest(
    repository,
    `/releases/tags/${encodeURIComponent(identity.tag)}`,
    { allowNotFound: true },
  );
  if (releaseResponse.status !== 404) {
    throw new Error(`GitHub Release ${identity.tag} already exists; refusing to overwrite or replace its assets`);
  }
  return { ...identity, releaseBranch: branch, mainCommit };
}

export async function verifyPrepublish({
  repository,
  tag,
  expectedCommit,
  expectedProvenance,
  root = repositoryRoot,
} = {}) {
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/u.test(repository || '')) {
    throw new Error(`invalid GitHub repository: ${repository || 'missing'}`);
  }
  const identity = await validateSourceIdentity({ root, expectedCommit, requireClean: true });
  if (identity.version.includes('-')) {
    throw new Error(`stable release workflow does not accept prerelease version ${identity.version}`);
  }
  if (tag !== identity.tag) throw new Error(`release tag ${tag || 'missing'} does not match source version ${identity.tag}`);
  const provenance = String(expectedProvenance || '').trim();
  const provenancePattern = new RegExp(`^yunspire-release-run:[0-9]+:[0-9]+:${identity.sourceCommit}$`, 'u');
  if (!provenancePattern.test(provenance)) {
    throw new Error('prepublish verification requires the exact workflow run provenance for this source commit');
  }
  const mainCommit = await remoteBranchCommit(repository, 'main');
  if (mainCommit !== identity.sourceCommit) {
    throw new Error(`release source ${identity.sourceCommit} is not the remote main HEAD ${mainCommit}`);
  }
  const remoteTag = await remoteAnnotatedTag(repository, identity.tag);
  if (remoteTag.commit !== identity.sourceCommit) {
    throw new Error(`remote tag ${identity.tag} points to ${remoteTag.commit}, not ${identity.sourceCommit}`);
  }
  if (!remoteTag.message.split(/\r?\n/u).some((line) => line.trim() === provenance)) {
    throw new Error(`remote annotated tag ${identity.tag} does not carry this workflow run provenance`);
  }
  const { body: release } = await githubRequest(repository, `/releases/tags/${encodeURIComponent(identity.tag)}`);
  if (release.tag_name !== identity.tag || release.draft !== true || release.prerelease === true) {
    throw new Error(`GitHub Release ${identity.tag} must be the unpublished stable draft created by this workflow`);
  }
  if (release.target_commitish !== identity.sourceCommit) {
    throw new Error(`GitHub Release ${identity.tag} targets ${release.target_commitish}, not ${identity.sourceCommit}`);
  }
  if (!String(release.body || '').includes(`<!-- ${provenance} -->`)) {
    throw new Error(`GitHub Release ${identity.tag} does not carry this workflow run provenance`);
  }
  return { ...identity, releaseId: release.id, mainCommit, provenance, tagObjectSha: remoteTag.tagObjectSha };
}

function manifestSourceCommit(manifest) {
  return manifest.sourceCommit || manifest.sourceSha || null;
}

async function sha256(filePath) {
  const handle = await open(filePath, 'r');
  const hash = createHash('sha256');
  try {
    for await (const chunk of handle.createReadStream({ autoClose: false })) hash.update(chunk);
  } finally {
    await handle.close();
  }
  return hash.digest('hex');
}

export async function verifyReleaseManifests({
  manifestPaths,
  expectedTag,
  expectedCommit,
  expectedTree,
  requireSigned = false,
  root = repositoryRoot,
} = {}) {
  if (!manifestPaths?.length) throw new Error('at least one --manifest path is required');
  const version = await readReleaseVersion(root);
  const tag = releaseTag(version);
  if (expectedTag && expectedTag !== tag) throw new Error(`expected tag ${expectedTag} does not match ${tag}`);
  const sourceCommit = normalizedCommit(expectedCommit, 'expected manifest source commit');
  const sourceTree = normalizedCommit(expectedTree, 'expected manifest source tree');
  const manifests = await Promise.all(manifestPaths.map(async (manifestPath) => {
    const absolutePath = path.resolve(root, manifestPath);
    return {
      path: manifestPath,
      absolutePath,
      value: JSON.parse(await readFile(absolutePath, 'utf8')),
    };
  }));
  const platforms = new Map();
  const signingModes = new Set();
  for (const { path: manifestPath, absolutePath, value: manifest } of manifests) {
    if (manifest.version !== version) throw new Error(`${manifestPath} version ${manifest.version} does not match ${version}`);
    if (manifest.tag !== tag) throw new Error(`${manifestPath} tag ${manifest.tag || 'missing'} does not match ${tag}`);
    if (manifestSourceCommit(manifest) !== sourceCommit) {
      throw new Error(`${manifestPath} source commit ${manifestSourceCommit(manifest) || 'missing'} does not match ${sourceCommit}`);
    }
    if (manifest.sourceTree !== sourceTree) {
      throw new Error(`${manifestPath} source tree ${manifest.sourceTree || 'missing'} does not match ${sourceTree}`);
    }
    if (typeof manifest.signed !== 'boolean') throw new Error(`${manifestPath} has no explicit signed boolean`);
    const expectedSigningMode = manifest.signed ? 'signed' : 'unsigned';
    if (manifest.signingMode !== expectedSigningMode) {
      throw new Error(`${manifestPath} signingMode ${manifest.signingMode || 'missing'} does not match signed=${manifest.signed}`);
    }
    if (requireSigned && manifest.signed !== true) throw new Error(`${manifestPath} is not marked as a signed artifact`);
    signingModes.add(manifest.signingMode);
    if (!['macos', 'windows'].includes(manifest.platform)) {
      throw new Error(`${manifestPath} has unsupported platform ${manifest.platform || 'missing'}`);
    }
    if (platforms.has(manifest.platform)) throw new Error(`multiple manifests claim platform ${manifest.platform}`);
    if (!manifest.file?.includes(`_${version}_`)) throw new Error(`${manifestPath} artifact name does not contain ${version}`);
    if (path.basename(manifest.file) !== manifest.file) throw new Error(`${manifestPath} artifact name is not a safe basename`);
    if (!/^[0-9a-f]{64}$/u.test(manifest.sha256 || '')) throw new Error(`${manifestPath} has an invalid SHA-256`);
    if (!Number.isSafeInteger(manifest.bytes) || manifest.bytes <= 0) throw new Error(`${manifestPath} has an invalid byte length`);
    const artifactPath = path.join(path.dirname(absolutePath), manifest.file);
    const artifactStat = await stat(artifactPath);
    if (!artifactStat.isFile() || artifactStat.size !== manifest.bytes) {
      throw new Error(`${manifestPath} byte length does not match ${manifest.file}`);
    }
    const artifactDigest = await sha256(artifactPath);
    if (artifactDigest !== manifest.sha256) throw new Error(`${manifestPath} SHA-256 does not match ${manifest.file}`);
    const checksum = await readFile(`${artifactPath}.sha256`, 'utf8');
    if (checksum.trim() !== `${manifest.sha256}  ${manifest.file}`) {
      throw new Error(`${manifestPath} checksum file does not bind the exact artifact name and SHA-256`);
    }
    platforms.set(manifest.platform, manifest);
  }
  for (const platform of ['macos', 'windows']) {
    if (!platforms.has(platform)) throw new Error(`release manifest set is missing ${platform}`);
  }
  if (platforms.size !== 2) throw new Error(`release manifest set must contain exactly macos and windows`);
  if (signingModes.size !== 1) throw new Error('macOS and Windows manifests must use the same signingMode');
  return {
    version,
    tag,
    sourceCommit,
    sourceTree,
    signingMode: [...signingModes][0],
    manifests: manifests.length,
  };
}

function parseArguments(argumentsList) {
  const [command, ...values] = argumentsList;
  const options = new Map();
  for (let index = 0; index < values.length; index += 1) {
    const key = values[index];
    if (!key.startsWith('--')) throw new Error(`unexpected argument: ${key}`);
    const value = values[index + 1];
    if (!value || value.startsWith('--')) throw new Error(`missing value for ${key}`);
    const name = key.slice(2);
    if (name === 'manifest') {
      options.set(name, [...(options.get(name) || []), value]);
    } else {
      options.set(name, value);
    }
    index += 1;
  }
  return { command, options };
}

async function writeOutputs(outputPath, values) {
  if (!outputPath) return;
  await appendFile(outputPath, Object.entries(values).map(([key, value]) => `${key}=${value}\n`).join(''), 'utf8');
}

async function main() {
  const { command, options } = parseArguments(process.argv.slice(2));
  if (command === 'source') {
    const identity = await validateSourceIdentity({
      suppliedTag: options.get('tag'),
      expectedCommit: options.get('source-commit'),
      requireTag: options.get('require-tag') === 'true',
      requireClean: options.get('require-clean') === 'true',
    });
    await writeOutputs(options.get('github-output') || process.env.GITHUB_OUTPUT, {
      version: identity.version,
      tag: identity.tag,
      source_commit: identity.sourceCommit,
      source_tree: identity.sourceTree,
    });
    console.log(`RELEASE_SOURCE_IDENTITY_OK version=${identity.version} tag=${identity.tag} commit=${identity.sourceCommit} tree=${identity.sourceTree}`);
    return;
  }
  if (command === 'prepublish') {
    const identity = await verifyPrepublish({
      repository: options.get('repository') || process.env.GITHUB_REPOSITORY,
      tag: options.get('tag'),
      expectedCommit: options.get('source-commit'),
      expectedProvenance: options.get('run-provenance'),
    });
    console.log(`RELEASE_PREPUBLISH_OK tag=${identity.tag} commit=${identity.sourceCommit} main=${identity.mainCommit} draft=${identity.releaseId} tag_object=${identity.tagObjectSha}`);
    return;
  }
  if (command === 'prepare') {
    const identity = await verifyPrepare({
      repository: options.get('repository') || process.env.GITHUB_REPOSITORY,
      releaseBranch: options.get('release-branch'),
      expectedCommit: options.get('source-commit'),
    });
    console.log(`RELEASE_PREPARE_OK branch=${identity.releaseBranch} tag=${identity.tag} commit=${identity.sourceCommit} main=${identity.mainCommit} tag=absent release=absent`);
    return;
  }
  if (command === 'manifests') {
    const result = await verifyReleaseManifests({
      manifestPaths: options.get('manifest'),
      expectedTag: options.get('tag'),
      expectedCommit: options.get('source-commit'),
      expectedTree: options.get('source-tree'),
      requireSigned: options.get('require-signed') === 'true',
    });
    console.log(`RELEASE_MANIFESTS_OK version=${result.version} tag=${result.tag} commit=${result.sourceCommit} tree=${result.sourceTree} signing=${result.signingMode}`);
    return;
  }
  throw new Error('usage: verify-release-version.mjs <source|prepare|prepublish|manifests> [options]');
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    console.error(`RELEASE_VERSION_VERIFY_FAILED ${error.message}`);
    process.exitCode = 1;
  });
}
