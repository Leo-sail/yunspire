import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { chmod, mkdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import process from 'node:process';

if (process.platform !== 'darwin') {
  throw new Error('macOS 原生 helper 只能在 macOS 构建机上编译');
}

const root = resolve(import.meta.dirname, '..');
const sourceDirectory = join(root, 'skills', 'video-content-analysis', 'scripts');
const documentSourceDirectory = join(root, 'skills', 'document-content-analysis', 'scripts');
const outputDirectory = join(root, 'src-tauri', 'target', 'yunspire-native', 'macos');
const mediaSource = join(sourceDirectory, 'yunspire_media.m');
const speechSource = join(sourceDirectory, 'yunspire_speech.m');
const pdfSource = join(documentSourceDirectory, 'yunspire_pdf.m');
const speechPlistTemplate = join(sourceDirectory, 'yunspire_speech_info.plist');
const mediaOutput = join(outputDirectory, 'yunspire-media');
const pdfOutput = join(outputDirectory, 'yunspire-pdf');
const speechBundle = join(outputDirectory, 'Yunspire Speech Helper.app');
const speechContents = join(speechBundle, 'Contents');
const speechExecutable = join(speechContents, 'MacOS', 'yunspire-speech');
const speechPlist = join(speechContents, 'Info.plist');
const manifestPath = join(outputDirectory, 'helpers-manifest.json');
const tauriConfigPath = join(root, 'src-tauri', 'tauri.conf.json');

function normalizedArchitectures() {
  const configured = process.env.YUNSPIRE_MACOS_ARCHS?.trim() || 'arm64,x86_64';
  const aliases = new Map([
    ['aarch64', 'arm64'],
    ['arm64', 'arm64'],
    ['x64', 'x86_64'],
    ['x86_64', 'x86_64'],
  ]);
  const values = configured
    .split(/[,+\s]+/u)
    .filter(Boolean)
    .map((value) => aliases.get(value.toLowerCase()));
  if (values.some((value) => !value) || values.length === 0) {
    throw new Error(`YUNSPIRE_MACOS_ARCHS 只允许 arm64、aarch64、x86_64 或 x64：${configured}`);
  }
  return [...new Set(values)];
}

function run(program, args, label) {
  const result = spawnSync(program, args, {
    cwd: root,
    encoding: 'utf8',
    maxBuffer: 16 * 1024 * 1024,
    timeout: 5 * 60_000,
  });
  if (result.error || result.status !== 0) {
    throw new Error(`${label}失败\n${result.error || ''}\n${result.stdout || ''}\n${result.stderr || ''}`.trim());
  }
  return result.stdout.trim();
}

function replacePlistString(plist, key, value) {
  const escapedKey = key.replaceAll(/[.*+?^${}()|[\]\\]/gu, '\\$&');
  const pattern = new RegExp(`(<key>${escapedKey}<\\/key>\\s*<string>)[^<]*(<\\/string>)`, 'u');
  if (!pattern.test(plist)) throw new Error(`语音 helper Info.plist 缺少 ${key}`);
  return plist.replace(pattern, `$1${value}$2`);
}

async function sha256(path) {
  return createHash('sha256').update(await readFile(path)).digest('hex');
}

async function assertFile(path, label) {
  const value = await stat(path).catch(() => null);
  if (!value?.isFile() || value.size <= 0) throw new Error(`${label}未生成：${path}`);
  return value.size;
}

const architectures = normalizedArchitectures();
const tauriConfig = JSON.parse(await readFile(tauriConfigPath, 'utf8'));
const version = String(tauriConfig.version || '').trim();
if (!/^\d+\.\d+\.\d+$/u.test(version)) {
  throw new Error(`Tauri 应用版本无效：${version || 'missing'}`);
}
const minimumSystemVersion = String(tauriConfig.bundle?.macOS?.minimumSystemVersion || '').trim();
if (!/^\d+\.\d+(?:\.\d+)?$/u.test(minimumSystemVersion)) {
  throw new Error(`Tauri macOS 最低系统版本无效：${minimumSystemVersion || 'missing'}`);
}

await rm(outputDirectory, { recursive: true, force: true });
await mkdir(join(speechContents, 'MacOS'), { recursive: true });

let generatedPlist = await readFile(speechPlistTemplate, 'utf8');
generatedPlist = replacePlistString(generatedPlist, 'CFBundleShortVersionString', version);
await writeFile(speechPlist, generatedPlist, 'utf8');

const architectureArgs = architectures.flatMap((architecture) => ['-arch', architecture]);
const commonArgs = [
  '--sdk', 'macosx', 'clang',
  '-fobjc-arc',
  '-O2',
  '-Wall',
  '-Wextra',
  `-mmacosx-version-min=${minimumSystemVersion}`,
  ...architectureArgs,
];

run('xcrun', [
  ...commonArgs,
  mediaSource,
  '-framework', 'AVFoundation',
  '-framework', 'CoreMedia',
  '-framework', 'CoreGraphics',
  '-framework', 'Foundation',
  '-framework', 'ImageIO',
  '-o', mediaOutput,
], 'macOS 媒体 helper 编译');

run('xcrun', [
  ...commonArgs,
  pdfSource,
  '-framework', 'PDFKit',
  '-framework', 'AppKit',
  '-framework', 'Foundation',
  '-o', pdfOutput,
], 'macOS PDF helper 编译');

run('xcrun', [
  ...commonArgs,
  speechSource,
  '-framework', 'Speech',
  '-framework', 'Foundation',
  `-Wl,-sectcreate,__TEXT,__info_plist,${speechPlist}`,
  '-o', speechExecutable,
], 'macOS 语音 helper 编译');

await chmod(mediaOutput, 0o755);
await chmod(pdfOutput, 0o755);
await chmod(speechExecutable, 0o755);
const mediaSize = await assertFile(mediaOutput, 'macOS 媒体 helper');
const pdfSize = await assertFile(pdfOutput, 'macOS PDF helper');
const speechSize = await assertFile(speechExecutable, 'macOS 语音 helper');

const manifest = {
  schema: 'yunspire.macos-native-helpers.v1',
  version,
  minimumSystemVersion,
  architectures,
  helpers: {
    media: {
      path: 'yunspire-media',
      byteLength: mediaSize,
      sha256: await sha256(mediaOutput),
      sourceSha256: await sha256(mediaSource),
    },
    pdf: {
      path: 'yunspire-pdf',
      byteLength: pdfSize,
      sha256: await sha256(pdfOutput),
      sourceSha256: await sha256(pdfSource),
    },
    speech: {
      path: 'Yunspire Speech Helper.app/Contents/MacOS/yunspire-speech',
      byteLength: speechSize,
      sha256: await sha256(speechExecutable),
      sourceSha256: await sha256(speechSource),
      infoPlistSha256: await sha256(speechPlist),
    },
  },
};
await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');

console.log(`MACOS_NATIVE_HELPERS_OK version=${version} architectures=${architectures.join(',')}`);
