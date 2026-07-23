import { access, mkdir, rm, writeFile } from 'node:fs/promises';
import { constants } from 'node:fs';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import process from 'node:process';

const root = process.cwd();
const sourceDirectory = path.join(root, 'skills', 'video-content-analysis', 'scripts');
const binaryDirectory = path.join(root, 'src-tauri', 'target', 'yunspire-native');

if (process.platform !== 'win32') {
  console.log('WINDOWS_MEDIA_HELPER_SKIPPED platform!=win32');
  process.exit(0);
}

const programFilesX86 = process.env['ProgramFiles(x86)'];
const vswhere = programFilesX86 && path.join(programFilesX86, 'Microsoft Visual Studio', 'Installer', 'vswhere.exe');
try {
  await access(vswhere, constants.X_OK);
} catch {
  throw new Error('未找到 Windows Visual Studio Build Tools；无法构建 Yunspire 本地媒体适配器');
}

const visualStudio = spawnSync(vswhere, ['-latest', '-products', '*', '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-property', 'installationPath'], { encoding: 'utf8' });
const installation = visualStudio.status === 0 ? visualStudio.stdout.trim() : '';
const developerCommand = installation && path.join(installation, 'Common7', 'Tools', 'VsDevCmd.bat');
try {
  await access(developerCommand, constants.R_OK);
} catch {
  throw new Error('未找到 Windows C++ 工具链；无法构建 Yunspire 本地媒体适配器');
}

await mkdir(binaryDirectory, { recursive: true });
const targets = [
  ['yunspire_media_windows.cpp', 'yunspire-media.exe', 'mfplat.lib mfreadwrite.lib mfuuid.lib windowscodecs.lib ole32.lib oleaut32.lib'],
  ['yunspire_speech_windows.cpp', 'yunspire-speech.exe', 'sapi.lib ole32.lib'],
];
for (const [sourceName, outputName, libraries] of targets) {
  const source = path.join(sourceDirectory, sourceName);
  const output = path.join(binaryDirectory, outputName);
  const object = path.join(binaryDirectory, `${path.parse(outputName).name}.obj`);
  await rm(output, { force: true });
  await rm(object, { force: true });
  const commandFile = path.join(binaryDirectory, `.build-${path.parse(outputName).name}.cmd`);
  const command = [
    '@echo off',
    `call "${developerCommand}" -arch=x64 -host_arch=x64 >nul`,
    'if errorlevel 1 exit /b 1',
    `cl.exe /nologo /std:c++17 /O2 /EHsc /MT /utf-8 /permissive- /W4 /WX /external:W0 /external:anglebrackets /DUNICODE /D_UNICODE /DNOMINMAX /Fo:"${object}" "${source}" /Fe:"${output}" /link ${libraries}`,
  ].join('\r\n') + '\r\n';
  await writeFile(commandFile, command, 'utf8');
  try {
    const result = spawnSync('cmd.exe', ['/d', '/c', commandFile], { encoding: 'utf8', maxBuffer: 8 * 1024 * 1024 });
    if (result.status !== 0) {
      throw new Error(`构建 ${outputName} 失败：\n${result.stdout}\n${result.stderr}`.trim());
    }
    await access(output, constants.X_OK);
    await rm(object, { force: true });
  } finally {
    await rm(commandFile, { force: true });
  }
}
console.log('WINDOWS_MEDIA_HELPERS_OK yunspire-media.exe yunspire-speech.exe');
