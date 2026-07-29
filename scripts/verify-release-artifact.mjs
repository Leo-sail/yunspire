import { execFile } from 'node:child_process';
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
import { promisify } from 'node:util';

const execFileAsync = promisify(execFile);
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const [command, ...argumentsList] = process.argv.slice(2);
const windowsReleaseFiles = [
  'scripts/generate-third-party-notices.mjs',
  'scripts/build-windows-python-runtime.mjs',
  'scripts/build-windows-native.mjs',
  'scripts/build-windows-media-helper.mjs',
  'scripts/verify-windows-release.mjs',
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

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

async function appendOutputs(values) {
  const outputPath = option('github-output', { fallback: process.env.GITHUB_OUTPUT });
  if (!outputPath) return;
  const body = Object.entries(values).map(([key, value]) => `${key}=${value}\n`).join('');
  await appendFile(outputPath, body, 'utf8');
}

async function readVersions() {
  const [packageJson, packageLock, tauriConfig, cargoToml, changelog] = await Promise.all([
    readFile(path.join(root, 'package.json'), 'utf8').then(JSON.parse),
    readFile(path.join(root, 'package-lock.json'), 'utf8').then(JSON.parse),
    readFile(path.join(root, 'src-tauri/tauri.conf.json'), 'utf8').then(JSON.parse),
    readFile(path.join(root, 'src-tauri/Cargo.toml'), 'utf8'),
    readFile(path.join(root, 'CHANGELOG.md'), 'utf8'),
  ]);
  const cargoVersion = cargoToml.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
  const version = packageJson.version;
  const versions = {
    'package.json': version,
    'package-lock.json': packageLock.version,
    'package-lock.json root package': packageLock.packages?.['']?.version,
    'src-tauri/tauri.conf.json': tauriConfig.version,
    'src-tauri/Cargo.toml': cargoVersion,
  };
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(version)) {
    throw new Error(`unsupported release version: ${version}`);
  }
  const mismatches = Object.entries(versions).filter(([, value]) => value !== version);
  if (mismatches.length) {
    throw new Error(`release version mismatch: ${JSON.stringify(versions)}`);
  }
  if (!new RegExp(`^### ${escapeRegExp(version)} (?:-|$)`, 'm').test(changelog)) {
    throw new Error(`CHANGELOG.md has no release section for ${version}`);
  }
  return version;
}

async function verifySource() {
  const version = await readVersions();
  const expectedTag = `v${version}`;
  const suppliedTag = option('tag');
  if (suppliedTag && suppliedTag !== expectedTag) {
    throw new Error(`release tag ${suppliedTag} does not match source version ${expectedTag}`);
  }
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
  await appendOutputs({ version, tag: expectedTag });
  console.log(`RELEASE_SOURCE_OK version=${version} tag=${expectedTag}`);
}

async function readCommit() {
  if (process.env.GITHUB_SHA) return process.env.GITHUB_SHA;
  try {
    const { stdout } = await execFileAsync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' });
    return stdout.trim();
  } catch {
    return null;
  }
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
  const version = await readVersions();
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
    schemaVersion: 1,
    product: 'Yunspire',
    version,
    platform,
    architecture: platform === 'macos' ? ['arm64', 'x86_64'] : ['x86_64'],
    signed,
    file: artifactName,
    bytes: inputStat.size,
    sha256: checksum,
    sourceCommit: await readCommit(),
    createdAt: new Date().toISOString(),
  };
  await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');
  await appendOutputs({ artifact_path: artifactPath, checksum_path: checksumPath, manifest_path: manifestPath });
  console.log(`RELEASE_ARTIFACT_OK platform=${platform} file=${artifactName} sha256=${checksum}`);
}

try {
  if (command === 'source') await verifySource();
  else if (command === 'artifact') await verifyArtifact();
  else throw new Error('usage: verify-release-artifact.mjs <source|artifact> [options]');
} catch (error) {
  console.error(`RELEASE_VERIFY_FAILED ${error.message}`);
  process.exitCode = 1;
}
