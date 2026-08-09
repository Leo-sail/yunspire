import {
  readFile,
  readdir,
  readlink,
  stat,
} from 'node:fs/promises';
import { homedir } from 'node:os';
import { basename, dirname, join, relative, resolve, sep } from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

const forbiddenDirectories = new Set([
  '.git',
  '.obsidian',
  '.storybook',
  '__pycache__',
  '__tests__',
  'cache',
  'coverage',
  'logs',
  'playwright-report',
  'screenshots',
  'test',
  'test-results',
  'tests',
  'vault',
]);
const forbiddenFilePatterns = [
  /^\.DS_Store$/u,
  /^\.env(?:\.|$)/u,
  /\.(?:db|log|pem|pfx|p12|pyc|sqlite|sqlite3|trace|tmp)$/iu,
  /\.(?:spec|stories|test)\.(?:cjs|js|jsx|mjs|ts|tsx)$/iu,
];
const contentPatterns = [
  { label: 'macOS absolute user path', pattern: /\/Users\/[A-Za-z0-9._-]{1,64}\//gu },
  { label: 'Linux absolute user path', pattern: /\/home\/[A-Za-z0-9._-]{1,64}\//gu },
  { label: 'Windows absolute user path', pattern: /[A-Za-z]:\\Users\\[^\\\u0000-\u001f]{1,128}\\/gu },
  { label: 'private temporary path', pattern: /\/private\/tmp\/[A-Za-z0-9._-]+\//gu },
  { label: 'OpenAI-style secret', pattern: /\bsk-[A-Za-z0-9_-]{20,}\b/gu },
  { label: 'GitHub token', pattern: /\b(?:gh[op]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})\b/gu },
  { label: 'AWS access key', pattern: /\bAKIA[0-9A-Z]{16}\b/gu },
  { label: 'private key', pattern: /-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----/gu },
  { label: 'private Yunspire workspace', pattern: /Yunspire-private-docs/gu },
];
const buildWorkspace = resolve(import.meta.dirname, '..');
const cargoHome = resolve(process.env.CARGO_HOME || join(homedir(), '.cargo'));
const forbiddenLiteralBuildPaths = [...new Set([
  buildWorkspace,
  buildWorkspace.replaceAll('\\', '/'),
  cargoHome,
  cargoHome.replaceAll('\\', '/'),
])].filter((value) => value.length > 4);

function portablePath(root, path) {
  return relative(root, path).split(sep).join('/');
}

function pathInside(path, root) {
  return path === root || path.startsWith(`${root}${sep}`);
}

function createState() {
  return {
    byteLength: 0,
    failures: [],
    fileCount: 0,
    symlinkCount: 0,
  };
}

function scanBytes(bytes, relativePath, state) {
  state.fileCount += 1;
  state.byteLength += bytes.length;
  for (const buildPath of forbiddenLiteralBuildPaths) {
    for (const encoding of ['utf8', 'utf16le']) {
      if (bytes.includes(Buffer.from(buildPath, encoding))) {
        state.failures.push(`build-machine path (${encoding}): ${relativePath}`);
      }
    }
  }
  const textVariants = [
    { encoding: 'single-byte', text: bytes.toString('latin1') },
    { encoding: 'UTF-16LE', text: bytes.toString('utf16le') },
  ];
  for (const { label, pattern } of contentPatterns) {
    for (const { encoding, text } of textVariants) {
      pattern.lastIndex = 0;
      if (pattern.test(text)) {
        state.failures.push(`${label} (${encoding}): ${relativePath}`);
        break;
      }
    }
  }
}

async function scanFile(path, relativePath, state) {
  scanBytes(await readFile(path), relativePath, state);
}

async function collect(directory, root, state) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = resolve(directory, entry.name);
    const relativePath = portablePath(root, path);
    if (entry.isDirectory()) {
      if (forbiddenDirectories.has(entry.name.toLowerCase())) {
        state.failures.push(`forbidden packaged directory: ${relativePath}`);
        continue;
      }
      await collect(path, root, state);
    } else if (entry.isSymbolicLink()) {
      const target = await readlink(path);
      const resolvedTarget = resolve(dirname(path), target);
      if (target.startsWith('/') || !pathInside(resolvedTarget, root)) {
        state.failures.push(`packaged symlink escapes the application: ${relativePath} -> ${target}`);
      }
      state.symlinkCount += 1;
    } else if (entry.isFile()) {
      if (forbiddenFilePatterns.some((pattern) => pattern.test(entry.name))) {
        state.failures.push(`forbidden packaged file: ${relativePath}`);
        continue;
      }
      await scanFile(path, relativePath, state);
    }
  }
}

function verifiedResult(state, platform) {
  if (state.failures.length) {
    throw new Error(`packaged privacy verification failed for ${platform}\n${state.failures.join('\n')}`);
  }
  return {
    platform,
    fileCount: state.fileCount,
    byteLength: state.byteLength,
    symlinkCount: state.symlinkCount,
  };
}

export async function verifyPackagedPrivacy(directory, { platform = process.platform } = {}) {
  const root = resolve(directory);
  const rootInfo = await stat(root).catch(() => null);
  if (!rootInfo?.isDirectory()) throw new Error(`packaged privacy root is not a directory: ${root}`);
  const state = createState();
  await collect(root, root, state);
  return verifiedResult(state, platform);
}

export async function verifyPackagedFilePrivacy(file, { platform = process.platform } = {}) {
  const path = resolve(file);
  const info = await stat(path).catch(() => null);
  if (!info?.isFile()) throw new Error(`packaged privacy target is not a file: ${path}`);
  const state = createState();
  await scanFile(path, basename(path), state);
  return verifiedResult(state, platform);
}

function option(name, fallback = null) {
  const index = process.argv.indexOf(`--${name}`);
  return index >= 0 ? process.argv[index + 1] : fallback;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const directory = option('directory');
    const file = option('file');
    if (Boolean(directory) === Boolean(file)) {
      throw new Error('usage: verify-packaged-privacy.mjs (--directory <path> | --file <path>) [--platform <name>]');
    }
    const options = { platform: option('platform', process.platform) };
    const result = directory
      ? await verifyPackagedPrivacy(directory, options)
      : await verifyPackagedFilePrivacy(file, options);
    console.log(`PACKAGED_PRIVACY_OK platform=${result.platform} files=${result.fileCount} bytes=${result.byteLength} symlinks=${result.symlinkCount}`);
  } catch (error) {
    console.error(`PACKAGED_PRIVACY_FAILED ${error.message}`);
    process.exitCode = 1;
  }
}
