import { execFileSync, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import process from 'node:process';

if (process.platform !== 'win32') {
  console.log('WINDOWS_NATIVE_HELPERS_SKIPPED platform=' + process.platform);
  process.exit(0);
}

const root = resolve(import.meta.dirname, '..');
const sourceDirectory = join(root, 'skills', 'document-content-analysis', 'scripts');
const outputDirectory = join(root, 'src-tauri', 'target', 'yunspire-native');
const helpers = [
  {
    name: 'PDF',
    baseName: 'yunspire_pdf_windows',
    libraries: ['windowsapp.lib'],
  },
  {
    name: 'WIC 图片派生',
    baseName: 'yunspire_image_windows',
    libraries: ['windowscodecs.lib', 'ole32.lib', 'oleaut32.lib'],
  },
].map((helper) => ({
  ...helper,
  source: join(sourceDirectory, `${helper.baseName}.cpp`),
  output: join(outputDirectory, `${helper.baseName}.exe`),
  object: join(outputDirectory, `${helper.baseName}.obj`),
  stamp: join(outputDirectory, `${helper.baseName}.sha256`),
}));

function quote(value) {
  return `"${String(value).replaceAll('"', '""')}"`;
}

async function findVsWhere() {
  const candidates = [
    process.env.VSWHERE,
    process.env['ProgramFiles(x86)'] && join(process.env['ProgramFiles(x86)'], 'Microsoft Visual Studio', 'Installer', 'vswhere.exe'),
    process.env.ProgramFiles && join(process.env.ProgramFiles, 'Microsoft Visual Studio', 'Installer', 'vswhere.exe'),
  ].filter(Boolean);
  for (const candidate of candidates) if (await existingFile(candidate)) return candidate;
  return null;
}

async function existingFile(path) {
  return stat(path).then((value) => value.isFile()).catch(() => false);
}

async function msvcEnvironment() {
  const vswhere = await findVsWhere();
  if (!vswhere) {
    throw new Error('未找到 Visual Studio vswhere.exe，无法构建 Windows 原生执行器');
  }
  const installationPath = execFileSync(vswhere, [
    '-latest',
    '-products', '*',
    '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64',
    '-property', 'installationPath',
  ], { encoding: 'utf8' }).trim();
  if (!installationPath) throw new Error('未找到包含 MSVC x64 工具链的 Visual Studio Build Tools');
  const vcvars = join(installationPath, 'VC', 'Auxiliary', 'Build', 'vcvars64.bat');
  if (!await existingFile(vcvars)) throw new Error(`MSVC 环境脚本不存在：${vcvars}`);
  return vcvars;
}

async function compile(helper, vcvars) {
  const sourceBytes = await readFile(helper.source);
  const sourceHash = createHash('sha256')
    .update(sourceBytes)
    .update('\0msvc-cxx20-mt-v2\0')
    .update(helper.libraries.join('\0'))
    .digest('hex');
  const currentHash = await readFile(helper.stamp, 'utf8').catch(() => '');
  if (currentHash.trim() === sourceHash && await existingFile(helper.output)) return;

  const commandFile = join(outputDirectory, `.build-${helper.baseName}.cmd`);
  const command = [
    '@echo off',
    `call ${quote(vcvars)} >nul`,
    'if errorlevel 1 exit /b 1',
    [
      'cl.exe', '/nologo', '/std:c++20', '/EHsc', '/O2', '/MT', '/utf-8', '/permissive-',
      '/W4', '/WX', '/external:W0', '/external:anglebrackets',
      '/DUNICODE', '/D_UNICODE', '/DNOMINMAX', quote(helper.source),
      `/Fo:${quote(helper.object)}`, `/Fe:${quote(helper.output)}`,
      '/link', ...helper.libraries,
    ].join(' '),
  ].join('\r\n') + '\r\n';
  await writeFile(commandFile, command, 'utf8');
  try {
    const result = spawnSync(process.env.ComSpec || 'cmd.exe', ['/d', '/c', commandFile], {
      cwd: root,
      encoding: 'utf8',
      windowsHide: true,
    });
    if (result.status !== 0) {
      throw new Error(`Windows 原生${helper.name}执行器构建失败\n${result.stdout || ''}\n${result.stderr || ''}`.trim());
    }
    await writeFile(helper.stamp, sourceHash + '\n', 'utf8');
    await rm(helper.object, { force: true });
  } finally {
    await rm(commandFile, { force: true });
  }
}

await mkdir(outputDirectory, { recursive: true });
const vcvars = await msvcEnvironment();
for (const helper of helpers) await compile(helper, vcvars);
console.log(`WINDOWS_NATIVE_HELPERS_OK count=${helpers.length}`);
