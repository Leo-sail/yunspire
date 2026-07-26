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
const smokeDirectory = join(outputDirectory, '.pdf-smoke');
const helpers = [
  {
    name: 'PDF',
    baseName: 'yunspire_pdf_windows',
    libraries: ['windowsapp.lib'],
  },
  {
    name: 'WIC 图片派生',
    baseName: 'yunspire_image_windows',
    libraries: ['windowscodecs.lib', 'ole32.lib'],
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

function createSmokePdf() {
  const stream = 'BT /F1 18 Tf 24 92 Td (Yunspire Windows PDF) Tj ET\n';
  const objects = [
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 240 120] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>',
    `<< /Length ${Buffer.byteLength(stream, 'ascii')} >>\nstream\n${stream}endstream`,
    '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
  ];
  const parts = ['%PDF-1.4\n'];
  const offsets = [0];
  let byteLength = Buffer.byteLength(parts[0], 'ascii');
  for (let index = 0; index < objects.length; index += 1) {
    offsets.push(byteLength);
    const objectText = `${index + 1} 0 obj\n${objects[index]}\nendobj\n`;
    parts.push(objectText);
    byteLength += Buffer.byteLength(objectText, 'ascii');
  }
  const xrefOffset = byteLength;
  parts.push(`xref\n0 ${objects.length + 1}\n0000000000 65535 f \n`);
  for (const offset of offsets.slice(1)) parts.push(`${String(offset).padStart(10, '0')} 00000 n \n`);
  parts.push(`trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xrefOffset}\n%%EOF\n`);
  return Buffer.from(parts.join(''), 'ascii');
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
    .update('\0msvc-cxx17-mt-v1\0')
    .update(helper.libraries.join('\0'))
    .digest('hex');
  const currentHash = await readFile(helper.stamp, 'utf8').catch(() => '');
  if (currentHash.trim() === sourceHash && await existingFile(helper.output)) return;

  const command = [
    'call', quote(vcvars), '>nul', '&&',
    'cl.exe', '/nologo', '/std:c++17', '/EHsc', '/O2', '/MT', '/utf-8', '/permissive-',
    '/W4', '/WX', '/external:W0', '/external:anglebrackets',
    '/DUNICODE', '/D_UNICODE', quote(helper.source),
    `/Fo:${quote(helper.object)}`, `/Fe:${quote(helper.output)}`,
    '/link', ...helper.libraries,
  ].join(' ');
  const result = spawnSync(process.env.ComSpec || 'cmd.exe', ['/d', '/s', '/c', command], {
    cwd: root,
    encoding: 'utf8',
    windowsHide: true,
  });
  if (result.status !== 0) {
    throw new Error(`Windows 原生${helper.name}执行器构建失败\n${result.stdout || ''}\n${result.stderr || ''}`.trim());
  }
  await writeFile(helper.stamp, sourceHash + '\n', 'utf8');
  await rm(helper.object, { force: true });
}

async function smokeTest() {
  await rm(smokeDirectory, { recursive: true, force: true });
  await mkdir(smokeDirectory, { recursive: true });
  try {
    const sourcePdf = join(smokeDirectory, 'input.pdf');
    const renderedDirectory = join(smokeDirectory, 'pages');
    await writeFile(sourcePdf, createSmokePdf());
    await mkdir(renderedDirectory, { recursive: true });
    const pdfOutput = helpers.find((helper) => helper.baseName === 'yunspire_pdf_windows').output;
    const imageOutput = helpers.find((helper) => helper.baseName === 'yunspire_image_windows').output;
    const result = spawnSync(pdfOutput, [sourcePdf, renderedDirectory], {
      cwd: root,
      encoding: 'utf8',
      windowsHide: true,
      timeout: 60_000,
    });
    if (result.status !== 0) throw new Error(`适配器异常退出：${result.status}\n${result.stderr || ''}`);
    const payload = JSON.parse(result.stdout);
    if (payload.schema !== 'yunspire.windows-pdf.v1'
      || payload.renderer !== 'Windows.Data.Pdf'
      || payload.page_count !== 1
      || payload.rendered_page_count !== 1
      || payload.errors?.length) {
      throw new Error(`适配器冒烟结果不完整：${result.stdout}`);
    }
    const page = payload.pages?.[0];
    const bytes = await readFile(page?.path || '');
    if (bytes.length !== page.byte_length || bytes[0] !== 0xff || bytes[1] !== 0xd8) {
      throw new Error('适配器没有生成经过字节校验的 JPEG 页面');
    }
    const derivativePath = join(smokeDirectory, 'derived.jpg');
    const derivative = spawnSync(imageOutput, [page.path, derivativePath, '96'], {
      cwd: root,
      encoding: 'utf8',
      windowsHide: true,
      timeout: 60_000,
    });
    if (derivative.status !== 0) throw new Error(`WIC 执行器异常退出：${derivative.status}\n${derivative.stderr || ''}`);
    const derivativePayload = JSON.parse(derivative.stdout);
    if (derivativePayload.schema !== 'yunspire.windows-image-derivative.v1'
      || derivativePayload.encoder !== 'Windows Imaging Component'
      || derivativePayload.output_width > 96
      || derivativePayload.output_height > 96
      || !derivativePayload.derived
      || derivativePayload.errors?.length) {
      throw new Error(`WIC 图片派生结果不完整：${derivative.stdout}`);
    }
    const derivativeBytes = await readFile(derivativePayload.path || '');
    if (derivativeBytes.length !== derivativePayload.byte_length
      || derivativeBytes[0] !== 0xff || derivativeBytes[1] !== 0xd8) {
      throw new Error('WIC 执行器没有生成经过尺寸与字节校验的 JPEG');
    }
  } finally {
    await rm(smokeDirectory, { recursive: true, force: true });
  }
}

await mkdir(outputDirectory, { recursive: true });
const vcvars = await msvcEnvironment();
for (const helper of helpers) await compile(helper, vcvars);
await smokeTest();
console.log(`WINDOWS_NATIVE_HELPERS_OK count=${helpers.length}`);
