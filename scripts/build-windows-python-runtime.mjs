import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { access, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { constants } from 'node:fs';
import { join, resolve } from 'node:path';
import process from 'node:process';

const PYTHON_VERSION = '3.13.7';
const ARCHIVE_NAME = `python-${PYTHON_VERSION}-embed-amd64.zip`;
const DOWNLOAD_URL = `https://www.python.org/ftp/python/${PYTHON_VERSION}/${ARCHIVE_NAME}`;
const EXPECTED_SHA256 = 'f6cca216a359be84797cabb54149ce5e062afb16cc7567eb7fc51cacb2d86b65';
const EXPECTED_MD5 = '77f294ec267596827a2ab06e8fa3f18c';
const MAX_ARCHIVE_BYTES = 16 * 1024 * 1024;

if (process.platform !== 'win32') {
  console.log(`WINDOWS_PYTHON_RUNTIME_SKIPPED platform=${process.platform}`);
  process.exit(0);
}

const root = resolve(import.meta.dirname, '..');
const outputRoot = join(root, 'src-tauri', 'target', 'yunspire-runtime');
const runtimeDirectory = join(outputRoot, 'python');
const archivePath = join(outputRoot, ARCHIVE_NAME);
const manifestPath = join(runtimeDirectory, 'YUNSPIRE_RUNTIME.json');
const pythonExecutable = join(runtimeDirectory, 'python.exe');

async function isFile(path) {
  try {
    await access(path, constants.R_OK);
    return true;
  } catch {
    return false;
  }
}

function digest(algorithm, bytes) {
  return createHash(algorithm).update(bytes).digest('hex');
}

async function runtimeIsCurrent() {
  if (!await isFile(pythonExecutable) || !await isFile(manifestPath)) return false;
  try {
    const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
    return manifest.version === PYTHON_VERSION
      && manifest.archiveSha256 === EXPECTED_SHA256
      && manifest.archiveMd5 === EXPECTED_MD5
      && manifest.sourceUrl === DOWNLOAD_URL;
  } catch {
    return false;
  }
}

async function downloadArchive() {
  const response = await fetch(DOWNLOAD_URL, {
    redirect: 'follow',
    signal: AbortSignal.timeout(120_000),
    headers: { 'User-Agent': 'Yunspire Windows build/0.1' },
  });
  if (!response.ok || response.url !== DOWNLOAD_URL) {
    throw new Error(`无法从 Python 官方地址下载嵌入式运行时：HTTP ${response.status} ${response.url}`);
  }
  const declaredLength = Number(response.headers.get('content-length') || 0);
  if (declaredLength <= 0 || declaredLength > MAX_ARCHIVE_BYTES) {
    throw new Error(`Python 嵌入式运行时响应大小异常：${declaredLength}`);
  }
  const bytes = Buffer.from(await response.arrayBuffer());
  if (bytes.length !== declaredLength || bytes.length > MAX_ARCHIVE_BYTES) {
    throw new Error(`Python 嵌入式运行时下载不完整：${bytes.length}/${declaredLength}`);
  }
  const sha256 = digest('sha256', bytes);
  const md5 = digest('md5', bytes);
  if (sha256 !== EXPECTED_SHA256 || md5 !== EXPECTED_MD5) {
    throw new Error(`Python 嵌入式运行时校验失败：SHA-256=${sha256} MD5=${md5}`);
  }
  await writeFile(archivePath, bytes);
}

function extractWithTar() {
  return spawnSync('tar.exe', ['-xf', archivePath, '-C', runtimeDirectory], {
    cwd: root,
    encoding: 'utf8',
    windowsHide: true,
  });
}

function extractWithPowerShell() {
  return spawnSync('powershell.exe', [
    '-NoLogo', '-NoProfile', '-NonInteractive', '-Command',
    'Expand-Archive -LiteralPath $args[0] -DestinationPath $args[1] -Force',
    archivePath, runtimeDirectory,
  ], {
    cwd: root,
    encoding: 'utf8',
    windowsHide: true,
  });
}

async function extractArchive() {
  await rm(runtimeDirectory, { recursive: true, force: true });
  await mkdir(runtimeDirectory, { recursive: true });
  let result = extractWithTar();
  if (result.status !== 0) result = extractWithPowerShell();
  if (result.status !== 0) {
    throw new Error(`无法解压 Python 嵌入式运行时\n${result.stdout || ''}\n${result.stderr || ''}`.trim());
  }
}

async function configureRuntime() {
  const pthPath = join(runtimeDirectory, 'python313._pth');
  if (!await isFile(pthPath)) throw new Error('Python 嵌入式运行时缺少 python313._pth');
  await writeFile(pthPath, [
    'python313.zip',
    '.',
    '',
  ].join('\r\n'), 'utf8');
  await writeFile(manifestPath, JSON.stringify({
    schema: 'yunspire.windows-python-runtime.v1',
    version: PYTHON_VERSION,
    architecture: 'x64',
    sourceUrl: DOWNLOAD_URL,
    archiveSha256: EXPECTED_SHA256,
    archiveMd5: EXPECTED_MD5,
    licenseFile: 'LICENSE.txt',
  }, null, 2) + '\n', 'utf8');
}

await mkdir(outputRoot, { recursive: true });
if (!await runtimeIsCurrent()) {
  await downloadArchive();
  await extractArchive();
  await configureRuntime();
}
console.log(`WINDOWS_PYTHON_RUNTIME_OK version=${PYTHON_VERSION} sha256=${EXPECTED_SHA256}`);
