import { createHash } from 'node:crypto';
import {
  appendFile,
  copyFile,
  mkdir,
  open,
  readFile,
  stat,
  writeFile,
} from 'node:fs/promises';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';
import {
  validateSourceIdentity,
} from './verify-release-version.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const [command, ...argumentsList] = process.argv.slice(2);
const windowsReleaseFiles = [
  'scripts/generate-third-party-notices.mjs',
  'scripts/build-windows-python-runtime.mjs',
  'scripts/build-windows-native.mjs',
  'scripts/build-windows-media-helper.mjs',
  'scripts/sign-windows.ps1',
  'scripts/verify-windows-release.mjs',
  'src-tauri/tauri.release.conf.json',
  'src-tauri/tauri.windows.conf.json',
  'skills/document-content-analysis/scripts/yunspire_image_windows.cpp',
  'skills/document-content-analysis/scripts/yunspire_pdf_windows.cpp',
  'skills/video-content-analysis/scripts/yunspire_media_windows.cpp',
  'skills/video-content-analysis/scripts/yunspire_speech_windows.cpp',
];

function option(name, { required = false, fallback } = {}) {
  const index = argumentsList.indexOf(`--${name}`);
  const value = index >= 0 ? argumentsList[index + 1] : fallback;
  if (required && (!value || value.startsWith('--'))) {
    throw new Error(`missing required option --${name}`);
  }
  return value;
}

async function appendOutputs(values) {
  const outputPath = option('github-output', { fallback: process.env.GITHUB_OUTPUT });
  if (!outputPath) return;
  const body = Object.entries(values).map(([key, value]) => `${key}=${value}\n`).join('');
  await appendFile(outputPath, body, 'utf8');
}

async function verifySource() {
  const identity = await validateSourceIdentity({
    root,
    suppliedTag: option('tag'),
    expectedCommit: option('source-commit'),
    requireTag: option('require-tag', { fallback: 'false' }) === 'true',
    requireClean: option('require-clean', { fallback: 'false' }) === 'true',
  });
  if (option('require-windows', { fallback: 'false' }) === 'true') {
    const missing = [];
    for (const relativePath of windowsReleaseFiles) {
      const value = await stat(path.join(root, relativePath)).catch(() => null);
      if (!value?.isFile() || value.size === 0) missing.push(relativePath);
    }
    const packageJson = JSON.parse(await readFile(path.join(root, 'package.json'), 'utf8'));
    const buildCommand = packageJson.scripts?.build ?? '';
    for (const scriptName of [
      'generate-third-party-notices.mjs',
      'build-windows-python-runtime.mjs',
      'build-windows-native.mjs',
      'build-windows-media-helper.mjs',
    ]) {
      if (!buildCommand.includes(scriptName)) missing.push(`package.json scripts.build -> ${scriptName}`);
    }
    if (missing.length) {
      throw new Error(`Windows release prerequisites are missing: ${missing.join(', ')}`);
    }
  }
  await appendOutputs({
    version: identity.version,
    tag: identity.tag,
    source_commit: identity.sourceCommit,
    source_tree: identity.sourceTree,
  });
  console.log(`RELEASE_SOURCE_OK version=${identity.version} tag=${identity.tag} commit=${identity.sourceCommit} tree=${identity.sourceTree}`);
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

async function verifyMagic(platform, inputPath, size) {
  const handle = await open(inputPath, 'r');
  try {
    if (platform === 'windows') {
      const header = Buffer.alloc(2);
      await handle.read(header, 0, header.length, 0);
      if (header.toString('ascii') !== 'MZ') throw new Error('Windows installer has no PE MZ header');
      return;
    }
    const trailer = Buffer.alloc(512);
    await handle.read(trailer, 0, trailer.length, size - trailer.length);
    if (trailer.subarray(0, 4).toString('ascii') !== 'koly') {
      throw new Error('macOS installer has no UDIF koly trailer');
    }
  } finally {
    await handle.close();
  }
}

async function verifyArtifact() {
  const platform = option('platform', { required: true });
  if (!['macos', 'windows'].includes(platform)) throw new Error(`unsupported platform: ${platform}`);
  const inputPath = path.resolve(root, option('input', { required: true }));
  const outputDirectory = path.resolve(root, option('output', { required: true }));
  const signedValue = option('signed', { fallback: 'false' });
  if (!['true', 'false'].includes(signedValue)) throw new Error('--signed must be true or false');
  const signed = signedValue === 'true';
  const identity = await validateSourceIdentity({
    root,
    suppliedTag: option('tag'),
    expectedCommit: option('source-commit'),
    requireTag: option('require-tag', { fallback: process.env.GITHUB_ACTIONS === 'true' ? 'true' : 'false' }) === 'true',
    requireClean: option('require-clean', { fallback: 'true' }) === 'true',
  });
  const version = identity.version;
  const inputStat = await stat(inputPath);
  if (!inputStat.isFile() || inputStat.size < 1024 * 1024) {
    throw new Error(`release artifact is missing or unexpectedly small: ${inputPath}`);
  }
  const inputName = path.basename(inputPath);
  const expectedInputExtension = platform === 'macos' ? '.dmg' : '.exe';
  if (!inputName.endsWith(expectedInputExtension) || !inputName.includes(`_${version}_`)) {
    throw new Error(`release artifact name does not identify ${platform} version ${version}: ${inputName}`);
  }
  await verifyMagic(platform, inputPath, inputStat.size);

  const platformName = platform === 'macos' ? 'macOS-universal' : 'Windows-x64';
  const extension = platform === 'macos' ? '.dmg' : '-setup.exe';
  const signatureLabel = signed ? 'signed' : 'unsigned';
  const artifactName = `Yunspire_${version}_${platformName}_${signatureLabel}${extension}`;
  await mkdir(outputDirectory, { recursive: true });
  const artifactPath = path.join(outputDirectory, artifactName);
  await copyFile(inputPath, artifactPath);
  const checksum = await sha256(artifactPath);
  const checksumPath = `${artifactPath}.sha256`;
  await writeFile(checksumPath, `${checksum}  ${artifactName}\n`, 'utf8');

  const manifestPath = `${artifactPath}.manifest.json`;
  const manifest = {
    schemaVersion: 2,
    product: 'Yunspire',
    version,
    tag: identity.tag,
    platform,
    architecture: platform === 'macos' ? ['arm64', 'x86_64'] : ['x86_64'],
    signed,
    signingMode: signed ? 'signed' : 'unsigned',
    file: artifactName,
    bytes: inputStat.size,
    sha256: checksum,
    sourceCommit: identity.sourceCommit,
    sourceTree: identity.sourceTree,
    repository: process.env.GITHUB_REPOSITORY || 'Leo-sail/yunspire',
    workflowRunId: process.env.GITHUB_RUN_ID || null,
    workflowRunAttempt: process.env.GITHUB_RUN_ATTEMPT || null,
    createdAt: new Date().toISOString(),
  };
  await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');
  await appendOutputs({
    artifact_path: artifactPath,
    checksum_path: checksumPath,
    manifest_path: manifestPath,
    version,
    tag: identity.tag,
    source_commit: identity.sourceCommit,
    source_tree: identity.sourceTree,
  });
  console.log(`RELEASE_ARTIFACT_OK platform=${platform} file=${artifactName} sha256=${checksum} commit=${identity.sourceCommit} tree=${identity.sourceTree}`);
}

try {
  if (command === 'source') await verifySource();
  else if (command === 'artifact') await verifyArtifact();
  else throw new Error('usage: verify-release-artifact.mjs <source|artifact> [options]');
} catch (error) {
  console.error(`RELEASE_VERIFY_FAILED ${error.message}`);
  process.exitCode = 1;
}
