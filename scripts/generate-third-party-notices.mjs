import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { constants } from 'node:fs';
import {
  access,
  mkdir,
  readFile,
  readdir,
  rename,
  rm,
  stat,
  writeFile,
} from 'node:fs/promises';
import path from 'node:path';
import process from 'node:process';

const root = path.resolve(import.meta.dirname, '..');
const cargoManifest = path.join(root, 'src-tauri', 'Cargo.toml');
const cargoLockPath = path.join(root, 'src-tauri', 'Cargo.lock');
const packageLockPath = path.join(root, 'package-lock.json');
const outputDirectory = path.join(root, 'src-tauri', 'target', 'yunspire-licenses');
const outputPath = path.join(outputDirectory, 'THIRD_PARTY_NOTICES.txt');
const licenseNamePattern = /^(?:licen[cs]es?|copying|notice|copyright|unlicense)(?:[._-].*)?$/iu;
const maxLicenseBytes = 2 * 1024 * 1024;
const maxOutputBytes = 32 * 1024 * 1024;
const reviewedTextlessPackages = new Set([
  'Cargo|alloc-stdlib|0.2.4|BSD-3-Clause|sha256-0e76a019e91224d279006ff972f1e984179a6e9feb050adba6ce8274aef23195',
  'Cargo|block2|0.6.2|MIT|sha256-cdeb9d870516001442e364c5220d3574d2da8dc765554b4a617230d33fa58ef5',
  'Cargo|dispatch2|0.3.1|Zlib OR Apache-2.0 OR MIT|sha256-1e0e367e4e7da84520dedcac1901e4da967309406d1e51017ae1abfb97adbd38',
  'Cargo|objc2|0.6.4|MIT|sha256-3a12a8ed07aefc768292f076dc3ac8c48f3781c8f2d5851dd3d98950e8c5a89f',
  'Cargo|objc2-app-kit|0.3.2|Zlib OR Apache-2.0 OR MIT|sha256-d49e936b501e5c5bf01fda3a9452ff86dc3ea98ad5f283e1455153142d97518c',
  'Cargo|objc2-core-foundation|0.3.2|Zlib OR Apache-2.0 OR MIT|sha256-2a180dd8642fa45cdb7dd721cd4c11b1cadd4929ce112ebd8b9f5803cc79d536',
  'Cargo|objc2-core-graphics|0.3.2|Zlib OR Apache-2.0 OR MIT|sha256-e022c9d066895efa1345f8e33e584b9f958da2fd4cd116792e15e07e4720a807',
  'Cargo|objc2-encode|4.1.0|MIT|sha256-ef25abbcd74fb2609453eb695bd2f860d389e457f67dc17cafc8b8cbc89d0c33',
  'Cargo|objc2-exception-helper|0.1.1|Zlib OR Apache-2.0 OR MIT|sha256-c7a1c5fbb72d7735b076bb47b578523aedc40f3c439bea6dfd595c089d79d98a',
  'Cargo|objc2-foundation|0.3.2|MIT|sha256-e3e0adef53c21f888deb4fa59fc59f7eb17404926ee8a6f59f5df0fd7f9f3272',
  'Cargo|objc2-io-surface|0.3.2|Zlib OR Apache-2.0 OR MIT|sha256-180788110936d59bab6bd83b6060ffdfffb3b922ba1396b312ae795e1de9d81d',
  'Cargo|objc2-web-kit|0.3.2|Zlib OR Apache-2.0 OR MIT|sha256-b2e5aaab980c433cf470df9d7af96a7b46a9d892d521a2cbbb2f8a4c16751e7f',
  'Cargo|selectors|0.36.1|MPL-2.0|sha256-c5d9c0c92a92d33f08817311cf3f2c29a3538a8240e94a6a3c622ce652d7e00c',
  'Cargo|tauri-plugin|2.6.3|Apache-2.0 OR MIT|sha256-74be5dd4bed9afbd145e5716b5fa2ec28cbc29c34ffa61c258c9273d896c8020',
  'Cargo|unic-char-property|0.9.0|MIT/Apache-2.0|sha256-a8c57a407d9b6fa02b4795eb81c5b6652060a15a7903ea981f3d723e6c0be221',
  'Cargo|unic-char-range|0.9.0|MIT/Apache-2.0|sha256-0398022d5f700414f6b899e10b8348231abf9173fa93144cbc1a43b9793c1fbc',
  'Cargo|unic-common|0.9.0|MIT/Apache-2.0|sha256-80d7ff825a6a654ee85a63e80f92f054f904f21e7d12da4e22f9834a4aaa35bc',
  'Cargo|unic-ucd-ident|0.9.0|MIT/Apache-2.0|sha256-e230a37c0381caa9219d67cf063aa3a375ffed5bf541a452db16e744bdab6987',
  'Cargo|unic-ucd-version|0.9.0|MIT/Apache-2.0|sha256-96bd2f2237fe450fcd0a1d2f5f4e91711124f7857ba2e964247776ebeeb7b0c4',
  'Cargo|webview2-com|0.38.2|MIT|sha256-7130243a7a5b33c54a444e54842e6a9e133de08b5ad7b5861cd8ed9a6a5bc96a',
  'Cargo|webview2-com-macros|0.8.1|MIT|sha256-67a921c1b6914c367b2b823cd4cde6f96beec77d30a939c8199bb377cf9b9b54',
  'Cargo|webview2-com-sys|0.38.2|MIT|sha256-381336cfffd772377d291702245447a5251a2ffa5bad679c99e61bc48bacbf9c',
  'npm|@esbuild/darwin-arm64|0.27.2|MIT|sha512-davCD2Zc80nzDVRwXTcQP/28fiJbcOwvdolL0sOiOsbwBa72kegmVU0Wrh1MYrbuCL98Omp5dVhQFWRKR2ZAlg==',
  'npm|@esbuild/darwin-x64|0.27.2|MIT|sha512-ZxtijOmlQCBWGwbVmwOF/UCzuGIbUkqB1faQRf5akQmxRJ1ujusWsb3CVfk/9iZKr2L5SMU5wPBi1UWbvL+VQA==',
  'npm|@esbuild/win32-x64|0.27.2|MIT|sha512-sRdU18mcKf7F+YgheI/zGf5alZatMUTKj/jNS6l744f9u3WFu4v7twcUI9vu4mknF4Y9aDlblIie0IM+5xxaqQ==',
  'npm|@rolldown/binding-darwin-arm64|1.1.5|MIT|sha512-51Bnx9pNiMRKSUNtBfySkNJ9vMU9Hh3I1ozDd6gyPPYzaXCfnptUcEZxXGYFn+ul2dtcMUiqGR1Yai2K10uoTw==',
  'npm|@rolldown/binding-darwin-x64|1.1.5|MIT|sha512-Tm+gbfC0aHu1tBA/JvKQh32S0K6YgCHkiAF4/W6xX0K0RmNuc94VeK419dJoE65R5aRxmo+noZQSWrAMF6yb6g==',
  'npm|@rolldown/binding-win32-x64-msvc|1.1.5|MIT|sha512-tTZuDBPw85tEN5PQi1pnEBzDy0Z49HtScLAbD5t6hyeU92A95pRWaSMw1GZZi/RwgSgUIl0xrSlXIT/9QzvYSA==',
  'npm|@tauri-apps/cli-darwin-arm64|2.11.4|Apache-2.0 OR MIT|sha512-1ryOF3ZhpZ/nemHV5zVwBQBz9jDGKmKPvWPADOhc83ig0P4bMc2iER4NbC6r9sjeIZ6RVQ4g3RZIYvezhcl4TQ==',
  'npm|@tauri-apps/cli-darwin-x64|2.11.4|Apache-2.0 OR MIT|sha512-uFsGQAAfuyz1k/yGLmkWfkBlgKAqZfxqlHmLWx81QU27RJWfmbNHCIq8T8w1e+VClleIuZUjpHWfoE4E3DLo3A==',
  'npm|@tauri-apps/cli-win32-x64-msvc|2.11.4|Apache-2.0 OR MIT|sha512-+vDiqBIU5dMISg/wNvX3sF+ZHfgJGJ5T0AcO+EHNXV9GGAG+P5fzodlDXD3QdKCRgZxMoCm5PPvj3BqLNjBthw==',
]);

function compareText(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
}

function isInside(directory, candidate) {
  const relative = path.relative(directory, candidate);
  return relative === ''
    || (!relative.startsWith(`..${path.sep}`) && relative !== '..' && !path.isAbsolute(relative));
}

function run(pathname, argumentsList) {
  const result = spawnSync(pathname, argumentsList, {
    cwd: root,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
    windowsHide: true,
  });
  if (result.error || result.status !== 0) {
    throw new Error([
      `无法执行 ${pathname} ${argumentsList.join(' ')}`,
      result.error?.message,
      result.stdout,
      result.stderr,
    ].filter(Boolean).join('\n'));
  }
  return result.stdout;
}

function buildTargets() {
  const configured = process.env.YUNSPIRE_LICENSE_TARGETS?.trim();
  const values = configured
    ? configured.split(',')
    : [run('rustc', ['-vV']).match(/^host:\s*(\S+)$/mu)?.[1]];
  const targets = [...new Set(values.map((value) => value?.trim()).filter(Boolean))].sort();
  if (!targets.length || targets.some((value) => !/^[A-Za-z0-9_.-]+$/u.test(value))) {
    throw new Error('第三方许可目标必须是逗号分隔的有效 Rust target triple');
  }
  return targets;
}

function cargoMetadata(target) {
  const raw = run('cargo', [
    'metadata',
    '--manifest-path', cargoManifest,
    '--format-version', '1',
    '--locked',
    '--filter-platform', target,
  ]);
  return JSON.parse(raw);
}

function reachableCargoPackages(metadata) {
  if (!metadata.resolve) throw new Error('Cargo metadata 缺少依赖解析结果');
  const nodes = new Map(metadata.resolve.nodes.map((node) => [node.id, node]));
  const pending = [...metadata.workspace_members];
  const reachable = new Set();
  while (pending.length) {
    const id = pending.pop();
    if (!id || reachable.has(id)) continue;
    reachable.add(id);
    const node = nodes.get(id);
    for (const dependency of node?.deps || []) pending.push(dependency.pkg);
  }
  return metadata.packages.filter((item) => item.source && reachable.has(item.id));
}

function normalizeRepository(value) {
  const candidate = typeof value === 'string' ? value : value?.url;
  return typeof candidate === 'string' && candidate.trim() ? candidate.trim() : null;
}

function reviewedTextlessKey(item) {
  return [item.ecosystem, item.name, item.version, item.license, item.integrity].join('|');
}

function assertReviewedTextlessPackage(item) {
  const key = reviewedTextlessKey(item);
  if (!reviewedTextlessPackages.has(key)) {
    throw new Error(
      `${item.ecosystem}:${item.name}@${item.version} 缺少上游许可正文，且未命中版本与哈希审查白名单：${key}`,
    );
  }
}

async function cargoLockChecksums() {
  const entries = new Map();
  let current = null;
  const commit = () => {
    if (!current?.name || !current.version || !current.source) return;
    if (!current.checksum) {
      throw new Error(`Cargo.lock 的注册表包缺少 checksum：${current.name}@${current.version}`);
    }
    const key = `${current.name}@${current.version}|${current.source}`;
    if (entries.has(key)) throw new Error(`Cargo.lock 包键重复：${key}`);
    entries.set(key, current.checksum);
  };
  for (const rawLine of (await readFile(cargoLockPath, 'utf8')).split(/\r?\n/gu)) {
    const line = rawLine.trim();
    if (line === '[[package]]') {
      commit();
      current = {};
      continue;
    }
    if (!current) continue;
    const match = /^(name|version|source|checksum) = ("(?:[^"\\]|\\.)*")$/u.exec(line);
    if (match) current[match[1]] = JSON.parse(match[2]);
  }
  commit();
  return entries;
}

async function readableFile(filePath) {
  try {
    await access(filePath, constants.R_OK);
    return (await stat(filePath)).isFile();
  } catch {
    return false;
  }
}

async function discoverLicenseFiles(packageRoot, declaredLicenseFile) {
  const candidates = new Map();
  if (declaredLicenseFile) {
    const declared = path.isAbsolute(declaredLicenseFile)
      ? declaredLicenseFile
      : path.resolve(packageRoot, declaredLicenseFile);
    if (isInside(packageRoot, declared) && await readableFile(declared)) {
      candidates.set(declared, path.basename(declared));
    }
  }
  const entries = await readdir(packageRoot, { withFileTypes: true });
  for (const entry of entries) {
    const absolute = path.join(packageRoot, entry.name);
    if (entry.isFile() && licenseNamePattern.test(entry.name)) {
      candidates.set(absolute, entry.name);
    } else if (entry.isDirectory() && /^licenses$/iu.test(entry.name)) {
      const nested = await readdir(absolute, { withFileTypes: true });
      for (const nestedEntry of nested) {
        if (!nestedEntry.isFile()) continue;
        candidates.set(
          path.join(absolute, nestedEntry.name),
          `${entry.name}/${nestedEntry.name}`,
        );
      }
    }
  }
  return [...candidates.entries()]
    .map(([filePath, label]) => ({ filePath, label }))
    .sort((left, right) => compareText(left.label, right.label));
}

async function readLicenseText(filePath, packageId) {
  const value = await stat(filePath);
  if (!value.isFile() || value.size <= 0 || value.size > maxLicenseBytes) {
    throw new Error(`${packageId} 的许可文件大小无效：${path.basename(filePath)}`);
  }
  const text = await readFile(filePath, 'utf8');
  if (text.includes('\0') || !text.trim()) {
    throw new Error(`${packageId} 的许可文件不是非空文本：${path.basename(filePath)}`);
  }
  return `${text.replace(/\r\n?/gu, '\n').trim()}\n`;
}

async function cargoInventory(targets, lockChecksums) {
  const packages = new Map();
  for (const target of targets) {
    for (const item of reachableCargoPackages(cargoMetadata(target))) {
      const key = `${item.name}@${item.version}|${item.source}`;
      const current = packages.get(key) || { item, targets: new Set() };
      current.targets.add(target);
      packages.set(key, current);
    }
  }
  return Promise.all([...packages.values()].map(async ({ item, targets: packageTargets }) => {
    const packageRoot = path.dirname(item.manifest_path);
    const files = await discoverLicenseFiles(packageRoot, item.license_file);
    const id = `cargo:${item.name}@${item.version}`;
    const checksum = lockChecksums.get(`${item.name}@${item.version}|${item.source}`);
    if (!checksum) throw new Error(`${id} 无法在 Cargo.lock 中找到固定 checksum`);
    const sourceUrl = normalizeRepository(item.repository)
      || `https://crates.io/crates/${encodeURIComponent(item.name)}/${encodeURIComponent(item.version)}`;
    const texts = await Promise.all(files.map(async (file) => ({
      label: file.label,
      text: await readLicenseText(file.filePath, id),
    })));
    if (!item.license && !texts.length) {
      throw new Error(`${id} 既没有 SPDX 声明，也没有可读取的许可正文`);
    }
    const result = {
      ecosystem: 'Cargo',
      name: item.name,
      version: item.version,
      license: item.license || 'SEE BUNDLED LICENSE FILE',
      sourceUrl,
      integrity: `sha256-${checksum}`,
      authors: item.authors || [],
      targets: [...packageTargets].sort(),
      texts,
    };
    if (!texts.length) assertReviewedTextlessPackage(result);
    return result;
  }));
}

function npmConstraintMatches(values, actual) {
  if (!Array.isArray(values) || !values.length) return true;
  const denied = values.filter((value) => value.startsWith('!')).map((value) => value.slice(1));
  if (denied.includes(actual)) return false;
  const allowed = values.filter((value) => !value.startsWith('!'));
  return !allowed.length || allowed.includes(actual);
}

function npmPackageApplies(lockEntry) {
  return npmConstraintMatches(lockEntry.os, process.platform)
    && npmConstraintMatches(lockEntry.cpu, process.arch);
}

function npmPackageHasPlatformConstraint(lockEntry) {
  return (Array.isArray(lockEntry.os) && lockEntry.os.length > 0)
    || (Array.isArray(lockEntry.cpu) && lockEntry.cpu.length > 0);
}

function npmPackageName(packagePath) {
  const marker = 'node_modules/';
  const offset = packagePath.lastIndexOf(marker);
  if (offset < 0) throw new Error(`package-lock.json 包路径无效：${packagePath}`);
  const components = packagePath.slice(offset + marker.length).split('/');
  return components[0].startsWith('@') ? components.slice(0, 2).join('/') : components[0];
}

async function npmInventory() {
  const lock = JSON.parse(await readFile(packageLockPath, 'utf8'));
  const packages = [];
  for (const packagePath of Object.keys(lock.packages || {}).filter(Boolean).sort()) {
    const packageRoot = path.resolve(root, packagePath);
    if (!isInside(path.join(root, 'node_modules'), packageRoot)) {
      throw new Error(`package-lock.json 包路径越过 node_modules：${packagePath}`);
    }
    const packageJsonPath = path.join(packageRoot, 'package.json');
    const lockEntry = lock.packages[packagePath] || {};
    if (!await readableFile(packageJsonPath)) {
      if (lockEntry.optional
        && (!npmPackageHasPlatformConstraint(lockEntry) || !npmPackageApplies(lockEntry))) continue;
      throw new Error(`锁定的 npm 包未安装：${packagePath}`);
    }
    const item = JSON.parse(await readFile(packageJsonPath, 'utf8'));
    const expectedName = npmPackageName(packagePath);
    if (item.name !== expectedName || item.version !== lockEntry.version) {
      throw new Error(
        `npm 安装内容与锁文件不一致：${packagePath} expected=${expectedName}@${lockEntry.version}`
          + ` actual=${item.name}@${item.version}`,
      );
    }
    const id = `npm:${item.name}@${item.version}`;
    const files = await discoverLicenseFiles(packageRoot, item.licenseFile);
    const texts = await Promise.all(files.map(async (file) => ({
      label: file.label,
      text: await readLicenseText(file.filePath, id),
    })));
    const license = typeof item.license === 'string' ? item.license.trim() : '';
    const integrity = typeof lockEntry.integrity === 'string' ? lockEntry.integrity.trim() : '';
    if (!integrity) throw new Error(`${id} 的 package-lock.json 条目缺少 integrity`);
    if (!license && !texts.length) {
      throw new Error(`${id} 既没有许可证声明，也没有可读取的许可正文`);
    }
    const result = {
      ecosystem: 'npm',
      name: item.name,
      version: item.version,
      license: license || 'SEE BUNDLED LICENSE FILE',
      sourceUrl: normalizeRepository(item.repository)
        || (typeof item.homepage === 'string' ? item.homepage : null)
        || lockEntry.resolved
        || `https://www.npmjs.com/package/${encodeURIComponent(item.name)}/v/${encodeURIComponent(item.version)}`,
      integrity,
      authors: item.author ? [typeof item.author === 'string' ? item.author : item.author.name] : [],
      targets: [],
      texts,
    };
    if (!texts.length) assertReviewedTextlessPackage(result);
    packages.push(result);
  }
  return packages;
}

function singleLine(value) {
  return String(value || '').replace(/\s+/gu, ' ').replaceAll('|', '/').trim();
}

function render(packages, targets) {
  const ordered = [...packages].sort((left, right) => (
    compareText(left.ecosystem, right.ecosystem)
      || compareText(left.name, right.name)
      || compareText(left.version, right.version)
  ));
  const textGroups = new Map();
  const withoutText = [];
  for (const item of ordered) {
    if (!item.texts.length) withoutText.push(item);
    for (const license of item.texts) {
      const digest = createHash('sha256').update(license.text, 'utf8').digest('hex');
      const group = textGroups.get(digest) || { text: license.text, references: [] };
      group.references.push(`${item.ecosystem}:${item.name}@${item.version} (${license.label})`);
      textGroups.set(digest, group);
    }
  }

  const lines = [
    'Yunspire Third-Party Notices',
    '=============================',
    '',
    'This file is generated from the exact Cargo and npm dependency metadata used by the build.',
    'It contains third-party package identity, declared license information, source locations,',
    'and every license, notice, or copying text shipped by those installed packages.',
    'Yunspire first-party source code is governed separately by the bundled legal/LICENSE file.',
    '',
    `Rust build targets: ${targets.join(', ')}`,
    `Packages: ${ordered.length}`,
    `Distinct bundled license texts: ${textGroups.size}`,
    '',
    'Package Inventory',
    '-----------------',
  ];
  for (const item of ordered) {
    const targetLabel = item.targets.length ? ` | targets: ${item.targets.join(',')}` : '';
    lines.push(
      `- ${item.ecosystem}:${singleLine(item.name)}@${singleLine(item.version)}`
        + ` | license: ${singleLine(item.license)}`
        + ` | source: ${singleLine(item.sourceUrl)}`
        + ` | integrity: ${singleLine(item.integrity)}`
        + targetLabel,
    );
  }
  if (withoutText.length) {
    lines.push(
      '',
      'Reviewed Packages Without an Upstream License File',
      '--------------------------------------------------',
      'The following installed packages do not ship a separate license text. Each entry is',
      'accepted only through an exact name, version, declared-license, and lock-integrity review.',
    );
    for (const item of withoutText) {
      const authors = item.authors.filter(Boolean).map(singleLine).join(', ') || 'not declared';
      lines.push(
        `- ${item.ecosystem}:${singleLine(item.name)}@${singleLine(item.version)}`
          + ` | license: ${singleLine(item.license)}`
          + ` | integrity: ${singleLine(item.integrity)}`
          + ` | authors: ${authors}`,
      );
    }
  }
  lines.push('', 'Bundled License Texts', '---------------------');
  for (const [digest, group] of [...textGroups.entries()].sort(([left], [right]) => compareText(left, right))) {
    lines.push('', `SHA-256: ${digest}`, 'Applies to:');
    for (const reference of group.references.sort()) lines.push(`- ${reference}`);
    lines.push('', group.text.trimEnd());
  }
  return `${lines.join('\n')}\n`;
}

const targets = buildTargets();
const lockChecksums = await cargoLockChecksums();
const [cargoPackages, npmPackages] = await Promise.all([
  cargoInventory(targets, lockChecksums),
  npmInventory(),
]);
const output = render([...cargoPackages, ...npmPackages], targets);
if (Buffer.byteLength(output, 'utf8') > maxOutputBytes) {
  throw new Error('第三方许可汇总超过 32 MB 安全上限');
}
await mkdir(outputDirectory, { recursive: true });
const temporaryPath = `${outputPath}.tmp-${process.pid}`;
try {
  await writeFile(temporaryPath, output, { encoding: 'utf8', mode: 0o600 });
  await rename(temporaryPath, outputPath);
} finally {
  await rm(temporaryPath, { force: true });
}
console.log(
  `THIRD_PARTY_NOTICES_OK cargo=${cargoPackages.length} npm=${npmPackages.length}`
    + ` texts=${new Set([...cargoPackages, ...npmPackages].flatMap((item) => item.texts.map((entry) => createHash('sha256').update(entry.text).digest('hex')))).size}`
    + ` bytes=${Buffer.byteLength(output, 'utf8')} targets=${targets.join(',')}`,
);
