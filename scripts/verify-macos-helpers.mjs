import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  cp,
  mkdtemp,
  mkdir,
  readFile,
  readdir,
  readlink,
  realpath,
  rm,
  stat,
  writeFile,
} from 'node:fs/promises';
import { basename, dirname, join, relative, resolve, sep } from 'node:path';
import { tmpdir } from 'node:os';
import process from 'node:process';
import { verifyPackagedPrivacy } from './verify-packaged-privacy.mjs';

if (process.platform !== 'darwin') {
  throw new Error('macOS helper 核验只能在 macOS 构建机上执行');
}

const PYTHON_VERSION = '3.13.7';
const PYTHON_SOURCE_URL = 'https://www.python.org/ftp/python/3.13.7/python-3.13.7-macos11.pkg';
const PYTHON_ARCHIVE_BYTES = 71_105_747;
const PYTHON_ARCHIVE_SHA256 = 'f7e8c8d63ab0a4e736b5864aa369098b16af622042c079addb2f1a08400560c5';
const PYTHON_ARCHIVE_MD5 = 'ac0421b04eef155f4daab0b023cf3956';
const PYTHON_LICENSE_SHA256 = '78b12c3a81360b357002334f0e70ea0e92eebf7a9b358805c03c48484945f3bb';
const PYTHON_INSTALLER_LICENSE_SHA256 = '09827568690fa00485c96fa6d100241839be94e3167a250195fe20c49c677336';
const PYTHON_RELOCATION_PREFIX = '/Library/Frameworks/Python.framework/Versions/3.13/';
const PYTHON_EXECUTABLE = 'Resources/Python.app/Contents/MacOS/Python';
const PYTHON_FRAMEWORK_BINARY = 'Python';
const PYTHON_SYSTEM_CERTIFICATE = '/etc/ssl/cert.pem';
const EXPECTED_ARCHITECTURES = ['arm64', 'x86_64'];
const PYTHON_BUILD_PATH_POLICY = 'no-absolute-user-build-paths-v1';
const PYTHON_PRUNED_TEST_MODULES = ['_ctypes_test', 'xxsubtype'];

const root = resolve(import.meta.dirname, '..');
const outputDirectory = join(root, 'src-tauri', 'target', 'yunspire-native', 'macos');
const runtimeDirectory = join(root, 'src-tauri', 'target', 'yunspire-runtime', 'macos-python');
const mediaPath = join(outputDirectory, 'yunspire-media');
const pdfPath = join(outputDirectory, 'yunspire-pdf');
const speechBundle = join(outputDirectory, 'Yunspire Speech Helper.app');
const speechPath = join(speechBundle, 'Contents', 'MacOS', 'yunspire-speech');
const speechPlistPath = join(speechBundle, 'Contents', 'Info.plist');
const manifestPath = join(outputDirectory, 'helpers-manifest.json');
const macosConfigPath = join(root, 'src-tauri', 'tauri.macos.conf.json');
const tauriConfigPath = join(root, 'src-tauri', 'tauri.conf.json');
const capturePipelinePath = join(root, 'src-tauri', 'src', 'capture_pipeline.rs');
const pythonBuilderPath = join(root, 'scripts', 'build-macos-python-runtime.mjs');
const videoScriptPath = join(root, 'skills', 'video-content-analysis', 'scripts', 'extract_video.py');
const speechScriptPath = join(root, 'skills', 'video-content-analysis', 'scripts', 'yunspire_transcribe.py');
const mediaSourcePath = join(root, 'skills', 'video-content-analysis', 'scripts', 'yunspire_media.m');
const speechSourcePath = join(root, 'skills', 'video-content-analysis', 'scripts', 'yunspire_speech.m');
const documentScriptPath = join(root, 'skills', 'document-content-analysis', 'scripts', 'extract_document.py');
const pdfSourcePath = join(root, 'skills', 'document-content-analysis', 'scripts', 'yunspire_pdf.m');

let installedApp = null;
for (let index = 2; index < process.argv.length; index += 1) {
  const argument = process.argv[index];
  if (argument === '--app') {
    const value = process.argv[index + 1];
    if (!value) throw new Error('--app 缺少 Yunspire.app 路径');
    installedApp = resolve(value);
    index += 1;
  } else {
    throw new Error(`未知参数：${argument}`);
  }
}

function run(program, args, label, options = {}) {
  const result = spawnSync(program, args, {
    cwd: options.cwd || root,
    encoding: 'utf8',
    env: options.env || process.env,
    maxBuffer: options.maxBuffer || 32 * 1024 * 1024,
    timeout: options.timeout || 60_000,
  });
  if (result.error || result.status !== (options.status ?? 0)) {
    throw new Error(`${label}失败\n${result.error || ''}\n${result.stdout || ''}\n${result.stderr || ''}`.trim());
  }
  return {
    stdout: result.stdout.trim(),
    stderr: result.stderr.trim(),
  };
}

async function sha256(path) {
  return createHash('sha256').update(await readFile(path)).digest('hex');
}

async function fileSize(path, label) {
  const value = await stat(path).catch(() => null);
  if (!value?.isFile() || value.size <= 0) throw new Error(`${label}不存在或为空：${path}`);
  return value.size;
}

function parseJsonOutput(path, label) {
  const output = run(path, [], `${label}启动冒烟`).stdout;
  try {
    return JSON.parse(output);
  } catch {
    throw new Error(`${label}没有返回有效 JSON：${output}`);
  }
}

function assertContains(source, text, label) {
  if (!source.includes(text)) throw new Error(`${label}缺少：${text}`);
}

async function collectEntries(directory) {
  const entries = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) entries.push({ path, type: 'directory' }, ...await collectEntries(path));
    else if (entry.isSymbolicLink()) entries.push({ path, type: 'symlink' });
    else if (entry.isFile()) entries.push({ path, type: 'file' });
  }
  return entries;
}

async function runtimeMetrics(directory, pythonManifestPath) {
  const entries = (await collectEntries(directory)).sort((left, right) => left.path.localeCompare(right.path));
  let payloadByteLength = 0;
  let payloadFileCount = 0;
  let symlinkCount = 0;
  const payloadHash = createHash('sha256');
  for (const entry of entries) {
    if (entry.path === pythonManifestPath) continue;
    const relativePath = relative(directory, entry.path).split(sep).join('/');
    if (entry.type === 'file') {
      const bytes = await readFile(entry.path);
      payloadByteLength += bytes.length;
      payloadFileCount += 1;
      payloadHash.update(`file\0${relativePath}\0${bytes.length}\0`);
      payloadHash.update(bytes);
    } else if (entry.type === 'symlink') {
      const target = await readlink(entry.path);
      if (target.startsWith(PYTHON_RELOCATION_PREFIX)) {
        throw new Error(`Python runtime 符号链仍引用安装机绝对路径：${entry.path} -> ${target}`);
      }
      symlinkCount += 1;
      payloadHash.update(`symlink\0${relativePath}\0${target}\0`);
    }
  }
  return {
    payloadByteLength,
    payloadFileCount,
    payloadSha256: payloadHash.digest('hex'),
    symlinkCount,
  };
}

async function candidateMachOFiles(directory) {
  const executable = join(directory, PYTHON_EXECUTABLE);
  const framework = join(directory, PYTHON_FRAMEWORK_BINARY);
  const candidates = (await collectEntries(directory))
    .filter((entry) => entry.type === 'file')
    .map((entry) => entry.path)
    .filter((path) => path === executable || path === framework || path.endsWith('.dylib') || path.endsWith('.so'));
  const files = [];
  for (const path of candidates) {
    const description = run('/usr/bin/file', ['-b', path], `Mach-O 类型核验 ${path}`).stdout;
    if (description.includes('Mach-O') && !description.includes('archive')) files.push(path);
  }
  return files;
}

function assertUniversal2(path, label) {
  const actual = new Set(run('/usr/bin/lipo', ['-archs', path], `${label}架构核验`).stdout.split(/\s+/u));
  for (const expected of EXPECTED_ARCHITECTURES) {
    if (!actual.has(expected)) throw new Error(`${label}缺少 ${expected} 架构：${path}`);
  }
}

async function smokePythonRuntime(directory, label) {
  const temporaryRoot = await mkdtemp(join(tmpdir(), 'yunspire-python-verify-'));
  const relocatedDirectory = join(temporaryRoot, 'runtime', 'python');
  try {
    await mkdir(dirname(relocatedDirectory), { recursive: true });
    await cp(directory, relocatedDirectory, {
      recursive: true,
      dereference: false,
      preserveTimestamps: true,
      verbatimSymlinks: true,
    });
    const executable = join(relocatedDirectory, PYTHON_EXECUTABLE);
    const script = [
      'import bz2,hashlib,json,lzma,platform,sqlite3,ssl,sys,urllib.request,zlib',
      'context=ssl.create_default_context()',
      'print(json.dumps({"version":platform.python_version(),"implementation":platform.python_implementation(),"prefix":sys.prefix,"executable":sys.executable,"certificateFile":ssl.get_default_verify_paths().cafile,"certificateCount":len(context.get_ca_certs()),"sha256":hashlib.sha256(b"yunspire").hexdigest()}))',
    ].join('\n');
    const environment = {
      PATH: '/usr/bin:/bin',
      HOME: temporaryRoot,
      TMPDIR: temporaryRoot,
      PYTHONHOME: relocatedDirectory,
      PYTHONNOUSERSITE: '1',
      PYTHONSAFEPATH: '1',
      PYTHONDONTWRITEBYTECODE: '1',
      PYTHONUTF8: '1',
      PYTHONIOENCODING: 'utf-8',
      SSL_CERT_FILE: PYTHON_SYSTEM_CERTIFICATE,
    };
    const result = run(executable, ['-I', '-c', script], `${label}任意路径冒烟`, {
      cwd: temporaryRoot,
      env: environment,
      timeout: 60_000,
    });
    if (result.stderr.includes(PYTHON_RELOCATION_PREFIX)) {
      throw new Error(`${label}仍加载系统 Python framework\n${result.stderr}`);
    }
    const payload = JSON.parse(result.stdout);
    if (payload.version !== PYTHON_VERSION
      || payload.implementation !== 'CPython'
      || payload.certificateFile !== PYTHON_SYSTEM_CERTIFICATE
      || payload.certificateCount <= 0
      || payload.sha256 !== '9cff10f44fced5540177e50bcb6d67724c09fc52fffb336341b6fdefdfb2945a') {
      throw new Error(`${label}冒烟元数据无效：${JSON.stringify(payload)}`);
    }
    if (await realpath(payload.prefix) !== await realpath(relocatedDirectory)
      || await realpath(payload.executable) !== await realpath(executable)) {
      throw new Error(`${label}仍关联外部 Python runtime：${JSON.stringify(payload)}`);
    }
  } finally {
    await rm(temporaryRoot, { recursive: true, force: true });
  }
}

async function verifyPythonRuntime(directory, label) {
  const pythonManifestPath = join(directory, 'YUNSPIRE_RUNTIME.json');
  const manifest = JSON.parse(await readFile(pythonManifestPath, 'utf8'));
  const expectedBuilderSha256 = await sha256(pythonBuilderPath);
  const expected = {
    schema: 'yunspire.macos-python-runtime.v1',
    version: PYTHON_VERSION,
    sourceUrl: PYTHON_SOURCE_URL,
    archiveByteLength: PYTHON_ARCHIVE_BYTES,
    archiveSha256: PYTHON_ARCHIVE_SHA256,
    archiveMd5: PYTHON_ARCHIVE_MD5,
    licenseFile: 'lib/python3.13/LICENSE.txt',
    licenseSha256: PYTHON_LICENSE_SHA256,
    installerLicenseFile: 'Resources/Python.app/Contents/Resources/PYTHON_INSTALLER_LICENSE.rtf',
    installerLicenseSha256: PYTHON_INSTALLER_LICENSE_SHA256,
    executable: PYTHON_EXECUTABLE,
    frameworkBinary: PYTHON_FRAMEWORK_BINARY,
    relocationPrefix: PYTHON_RELOCATION_PREFIX,
    certificateFile: PYTHON_SYSTEM_CERTIFICATE,
    buildPathPolicy: PYTHON_BUILD_PATH_POLICY,
    builderSha256: expectedBuilderSha256,
  };
  for (const [key, value] of Object.entries(expected)) {
    if (manifest[key] !== value) throw new Error(`${label} manifest ${key} 无效：${manifest[key] || 'missing'}`);
  }
  if (JSON.stringify(manifest.architectures) !== JSON.stringify(EXPECTED_ARCHITECTURES)) {
    throw new Error(`${label} manifest 架构无效：${JSON.stringify(manifest.architectures)}`);
  }
  if (JSON.stringify(manifest.prunedTestModules) !== JSON.stringify(PYTHON_PRUNED_TEST_MODULES)) {
    throw new Error(`${label} manifest 测试模块裁剪清单无效：${JSON.stringify(manifest.prunedTestModules)}`);
  }
  if (manifest.sourceSignature?.signer !== 'Developer ID Installer: Python Software Foundation (BMM5U3QVKW)'
    || manifest.sourceSignature?.notarization !== 'trusted') {
    throw new Error(`${label} manifest 缺少 Python.org 供应商签名/notarization 证据`);
  }
  const license = join(directory, manifest.licenseFile);
  const installerLicense = join(directory, manifest.installerLicenseFile);
  if (await sha256(license) !== PYTHON_LICENSE_SHA256
    || await sha256(installerLicense) !== PYTHON_INSTALLER_LICENSE_SHA256) {
    throw new Error(`${label}许可证与固定官方来源不一致`);
  }
  const executable = join(directory, PYTHON_EXECUTABLE);
  const framework = join(directory, PYTHON_FRAMEWORK_BINARY);
  assertUniversal2(executable, `${label} launcher`);
  assertUniversal2(framework, `${label} framework`);
  const files = await candidateMachOFiles(directory);
  if (files.length < 70 || files.length !== manifest.machOFileCount) {
    throw new Error(`${label} Mach-O 数量异常：${files.length}/${manifest.machOFileCount}`);
  }
  for (const path of files) {
    const dependencies = run('/usr/bin/otool', ['-L', path], `${label} Mach-O 依赖 ${path}`).stdout;
    const ids = run('/usr/bin/otool', ['-D', path], `${label} Mach-O ID ${path}`).stdout;
    if (dependencies.includes(PYTHON_RELOCATION_PREFIX) || ids.includes(PYTHON_RELOCATION_PREFIX)) {
      throw new Error(`${label} Mach-O 仍引用系统 Python framework：${path}`);
    }
    run('/usr/bin/codesign', ['--verify', '--strict', path], `${label} Mach-O ad-hoc 签名 ${path}`);
  }
  for (const path of [executable, framework]) {
    const signature = run('/usr/bin/codesign', ['--display', '--verbose=4', path], `${label} ad-hoc 签名属性 ${path}`).stderr;
    if (!signature.includes('Signature=adhoc')) throw new Error(`${label}内部 Mach-O 不是 ad-hoc 签名：${path}`);
  }
  const metrics = await runtimeMetrics(directory, pythonManifestPath);
  for (const key of ['payloadByteLength', 'payloadFileCount', 'payloadSha256', 'symlinkCount']) {
    if (metrics[key] !== manifest[key]) {
      throw new Error(`${label} ${key} 与 manifest 不一致：${metrics[key]}/${manifest[key]}`);
    }
  }
  if (metrics.symlinkCount !== 0) {
    throw new Error(`${label}仍含有会被 Tauri 解引用的符号链：${metrics.symlinkCount}`);
  }
  const dynamicDirectory = join(directory, 'lib', 'python3.13', 'lib-dynload');
  const dynamicEntries = await readdir(dynamicDirectory);
  for (const moduleName of PYTHON_PRUNED_TEST_MODULES) {
    if (dynamicEntries.some((entry) => entry.startsWith(`${moduleName}.`) || entry === moduleName)) {
      throw new Error(`${label}仍含有测试扩展：${moduleName}`);
    }
  }
  if (await stat(join(directory, 'lib', 'python3.13', 'venv')).catch(() => null)) {
    throw new Error(`${label}仍含有不可用的 venv 模块`);
  }
  await verifyPackagedPrivacy(directory, { platform: 'macos-python-runtime' });
  if (manifest.pythonExecutableSmoke?.version !== PYTHON_VERSION
    || manifest.pythonExecutableSmoke?.arbitraryPathVerified !== true
    || manifest.pythonExecutableSmoke?.certificateFile !== PYTHON_SYSTEM_CERTIFICATE) {
    throw new Error(`${label} Python 冒烟 manifest 无效`);
  }
  await smokePythonRuntime(directory, label);
  return manifest;
}

async function smokeInstalledDocumentCapture(application, pythonDirectory) {
  const temporaryRoot = await mkdtemp(join(tmpdir(), 'yunspire-installed-capture-'));
  try {
    const source = join(temporaryRoot, '云枢安装后采集冒烟.txt');
    const marker = 'Yunspire installed capture smoke v0.4.1';
    await writeFile(source, `${marker}\n`, 'utf8');
    const executable = join(pythonDirectory, PYTHON_EXECUTABLE);
    const script = join(
      application,
      'Contents',
      'Resources',
      'skills',
      'document-content-analysis',
      'scripts',
      'extract_document.py',
    );
    const scriptDirectory = dirname(script);
    const environment = {
      PATH: '/usr/bin:/bin',
      HOME: temporaryRoot,
      TMPDIR: temporaryRoot,
      PYTHONHOME: pythonDirectory,
      PYTHONPATH: scriptDirectory,
      PYTHONNOUSERSITE: '1',
      PYTHONSAFEPATH: '1',
      PYTHONDONTWRITEBYTECODE: '1',
      PYTHONUTF8: '1',
      PYTHONIOENCODING: 'utf-8',
      SSL_CERT_FILE: PYTHON_SYSTEM_CERTIFICATE,
      YUNSPIRE_MACOS_PDF_ADAPTER: join(scriptDirectory, 'bin', 'yunspire-pdf'),
    };
    const output = run(executable, [script, source], '安装后 macOS 文档采集脚本冒烟', {
      cwd: temporaryRoot,
      env: environment,
      timeout: 60_000,
    }).stdout;
    let payload;
    try {
      payload = JSON.parse(output);
    } catch {
      throw new Error(`安装后 macOS 文档采集脚本没有返回有效 JSON：${output}`);
    }
    if (!payload.content_markdown?.includes(marker)
      || !Array.isArray(payload.files)
      || payload.files.length !== 1
      || !Array.isArray(payload.errors)
      || payload.errors.length !== 0) {
      throw new Error(`安装后 macOS 文档采集脚本输出无效：${JSON.stringify(payload)}`);
    }
  } finally {
    await rm(temporaryRoot, { recursive: true, force: true });
  }
}

const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
if (manifest.schema !== 'yunspire.macos-native-helpers.v1') {
  throw new Error(`macOS helper manifest schema 无效：${manifest.schema || 'missing'}`);
}
const tauriConfig = JSON.parse(await readFile(tauriConfigPath, 'utf8'));
if (manifest.version !== tauriConfig.version) {
  throw new Error(`macOS helper 版本 ${manifest.version || 'missing'} 与 Tauri ${tauriConfig.version || 'missing'} 不一致`);
}
if (manifest.minimumSystemVersion !== tauriConfig.bundle?.macOS?.minimumSystemVersion) {
  throw new Error('macOS helper 最低系统版本与 Tauri 配置不一致');
}
const mediaSize = await fileSize(mediaPath, 'macOS 媒体 helper');
const pdfSize = await fileSize(pdfPath, 'macOS PDF helper');
const speechSize = await fileSize(speechPath, 'macOS 语音 helper');
await fileSize(speechPlistPath, 'macOS 语音 helper Info.plist');
if (manifest.helpers?.media?.byteLength !== mediaSize
  || manifest.helpers?.media?.sha256 !== await sha256(mediaPath)) throw new Error('macOS 媒体 helper 与构建 manifest 不一致');
if (manifest.helpers?.pdf?.byteLength !== pdfSize
  || manifest.helpers?.pdf?.sha256 !== await sha256(pdfPath)) throw new Error('macOS PDF helper 与构建 manifest 不一致');
if (manifest.helpers?.speech?.byteLength !== speechSize
  || manifest.helpers?.speech?.sha256 !== await sha256(speechPath)
  || manifest.helpers?.speech?.infoPlistSha256 !== await sha256(speechPlistPath)) {
  throw new Error('macOS 语音 helper 与构建 manifest 不一致');
}
if (manifest.helpers?.media?.sourceSha256 !== await sha256(mediaSourcePath)
  || manifest.helpers?.pdf?.sourceSha256 !== await sha256(pdfSourcePath)
  || manifest.helpers?.speech?.sourceSha256 !== await sha256(speechSourcePath)) {
  throw new Error('macOS helper 构建产物来自旧版 Objective-C 源码');
}

const generatedPlist = await readFile(speechPlistPath, 'utf8');
if (!generatedPlist.includes(`<key>CFBundleShortVersionString</key>\n    <string>${manifest.version}</string>`)) {
  throw new Error('macOS 语音 helper Info.plist 版本与构建 manifest 不一致');
}
for (const [path, label] of [[mediaPath, '媒体 helper'], [pdfPath, 'PDF helper'], [speechPath, '语音 helper']]) {
  assertUniversal2(path, label);
}

const mediaSmoke = parseJsonOutput(mediaPath, 'macOS 媒体 helper');
if (!Array.isArray(mediaSmoke.errors) || !mediaSmoke.errors.includes('media_arguments_missing')) {
  throw new Error('macOS 媒体 helper 启动冒烟没有返回预期参数错误');
}
const speechSmoke = parseJsonOutput(speechPath, 'macOS 语音 helper');
if (!Array.isArray(speechSmoke.errors) || !speechSmoke.errors.includes('audio_path_missing')) {
  throw new Error('macOS 语音 helper 启动冒烟没有返回预期参数错误');
}
const pdfSmoke = parseJsonOutput(pdfPath, 'macOS PDF helper');
if (!Array.isArray(pdfSmoke.errors) || !pdfSmoke.errors.includes('pdf_path_missing')) {
  throw new Error('macOS PDF helper 启动冒烟没有返回预期参数错误');
}

const macosConfig = JSON.parse(await readFile(macosConfigPath, 'utf8'));
const expectedBuildCommand = 'node scripts/build-macos-python-runtime.mjs && node scripts/build-macos-native.mjs && npm run build';
if (macosConfig.build?.beforeBuildCommand !== expectedBuildCommand) {
  throw new Error('tauri.macos.conf.json 没有在每次 macOS 打包前强制重建 Python runtime 和原生 helper');
}
const resources = macosConfig.bundle?.resources || {};
const expectedResources = new Map([
  ['target/yunspire-runtime/macos-python/', 'runtime/python/'],
  ['target/yunspire-native/macos/yunspire-media', 'skills/video-content-analysis/scripts/bin/yunspire-media'],
  ['target/yunspire-native/macos/yunspire-pdf', 'skills/document-content-analysis/scripts/bin/yunspire-pdf'],
  ['target/yunspire-native/macos/Yunspire Speech Helper.app/', 'skills/video-content-analysis/scripts/bin/Yunspire Speech Helper.app/'],
  ['target/yunspire-native/macos/helpers-manifest.json', 'skills/video-content-analysis/scripts/bin/yunspire-macos-helpers.json'],
]);
for (const [source, destination] of expectedResources) {
  if (resources[source] !== destination) {
    throw new Error(`tauri.macos.conf.json 资源映射无效：${source} -> ${resources[source] || 'missing'}`);
  }
}

const [capturePipeline, videoScript, speechScript, documentScript] = await Promise.all([
  readFile(capturePipelinePath, 'utf8'),
  readFile(videoScriptPath, 'utf8'),
  readFile(speechScriptPath, 'utf8'),
  readFile(documentScriptPath, 'utf8'),
]);
for (const [needle, label] of [
  ['YUNSPIRE_MACOS_MEDIA_ADAPTER', 'Rust 媒体 helper 注入'],
  ['YUNSPIRE_MACOS_SPEECH_HELPER_APP', 'Rust 语音 helper 注入'],
  ['YUNSPIRE_MACOS_PDF_ADAPTER', 'Rust PDF helper 注入'],
  ['YUNSPIRE_MACOS_ALLOW_RUNTIME_COMPILE', 'Rust debug fallback 门禁'],
  ['cfg!(debug_assertions)', 'Rust release/debug 分流'],
  ['join("macos-python")', 'Rust 本地内置 Python runtime'],
  ['云枢安装包内置 Python runtime', 'Rust release Python 硬失败'],
  ['#[cfg(not(debug_assertions))]', 'Rust release 不编译 debug fallback'],
  ['.env_remove("PYTHONHOME")', 'Rust 清除外部 PYTHONHOME'],
  ['.env_remove("PYTHONPATH")', 'Rust 清除外部 PYTHONPATH'],
  ['.env("PYTHONPATH", scripts_directory)', 'Rust 只注入包内 Skill 脚本目录'],
  ['PYTHONNOUSERSITE', 'Rust 禁用用户 site-packages'],
  ['PYTHONSAFEPATH', 'Rust Python 安全路径'],
  ['SSL_CERT_FILE', 'Rust macOS CA 路径'],
]) assertContains(capturePipeline, needle, label);
const manifestDirectoryReferences = capturePipeline.match(/env!\("CARGO_MANIFEST_DIR"\)/gu)?.length || 0;
const debugGatedManifestDirectoryReferences = capturePipeline
  .match(/#\[cfg\(debug_assertions\)\][\s\S]{0,400}?env!\("CARGO_MANIFEST_DIR"\)/gu)?.length || 0;
if (manifestDirectoryReferences === 0
  || manifestDirectoryReferences !== debugGatedManifestDirectoryReferences) {
  throw new Error(`Rust CARGO_MANIFEST_DIR 引用没有全部隔离在 debug 构建：${debugGatedManifestDirectoryReferences}/${manifestDirectoryReferences}`);
}
if (capturePipeline.includes('Library/Python/3.9/lib/python/site-packages')) {
  throw new Error('Rust 仍向 macOS 用户 Python 3.9 site-packages 注入路径');
}
for (const [source, label] of [[videoScript, '视频 Python 运行时'], [speechScript, '语音 Python 运行时']]) {
  assertContains(source, 'YUNSPIRE_MACOS_ALLOW_RUNTIME_COMPILE', `${label} fallback 门禁`);
}
assertContains(videoScript, 'YUNSPIRE_MACOS_MEDIA_ADAPTER', '视频 Python 预编译 helper');
assertContains(speechScript, 'YUNSPIRE_MACOS_SPEECH_HELPER_APP', '语音 Python 预编译 helper');
assertContains(documentScript, 'YUNSPIRE_MACOS_PDF_ADAPTER', '文档 Python 预编译 helper');
assertContains(documentScript, 'YUNSPIRE_MACOS_ALLOW_RUNTIME_COMPILE', '文档 Python fallback 门禁');

const builtPythonManifest = await verifyPythonRuntime(runtimeDirectory, '构建目录 macOS Python runtime');
if (installedApp) {
  const appInfo = await stat(installedApp).catch(() => null);
  if (!appInfo?.isDirectory() || basename(installedApp) !== 'Yunspire.app') {
    throw new Error(`--app 不是 Yunspire.app 目录：${installedApp}`);
  }
  const installedRuntime = join(installedApp, 'Contents', 'Resources', 'runtime', 'python');
  const installedPythonManifest = await verifyPythonRuntime(installedRuntime, '安装后 macOS Python runtime');
  if (installedPythonManifest.payloadSha256 !== builtPythonManifest.payloadSha256
    || installedPythonManifest.builderSha256 !== builtPythonManifest.builderSha256) {
    throw new Error('安装后 macOS Python runtime 与当前选定的构建产物不一致');
  }
  await verifyPackagedPrivacy(installedApp, { platform: 'macos' });
  await smokeInstalledDocumentCapture(installedApp, installedRuntime);
}

console.log(`MACOS_HELPERS_VERIFIED version=${manifest.version} architectures=${manifest.architectures.join(',')} python=${builtPythonManifest.version}${installedApp ? ' installed=true' : ''}`);
