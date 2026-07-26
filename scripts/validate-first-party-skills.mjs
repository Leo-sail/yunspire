import { readFile, readdir, stat } from "node:fs/promises";
import { basename, extname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../skills", import.meta.url));
const desktopRoot = fileURLToPath(new URL("..", import.meta.url));
const pythonStandardLibrary = new Set([
  "__future__", "argparse", "base64", "ctypes", "dataclasses", "datetime", "errno", "hashlib", "html", "http", "ipaddress", "json", "locale", "math", "mimetypes", "os", "pathlib", "posixpath", "re", "shutil", "socket", "ssl", "subprocess", "sys", "tempfile", "threading", "time", "urllib", "warnings", "zipfile", "xml",
]);
const appleSystemFrameworks = new Set([
  "AppKit", "AVFoundation", "CoreGraphics", "CoreMedia", "Foundation", "ImageIO", "PDFKit", "Speech", "UniformTypeIdentifiers",
]);
const cppStandardHeaders = new Set([
  "algorithm", "cmath", "cstdint", "cstdio", "cstring", "cwchar", "filesystem", "fstream", "iomanip", "iostream", "limits", "sstream", "stdexcept", "string", "tuple", "vector",
]);
const windowsSystemHeaders = new Set([
  "windows.h", "wincodec.h", "oleauto.h", "mfapi.h", "mferror.h", "mfidl.h", "mfreadwrite.h", "sapi.h", "sphelper.h", "wrl/client.h",
  "winrt/base.h", "winrt/Windows.Data.Pdf.h", "winrt/Windows.Foundation.h", "winrt/Windows.Graphics.Imaging.h", "winrt/Windows.Storage.h", "winrt/Windows.Storage.Streams.h", "winrt/Windows.UI.h",
]);
const forbiddenFragments = [
  "yt" + "_dlp", "yt" + "-dlp", "whis" + "per", "openai" + "_whisper", "py" + "pdf", "open" + "pyxl", "imageio" + "_ffmpeg", "torch", "play" + "wright", "sele" + "nium",
];

async function filesUnder(directory) {
  const output = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    if (entry.name === "__pycache__" || entry.name.endsWith(".pyc")) continue;
    const path = join(directory, entry.name);
    if (entry.isDirectory()) output.push(...await filesUnder(path));
    else output.push(path);
  }
  return output;
}

const skillDirectories = (await readdir(root, { withFileTypes: true })).filter((entry) => entry.isDirectory());
for (const entry of skillDirectories) {
  const skillRoot = join(root, entry.name);
  const originPath = join(skillRoot, "origin.json");
  const origin = JSON.parse(await readFile(originPath, "utf8"));
  if (origin.owner !== "Yunspire" || origin.policy !== "yunspire_first_party" || origin.implementation !== "independently_designed") {
    throw new Error(`${entry.name}: 第一方来源声明无效`);
  }
  if (origin.runtime_scope !== "background_system" || origin.ui_visibility !== "hidden") {
    throw new Error(`${entry.name}: 系统 Skill 必须仅在后台运行且禁止进入技能页面`);
  }
  if (!Array.isArray(origin.external_code) || origin.external_code.length !== 0) {
    throw new Error(`${entry.name}: 禁止包含外部代码来源`);
  }
  const scriptsDirectory = join(skillRoot, "scripts");
  const hasScripts = await stat(scriptsDirectory).then((value) => value.isDirectory()).catch(() => false);
  const actualScripts = hasScripts
    ? (await filesUnder(scriptsDirectory))
      .map((path) => relative(skillRoot, path).replaceAll("\\", "/"))
      .sort()
    : [];
  const declaredScripts = [...(origin.scripts || [])].sort();
  if (JSON.stringify(actualScripts) !== JSON.stringify(declaredScripts)) {
    throw new Error(`${entry.name}: origin.json 必须声明全部且仅声明真实脚本`);
  }
  const firstPartyPythonModules = new Set(actualScripts
    .filter((scriptPath) => extname(scriptPath) === ".py")
    .map((scriptPath) => basename(scriptPath, ".py")));
  for (const scriptPath of actualScripts) {
    const absolute = join(skillRoot, scriptPath);
    const source = await readFile(absolute, "utf8");
    const lower = source.toLowerCase();
    const forbidden = forbiddenFragments.find((fragment) => lower.includes(fragment));
    if (forbidden) throw new Error(`${entry.name}/${scriptPath}: 检测到禁用的第三方实现 ${forbidden}`);
    if (extname(scriptPath) === ".py") {
      const imports = [...source.matchAll(/^\s*(?:from|import)\s+([A-Za-z0-9_.]+)/gm)].map((match) => match[1].split(".")[0]);
      const external = imports.find((name) => !pythonStandardLibrary.has(name) && !firstPartyPythonModules.has(name));
      if (external) throw new Error(`${entry.name}/${scriptPath}: Python 导入不是标准库：${external}`);
    }
    if (extname(scriptPath) === ".m") {
      const imports = [...source.matchAll(/^\s*#import\s+<([A-Za-z0-9_]+)\//gm)].map((match) => match[1]);
      const external = imports.find((name) => !appleSystemFrameworks.has(name));
      if (external) throw new Error(`${entry.name}/${scriptPath}: Objective-C 导入不是 Apple 系统框架：${external}`);
    }
    if (extname(scriptPath) === ".mjs") {
      const imports = [...source.matchAll(/from\s+["']([^"']+)["']/g)].map((match) => match[1]);
      const external = imports.find((name) => !name.startsWith("node:"));
      if (external) throw new Error(`${entry.name}/${scriptPath}: Node 导入不是内置模块：${external}`);
    }
    if (extname(scriptPath) === ".cpp") {
      const imports = [...source.matchAll(/^\s*#include\s+<([^>]+)>/gm)].map((match) => match[1]);
      const external = imports.find((name) => !cppStandardHeaders.has(name) && !windowsSystemHeaders.has(name));
      if (external) throw new Error(`${entry.name}/${scriptPath}: C++ 导入不是标准库或 Windows SDK：${external}`);
    }
  }
}

const productionHtml = await readFile(join(desktopRoot, "desktop-ui", "index.html"), "utf8");
const skillView = productionHtml.match(/<section class="view" data-view="skills">([\s\S]*?)<section class="view" data-view="tasks">/)?.[1] || "";
if (!skillView || /<button class="skill-list-row\b/.test(skillView)) {
  throw new Error("技能页面禁止静态展示系统 Skill；列表只能由用户 Skill 数据动态生成");
}

console.log(`FIRST_PARTY_SKILLS_OK ${skillDirectories.length}`);
