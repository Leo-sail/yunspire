const FIRST_PARTY_SOURCE = Object.freeze({
  policy: 'yunspire_first_party',
  authoredBy: 'Yunspire',
  repository: 'https://github.com/Leo-sail/yunspire',
  upstreamCodeCopied: false,
  researchBoundary: '云枢原创语义组件；外部项目仅用于能力研究，不复制代码、模板、素材或提示词。',
});

const FIRST_PARTY_LICENSE = Object.freeze({
  scope: 'yunspire_first_party_project_asset',
  notice: '本组件清单和渲染标识由云枢项目原创维护。',
  thirdPartyAssets: [],
});

const COMPONENT_DEFINITIONS = Object.freeze([
  { id: 'lead', name: '导读', description: '交代内容解决的问题和读者收益。', category: 'structure', blockKind: 'container', role: 'note', fallback: 'callout', template: '> [!abstract] 导读\n> 用一两句话说明这篇内容解决什么问题，以及读者能获得什么。' },
  { id: 'quote', name: '金句', description: '突出一个核心判断。', category: 'emphasis', blockKind: 'leaf', role: 'blockquote', fallback: 'blockquote', template: '> **把最重要的判断写在这里。**' },
  { id: 'notice', name: '提示', description: '补充前提、风险或注意事项。', category: 'information', blockKind: 'container', role: 'note', fallback: 'callout', template: '> [!note] 提示\n> 补充阅读前提、限制条件或需要特别注意的风险。' },
  { id: 'steps', name: '步骤', description: '呈现有顺序的操作流程。', category: 'sequence', blockKind: 'collection', role: 'list', fallback: 'list', template: '### 落地步骤\n\n1. 先完成第一步\n2. 再处理关键环节\n3. 最后检查结果' },
  { id: 'metrics', name: '数据', description: '强调关键数字与说明。', category: 'information', blockKind: 'collection', role: 'group', fallback: 'table', template: '| 指标 | 数值 |\n| --- | --- |\n| 关键指标 | 42% |\n| 核心变化 | 3 项 |\n| 观察周期 | 7 天 |' },
  { id: 'compare', name: '对比', description: '并列两种方案或状态。', category: 'comparison', blockKind: 'collection', role: 'group', fallback: 'table', template: '| 方案 A | 方案 B |\n| --- | --- |\n| 写清优势、成本和适用条件。 | 写清差异、风险和取舍。 |' },
  { id: 'dialogue', name: '对话', description: '用问答推进内容。', category: 'conversation', blockKind: 'collection', role: 'dialog', fallback: 'blockquote', template: '> **问：**读者最关心的问题是什么？\n>\n> **答：**用直接、具体的话回答。' },
  { id: 'timeline', name: '时间线', description: '呈现阶段和里程碑。', category: 'sequence', blockKind: 'collection', role: 'timeline', fallback: 'list', template: '### 时间线\n\n1. 阶段一：问题出现\n2. 阶段二：关键转折\n3. 阶段三：当前结果' },
  { id: 'divider', name: '分隔', description: '切换章节或叙事节奏。', category: 'navigation', blockKind: 'divider', role: 'separator', fallback: 'thematicBreak', template: '---' },
  { id: 'cta', name: '行动提示', description: '给出清晰、单一的下一步。', category: 'conversion', blockKind: 'container', role: 'callToAction', fallback: 'callout', template: '> [!tip] 下一步\n> 告诉读者现在可以做什么，保持单一、清晰、可执行。' },
]);

function isRecord(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

function stringValue(value, fallback = '', maximum = 2000) {
  const candidate = typeof value === 'string' || typeof value === 'number' ? String(value).trim() : '';
  return (candidate || fallback).slice(0, maximum);
}

function markdownValue(value, fallback = '') {
  if (typeof value === 'string') return value || fallback;
  if (typeof value === 'number') return String(value);
  return fallback;
}

function identifier(value, fallback) {
  const candidate = stringValue(value).toLowerCase().replace(/[^a-z0-9-]+/gu, '-').replace(/^-+|-+$/gu, '');
  return /^[a-z]/u.test(candidate) ? candidate.slice(0, 80) : fallback;
}

function uniqueStrings(value, maximum = 100) {
  return [...new Set((Array.isArray(value) ? value : []).map((item) => stringValue(item)).filter(Boolean))].slice(0, maximum);
}

function integer(value, fallback, minimum, maximum) {
  const candidate = Number(value);
  return Number.isFinite(candidate) ? Math.max(minimum, Math.min(maximum, Math.round(candidate))) : fallback;
}

function stableHash(value) {
  let hash = 0x811c9dc5;
  for (const character of String(value || '')) {
    hash ^= character.codePointAt(0);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(16).padStart(8, '0');
}

function normalizeSlot(slot, index) {
  const source = isRecord(slot) ? slot : {};
  const rawId = stringValue(source.id, `field${index + 1}`, 60).replace(/[^a-zA-Z0-9]/gu, '');
  const id = /^[a-z]/u.test(rawId) ? rawId : `field${index + 1}`;
  return {
    id,
    kind: ['text', 'richText', 'number', 'list', 'image', 'link', 'component'].includes(source.kind) ? source.kind : 'text',
    required: source.required === true,
    maxLength: source.maxLength == null ? null : integer(source.maxLength, null, 1, 100000),
  };
}

function attachComponentAliases(component, source, definition) {
  const templateSource = source.templateMarkdown ?? source.template ?? definition?.template;
  const templateMarkdown = () => markdownValue(templateSource);
  Object.defineProperties(component, {
    name: { enumerable: false, configurable: true, get: () => component.displayName },
    aliases: { enumerable: false, configurable: true, get: () => component.legacyIds },
    tags: { enumerable: false, configurable: true, get: () => uniqueStrings(source.tags, 30) },
    allowedChildren: {
      enumerable: false,
      configurable: true,
      get: () => component.slots.filter((slot) => slot.kind === 'component').map((slot) => slot.id),
    },
    fields: { enumerable: false, configurable: true, get: () => component.slots.map((slot) => slot.id) },
    templateMarkdown: { enumerable: false, configurable: true, get: templateMarkdown },
    template: { enumerable: false, configurable: true, get: templateMarkdown },
  });
  return component;
}

export function normalizeComponentManifest(value, index = 0) {
  const source = isRecord(value) ? value : {};
  const id = identifier(source.id, `component-${index + 1}`);
  const definition = COMPONENT_DEFINITIONS.find((item) => item.id === id);
  const slots = (Array.isArray(source.slots) ? source.slots : (Array.isArray(source.fields) ? source.fields.map((field) => ({ id: field })) : []))
    .slice(0, 30)
    .map(normalizeSlot);
  const category = source.category || definition?.category;
  const role = source.semantics?.role || definition?.role;
  const fallback = source.semantics?.markdownFallback || definition?.fallback;
  const manifest = {
    schemaVersion: '1.0',
    manifestType: 'component',
    catalogVersion: stringValue(source.catalogVersion, '0.3.0', 40),
    id,
    version: stringValue(source.version, '1.0.0', 40),
    displayName: stringValue(source.displayName || source.name || definition?.name, id, 80),
    description: stringValue(source.description || definition?.description, '云枢原创内容组件。', 500),
    status: ['active', 'experimental', 'deprecated'].includes(source.status) ? source.status : 'active',
    category: ['structure', 'emphasis', 'information', 'comparison', 'sequence', 'conversation', 'navigation', 'media', 'conversion'].includes(category) ? category : 'information',
    legacyIds: uniqueStrings(source.legacyIds || source.aliases, 20).map((item) => identifier(item, '')).filter(Boolean),
    blockKind: ['container', 'leaf', 'divider', 'media', 'collection'].includes(source.blockKind || definition?.blockKind) ? (source.blockKind || definition.blockKind) : 'container',
    slots,
    semantics: {
      role: ['note', 'blockquote', 'list', 'group', 'separator', 'dialog', 'timeline', 'callToAction', 'figure'].includes(role) ? role : 'group',
      ariaLabel: stringValue(source.semantics?.ariaLabel, source.displayName || source.name || definition?.name || id, 120),
      markdownFallback: ['callout', 'blockquote', 'list', 'paragraphs', 'thematicBreak', 'table', 'figure'].includes(fallback) ? fallback : 'paragraphs',
    },
    constraints: {
      minItems: integer(source.constraints?.minItems, definition?.blockKind === 'divider' ? 0 : 1, 0, 1000),
      maxItems: integer(source.constraints?.maxItems, 1, 1, 1000),
      allowNestedComponents: source.constraints?.allowNestedComponents === true,
      spanLeaf: source.constraints?.spanLeaf !== false,
      allowScripts: false,
      allowExternalStyles: false,
    },
    renderers: {
      markdown: stringValue(source.renderers?.markdown, `creation.component.${id}.markdown.v1`, 120),
      html: stringValue(source.renderers?.html, `creation.component.${id}.html.v1`, 120),
      wechatRichText: stringValue(source.renderers?.wechatRichText, `creation.component.${id}.wechat.v1`, 120),
    },
    compatibility: {
      targets: uniqueStrings(source.compatibility?.targets, 3).filter((item) => ['markdown', 'html', 'wechatRichText'].includes(item)).length
        ? uniqueStrings(source.compatibility.targets, 3).filter((item) => ['markdown', 'html', 'wechatRichText'].includes(item))
        : ['markdown', 'html', 'wechatRichText'],
      minRuntimeVersion: stringValue(source.compatibility?.minRuntimeVersion, '0.3.0', 40),
    },
    source: { ...FIRST_PARTY_SOURCE, ...(isRecord(source.source) ? source.source : {}) },
    license: { ...FIRST_PARTY_LICENSE, ...(isRecord(source.license) ? source.license : {}) },
  };
  return attachComponentAliases(manifest, source, definition);
}

export const legacyComponentManifests = Object.freeze(COMPONENT_DEFINITIONS.map((definition) => normalizeComponentManifest({
  ...definition,
  displayName: definition.name,
  constraints: { spanLeaf: true },
})));

export function normalizeComponentCatalog(value, { includeLegacyFallback = true } = {}) {
  const source = Array.isArray(value) ? value : (Array.isArray(value?.components) ? value.components : (Array.isArray(value?.items) ? value.items : []));
  const manifests = source.map(normalizeComponentManifest);
  const merged = manifests.length || !includeLegacyFallback ? manifests : [...legacyComponentManifests];
  const byId = new Map();
  for (const component of merged) if (!byId.has(component.id)) byId.set(component.id, component);
  return [...byId.values()];
}

async function resolveComponentSource(source, options) {
  let resolved = typeof source === 'function' ? await source() : await source;
  if (typeof resolved === 'string') {
    const fetchImplementation = options.fetch || globalThis.fetch;
    if (typeof fetchImplementation !== 'function') throw new Error('Component catalog URL requires a fetch implementation');
    const response = await fetchImplementation(resolved);
    if (!response?.ok) throw new Error(`Unable to load component catalog (${response?.status || 'network error'})`);
    resolved = await response.json();
  } else if (resolved && typeof resolved.json === 'function') {
    resolved = await resolved.json();
  }
  return resolved;
}

export async function loadComponentManifests(source, options = {}) {
  const resolved = await resolveComponentSource(source, options);
  if (Array.isArray(resolved) && resolved.every((item) => typeof item === 'string')) {
    const catalogs = await Promise.all(resolved.map((item) => resolveComponentSource(item, options)));
    return normalizeComponentCatalog(catalogs.flatMap((catalog) => Array.isArray(catalog) ? catalog : (catalog?.components || [catalog])), options);
  }
  return normalizeComponentCatalog(resolved, options);
}

export function resolveComponentManifest(catalog, id) {
  const components = normalizeComponentCatalog(catalog);
  const candidate = stringValue(id).toLowerCase();
  return components.find((component) => component.id === candidate || component.legacyIds.includes(candidate)) || null;
}

export function createComponentBrowserViewModel(catalog, options = {}) {
  const selectedId = stringValue(options.selectedId);
  const query = stringValue(options.query).toLocaleLowerCase('zh-CN');
  const category = stringValue(options.category);
  const components = normalizeComponentCatalog(catalog).filter((component) => {
    if (options.includeDeprecated !== true && component.status === 'deprecated') return false;
    if (category && category !== 'all' && component.category !== category) return false;
    if (!query) return true;
    return [component.id, component.displayName, component.description, component.category, ...component.tags]
      .join(' ')
      .toLocaleLowerCase('zh-CN')
      .includes(query);
  }).map((component) => ({
    manifest: component,
    id: component.id,
    name: component.displayName,
    description: component.description,
    category: component.category,
    selected: component.id === selectedId || component.legacyIds.includes(selectedId),
    disabled: component.status === 'deprecated',
    spanLeaf: component.constraints.spanLeaf,
    fieldCount: component.slots.length,
  }));
  return {
    total: components.length,
    selected: components.find((component) => component.selected) || null,
    categories: [...new Set(components.map((component) => component.category))],
    groups: Object.fromEntries([...new Set(components.map((component) => component.category))]
      .map((group) => [group, components.filter((component) => component.category === group)])),
    all: components,
    empty: components.length === 0,
  };
}

export function createComponentInsertion(catalog, id, fields = {}, options = {}) {
  const manifest = resolveComponentManifest(catalog, id) || normalizeComponentManifest({ id });
  const typedFields = Object.fromEntries(manifest.slots.map((slot) => [slot.id, fields[slot.id] ?? null]));
  Object.assign(typedFields, isRecord(fields) ? fields : {});
  const markdown = markdownValue(options.markdown ?? manifest.template, `<!-- yunspire-component:${manifest.id} -->`);
  const seed = `${manifest.id}:${markdown}:${options.index || 0}`;
  const start = integer(options.start, 0, 0, Number.MAX_SAFE_INTEGER);
  const block = {
    id: stringValue(options.id || options.stableId, `block-${manifest.id}-${stableHash(seed)}`, 160),
    kind: 'component',
    componentId: manifest.id,
    sourceRange: { start, end: start + markdown.length },
    children: [],
    attributes: Object.fromEntries(Object.entries({ componentVersion: manifest.version, ...typedFields }).map(([key, value]) => [
      key,
      value === null || ['string', 'number', 'boolean'].includes(typeof value) ? value : JSON.stringify(value),
    ])),
  };
  Object.defineProperties(block, {
    markdown: { enumerable: false, configurable: true, value: markdown },
    stableId: { enumerable: false, configurable: true, get: () => block.id },
    componentType: { enumerable: false, configurable: true, get: () => block.componentId },
    componentVersion: { enumerable: false, configurable: true, get: () => manifest.version },
    typedFields: { enumerable: false, configurable: true, get: () => typedFields },
  });
  return block;
}

export function toRuntimeComponentManifest(value) {
  const normalized = normalizeComponentManifest(value);
  return {
    ...JSON.parse(JSON.stringify(normalized)),
    templateMarkdown: normalized.templateMarkdown,
  };
}
