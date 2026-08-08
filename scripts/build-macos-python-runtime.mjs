import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  access,
  chmod,
  copyFile,
  cp,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  readlink,
  realpath,
  rename,
  rm,
  stat,
  writeFile,
} from 'node:fs/promises';
import { constants } from 'node:fs';
import { basename, dirname, join, relative, resolve, sep } from 'node:path';
import { tmpdir } from 'node:os';
import process from 'node:process';

const PYTHON_VERSION = '3.13.7';
const ARCHIVE_NAME = `python-${PYTHON_VERSION}-macos11.pkg`;
const DOWNLOAD_URL = `https://www.python.org/ftp/python/${PYTHON_VERSION}/${ARCHIVE_NAME}`;
const EXPECTED_ARCHIVE_BYTES = 71_105_747;
const EXPECTED_SHA256 = 'f7e8c8d63ab0a4e736b5864aa369098b16af622042c079addb2f1a08400560c5';
const EXPECTED_MD5 = 'ac0421b04eef155f4daab0b023cf3956';
const EXPECTED_LICENSE_SHA256 = '78b12c3a81360b357002334f0e70ea0e92eebf7a9b358805c03c48484945f3bb';
const EXPECTED_INSTALLER_LICENSE_SHA256 = '09827568690fa00485c96fa6d100241839be94e3167a250195fe20c49c677336';
const RELOCATION_PREFIX = '/Library/Frameworks/Python.framework/Versions/3.13/';
const EXPECTED_ARCHITECTURES = ['arm64', 'x86_64'];
const RUNTIME_SCHEMA = 'yunspire.macos-python-runtime.v1';
const EXECUTABLE_RELATIVE_PATH = 'Resources/Python.app/Contents/MacOS/Python';
const FRAMEWORK_BINARY_RELATIVE_PATH = 'Python';
const LICENSE_RELATIVE_PATH = 'lib/python3.13/LICENSE.txt';
const INSTALLER_LICENSE_RELATIVE_PATH = 'Resources/Python.app/Contents/Resources/PYTHON_INSTALLER_LICENSE.rtf';
const SYSTEM_CERTIFICATE_FILE = '/etc/ssl/cert.pem';

if (process.platform !== 'darwin') {
  console.log(`MACOS_PYTHON_RUNTIME_SKIPPED platform=${process.platform}`);
  process.exit(0);
}

const root = resolve(import.meta.dirname, '..');
const outputRoot = join(root, 'src-tauri', 'target', 'yunspire-runtime');
const runtimeDirectory = join(outputRoot, 'macos-python');
const archivePath = join(outputRoot, ARCHIVE_NAME);
const manifestPath = join(runtimeDirectory, 'YUNSPIRE_RUNTIME.json');
const pythonExecutable = join(runtimeDirectory, EXECUTABLE_RELATIVE_PATH);
const frameworkBinary = join(runtimeDirectory, FRAMEWORK_BINARY_RELATIVE_PATH);
const licensePath = join(runtimeDirectory, LICENSE_RELATIVE_PATH);
const installerLicensePath = join(runtimeDirectory, INSTALLER_LICENSE_RELATIVE_PATH);

function run(program, args, label, options = {}) {
  const result = spawnSync(program, args, {
    cwd: options.cwd || root,
    encoding: 'utf8',
    env: options.env || process.env,
    maxBuffer: options.maxBuffer || 32 * 1024 * 1024,
    timeout: options.timeout || 5 * 60_000,
  });
  if (result.error || result.status !== (options.status ?? 0)) {
    throw new Error(`${label}失败\n${result.error || ''}\n${result.stdout || ''}\n${result.stderr || ''}`.trim());
  }
  return {
    stdout: result.stdout.trim(),
    stderr: result.stderr.trim(),
  };
}

function digest(algorithm, bytes) {
  return createHash(algorithm).update(bytes).digest('hex');
}

async function sha256(path) {
  return digest('sha256', await readFile(path));
}

async function isFile(path) {
  try {
    await access(path, constants.R_OK);
    return (await stat(path)).isFile();
  } catch {
    return false;
  }
}

async function collectEntries(directory) {
  const entries = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      entries.push({ path, type: 'directory' }, ...await collectEntries(path));
    } else if (entry.isSymbolicLink()) {
      entries.push({ path, type: 'symlink' });
    } else if (entry.isFile()) {
      entries.push({ path, type: 'file' });
    }
  }
  return entries;
}

async function removeNamedEntries(directory, names) {
  for (const entry of await readdir(directory, { withFileTypes: true }).catch(() => [])) {
    const path = join(directory, entry.name);
    if (names.has(entry.name)) {
      await rm(path, { recursive: true, force: true });
    } else if (entry.isDirectory()) {
      await removeNamedEntries(path, names);
    }
  }
}

async function archiveIsValid() {
  if (!await isFile(archivePath)) return false;
  const value = await readFile(archivePath);
  if (!(value.length === EXPECTED_ARCHIVE_BYTES
    && digest('sha256', value) === EXPECTED_SHA256
    && digest('md5', value) === EXPECTED_MD5)) return false;
  try {
    verifyArchiveSignature();
    return true;
  } catch {
    return false;
  }
}

function verifyArchiveSignature() {
  const output = run('/usr/sbin/pkgutil', ['--check-signature', archivePath], 'Python macOS pkg 供应商签名核验').stdout;
  if (!/Developer ID Installer: Python Software Foundation \(BMM5U3QVKW\)/u.test(output)
    || !/Notarization: trusted/u.test(output)) {
    throw new Error(`Python macOS pkg 供应商签名或 notarization 不符合预期：${output}`);
  }
}

async function downloadArchive() {
  if (await archiveIsValid()) return;
  await rm(archivePath, { force: true });
  const partialPath = `${archivePath}.partial`;
  await rm(partialPath, { force: true });
  const response = await fetch(DOWNLOAD_URL, {
    redirect: 'follow',
    signal: AbortSignal.timeout(20 * 60_000),
    headers: { 'User-Agent': 'Yunspire macOS build/0.3' },
  });
  if (!response.ok || response.url !== DOWNLOAD_URL) {
    throw new Error(`无法从 Python 官方地址下载 macOS 运行时：HTTP ${response.status} ${response.url}`);
  }
  const declaredLength = Number(response.headers.get('content-length') || 0);
  if (declaredLength !== EXPECTED_ARCHIVE_BYTES) {
    throw new Error(`Python macOS 运行时声明大小异常：${declaredLength}/${EXPECTED_ARCHIVE_BYTES}`);
  }
  const bytes = Buffer.from(await response.arrayBuffer());
  const archiveSha256 = digest('sha256', bytes);
  const archiveMd5 = digest('md5', bytes);
  if (bytes.length !== EXPECTED_ARCHIVE_BYTES
    || archiveSha256 !== EXPECTED_SHA256
    || archiveMd5 !== EXPECTED_MD5) {
    throw new Error(`Python macOS 运行时校验失败：bytes=${bytes.length} SHA-256=${archiveSha256} MD5=${archiveMd5}`);
  }
  await writeFile(partialPath, bytes);
  await rename(partialPath, archivePath);
  verifyArchiveSignature();
}

async function findFrameworkPayload(expandedDirectory) {
  const candidates = [];
  async function visit(directory) {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      if (!entry.isDirectory()) continue;
      const path = join(directory, entry.name);
      if (entry.name === '3.13'
        && path.includes(`${sep}Payload${sep}Versions${sep}`)
        && await isFile(join(path, FRAMEWORK_BINARY_RELATIVE_PATH))
        && await isFile(join(path, EXECUTABLE_RELATIVE_PATH))) {
        candidates.push(path);
      } else {
        await visit(path);
      }
    }
  }
  await visit(expandedDirectory);
  if (candidates.length !== 1) {
    throw new Error(`Python pkg 中应有且仅有一个可用框架 payload，实际 ${candidates.length}个：${candidates.join(', ')}`);
  }
  return candidates[0];
}

async function findInstallerLicense(expandedDirectory) {
  const candidates = [];
  async function visit(directory) {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) await visit(path);
      else if (entry.isFile() && entry.name === 'License.rtf'
        && await sha256(path) === EXPECTED_INSTALLER_LICENSE_SHA256) candidates.push(path);
    }
  }
  await visit(expandedDirectory);
  if (candidates.length === 0) {
    throw new Error('Python pkg 安装器缺少校验通过的 License.rtf');
  }
  // The installer may repeat the same license in an optional application package;
  // prefer the canonical top-level installer resource when present.
  return candidates.sort((left, right) => left.length - right.length)[0];
}

async function pruneRuntime() {
  const removable = [
    'bin',
    'include',
    'Headers',
    'share',
    'Frameworks',
    'etc',
    join('Resources', 'Info.plist'),
    join('lib', 'pkgconfig'),
    join('lib', 'python3.13', 'config-3.13-darwin'),
    join('lib', 'python3.13', 'ensurepip'),
    join('lib', 'python3.13', 'idlelib'),
    join('lib', 'python3.13', 'site-packages'),
    join('lib', 'python3.13', 'test'),
    join('lib', 'python3.13', 'tkinter'),
  ];
  for (const path of removable) {
    await rm(join(runtimeDirectory, path), { recursive: true, force: true });
  }
  await removeNamedEntries(runtimeDirectory, new Set(['_CodeSignature', '__pycache__', '.DS_Store']));
  const dynamicDirectory = join(runtimeDirectory, 'lib', 'python3.13', 'lib-dynload');
  for (const entry of await readdir(dynamicDirectory, { withFileTypes: true })) {
    if (!entry.isFile()) continue;
    if (/^(?:_tkinter|_test|xxlimited|_xxtestfuzz)/u.test(entry.name)) {
      await rm(join(dynamicDirectory, entry.name), { force: true });
    }
  }
}

async function materializeSymlinks(directory) {
  const symlinks = (await collectEntries(directory)).filter((entry) => entry.type === 'symlink');
  for (const entry of symlinks) {
    const target = await realpath(entry.path);
    const targetStat = await stat(target);
    if (!targetStat.isFile()) {
      throw new Error(`Python runtime 含有非文件符号链：${entry.path} -> ${target}`);
    }
    await rm(entry.path, { force: true });
    await copyFile(target, entry.path);
    await chmod(entry.path, targetStat.mode & 0o777);
  }
}

async function candidateMachOFiles(directory) {
  const entries = await collectEntries(directory);
  const candidates = entries
    .filter((entry) => entry.type === 'file')
    .map((entry) => entry.path)
    .filter((path) => path === frameworkBinary
      || path === pythonExecutable
      || path.endsWith('.dylib')
      || path.endsWith('.so'));
  const files = [];
  for (const path of candidates) {
    const description = run('/usr/bin/file', ['-b', path], `Mach-O 类型核验 ${path}`).stdout;
    if (description.includes('Mach-O') && !description.includes('archive')) files.push(path);
  }
  return files;
}

function dynamicDependencies(path) {
  const output = run('/usr/bin/otool', ['-L', path], `Mach-O 依赖读取 ${path}`).stdout;
  return [...new Set(output
    .split('\n')
    .map((line) => line.match(/^\s+(\S+)\s+\(/u)?.[1])
    .filter(Boolean))];
}

function dynamicLibraryIds(path) {
  const output = run('/usr/bin/otool', ['-D', path], `Mach-O ID 读取 ${path}`).stdout;
  return [...new Set(output
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line.startsWith(RELOCATION_PREFIX) && !line.includes(' (architecture')))];
}

function loaderRelativePath(source, target) {
  const value = relative(dirname(source), target).split(sep).join('/');
  return `@loader_path/${value || basename(target)}`;
}

async function relocateMachOFiles(files) {
  for (const path of files) {
    for (const oldDependency of dynamicDependencies(path)) {
      if (!oldDependency.startsWith(RELOCATION_PREFIX)) continue;
      const relativeTarget = oldDependency.slice(RELOCATION_PREFIX.length);
      const target = join(runtimeDirectory, relativeTarget);
      if (!await isFile(target)) {
        throw new Error(`Mach-O 依赖目标未随 runtime 部署：${path} -> ${oldDependency}`);
      }
      const replacement = path === pythonExecutable && relativeTarget === FRAMEWORK_BINARY_RELATIVE_PATH
        ? '@executable_path/../../../../Python'
        : loaderRelativePath(path, target);
      run('/usr/bin/install_name_tool', ['-change', oldDependency, replacement, path], `Mach-O 依赖重定位 ${path}`);
    }
    const ids = dynamicLibraryIds(path);
    if (ids.length > 1) throw new Error(`Mach-O 含有多个框架 ID：${path}`);
    if (ids.length === 1) {
      run('/usr/bin/install_name_tool', ['-id', `@loader_path/${basename(path)}`, path], `Mach-O ID 重定位 ${path}`);
    }
  }
}

function assertRelocated(files) {
  for (const path of files) {
    const dependencies = run('/usr/bin/otool', ['-L', path], `Mach-O 依赖终检 ${path}`).stdout;
    const ids = run('/usr/bin/otool', ['-D', path], `Mach-O ID 终检 ${path}`).stdout;
    if (dependencies.includes(RELOCATION_PREFIX) || ids.includes(RELOCATION_PREFIX)) {
      throw new Error(`Mach-O 仍引用安装机框架绝对路径：${path}`);
    }
  }
}

function architectures(path, label) {
  return new Set(run('/usr/bin/lipo', ['-archs', path], `${label}架构核验`).stdout.split(/\s+/u).filter(Boolean));
}

function assertUniversal2(path, label) {
  const actual = architectures(path, label);
  for (const expected of EXPECTED_ARCHITECTURES) {
    if (!actual.has(expected)) throw new Error(`${label}缺少 ${expected} 架构：${path}`);
  }
}

function signMachOFiles(files) {
  const ordered = [...files].sort((left, right) => {
    if (left === frameworkBinary) return 1;
    if (right === frameworkBinary) return -1;
    if (left === pythonExecutable) return 1;
    if (right === pythonExecutable) return -1;
    return left.localeCompare(right);
  });
  for (const path of ordered) {
    run('/usr/bin/codesign', ['--force', '--sign', '-', '--timestamp=none', path], `Mach-O ad-hoc 签名 ${path}`);
  }
  for (const path of ordered) {
    run('/usr/bin/codesign', ['--verify', '--strict', path], `Mach-O ad-hoc 签名核验 ${path}`);
  }
}

async function runtimeMetrics(directory) {
  const entries = (await collectEntries(directory)).sort((left, right) => left.path.localeCompare(right.path));
  let payloadByteLength = 0;
  let payloadFileCount = 0;
  let symlinkCount = 0;
  const payloadHash = createHash('sha256');
  for (const entry of entries) {
    if (entry.path === manifestPath) continue;
    const relativePath = relative(directory, entry.path).split(sep).join('/');
    if (entry.type === 'file') {
      const bytes = await readFile(entry.path);
      payloadFileCount += 1;
      payloadByteLength += bytes.length;
      payloadHash.update(`file\0${relativePath}\0${bytes.length}\0`);
      payloadHash.update(bytes);
    } else if (entry.type === 'symlink') {
      symlinkCount += 1;
      const target = await readlink(entry.path);
      if (target.startsWith(RELOCATION_PREFIX)) {
        throw new Error(`runtime 符号链仍引用安装机绝对路径：${entry.path} -> ${target}`);
      }
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

async function smokeRuntime(sourceDirectory) {
  const temporaryRoot = await mkdtemp(join(tmpdir(), 'yunspire-python-relocated-'));
  const relocatedDirectory = join(temporaryRoot, 'runtime', 'python');
  try {
    await mkdir(dirname(relocatedDirectory), { recursive: true });
    await cp(sourceDirectory, relocatedDirectory, {
      recursive: true,
      dereference: false,
      preserveTimestamps: true,
      verbatimSymlinks: true,
    });
    const executable = join(relocatedDirectory, EXECUTABLE_RELATIVE_PATH);
    const script = [
      'import bz2,hashlib,json,lzma,platform,sqlite3,ssl,sys,urllib.request,zlib',
      'context=ssl.create_default_context()',
      'print(json.dumps({"version":platform.python_version(),"implementation":platform.python_implementation(),"machine":platform.machine(),"prefix":sys.prefix,"executable":sys.executable,"openssl":ssl.OPENSSL_VERSION,"certificateFile":ssl.get_default_verify_paths().cafile,"certificateCount":len(context.get_ca_certs()),"sha256":hashlib.sha256(b"yunspire").hexdigest(),"imports":["bz2","hashlib","lzma","sqlite3","ssl","urllib.request","zlib"]}))',
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
      SSL_CERT_FILE: SYSTEM_CERTIFICATE_FILE,
    };
    const result = run(executable, ['-I', '-c', script], 'macOS Python 任意路径冒烟', {
      cwd: temporaryRoot,
      env: environment,
      timeout: 60_000,
    });
    if (result.stderr.includes(RELOCATION_PREFIX)) {
      throw new Error(`macOS Python 任意路径冒烟仍加载系统 Python framework\n${result.stderr}`);
    }
    let payload;
    try {
      payload = JSON.parse(result.stdout);
    } catch {
      throw new Error(`macOS Python 任意路径冒烟输出无效：${result.stdout}`);
    }
    if (payload.version !== PYTHON_VERSION
      || payload.implementation !== 'CPython'
      || payload.sha256 !== '9cff10f44fced5540177e50bcb6d67724c09fc52fffb336341b6fdefdfb2945a'
      || payload.certificateFile !== SYSTEM_CERTIFICATE_FILE
      || !Number.isInteger(payload.certificateCount)
      || payload.certificateCount <= 0) {
      throw new Error(`macOS Python 任意路径冒烟元数据无效：${JSON.stringify(payload)}`);
    }
    if (await realpath(payload.prefix) !== await realpath(relocatedDirectory)
      || await realpath(payload.executable) !== await realpath(executable)) {
      throw new Error(`macOS Python 仍使用外部 runtime：${JSON.stringify(payload)}`);
    }
    return {
      version: payload.version,
      implementation: payload.implementation,
      hostArchitecture: payload.machine,
      openssl: payload.openssl,
      certificateFile: payload.certificateFile,
      certificateCount: payload.certificateCount,
      imports: payload.imports,
      isolatedMode: true,
      arbitraryPathVerified: true,
    };
  } finally {
    await rm(temporaryRoot, { recursive: true, force: true });
  }
}

async function runtimeIsCurrent(builderSha256) {
  if (!await isFile(manifestPath)
    || !await isFile(pythonExecutable)
    || !await isFile(frameworkBinary)
    || !await isFile(licensePath)
    || !await isFile(installerLicensePath)) return false;
  try {
    const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
    return manifest.schema === RUNTIME_SCHEMA
      && manifest.version === PYTHON_VERSION
      && manifest.sourceUrl === DOWNLOAD_URL
      && manifest.archiveByteLength === EXPECTED_ARCHIVE_BYTES
      && manifest.archiveSha256 === EXPECTED_SHA256
      && manifest.archiveMd5 === EXPECTED_MD5
      && manifest.licenseSha256 === EXPECTED_LICENSE_SHA256
      && manifest.installerLicenseSha256 === EXPECTED_INSTALLER_LICENSE_SHA256
      && manifest.builderSha256 === builderSha256
      && await sha256(licensePath) === EXPECTED_LICENSE_SHA256
      && await sha256(installerLicensePath) === EXPECTED_INSTALLER_LICENSE_SHA256;
  } catch {
    return false;
  }
}

async function buildRuntime(builderSha256) {
  await downloadArchive();
  verifyArchiveSignature();
  const temporaryRoot = await mkdtemp(join(tmpdir(), 'yunspire-python-pkg-'));
  const expandedDirectory = join(temporaryRoot, 'expanded');
  try {
    run('/usr/sbin/pkgutil', ['--expand-full', archivePath, expandedDirectory], 'Python macOS pkg 解包', {
      timeout: 10 * 60_000,
    });
    const sourceRuntime = await findFrameworkPayload(expandedDirectory);
    const sourceLicense = join(sourceRuntime, LICENSE_RELATIVE_PATH);
    if (!await isFile(sourceLicense) || await sha256(sourceLicense) !== EXPECTED_LICENSE_SHA256) {
      throw new Error('Python framework LICENSE.txt 缺失或 SHA-256 不匹配');
    }
    const sourceInstallerLicense = await findInstallerLicense(expandedDirectory);
    await rm(runtimeDirectory, { recursive: true, force: true });
    await mkdir(dirname(runtimeDirectory), { recursive: true });
    await cp(sourceRuntime, runtimeDirectory, {
      recursive: true,
      dereference: false,
      preserveTimestamps: true,
      verbatimSymlinks: true,
    });
    await mkdir(dirname(installerLicensePath), { recursive: true });
    await copyFile(sourceInstallerLicense, installerLicensePath);
    await pruneRuntime();
    await materializeSymlinks(runtimeDirectory);
    await chmod(pythonExecutable, 0o755);
    const machOFiles = await candidateMachOFiles(runtimeDirectory);
    if (machOFiles.length < 70) {
      throw new Error(`Python macOS runtime Mach-O 文件数量异常：${machOFiles.length}`);
    }
    await relocateMachOFiles(machOFiles);
    assertRelocated(machOFiles);
    assertUniversal2(pythonExecutable, 'Python launcher');
    assertUniversal2(frameworkBinary, 'Python framework');
    signMachOFiles(machOFiles);
    const smoke = await smokeRuntime(runtimeDirectory);
    const metrics = await runtimeMetrics(runtimeDirectory);
    if (metrics.symlinkCount !== 0) {
      throw new Error(`Python macOS runtime 打包前仍含有 ${metrics.symlinkCount} 个符号链`);
    }
    const manifest = {
      schema: RUNTIME_SCHEMA,
      version: PYTHON_VERSION,
      architectures: EXPECTED_ARCHITECTURES,
      sourceUrl: DOWNLOAD_URL,
      archiveByteLength: EXPECTED_ARCHIVE_BYTES,
      archiveSha256: EXPECTED_SHA256,
      archiveMd5: EXPECTED_MD5,
      sourceSignature: {
        signer: 'Developer ID Installer: Python Software Foundation (BMM5U3QVKW)',
        notarization: 'trusted',
      },
      licenseFile: LICENSE_RELATIVE_PATH,
      licenseSha256: EXPECTED_LICENSE_SHA256,
      installerLicenseFile: INSTALLER_LICENSE_RELATIVE_PATH,
      installerLicenseSha256: EXPECTED_INSTALLER_LICENSE_SHA256,
      executable: EXECUTABLE_RELATIVE_PATH,
      frameworkBinary: FRAMEWORK_BINARY_RELATIVE_PATH,
      relocationPrefix: RELOCATION_PREFIX,
      certificateFile: SYSTEM_CERTIFICATE_FILE,
      payloadByteLength: metrics.payloadByteLength,
      payloadFileCount: metrics.payloadFileCount,
      payloadSha256: metrics.payloadSha256,
      symlinkCount: metrics.symlinkCount,
      machOFileCount: machOFiles.length,
      pythonExecutableSmoke: smoke,
      builderSha256,
    };
    await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`, 'utf8');
  } finally {
    await rm(temporaryRoot, { recursive: true, force: true });
  }
}

async function verifyRuntime(builderSha256) {
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8'));
  if (manifest.schema !== RUNTIME_SCHEMA
    || manifest.version !== PYTHON_VERSION
    || manifest.builderSha256 !== builderSha256
    || manifest.archiveSha256 !== EXPECTED_SHA256
    || manifest.archiveMd5 !== EXPECTED_MD5
    || manifest.archiveByteLength !== EXPECTED_ARCHIVE_BYTES
    || manifest.sourceSignature?.signer !== 'Developer ID Installer: Python Software Foundation (BMM5U3QVKW)'
    || manifest.sourceSignature?.notarization !== 'trusted'
    || manifest.licenseSha256 !== EXPECTED_LICENSE_SHA256
    || manifest.installerLicenseSha256 !== EXPECTED_INSTALLER_LICENSE_SHA256) {
    throw new Error('macOS Python runtime manifest 与固定来源不一致');
  }
  if (await sha256(licensePath) !== EXPECTED_LICENSE_SHA256
    || await sha256(installerLicensePath) !== EXPECTED_INSTALLER_LICENSE_SHA256) {
    throw new Error('macOS Python runtime 许可证校验失败');
  }
  const machOFiles = await candidateMachOFiles(runtimeDirectory);
  if (machOFiles.length !== manifest.machOFileCount) {
    throw new Error(`macOS Python runtime Mach-O 数量不一致：${machOFiles.length}/${manifest.machOFileCount}`);
  }
  assertRelocated(machOFiles);
  assertUniversal2(pythonExecutable, 'Python launcher');
  assertUniversal2(frameworkBinary, 'Python framework');
  for (const path of machOFiles) {
    run('/usr/bin/codesign', ['--verify', '--strict', path], `Mach-O ad-hoc 签名复核 ${path}`);
  }
  const metrics = await runtimeMetrics(runtimeDirectory);
  if (metrics.payloadByteLength !== manifest.payloadByteLength
    || metrics.payloadFileCount !== manifest.payloadFileCount
    || metrics.payloadSha256 !== manifest.payloadSha256
    || metrics.symlinkCount !== manifest.symlinkCount) {
    throw new Error(`macOS Python runtime 体积清单不一致：${JSON.stringify(metrics)}`);
  }
  await smokeRuntime(runtimeDirectory);
  return manifest;
}

await mkdir(outputRoot, { recursive: true });
const builderSha256 = digest('sha256', await readFile(import.meta.filename));
if (!await runtimeIsCurrent(builderSha256)) await buildRuntime(builderSha256);
const manifest = await verifyRuntime(builderSha256);
console.log(`MACOS_PYTHON_RUNTIME_OK version=${manifest.version} architectures=${manifest.architectures.join(',')} machos=${manifest.machOFileCount}`);
