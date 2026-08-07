import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { copyFile, mkdir, readFile, readdir, rm, stat, writeFile } from 'node:fs/promises';
import { basename, dirname, join, resolve, sep } from 'node:path';
import process from 'node:process';

if (process.platform !== 'win32') {
  throw new Error('Windows 发布核验只能在 Windows Runner 上执行');
}

const phase = process.argv[2] || 'helpers';
if (!['helpers', 'bundle'].includes(phase)) {
  throw new Error(`未知 Windows 发布核验阶段：${phase}`);
}

const root = resolve(import.meta.dirname, '..');
const nativeDirectory = join(root, 'src-tauri', 'target', 'yunspire-native');
const runtimeDirectory = join(root, 'src-tauri', 'target', 'yunspire-runtime', 'python');
const artifactDirectory = join(root, 'artifacts', 'windows');
const windowsConfigPath = join(root, 'src-tauri', 'tauri.windows.conf.json');
const packagePath = join(root, 'package.json');
const EXPECTED_PYTHON_LICENSE_SHA256 = '62bec384df47b0328307db41455ff6ea2559e5546b394ac69148561b21703120';
const helperResources = [
  {
    source: 'target/yunspire-native/yunspire_pdf_windows.exe',
    destination: 'skills/document-content-analysis/scripts/yunspire_pdf_windows.exe',
    path: join(nativeDirectory, 'yunspire_pdf_windows.exe'),
  },
  {
    source: 'target/yunspire-native/yunspire_image_windows.exe',
    destination: 'skills/document-content-analysis/scripts/yunspire_image_windows.exe',
    path: join(nativeDirectory, 'yunspire_image_windows.exe'),
  },
  {
    source: 'target/yunspire-native/yunspire-media.exe',
    destination: 'skills/video-content-analysis/scripts/bin/yunspire-media.exe',
    path: join(nativeDirectory, 'yunspire-media.exe'),
  },
  {
    source: 'target/yunspire-native/yunspire-speech.exe',
    destination: 'skills/video-content-analysis/scripts/bin/yunspire-speech.exe',
    path: join(nativeDirectory, 'yunspire-speech.exe'),
  },
];
const legalResources = [
  { source: '../LICENSE', destination: 'legal/LICENSE', path: join(root, 'LICENSE') },
  { source: '../NOTICE', destination: 'legal/NOTICE', path: join(root, 'NOTICE') },
  {
    source: 'target/yunspire-licenses/THIRD_PARTY_NOTICES.txt',
    destination: 'legal/THIRD_PARTY_NOTICES.txt',
    path: join(root, 'src-tauri', 'target', 'yunspire-licenses', 'THIRD_PARTY_NOTICES.txt'),
  },
];

async function fileSize(path) {
  const value = await stat(path).catch(() => null);
  if (!value?.isFile() || value.size <= 0) throw new Error(`缺少构建资源：${path}`);
  return value.size;
}

async function sha256(path) {
  return createHash('sha256').update(await readFile(path)).digest('hex');
}

async function assertPortableExecutable(path) {
  const bytes = await readFile(path);
  if (bytes.length < 2 || bytes[0] !== 0x4d || bytes[1] !== 0x5a) {
    throw new Error(`原生执行器不是有效的 Windows PE 文件：${path}`);
  }
}

function assertAuthenticodeNotSigned(path, label) {
  const signaturePathVariable = 'YUNSPIRE_AUTHENTICODE_PATH';
  const signature = runJson('pwsh.exe', [
    '-NoLogo',
    '-NoProfile',
    '-NonInteractive',
    '-Command',
    '$ErrorActionPreference = "Stop"; '
      + `$signature = Get-AuthenticodeSignature -LiteralPath $env:${signaturePathVariable} -ErrorAction Stop; `
      + 'if ($null -eq $signature) { throw "Authenticode signature probe returned no result" }; '
      + '$status = $signature.Status; '
      + 'if ($null -eq $status) { throw "Authenticode signature probe returned no status" }; '
      + '$status = $status.ToString(); '
      + 'if ([string]::IsNullOrWhiteSpace($status)) { throw "Authenticode signature probe returned an empty status" }; '
      + 'ConvertTo-Json -InputObject ([string]$status) -Compress',
  ], {
    env: { ...process.env, [signaturePathVariable]: path },
    timeout: 60_000,
    rejectStderr: true,
  });
  if (signature !== 'NotSigned') {
    throw new Error(`${label} Authenticode 状态不是 NotSigned：${signature || 'unknown'}；${path}`);
  }
}

function runJson(path, args = [], options = {}) {
  const result = spawnSync(path, args, {
    cwd: options.cwd || root,
    encoding: 'utf8',
    env: options.env || process.env,
    maxBuffer: 16 * 1024 * 1024,
    windowsHide: true,
    timeout: options.timeout || 30_000,
  });
  if (result.error || result.status !== 0) {
    throw new Error(`原生执行器启动失败：${path}\n${result.error || ''}\n${result.stdout || ''}\n${result.stderr || ''}`.trim());
  }
  if (options.stdoutIncludes && !result.stdout.includes(options.stdoutIncludes)) {
    throw new Error(`执行器没有原样输出预期的 UTF-8 文本“${options.stdoutIncludes}”：${path}`);
  }
  if (options.rejectStderr && result.stderr.trim()) {
    throw new Error(`执行器写入了 stderr：${path}\n${result.stderr}`.trim());
  }
  try {
    return JSON.parse(result.stdout);
  } catch {
    throw new Error(`原生执行器没有返回 JSON：${path}\n${result.stdout || ''}`.trim());
  }
}

function gitRevision(revision) {
  const result = spawnSync('git', ['rev-parse', revision], {
    cwd: root,
    encoding: 'utf8',
    windowsHide: true,
    timeout: 30_000,
  });
  const value = result.stdout?.trim().toLowerCase();
  if (result.error || result.status !== 0 || !/^[0-9a-f]{40}$/u.test(value || '')) {
    throw new Error(`无法解析发布源码版本 ${revision}：${result.error || result.stderr || value || 'unknown'}`);
  }
  return value;
}

function githubRunValue(name) {
  const value = process.env[name]?.trim() || null;
  if (value && !/^\d+$/u.test(value)) {
    throw new Error(`无效的 GitHub Actions ${name}：${value}`);
  }
  if (process.env.GITHUB_ACTIONS === 'true' && !value) {
    throw new Error(`GitHub Actions 发布缺少 ${name}`);
  }
  return value;
}

function workflowGate(name) {
  const outcome = process.env[name]?.trim().toLowerCase() || null;
  if (process.env.GITHUB_ACTIONS === 'true' && outcome !== 'success') {
    throw new Error(`GitHub Actions 发布门禁未通过 ${name}：${outcome || 'missing'}`);
  }
  return outcome === 'success' ? true : null;
}

function pythonUtf8Environment() {
  return {
    ...process.env,
    PYTHONUTF8: '1',
    PYTHONIOENCODING: 'utf-8',
  };
}

const windowsRunpyBootstrap = "import runpy,sys;script=sys.argv[1];sys.path.insert(0,sys.argv[2]);sys.argv=[script,*sys.argv[3:]];runpy.run_path(script,run_name='__main__')";

function runInstalledPythonScript(python, script, args, options = {}) {
  return runJson(python, [
    '-c',
    windowsRunpyBootstrap,
    script,
    dirname(script),
    ...args,
  ], {
    ...options,
    cwd: options.cwd || dirname(script),
  });
}

function riffChunk(id, payload) {
  if (id.length !== 4) throw new Error(`RIFF FourCC 必须为 4 字节：${id}`);
  const header = Buffer.alloc(8);
  header.write(id, 0, 4, 'ascii');
  header.writeUInt32LE(payload.length, 4);
  return Buffer.concat([header, payload, payload.length % 2 ? Buffer.alloc(1) : Buffer.alloc(0)]);
}

function riffList(type, chunks) {
  return riffChunk('LIST', Buffer.concat([Buffer.from(type, 'ascii'), ...chunks]));
}

function createAviMainHeader({ width, height, frames, frameBytes, audioBytesPerSecond }) {
  const value = Buffer.alloc(56);
  value.writeUInt32LE(1_000_000, 0);
  value.writeUInt32LE(frameBytes + audioBytesPerSecond, 4);
  value.writeUInt32LE(0x00000910, 12); // HASINDEX | ISINTERLEAVED | TRUSTCKTYPE
  value.writeUInt32LE(frames, 16);
  value.writeUInt32LE(2, 24);
  value.writeUInt32LE(Math.max(frameBytes, audioBytesPerSecond), 28);
  value.writeUInt32LE(width, 32);
  value.writeUInt32LE(height, 36);
  return value;
}

function createAviStreamHeader({ type, handler, scale, rate, length, bufferSize, sampleSize, width = 0, height = 0 }) {
  const value = Buffer.alloc(56);
  value.write(type, 0, 4, 'ascii');
  value.write(handler, 4, 4, 'ascii');
  value.writeUInt32LE(scale, 20);
  value.writeUInt32LE(rate, 24);
  value.writeUInt32LE(length, 32);
  value.writeUInt32LE(bufferSize, 36);
  value.writeUInt32LE(0xffffffff, 40);
  value.writeUInt32LE(sampleSize, 44);
  value.writeInt16LE(width, 52);
  value.writeInt16LE(height, 54);
  return value;
}

function createBitmapInfo(width, height, frameBytes, compression = '') {
  const value = Buffer.alloc(40);
  value.writeUInt32LE(40, 0);
  value.writeInt32LE(width, 4);
  value.writeInt32LE(height, 8);
  value.writeUInt16LE(1, 12);
  value.writeUInt16LE(24, 14);
  if (compression) value.write(compression, 16, 4, 'ascii');
  value.writeUInt32LE(frameBytes, 20);
  return value;
}

function createWaveFormat(sampleRate, formatTag = 1) {
  const value = Buffer.alloc(16);
  value.writeUInt16LE(formatTag, 0); // PCM format for the generated probe header.
  value.writeUInt16LE(1, 2);
  value.writeUInt32LE(sampleRate, 4);
  value.writeUInt32LE(sampleRate * 2, 8);
  value.writeUInt16LE(2, 12);
  value.writeUInt16LE(16, 14);
  return value;
}

function createVideoFrame(width, height, frameIndex) {
  const stride = Math.ceil((width * 3) / 4) * 4;
  const value = Buffer.alloc(stride * height);
  for (let row = 0; row < height; row += 1) {
    for (let column = 0; column < width; column += 1) {
      const checker = (Math.floor(row / 6) + Math.floor(column / 6) + frameIndex) % 2;
      const offset = row * stride + column * 3;
      value[offset] = checker ? 230 - frameIndex * 20 : 20 + frameIndex * 30;
      value[offset + 1] = checker ? 40 + frameIndex * 35 : 210 - frameIndex * 20;
      value[offset + 2] = frameIndex === 2 ? (checker ? 220 : 35) : (checker ? 55 : 190);
    }
  }
  return value;
}

function createPcmSecond(sampleRate, secondIndex) {
  const value = Buffer.alloc(sampleRate * 2);
  const frequency = 330 + secondIndex * 110;
  for (let sample = 0; sample < sampleRate; sample += 1) {
    const envelope = sample < 320 || sample > sampleRate - 320 ? 0 : 1;
    const amplitude = Math.round(Math.sin((2 * Math.PI * frequency * sample) / sampleRate) * 9000 * envelope);
    value.writeInt16LE(amplitude, sample * 2);
  }
  return value;
}

function createMediaSmokeAvi({ videoHandler = 'DIB ', videoCompression = '', audioFormatTag = 1 } = {}) {
  const width = 64;
  const height = 48;
  const seconds = 3;
  const sampleRate = 16_000;
  const videoFrames = Array.from({ length: seconds }, (_, index) => createVideoFrame(width, height, index));
  const audioChunks = Array.from({ length: seconds }, (_, index) => createPcmSecond(sampleRate, index));
  const frameBytes = videoFrames[0].length;
  const audioBytesPerSecond = sampleRate * 2;
  const videoList = riffList('strl', [
    riffChunk('strh', createAviStreamHeader({
      type: 'vids', handler: videoHandler, scale: 1, rate: 1, length: seconds,
      bufferSize: frameBytes, sampleSize: 0, width, height,
    })),
    riffChunk('strf', createBitmapInfo(width, height, frameBytes, videoCompression)),
    riffChunk('strn', Buffer.from('Yunspire video smoke\0', 'ascii')),
  ]);
  const audioList = riffList('strl', [
    riffChunk('strh', createAviStreamHeader({
      type: 'auds', handler: '\0\0\0\0', scale: 2, rate: audioBytesPerSecond,
      length: sampleRate * seconds, bufferSize: audioBytesPerSecond, sampleSize: 2,
    })),
    riffChunk('strf', createWaveFormat(sampleRate, audioFormatTag)),
    riffChunk('strn', Buffer.from('Yunspire audio smoke\0', 'ascii')),
  ]);
  const headerList = riffList('hdrl', [
    riffChunk('avih', createAviMainHeader({ width, height, frames: seconds, frameBytes, audioBytesPerSecond })),
    videoList,
    audioList,
  ]);

  const mediaChunks = [];
  const indexEntries = [];
  let offset = 4;
  for (let index = 0; index < seconds; index += 1) {
    for (const item of [
      { id: '00db', flags: 0x10, payload: videoFrames[index] },
      { id: '01wb', flags: 0, payload: audioChunks[index] },
    ]) {
      const chunk = riffChunk(item.id, item.payload);
      mediaChunks.push(chunk);
      const entry = Buffer.alloc(16);
      entry.write(item.id, 0, 4, 'ascii');
      entry.writeUInt32LE(item.flags, 4);
      entry.writeUInt32LE(offset, 8);
      entry.writeUInt32LE(item.payload.length, 12);
      indexEntries.push(entry);
      offset += chunk.length;
    }
  }
  const body = Buffer.concat([
    Buffer.from('AVI ', 'ascii'),
    headerList,
    riffList('movi', mediaChunks),
    riffChunk('idx1', Buffer.concat(indexEntries)),
  ]);
  const riffHeader = Buffer.alloc(8);
  riffHeader.write('RIFF', 0, 4, 'ascii');
  riffHeader.writeUInt32LE(body.length, 4);
  return Buffer.concat([riffHeader, body]);
}

function createPdfSmokeDocument() {
  const pageOneContent = 'BT\n/F1 18 Tf\n72 720 Td\n(Yunspire Windows PDF smoke page 1) Tj\nET';
  const pageTwoContent = 'BT\n/F1 18 Tf\n72 720 Td\n(Yunspire Windows PDF smoke page 2) Tj\nET';
  const objects = [
    '<< /Type /Catalog /Pages 2 0 R >>',
    '<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>',
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 7 0 R >> >> /Contents 4 0 R >>',
    `<< /Length ${Buffer.byteLength(pageOneContent, 'ascii')} >>\nstream\n${pageOneContent}\nendstream`,
    '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 7 0 R >> >> /Contents 6 0 R >>',
    `<< /Length ${Buffer.byteLength(pageTwoContent, 'ascii')} >>\nstream\n${pageTwoContent}\nendstream`,
    '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
  ];
  const chunks = [Buffer.from('%PDF-1.4\n%YUNSPIRE\n', 'ascii')];
  const offsets = [0];
  for (let index = 0; index < objects.length; index += 1) {
    offsets.push(chunks.reduce((total, chunk) => total + chunk.length, 0));
    chunks.push(Buffer.from(`${index + 1} 0 obj\n${objects[index]}\nendobj\n`, 'ascii'));
  }
  const xrefOffset = chunks.reduce((total, chunk) => total + chunk.length, 0);
  const xref = [
    `xref\n0 ${objects.length + 1}\n`,
    '0000000000 65535 f \n',
    ...offsets.slice(1).map((offset) => `${String(offset).padStart(10, '0')} 00000 n \n`),
    `trailer\n<< /Size ${objects.length + 1} /Root 1 0 R >>\nstartxref\n${xrefOffset}\n%%EOF\n`,
  ].join('');
  chunks.push(Buffer.from(xref, 'ascii'));
  return Buffer.concat(chunks);
}

function assertPathInside(path, directory, label) {
  const absolutePath = resolve(path);
  const absoluteDirectory = resolve(directory);
  if (!absolutePath.startsWith(absoluteDirectory + sep)) {
    throw new Error(`${label} 逃逸媒体冒烟目录：${path}`);
  }
  return absolutePath;
}

async function assertJpeg(path, label) {
  const bytes = await readFile(path);
  if (bytes.length < 128 || bytes[0] !== 0xff || bytes[1] !== 0xd8
    || bytes.at(-2) !== 0xff || bytes.at(-1) !== 0xd9) {
    throw new Error(`${label} 不是完整 JPEG：${path}`);
  }
  return bytes.length;
}

async function smokePdfHelper(executable, smokeDirectory) {
  const sourceDirectory = join(smokeDirectory, 'PDF 中文来源');
  const outputDirectory = join(smokeDirectory, 'PDF 页面输出');
  await mkdir(sourceDirectory, { recursive: true });
  await mkdir(outputDirectory, { recursive: true });
  const source = join(sourceDirectory, '云枢 示例文档.pdf');
  await writeFile(source, createPdfSmokeDocument());
  const pdf = runJson(executable, [source, outputDirectory], {
    stdoutIncludes: 'PDF 页面输出',
    timeout: 120_000,
  });
  if (pdf.schema !== 'yunspire.windows-pdf.v1'
    || pdf.renderer !== 'Windows.Data.Pdf'
    || pdf.page_count !== 2
    || pdf.rendered_page_count !== 2
    || !Array.isArray(pdf.pages) || pdf.pages.length !== 2
    || !Array.isArray(pdf.errors) || pdf.errors.length) {
    throw new Error(`Windows.Data.Pdf 中文路径冒烟结果无效：${JSON.stringify(pdf)}`);
  }
  const jpegByteLengths = [];
  for (const page of pdf.pages) {
    const pagePath = assertPathInside(page.path, outputDirectory, 'PDF 页面');
    jpegByteLengths.push(await assertJpeg(pagePath, 'PDF 页面'));
  }
  return {
    source,
    outputDirectory,
    pageCount: pdf.page_count,
    jpegByteLength: jpegByteLengths[0],
    jpegByteLengths,
  };
}

async function smokeMediaHelpers() {
  const smokeDirectory = join(nativeDirectory, '.媒体冒烟');
  const outputDirectory = join(smokeDirectory, '正常输出');
  await rm(smokeDirectory, { recursive: true, force: true });
  await mkdir(outputDirectory, { recursive: true });
  try {
    const source = join(smokeDirectory, '云枢 音视频冒烟.avi');
    await writeFile(source, createMediaSmokeAvi());
    const media = runJson(join(nativeDirectory, 'yunspire-media.exe'), [source, outputDirectory]);
    if (media.schema !== 'yunspire.windows-media.v1'
      || media.frame_selection_method !== 'yunspire-windows-mediafoundation-v1'
      || !(media.duration_seconds > 0)
      || media.video_stream_present !== true
      || media.audio_stream_present !== true
      || media.errors?.length
      || !Array.isArray(media.frames) || media.frames.length < 1
      || media.frames.length !== media.frame_timestamps_ms?.length
      || media.frames.length !== media.frame_difference_scores?.length) {
      throw new Error(`Media Foundation/WIC 真实冒烟结果无效：${JSON.stringify(media)}`);
    }
    for (const framePath of media.frames) {
      await assertJpeg(assertPathInside(framePath, outputDirectory, 'JPEG 帧'), 'WIC 关键帧');
    }
    const audioPath = assertPathInside(media.audio_path || '', outputDirectory, 'PCM WAV');
    const wave = await readFile(audioPath);
    if (wave.length <= 44 || wave.toString('ascii', 0, 4) !== 'RIFF'
      || wave.toString('ascii', 8, 12) !== 'WAVE'
      || wave.readUInt16LE(20) !== 1
      || wave.readUInt16LE(34) !== 16
      || wave.readUInt32LE(40) + 44 !== wave.length) {
      throw new Error('Media Foundation 没有输出字节完整的 16-bit PCM WAV');
    }

    const speech = runJson(join(nativeDirectory, 'yunspire-speech.exe'), [audioPath, 'en-US']);
    const hasTranscript = typeof speech.transcript === 'string'
      && speech.transcript.trim().length > 0
      && Array.isArray(speech.segments) && speech.segments.length > 0
      && speech.segments.every((segment) => typeof segment.text === 'string' && segment.text.trim().length > 0
        && Number.isFinite(segment.start_ms) && Number.isFinite(segment.end_ms)
        && segment.end_ms >= segment.start_ms);
    const explicitOfflineSpeechErrors = new Set([
      'windows_sapi_dictation_unavailable',
      'windows_sapi_language_unavailable',
      'windows_sapi_recognizer_unavailable',
      'windows_sapi_transcript_unavailable',
    ]);
    const hasExplicitOfflineResult = Array.isArray(speech.errors)
      && speech.errors.length > 0
      && speech.errors.every((error) => explicitOfflineSpeechErrors.has(error));
    if (speech.schema !== 'yunspire.windows-speech.v1'
      || !Array.isArray(speech.warnings) || !Array.isArray(speech.errors)
      || !Array.isArray(speech.segments) || (!hasTranscript && !hasExplicitOfflineResult)) {
      throw new Error(`SAPI 真实 WAV 冒烟没有返回转写或明确离线能力错误：${JSON.stringify(speech)}`);
    }

    const codecSource = join(smokeDirectory, '缺少系统编解码器.avi');
    const codecOutput = join(smokeDirectory, '编解码错误输出');
    await mkdir(codecOutput, { recursive: true });
    await writeFile(codecSource, createMediaSmokeAvi({
      videoHandler: 'ZZZZ',
      videoCompression: 'ZZZZ',
      audioFormatTag: 0x7777,
    }));
    const codec = runJson(join(nativeDirectory, 'yunspire-media.exe'), [codecSource, codecOutput]);
    if (codec.schema !== 'yunspire.windows-media.v1'
      || !Array.isArray(codec.errors)
      || !codec.errors.includes('windows_media_codec_unavailable')) {
      throw new Error(`缺失系统编解码器没有返回稳定错误契约：${JSON.stringify(codec)}`);
    }
    return {
      durationSeconds: media.duration_seconds,
      frameCount: media.frames.length,
      wavByteLength: wave.length,
      speechVerification: hasTranscript
        ? { outcome: 'real_transcript', segmentCount: speech.segments.length }
        : { outcome: 'offline_capability_contract', errors: speech.errors },
      codecVerification: codec.errors,
    };
  } finally {
    await rm(smokeDirectory, { recursive: true, force: true, maxRetries: 5, retryDelay: 250 });
  }
}

async function verifyRuntime(resources) {
  const runtimeResource = resources['target/yunspire-runtime/python/'];
  if (runtimeResource !== 'runtime/python/') {
    throw new Error('Windows 配置缺少嵌入式 Python 资源映射');
  }
  const python = join(runtimeDirectory, 'python.exe');
  await fileSize(python);
  await assertPortableExecutable(python);
  const manifest = JSON.parse(await readFile(join(runtimeDirectory, 'YUNSPIRE_RUNTIME.json'), 'utf8'));
  const license = join(runtimeDirectory, 'LICENSE.txt');
  await fileSize(license);
  const licenseSha256 = await sha256(license);
  if (manifest.schema !== 'yunspire.windows-python-runtime.v1'
    || manifest.architecture !== 'x64'
    || !/^https:\/\/www\.python\.org\//u.test(manifest.sourceUrl || '')
    || !/^[a-f0-9]{64}$/u.test(manifest.archiveSha256 || '')
    || manifest.licenseFile !== 'LICENSE.txt'
    || manifest.licenseSha256 !== EXPECTED_PYTHON_LICENSE_SHA256
    || licenseSha256 !== EXPECTED_PYTHON_LICENSE_SHA256) {
    throw new Error('嵌入式 Python 运行时清单不完整或来源无效');
  }
  const unicodeProbe = '云枢中文输出/资料库';
  const smoke = spawnSync(python, [
    '-c',
    'import json,sys;print(json.dumps({"runtime":"ok","unicode":sys.argv[1]},ensure_ascii=False))',
    unicodeProbe,
  ], {
    cwd: runtimeDirectory,
    encoding: 'utf8',
    env: pythonUtf8Environment(),
    maxBuffer: 4 * 1024 * 1024,
    windowsHide: true,
    timeout: 30_000,
  });
  const smokePayload = smoke.status === 0 ? JSON.parse(smoke.stdout) : null;
  if (smoke.status !== 0 || smokePayload?.runtime !== 'ok' || smokePayload.unicode !== unicodeProbe
    || !smoke.stdout.includes(unicodeProbe)) {
    throw new Error(`嵌入式 Python 独立模式冒烟失败\n${smoke.stdout || ''}\n${smoke.stderr || ''}`.trim());
  }
  return { path: python, license, manifest };
}

async function verifyHelpers() {
  const windowsConfig = JSON.parse(await readFile(windowsConfigPath, 'utf8'));
  const resources = windowsConfig.bundle?.resources || {};
  const files = [];
  for (const helper of helperResources) {
    if (resources[helper.source] !== helper.destination) {
      throw new Error(`Windows 资源映射错误：${helper.source}`);
    }
    const size = await fileSize(helper.path);
    await assertPortableExecutable(helper.path);
    files.push({ path: helper.path, size, sha256: await sha256(helper.path) });
  }
  for (const legal of legalResources) {
    if (resources[legal.source] !== legal.destination) {
      throw new Error(`Windows 法律资源映射错误：${legal.source}`);
    }
    await fileSize(legal.path);
  }

  const mediaSmoke = await smokeMediaHelpers();
  const pdfSmokeDirectory = join(nativeDirectory, '.PDF-冒烟');
  await rm(pdfSmokeDirectory, { recursive: true, force: true });
  let pdfSmoke;
  try {
    pdfSmoke = await smokePdfHelper(
      join(nativeDirectory, 'yunspire_pdf_windows.exe'),
      pdfSmokeDirectory,
    );
  } finally {
    await rm(pdfSmokeDirectory, { recursive: true, force: true, maxRetries: 5, retryDelay: 250 });
  }
  const runtime = await verifyRuntime(resources);
  console.log(JSON.stringify({
    schema: 'yunspire.windows-build-inputs.v1',
    helpers: files,
    mediaSmoke,
    pdfSmoke: {
      pageCount: pdfSmoke.pageCount,
      jpegByteLength: pdfSmoke.jpegByteLength,
    },
    python: { path: runtime.path, version: runtime.manifest.version, sha256: runtime.manifest.archiveSha256 },
  }));
}

async function findInstaller() {
  const directories = [
    join(root, 'src-tauri', 'target', 'x86_64-pc-windows-msvc', 'release', 'bundle', 'nsis'),
    join(root, 'src-tauri', 'target', 'release', 'bundle', 'nsis'),
  ];
  const installers = [];
  for (const directory of directories) {
    const entries = await readdir(directory, { withFileTypes: true }).catch(() => []);
    installers.push(...entries
      .filter((entry) => entry.isFile() && entry.name.toLowerCase().endsWith('-setup.exe'))
      .map((entry) => join(directory, entry.name)));
  }
  if (installers.length !== 1) {
    throw new Error(`预期生成一个 NSIS 安装器，实际为 ${installers.length}：${directories.join(', ')}`);
  }
  return installers[0];
}

function assertSpeechVerification(payload, requestedLocale, label) {
  const errors = Array.isArray(payload.errors) ? payload.errors : [];
  const segments = Array.isArray(payload.transcript_segments) ? payload.transcript_segments : [];
  const hasTranscript = typeof payload.transcript === 'string'
    && payload.transcript.trim().length > 0
    && segments.length > 0
    && segments.every((segment) => typeof segment.text === 'string' && segment.text.trim().length > 0);
  const explicitOfflineErrors = new Set([
    'windows_sapi_dictation_unavailable',
    'windows_sapi_language_unavailable',
    'windows_sapi_recognizer_unavailable',
    'windows_sapi_transcript_unavailable',
  ]);
  const speechErrors = errors.filter((error) => explicitOfflineErrors.has(error));
  const unexpectedErrors = errors.filter((error) => !explicitOfflineErrors.has(error));
  if (payload.metadata?.speech_locale !== requestedLocale
    || unexpectedErrors.length
    || (!hasTranscript && !speechErrors.length)
    || (hasTranscript && payload.metadata?.speech_on_device !== true)) {
    throw new Error(`${label} 没有完成 locale 贯穿或返回精确本地转写契约：${JSON.stringify(payload)}`);
  }
  return hasTranscript
    ? { outcome: 'real_transcript', segmentCount: segments.length, locale: requestedLocale }
    : { outcome: 'offline_capability_contract', errors: speechErrors, locale: requestedLocale };
}

async function smokeInstalledResources(installDirectory, installedPython) {
  const smokeDirectory = join(installDirectory, '安装后验证', '中文 路径');
  const pythonEnvironment = pythonUtf8Environment();
  await mkdir(smokeDirectory, { recursive: true });

  const unicodeProbe = '云枢安装后中文 JSON';
  const python = runJson(installedPython, [
    '-c',
    'import json,sys;print(json.dumps({"runtime":"installed","unicode":sys.argv[1]},ensure_ascii=False))',
    unicodeProbe,
  ], {
    cwd: smokeDirectory,
    env: pythonEnvironment,
    stdoutIncludes: unicodeProbe,
  });
  if (python.runtime !== 'installed' || python.unicode !== unicodeProbe) {
    throw new Error(`安装后的嵌入式 Python 中文 JSON 冒烟无效：${JSON.stringify(python)}`);
  }

  const installedPdfHelper = join(
    installDirectory,
    'skills',
    'document-content-analysis',
    'scripts',
    'yunspire_pdf_windows.exe',
  );
  const pdf = await smokePdfHelper(installedPdfHelper, smokeDirectory);
  const documentScript = join(
    installDirectory,
    'skills',
    'document-content-analysis',
    'scripts',
    'extract_document.py',
  );
  await fileSize(documentScript);
  const documentAttachmentDirectory = join(smokeDirectory, '文档脚本附件输出');
  await mkdir(documentAttachmentDirectory, { recursive: true });
  const document = runInstalledPythonScript(installedPython, documentScript, [
    pdf.source,
    '--attachment-output-dir',
    documentAttachmentDirectory,
  ], {
    env: pythonEnvironment,
    stdoutIncludes: '云枢 示例文档.pdf',
    timeout: 180_000,
  });
  if (!Array.isArray(document.files) || document.files.length !== 1
    || !document.files[0].path?.includes('云枢 示例文档.pdf')
    || !Array.isArray(document.attachments) || document.attachments.length < 1
    || !Array.isArray(document.errors) || document.errors.length) {
    throw new Error(`安装后的 PDF 完整脚本冒烟无效：${JSON.stringify(document)}`);
  }

  const mediaSource = join(smokeDirectory, '本地语音与视频帧.avi');
  const mediaOutput = join(smokeDirectory, '视频脚本输出');
  await writeFile(mediaSource, createMediaSmokeAvi());
  await mkdir(mediaOutput, { recursive: true });
  const videoScript = join(
    installDirectory,
    'skills',
    'video-content-analysis',
    'scripts',
    'extract_video.py',
  );
  await fileSize(videoScript);
  const requestedLocale = 'en-US';
  const video = runInstalledPythonScript(installedPython, videoScript, [
    mediaSource,
    '--output-dir',
    mediaOutput,
    '--locale',
    requestedLocale,
  ], {
    env: pythonEnvironment,
    stdoutIncludes: '本地语音与视频帧',
    timeout: 900_000,
  });
  if (!Array.isArray(video.frames) || video.frames.length < 1
    || video.metadata?.frame_selection_method !== 'yunspire-windows-mediafoundation-v1') {
    throw new Error(`安装后视频帧完整脚本冒烟无效：${JSON.stringify(video)}`);
  }
  for (const framePath of video.frames) {
    await assertJpeg(assertPathInside(framePath, mediaOutput, '安装后视频帧'), '安装后 WIC 关键帧');
  }
  const speechVerification = assertSpeechVerification(video, requestedLocale, '安装后视频脚本');
  return {
    pythonUnicodeJson: true,
    pdf: { pageCount: pdf.pageCount, attachmentCount: document.attachments.length },
    video: { frameCount: video.frames.length, speechVerification },
  };
}

async function verifyInstalledBundle(installer) {
  const installDirectory = join(root, 'src-tauri', 'target', '云枢-Windows-安装冒烟');
  await rm(installDirectory, { recursive: true, force: true });
  try {
    const installation = spawnSync(installer, ['/S', `/D=${installDirectory}`], {
      cwd: root,
      encoding: 'utf8',
      windowsHide: true,
      timeout: 180_000,
    });
    if (installation.error || installation.status !== 0) {
      throw new Error(`NSIS 静默安装失败\n${installation.error || ''}\n${installation.stdout || ''}\n${installation.stderr || ''}`.trim());
    }
    const installedApplication = join(installDirectory, 'Yunspire.exe');
    await assertPortableExecutable(installedApplication);
    assertAuthenticodeNotSigned(installedApplication, '安装后的 Yunspire.exe');
    for (const helper of helperResources) {
      const installed = join(installDirectory, helper.destination);
      await assertPortableExecutable(installed);
      if (await sha256(installed) !== await sha256(helper.path)) {
        throw new Error(`安装后的原生资源哈希不一致：${helper.destination}`);
      }
    }
    for (const legal of legalResources) {
      const installed = join(installDirectory, legal.destination);
      await fileSize(installed);
      if (await sha256(installed) !== await sha256(legal.path)) {
        throw new Error(`安装后的法律资源哈希不一致：${legal.destination}`);
      }
    }
    const installedPython = join(installDirectory, 'runtime', 'python', 'python.exe');
    await assertPortableExecutable(installedPython);
    if (await sha256(installedPython) !== await sha256(join(runtimeDirectory, 'python.exe'))) {
      throw new Error('安装后的 Python 运行时哈希不一致');
    }
    const installedRuntimeManifest = JSON.parse(await readFile(
      join(installDirectory, 'runtime', 'python', 'YUNSPIRE_RUNTIME.json'),
      'utf8',
    ));
    if (installedRuntimeManifest.schema !== 'yunspire.windows-python-runtime.v1') {
      throw new Error('安装后的 Python 运行时清单无效');
    }
    if (installedRuntimeManifest.licenseSha256 !== EXPECTED_PYTHON_LICENSE_SHA256) {
      throw new Error('安装后的 Python 运行时清单许可哈希无效');
    }
    const installedRuntimeLicense = join(installDirectory, 'runtime', 'python', 'LICENSE.txt');
    await fileSize(installedRuntimeLicense);
    if (await sha256(installedRuntimeLicense) !== EXPECTED_PYTHON_LICENSE_SHA256) {
      throw new Error('安装后的 Python 许可文件哈希不一致');
    }
    return {
      unicodeSilentInstall: true,
      applicationPortableExecutable: true,
      applicationAuthenticodeStatus: 'NotSigned',
      installedResourceIntegrity: true,
      installedRuntimeIntegrity: true,
      ...await smokeInstalledResources(installDirectory, installedPython),
    };
  } finally {
    const entries = await readdir(installDirectory, { withFileTypes: true }).catch(() => []);
    const uninstaller = entries.find((entry) => entry.isFile() && /^uninstall.*\.exe$/iu.test(entry.name));
    if (uninstaller) {
      spawnSync(join(installDirectory, uninstaller.name), ['/S'], {
        cwd: root,
        encoding: 'utf8',
        windowsHide: true,
        timeout: 120_000,
      });
    }
    await rm(installDirectory, { recursive: true, force: true, maxRetries: 5, retryDelay: 500 });
  }
}

async function packageBundle() {
  await verifyHelpers();
  const installer = await findInstaller();
  await assertPortableExecutable(installer);
  assertAuthenticodeNotSigned(installer, 'NSIS 安装器');
  const installerSize = await fileSize(installer);
  if (installerSize < 1024 * 1024) throw new Error(`NSIS 安装器大小异常：${installerSize}`);
  const installedVerification = await verifyInstalledBundle(installer);

  const packageJson = JSON.parse(await readFile(packagePath, 'utf8'));
  const artifactName = `Yunspire_${packageJson.version}_Windows-x64_unsigned-setup.exe`;
  const artifactInstaller = join(artifactDirectory, artifactName);
  await rm(artifactDirectory, { recursive: true, force: true });
  await mkdir(artifactDirectory, { recursive: true });
  await copyFile(installer, artifactInstaller);

  const resourceFiles = [
    ...helperResources.map((helper) => ({ name: `resources/${basename(helper.path)}`, path: helper.path })),
    { name: 'resources/python.exe', path: join(runtimeDirectory, 'python.exe') },
    { name: 'resources/python-LICENSE.txt', path: join(runtimeDirectory, 'LICENSE.txt') },
    ...legalResources.map((legal) => ({ name: `resources/${legal.destination}`, path: legal.path })),
  ];
  const installerSha256 = await sha256(artifactInstaller);
  const resourceManifest = [];
  for (const file of resourceFiles) {
    resourceManifest.push({
      name: file.name,
      byteLength: await fileSize(file.path),
      sha256: await sha256(file.path),
    });
  }
  const sourceSha = gitRevision('HEAD');
  const githubSha = process.env.GITHUB_SHA?.trim().toLowerCase() || null;
  if (githubSha && githubSha !== sourceSha) {
    throw new Error(`GitHub Actions 源码 SHA 与检出版本不一致：${githubSha} != ${sourceSha}`);
  }
  const sourceTree = gitRevision('HEAD^{tree}');
  const manifestPath = join(artifactDirectory, 'windows-build-manifest.json');
  await writeFile(manifestPath, JSON.stringify({
    schema: 'yunspire.windows-installer.v1',
    version: packageJson.version,
    architecture: 'x64',
    runner: process.env.RUNNER_NAME || 'local-windows',
    sourceSha,
    sourceTree,
    workflowRunId: githubRunValue('GITHUB_RUN_ID'),
    workflowRunAttempt: githubRunValue('GITHUB_RUN_ATTEMPT'),
    unsigned: true,
    installer: { name: artifactName, byteLength: installerSize, sha256: installerSha256 },
    resources: resourceManifest,
    buildVerification: {
      releaseAudit: workflowGate('YUNSPIRE_RELEASE_AUDIT_OUTCOME'),
      lockedDependencies: workflowGate('YUNSPIRE_LOCKED_DEPENDENCIES_OUTCOME'),
      pythonRuntime: true,
      nativeHelpers: true,
      mediaAndSpeechHelpers: true,
      productVerification: workflowGate('YUNSPIRE_PRODUCT_VERIFICATION_OUTCOME'),
      helperVerification: true,
      nsisInstaller: true,
      installerAuthenticodeStatus: 'NotSigned',
    },
    installedVerification,
  }, null, 2) + '\n', 'utf8');
  const manifestSha256 = await sha256(manifestPath);
  await writeFile(
    join(artifactDirectory, 'SHA256SUMS.txt'),
    `${installerSha256}  ${artifactName}\n${manifestSha256}  windows-build-manifest.json\n`,
    'utf8',
  );
  console.log(`WINDOWS_NSIS_ARTIFACT_OK path=${artifactInstaller} sha256=${installerSha256}`);
}

if (phase === 'helpers') await verifyHelpers();
else await packageBundle();
