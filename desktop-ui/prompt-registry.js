const PROMPT_GLOB_PREFIX = '../prompts/';
const PROMPT_PLACEHOLDER = /\{\{([A-Z][A-Z0-9_]*)\}\}/gu;
const PROMPT_VALUE_NAME = /^[A-Z][A-Z0-9_]*$/u;

function promptIdForPath(path) {
  return String(path || '')
    .replace(/^\.\.\/prompts\//u, '')
    .replace(/\.md$/u, '')
    .replace(/\.template$/u, '')
    .replaceAll('/', '.');
}

function normalizedPrompt(value) {
  return String(value || '').replace(/\r\n?/gu, '\n').trim();
}

let bundledPrompts = {};
try {
  bundledPrompts = import.meta.glob('../prompts/**/*.md', {
    query: '?raw',
    import: 'default',
    eager: true,
  });
} catch {
  // Plain Node validation does not implement Vite's import.meta.glob transform.
}

const promptRegistry = new Map();
Object.entries(bundledPrompts).forEach(([path, value]) => {
  promptRegistry.set(promptIdForPath(path), normalizedPrompt(value));
});

function loadNodePrompts() {
  if (promptRegistry.size) return;
  const nodeProcess = globalThis.process;
  const fileSystem = typeof nodeProcess?.getBuiltinModule === 'function'
    ? nodeProcess.getBuiltinModule('fs')
    : null;
  if (!fileSystem) return;

  const walk = (directory, relativeDirectory = '') => {
    fileSystem.readdirSync(directory, { withFileTypes: true }).forEach((entry) => {
      const relativePath = `${relativeDirectory}${entry.name}`;
      const url = new URL(entry.isDirectory() ? `${entry.name}/` : entry.name, directory);
      if (entry.isDirectory()) {
        walk(url, `${relativePath}/`);
      } else if (entry.isFile() && entry.name.endsWith('.md')) {
        promptRegistry.set(
          promptIdForPath(`${PROMPT_GLOB_PREFIX}${relativePath}`),
          normalizedPrompt(fileSystem.readFileSync(url, 'utf8')),
        );
      }
    });
  };

  walk(new URL(PROMPT_GLOB_PREFIX, import.meta.url));
}

loadNodePrompts();

export function promptText(id) {
  const key = String(id || '').trim();
  const prompt = promptRegistry.get(key);
  if (!prompt) throw new Error(`Prompt“${key || 'unknown'}”未注册或文件为空`);
  return prompt;
}

export function renderPrompt(id, values = {}) {
  const template = promptText(id);
  if (!values || typeof values !== 'object' || Array.isArray(values)) {
    throw new Error(`Prompt“${id}”的占位符值必须是对象`);
  }
  const required = new Set([...template.matchAll(PROMPT_PLACEHOLDER)].map((match) => match[1]));
  const missing = [...required].filter((name) => !Object.hasOwn(values, name));
  if (missing.length) throw new Error(`Prompt“${id}”缺少占位符：${missing.join('、')}`);
  const supplied = Object.keys(values);
  const invalid = supplied.filter((name) => !PROMPT_VALUE_NAME.test(name));
  if (invalid.length) throw new Error(`Prompt“${id}”包含无效占位符名称：${invalid.join('、')}`);
  const extra = supplied.filter((name) => !required.has(name));
  if (extra.length) throw new Error(`Prompt“${id}”包含未声明的占位符：${extra.join('、')}`);
  return template.replace(PROMPT_PLACEHOLDER, (_, name) => String(values[name] ?? '')).trim();
}

export function registeredPromptIds() {
  return [...promptRegistry.keys()].sort();
}
