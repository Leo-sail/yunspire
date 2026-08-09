import { spawnSync } from 'node:child_process';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';
import process from 'node:process';

const root = resolve(import.meta.dirname, '..');
const argumentsList = process.argv.slice(2);
const requiredArguments = [
  'build',
  '--config',
  'src-tauri/tauri.release.conf.json',
  '--no-sign',
  '--ci',
];

for (const required of requiredArguments) {
  if (!argumentsList.includes(required)) {
    throw new Error(`release Tauri invocation is missing required argument: ${required}`);
  }
}
if (process.env.RUSTFLAGS?.trim()) {
  throw new Error('release builds must use CARGO_ENCODED_RUSTFLAGS so path remapping stays argument-safe');
}

const separator = '\u001f';
const existingFlags = String(process.env.CARGO_ENCODED_RUSTFLAGS || '')
  .split(separator)
  .filter(Boolean);
const cargoHome = resolve(process.env.CARGO_HOME || join(homedir(), '.cargo'));
const remapFlags = [
  `--remap-path-prefix=${root}=/workspace/yunspire`,
  `--remap-path-prefix=${cargoHome}=/cargo`,
];
const environment = {
  ...process.env,
  CARGO_ENCODED_RUSTFLAGS: [...existingFlags, ...remapFlags].join(separator),
  PYTHONDONTWRITEBYTECODE: '1',
};
if (process.platform === 'darwin') {
  environment.LANG = 'en_US.UTF-8';
  environment.LC_ALL = 'en_US.UTF-8';
  environment.LC_CTYPE = 'en_US.UTF-8';
}
delete environment.RUSTFLAGS;

const tauriCli = join(root, 'node_modules', '@tauri-apps', 'cli', 'tauri.js');
console.log('RELEASE_RUST_PATH_REMAP_OK workspace=/workspace/yunspire cargo=/cargo');
const result = spawnSync(process.execPath, [tauriCli, ...argumentsList], {
  cwd: root,
  env: environment,
  stdio: 'inherit',
});
if (result.error) throw result.error;
process.exitCode = result.status ?? 1;
