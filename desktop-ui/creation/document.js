export const CREATION_DOCUMENT_SCHEMA_VERSION = '2.0';
export const DEFAULT_CREATION_FOLDER = '创作成品';

const DEFAULT_THEME_ID = 'ink';
const DEFAULT_THEME_VERSION = '1.0.0';
const BLOCK_KINDS = new Set(['heading', 'paragraph', 'list', 'quote', 'code', 'table', 'image', 'component', 'divider', 'html']);
const ASSET_KINDS = new Set(['image', 'video', 'audio', 'file', 'cover', 'infographic', 'gallery', 'longImage']);
const ASSET_STATES = new Set(['draft', 'local', 'localized', 'upload_required', 'ready', 'failed']);
const SOURCE_KINDS = new Set(['vaultNote', 'knowledgeRecord', 'url', 'file', 'userInput', 'generated']);
const SOURCE_TRUST = new Set(['direct', 'inferred', 'generated', 'unverified', 'conflicted']);
const CONTENT_TYPES = new Set(['article', 'wechat', 'xiaohongshu', 'contract', 'paper']);
const GROUNDING_STATUSES = new Set(['unverified', 'verified', 'stale', 'failed']);
const GROUNDING_VERDICTS = new Set(['supported', 'unsupported', 'uncertain']);

function isRecord(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

function cleanString(value, fallback = '', maximum = Number.MAX_SAFE_INTEGER) {
  const candidate = typeof value === 'string' || typeof value === 'number' ? String(value).trim() : '';
  return (candidate || fallback).slice(0, maximum);
}

function boundedInteger(value, fallback, minimum = 0, maximum = Number.MAX_SAFE_INTEGER) {
  const candidate = Number(value);
  if (!Number.isFinite(candidate)) return fallback;
  return Math.max(minimum, Math.min(maximum, Math.trunc(candidate)));
}

function boundedNumber(value, fallback, minimum, maximum) {
  const candidate = Number(value);
  if (!Number.isFinite(candidate)) return fallback;
  return Math.max(minimum, Math.min(maximum, candidate));
}

function uniqueStrings(value, maximum = Number.MAX_SAFE_INTEGER) {
  return [...new Set((Array.isArray(value) ? value : []).map((item) => cleanString(item)).filter(Boolean))].slice(0, maximum);
}

function validDateTime(value, fallback) {
  return typeof value === 'string' && Number.isFinite(Date.parse(value)) ? new Date(value).toISOString() : fallback;
}

function stableHash(value) {
  let hash = 0x811c9dc5;
  for (const character of String(value || '')) {
    hash ^= character.codePointAt(0);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(16).padStart(8, '0');
}

export function safeCreationId(value, prefix = 'creation') {
  const source = cleanString(value, prefix, 240);
  if (/^[A-Za-z0-9][A-Za-z0-9._-]{0,159}$/u.test(source)) return source;
  const slug = source.normalize('NFKD')
    .replace(/[^A-Za-z0-9._-]+/gu, '-')
    .replace(/^[^A-Za-z0-9]+|[^A-Za-z0-9]+$/gu, '')
    .slice(0, 120);
  return `${slug || prefix}-${stableHash(source)}`.slice(0, 160);
}

function componentId(value, fallback = null) {
  const candidate = cleanString(value).toLowerCase().replace(/[^a-z0-9-]+/gu, '-').replace(/^-+|-+$/gu, '');
  return /^[a-z]/u.test(candidate) ? candidate.slice(0, 80) : fallback;
}

function semver(value, fallback = DEFAULT_THEME_VERSION) {
  const candidate = cleanString(value);
  if (/^\d+\.\d+\.\d+$/u.test(candidate)) return candidate;
  if (/^\d+\.\d+$/u.test(candidate)) return `${candidate}.0`;
  if (/^\d+$/u.test(candidate)) return `${candidate}.0.0`;
  return fallback;
}

function scalarProperties(value, maximum = 100) {
  if (!isRecord(value)) return {};
  return Object.fromEntries(Object.entries(value).slice(0, maximum).map(([key, item]) => {
    if (item === null || typeof item === 'string' || typeof item === 'boolean' || (typeof item === 'number' && Number.isFinite(item))) return [key, item];
    return [key, JSON.stringify(item)];
  }));
}

function jsonSafe(value, depth = 0) {
  if (depth > 20 || value === undefined || typeof value === 'function' || typeof value === 'symbol') return null;
  if (value === null || typeof value === 'string' || typeof value === 'boolean') return value;
  if (typeof value === 'number') return Number.isFinite(value) ? value : null;
  if (Array.isArray(value)) return value.map((item) => jsonSafe(item, depth + 1));
  if (!isRecord(value)) return cleanString(value);
  return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, jsonSafe(item, depth + 1)]));
}

function parseScalar(value) {
  const source = String(value || '').trim();
  if (!source) return '';
  if (source === 'null' || source === '~') return null;
  if (source === 'true') return true;
  if (source === 'false') return false;
  if (/^-?(?:0|[1-9]\d*)(?:\.\d+)?$/u.test(source)) return Number(source);
  if ((source.startsWith('"') && source.endsWith('"')) || (source.startsWith('[') && source.endsWith(']')) || (source.startsWith('{') && source.endsWith('}'))) {
    try {
      return JSON.parse(source);
    } catch {
      // Keep hand-authored YAML intact when it is not JSON-compatible.
    }
  }
  if (source.startsWith("'") && source.endsWith("'")) return source.slice(1, -1).replace(/''/gu, "'");
  return source;
}

export function parseCreationFrontmatter(markdown) {
  const source = String(markdown || '').replace(/^\uFEFF/u, '');
  const match = source.match(/^---[ \t]*\r?\n([\s\S]*?)\r?\n---[ \t]*(?:\r?\n|$)/u);
  if (!match) return { attributes: {}, body: source, raw: '' };
  const attributes = {};
  let activeListKey = '';
  for (const line of match[1].split(/\r?\n/u)) {
    const listItem = line.match(/^\s+-\s+(.*)$/u);
    if (listItem && activeListKey) {
      if (!Array.isArray(attributes[activeListKey])) attributes[activeListKey] = [];
      attributes[activeListKey].push(parseScalar(listItem[1]));
      continue;
    }
    const field = line.match(/^([^:#][^:]*):(?:\s*(.*))?$/u);
    if (!field) {
      activeListKey = '';
      continue;
    }
    const key = field[1].trim();
    const rawValue = field[2] || '';
    attributes[key] = rawValue.trim() ? parseScalar(rawValue) : [];
    activeListKey = rawValue.trim() ? '' : key;
  }
  return { attributes, body: source.slice(match[0].length).replace(/^\r?\n/u, ''), raw: match[0] };
}

function yamlKey(key) {
  return /^[A-Za-z_][A-Za-z0-9_.-]*$/u.test(key) ? key : JSON.stringify(key);
}

function yamlValue(value) {
  if (value === null) return 'null';
  if (typeof value === 'boolean' || typeof value === 'number') return String(value);
  return JSON.stringify(value);
}

export function serializeCreationFrontmatter(attributes, { keyOrder = ['title', 'author', 'tags'] } = {}) {
  if (!isRecord(attributes)) return '';
  const keys = [...new Set([...keyOrder, ...Object.keys(attributes).sort()])]
    .filter((key) => attributes[key] !== undefined && attributes[key] !== '' && attributes[key] !== null);
  if (!keys.length) return '';
  return `---\n${keys.map((key) => `${yamlKey(key)}: ${yamlValue(jsonSafe(attributes[key]))}`).join('\n')}\n---`;
}

function markdownBlockDescriptor(markdown) {
  const source = String(markdown || '').trim();
  const heading = source.match(/^(#{1,6})\s+(.+)$/u);
  if (heading) return { kind: 'heading', level: heading[1].length, attributes: { text: heading[2].trim() } };
  if (/^```/u.test(source)) return { kind: 'code' };
  if (/^>\s*\[!abstract\]/iu.test(source)) return { kind: 'component', componentId: 'lead' };
  if (/^>\s*\[!note\]/iu.test(source)) return { kind: 'component', componentId: 'notice' };
  if (/^>\s*\[!tip\]/iu.test(source)) return { kind: 'component', componentId: 'cta' };
  if (/^>/u.test(source)) return { kind: 'quote' };
  if (/^(?:[-*+]\s+|\d+[.)]\s+)/u.test(source)) return { kind: 'list' };
  if (/^(?:---|\*\*\*|___)$/u.test(source)) return { kind: 'divider' };
  if (/^!\[(?:[^\]]*)\]\([^)]+\)|^!\[\[[^\]]+\]\]/u.test(source)) return { kind: 'image' };
  if (/^\|[^\n]+\|\r?\n\|?\s*:?-{3,}/u.test(source)) return { kind: 'table' };
  if (/^</u.test(source)) return { kind: 'html' };
  return { kind: 'paragraph' };
}

function indexedBlock(markdown, index, start, end, override = {}) {
  const descriptor = { ...markdownBlockDescriptor(markdown), ...override };
  const kind = BLOCK_KINDS.has(descriptor.kind) ? descriptor.kind : 'paragraph';
  return {
    id: safeCreationId(descriptor.id || `block-${index + 1}-${stableHash(`${start}:${markdown}`)}`, 'block'),
    kind,
    ...(kind === 'heading' ? { level: boundedInteger(descriptor.level, 1, 1, 6) } : {}),
    ...(kind === 'component' ? { componentId: componentId(descriptor.componentId, 'notice') } : {}),
    ...(descriptor.assetId ? { assetId: safeCreationId(descriptor.assetId, 'asset') } : {}),
    sourceRange: { start, end },
    children: uniqueStrings(descriptor.children),
    ...(isRecord(descriptor.attributes) && Object.keys(descriptor.attributes).length ? { attributes: scalarProperties(descriptor.attributes, 40) } : {}),
  };
}

export function deriveCreationBlocks(markdown) {
  const source = String(markdown || '');
  if (!source.trim()) return [];
  const records = [];
  for (let start = 0; start < source.length;) {
    const newline = source.indexOf('\n', start);
    const rawEnd = newline === -1 ? source.length : newline;
    const end = rawEnd > start && source[rawEnd - 1] === '\r' ? rawEnd - 1 : rawEnd;
    records.push({ text: source.slice(start, end), start, end });
    start = newline === -1 ? source.length : newline + 1;
  }
  const blocks = [];
  const pushRange = (startIndex, endIndex, override) => {
    const start = records[startIndex].start;
    const end = records[endIndex].end;
    const snippet = source.slice(start, end).trimEnd();
    if (snippet.trim()) blocks.push(indexedBlock(snippet, blocks.length, start, end, override));
  };
  let index = 0;
  while (index < records.length) {
    if (!records[index].text.trim()) {
      index += 1;
      continue;
    }
    const line = records[index].text;
    if (/^\s*```/u.test(line)) {
      let end = index + 1;
      while (end < records.length && !/^\s*```\s*$/u.test(records[end].text)) end += 1;
      pushRange(index, Math.min(end, records.length - 1), { kind: 'code' });
      index = Math.min(end + 1, records.length);
      continue;
    }
    if (/^#{1,6}\s+/u.test(line) || /^\s*(?:---|\*\*\*|___)\s*$/u.test(line) || /^!\[(?:[^\]]*)\]\([^)]+\)|^!\[\[[^\]]+\]\]/u.test(line)) {
      pushRange(index, index);
      index += 1;
      continue;
    }
    if (line.includes('|') && /^\|?\s*:?-{3,}/u.test(records[index + 1]?.text || '')) {
      let end = index + 2;
      while (end < records.length && records[end].text.trim() && records[end].text.includes('|')) end += 1;
      pushRange(index, end - 1, { kind: 'table' });
      index = end;
      continue;
    }
    if (/^\s*>/u.test(line)) {
      let end = index + 1;
      while (end < records.length && /^\s*>/u.test(records[end].text)) end += 1;
      pushRange(index, end - 1);
      index = end;
      continue;
    }
    if (/^\s*(?:[-*+]\s+|\d+[.)]\s+)/u.test(line)) {
      let end = index + 1;
      while (end < records.length && (/^\s*(?:[-*+]\s+|\d+[.)]\s+)/u.test(records[end].text) || /^\s{2,}\S/u.test(records[end].text))) end += 1;
      pushRange(index, end - 1, { kind: 'list' });
      index = end;
      continue;
    }
    let end = index + 1;
    while (end < records.length && records[end].text.trim()
      && !/^\s*(?:```|#{1,6}\s+|>|[-*+]\s+|\d+[.)]\s+|---\s*$|\*\*\*\s*$|___\s*$)/u.test(records[end].text)) end += 1;
    pushRange(index, end - 1);
    index = end;
  }
  return blocks;
}

function normalizeBlock(value, index, canonicalMarkdown) {
  const source = isRecord(value) ? value : {};
  const legacyType = componentId(source.componentType || source.componentId);
  const sourceKind = BLOCK_KINDS.has(source.kind) ? source.kind : (legacyType && !BLOCK_KINDS.has(legacyType) ? 'component' : (BLOCK_KINDS.has(legacyType) ? legacyType : 'paragraph'));
  const range = isRecord(source.sourceRange) ? source.sourceRange : (isRecord(source.typedFields?.sourceRange) ? source.typedFields.sourceRange : {});
  let start = boundedInteger(range.start, -1, -1, canonicalMarkdown.length);
  let end = boundedInteger(range.end, -1, -1, canonicalMarkdown.length);
  const legacyMarkdown = typeof source.markdown === 'string' ? source.markdown : '';
  if (start < 0 && legacyMarkdown) start = canonicalMarkdown.indexOf(legacyMarkdown);
  if (start < 0) start = 0;
  if (end < start) end = legacyMarkdown ? Math.min(canonicalMarkdown.length, start + legacyMarkdown.length) : start;
  return indexedBlock(canonicalMarkdown.slice(start, end) || legacyMarkdown, index, start, end, {
    id: source.id || source.stableId,
    kind: sourceKind,
    level: source.level ?? source.typedFields?.level,
    componentId: source.componentId || (sourceKind === 'component' ? legacyType : null),
    assetId: source.assetId,
    children: Array.isArray(source.children) ? source.children.map((item) => isRecord(item) ? item.id || item.stableId : item) : [],
    attributes: source.attributes || (isRecord(source.typedFields) ? Object.fromEntries(Object.entries(source.typedFields).filter(([key]) => key !== 'sourceRange')) : {}),
  });
}

function normalizeAsset(value, index, resolveSourceRefId = (item) => cleanString(item, '', 160) || null) {
  const source = isRecord(value) ? value : {};
  const metadata = isRecord(source.metadata) ? source.metadata : {};
  const mimeType = cleanString(source.mimeType, '', 120) || null;
  const inferredKind = mimeType?.startsWith('image/') ? 'image' : 'file';
  const rawState = source.state || metadata.state;
  const relativePath = cleanString(source.relativePath || (/^yunspire-draft:/u.test(source.source || '') ? '' : source.source), '', 2048) || null;
  return {
    id: safeCreationId(source.id || `asset-${index + 1}-${source.name || source.source || ''}`, 'asset'),
    kind: ASSET_KINDS.has(source.kind) ? source.kind : inferredKind,
    name: cleanString(source.name || metadata.name, `asset-${index + 1}`, 240),
    mimeType,
    relativePath,
    contentHash: /^sha256:[a-f0-9]{64}$/u.test(source.contentHash || metadata.contentHash || '') ? (source.contentHash || metadata.contentHash) : null,
    alt: cleanString(source.alt, '', 500) || null,
    caption: cleanString(source.caption, '', 1000) || null,
    sourceRefId: resolveSourceRefId(source.sourceRefId || metadata.sourceRefId),
    state: ASSET_STATES.has(rawState) ? rawState : (relativePath ? 'local' : 'draft'),
    width: source.width == null ? null : boundedInteger(source.width, null, 1),
    height: source.height == null ? null : boundedInteger(source.height, null, 1),
  };
}

function normalizeSourceRef(value, index) {
  const source = isRecord(value) ? value : {};
  const ref = cleanString(source.ref || source.sourceRef || source.url || source.relativePath, `generated:${index + 1}`, 4096);
  const normalizedPath = cleanString(source.relativePath, '', 2048).replace(/\\/gu, '/');
  const relativePath = normalizedPath && !normalizedPath.startsWith('/') && !normalizedPath.split('/').includes('..')
    ? normalizedPath
    : null;
  return {
    id: safeCreationId(source.id || `source-${ref}`, 'source'),
    kind: SOURCE_KINDS.has(source.kind) ? source.kind : 'generated',
    ref,
    vaultId: cleanString(source.vaultId, '', 160) || null,
    relativePath,
    title: cleanString(source.title, '', 240) || null,
    excerpt: cleanString(source.excerpt, '', 4000) || null,
    contentHash: /^sha256:[a-f0-9]{64}$/u.test(source.contentHash || '') ? source.contentHash : null,
    excerptHash: /^sha256:[a-f0-9]{64}$/u.test(source.excerptHash || '') ? source.excerptHash : null,
    capturedAt: validDateTime(source.capturedAt, null),
    trust: SOURCE_TRUST.has(source.trust) ? source.trust : 'unverified',
  };
}

function normalizeSourceRefs(values) {
  const inputs = Array.isArray(values) ? values : [];
  const sourceRefs = inputs.map(normalizeSourceRef);
  const aliases = new Map();
  inputs.forEach((value, index) => {
    const source = isRecord(value) ? value : {};
    const normalized = sourceRefs[index];
    const rawId = cleanString(source.id);
    const rawRef = cleanString(source.ref || source.sourceRef || source.url || source.relativePath);
    if (rawId) aliases.set(rawId, normalized.id);
    if (rawRef) aliases.set(rawRef, normalized.id);
    aliases.set(normalized.id, normalized.id);
  });
  const resolve = (value) => {
    const candidate = cleanString(value, '', 4096);
    if (!candidate) return null;
    return aliases.get(candidate) || safeCreationId(candidate, 'source');
  };
  return { sourceRefs, resolve };
}

function normalizeGroundingLedger(value, { resolveSourceRefId }) {
  const ledger = isRecord(value) ? value : {};
  const blocks = (Array.isArray(ledger.blocks) ? ledger.blocks : []).map((item, index) => {
    const source = isRecord(item) ? item : {};
    const rawEvidence = Array.isArray(source.evidence) ? source.evidence : [];
    const localAliases = new Map();
    const evidence = rawEvidence.map((entry) => {
      const citation = isRecord(entry) ? entry : {};
      const rawSourceId = cleanString(citation.sourceId || citation.sourceToken, '', 160);
      const sourceRefId = resolveSourceRefId(citation.sourceRefId || citation.sourceRef || rawSourceId);
      if (rawSourceId && sourceRefId) localAliases.set(rawSourceId, sourceRefId);
      return {
        sourceRefId,
        quote: cleanString(citation.quote, '', 2000).replace(/\s+/gu, ' ').trim(),
      };
    }).filter((entry) => entry.sourceRefId && entry.quote);
    const declared = source.sourceRefIds || source.citedSourceIds || source.sourceIds;
    const sourceRefIds = uniqueStrings([
      ...(Array.isArray(declared) ? declared.map((sourceId) => localAliases.get(cleanString(sourceId)) || resolveSourceRefId(sourceId)) : []),
      ...evidence.map((entry) => entry.sourceRefId),
    ]);
    return {
      id: safeCreationId(source.id || source.blockId || `grounding-block-${index + 1}`, 'block'),
      sourceRefIds,
      verdict: GROUNDING_VERDICTS.has(source.verdict) ? source.verdict : 'uncertain',
      evidence,
    };
  });
  return {
    status: GROUNDING_STATUSES.has(ledger.status) ? ledger.status : 'unverified',
    blocks,
    verifiedAt: validDateTime(ledger.verifiedAt, null),
    contentHash: /^sha256:[a-f0-9]{64}$/u.test(ledger.contentHash || '') ? ledger.contentHash : null,
    generationTraceId: cleanString(ledger.generationTraceId, '', 160) || null,
    verificationTraceId: cleanString(ledger.verificationTraceId, '', 160) || null,
  };
}

function normalizeLayout(value, legacy = {}) {
  const source = isRecord(value) ? value : {};
  const typography = isRecord(source.typography) ? source.typography : {};
  const features = isRecord(source.features) ? source.features : {};
  const lineHeightRaw = typography.lineHeight ?? legacy.lineHeight;
  const lineHeight = boundedNumber(lineHeightRaw, 1.8, 1, 30);
  const rawTokens = isRecord(source.tokens) ? source.tokens : {};
  return {
    themeId: componentId(source.themeId || legacy.theme, DEFAULT_THEME_ID),
    themeVersion: semver(source.themeVersion),
    target: ['wechatRichText', 'markdown', 'html', 'multiTarget'].includes(source.target) ? source.target : 'wechatRichText',
    typography: {
      fontFamily: cleanString(typography.fontFamily || legacy.font, 'sans-serif', 240),
      fontSize: boundedInteger(typography.fontSize ?? legacy.fontSize, 16, 10, 32),
      lineHeight: lineHeight > 3 ? lineHeight / 10 : lineHeight,
      headingScale: boundedNumber(typography.headingScale, 1.25, 0.5, 3),
    },
    tokens: Object.fromEntries(Object.entries(rawTokens).slice(0, 100).flatMap(([key, item]) => (
      typeof item === 'string' || typeof item === 'boolean' || (typeof item === 'number' && Number.isFinite(item)) ? [[key, item]] : []
    ))),
    features: {
      autoNumbering: features.autoNumbering === true || source.numbering?.enabled === true,
      keywordUnderline: features.keywordUnderline === true,
      tableOfContents: features.tableOfContents === true || source.toc?.enabled === true,
      introduction: features.introduction === true,
      signature: features.signature === true,
      cjkSpacing: features.cjkSpacing === true || source.cjkPolicy?.enabled === true,
      externalLinks: ['preserve', 'footnote', 'remove'].includes(features.externalLinks)
        ? features.externalLinks
        : (source.footnotePolicy?.externalLinksToFootnotes ? 'footnote' : 'preserve'),
    },
  };
}

function extractWikiLinks(markdown) {
  return [...String(markdown || '').matchAll(/\[\[([^\]|]+)(?:\|[^\]]+)?\]\]/gu)].map((match) => match[1].trim()).filter(Boolean);
}

function normalizeMetadata(value, frontmatter, canonicalMarkdown, source) {
  const metadata = isRecord(value) ? value : {};
  const structured = isRecord(metadata.properties) || Array.isArray(metadata.tags) || metadata.language || metadata.wikiLinks;
  const reserved = new Set(['title', 'tags', 'language', 'author', 'brandProfileId', 'wikiLinks']);
  const frontmatterProperties = Object.fromEntries(Object.entries(frontmatter).filter(([key]) => !reserved.has(key)));
  const legacyProperties = structured ? metadata.properties : metadata;
  const properties = scalarProperties({ ...frontmatterProperties, ...legacyProperties });
  const now = validDateTime(source.updatedAt || metadata.updatedAt, null);
  const createdAt = validDateTime(source.createdAt || metadata.createdAt, null);
  if (createdAt) properties.createdAt = createdAt;
  if (now) properties.updatedAt = now;
  return {
    language: cleanString(metadata.language || frontmatter.language, 'zh-CN', 20),
    tags: uniqueStrings(metadata.tags || frontmatter.tags, 200).slice(0, 200),
    properties,
    wikiLinks: uniqueStrings([...(Array.isArray(metadata.wikiLinks) ? metadata.wikiLinks : []), ...extractWikiLinks(canonicalMarkdown)]),
    brandProfileId: componentId(metadata.brandProfileId || frontmatter.brandProfileId),
    author: cleanString(metadata.author || frontmatter.author, '', 120) || null,
  };
}

function normalizePublishing(value, title) {
  const source = isRecord(value) ? value : {};
  let targets = uniqueStrings(source.targets, 6).filter((item) => ['obsidian', 'wechat', 'html', 'markdown', 'pdf', 'image'].includes(item));
  if (!targets.length) {
    const legacyTarget = cleanString(source.target);
    targets = legacyTarget.includes('wechat') ? ['obsidian', 'wechat'] : ['obsidian'];
  }
  return {
    targets,
    status: ['draft', 'preparing', 'readyForExport', 'exported', 'blocked'].includes(source.status) ? source.status : 'draft',
    titleCandidates: uniqueStrings(source.titleCandidates, 20).map((item) => item.slice(0, 240)),
    selectedTitle: cleanString(source.selectedTitle || title, '', 240) || null,
    coverAssetId: cleanString(source.coverAssetId, '', 160) || null,
    infographicAssetIds: uniqueStrings(source.infographicAssetIds),
    lastExportedAt: validDateTime(source.lastExportedAt, null),
  };
}

function normalizeProvenance(value, legacyDetected, resolveSourceRefId) {
  const source = isRecord(value) ? value : {};
  const legacyKind = cleanString(source.sourceKind);
  const createdBy = ['user', 'assistant', 'import', 'system'].includes(source.createdBy)
    ? source.createdBy
    : (legacyDetected || legacyKind.includes('legacy') ? 'import' : 'user');
  return {
    createdBy,
    canonicalAuthority: 'obsidianMarkdown',
    sourceIds: uniqueStrings(source.sourceIds).map(resolveSourceRefId).filter(Boolean),
    derivation: ['original', 'modelCandidate', 'imported', 'revised'].includes(source.derivation)
      ? source.derivation
      : (legacyDetected ? 'imported' : 'original'),
    modelRunIds: uniqueStrings(source.modelRunIds),
  };
}

function normalizeValidationReceipt(value, now, hasBody) {
  const source = isRecord(value) ? value : {};
  return {
    schemaValid: source.schemaValid !== false,
    astValid: source.astValid ?? hasBody,
    htmlValid: source.htmlValid === true,
    issues: (Array.isArray(source.issues) ? source.issues : []).map((issue) => ({
      code: cleanString(issue?.code, 'validation.issue', 100).toLowerCase().replace(/[^a-z0-9._-]+/gu, '-').replace(/^[^a-z]+/u, 'validation.'),
      severity: ['info', 'warning', 'error'].includes(issue?.severity) ? issue.severity : 'warning',
      message: cleanString(issue?.message, '文档需要复核。', 1000),
      blockId: cleanString(issue?.blockId, '', 160) || null,
    })),
    validatedAt: validDateTime(source.validatedAt, now),
    validatorVersion: cleanString(source.validatorVersion, '0.4.1', 80),
    contentHash: /^sha256:[a-f0-9]{64}$/u.test(source.contentHash || '') ? source.contentHash : null,
  };
}

function attachCompatibilityAliases(document) {
  Object.defineProperties(document, {
    markdown: { enumerable: false, configurable: true, get: () => document.canonicalMarkdown },
    theme: { enumerable: false, configurable: true, get: () => document.layout.themeId },
    components: { enumerable: false, configurable: true, get: () => document.blocks.filter((block) => block.kind === 'component') },
    version: { enumerable: false, configurable: true, get: () => 2 },
    createdAt: { enumerable: false, configurable: true, get: () => document.metadata.properties.createdAt || '' },
    updatedAt: { enumerable: false, configurable: true, get: () => document.metadata.properties.updatedAt || '' },
  });
  return document;
}

export function normalizeCreationDocument(value = {}, options = {}) {
  const source = typeof value === 'string' ? { canonicalMarkdown: value } : (isRecord(value) ? value : {});
  const blockMarkdownFallback = Array.isArray(source.blocks) ? source.blocks.map((block) => block?.markdown).filter((item) => typeof item === 'string').join('\n\n') : '';
  const rawMarkdown = String(source.canonicalMarkdown ?? source.markdown ?? source.content ?? blockMarkdownFallback);
  const parsed = parseCreationFrontmatter(rawMarkdown);
  const canonicalMarkdown = parsed.body.replace(/\s+$/u, '');
  const sourceMetadata = isRecord(source.metadata) ? source.metadata : {};
  const inferredTitle = canonicalMarkdown.match(/^#\s+(.+)$/mu)?.[1]?.trim();
  const title = cleanString(source.title || parsed.attributes.title || sourceMetadata.title || inferredTitle, '未命名文档', 240);
  const now = validDateTime(options.now, new Date().toISOString());
  const legacyDetected = source.schemaVersion !== CREATION_DOCUMENT_SCHEMA_VERSION || source.version !== undefined || source.html !== undefined;
  const blocks = Array.isArray(source.blocks) && source.blocks.length
    ? source.blocks.map((block, index) => normalizeBlock(block, index, canonicalMarkdown))
    : deriveCreationBlocks(canonicalMarkdown);
  const { sourceRefs, resolve: resolveSourceRefId } = normalizeSourceRefs(source.sourceRefs);
  const groundingLedger = normalizeGroundingLedger(source.groundingLedger || sourceMetadata.groundingLedger, { resolveSourceRefId });
  const metadataContentType = sourceMetadata.properties?.contentType || sourceMetadata.contentType;
  const contentType = CONTENT_TYPES.has(source.contentType)
    ? source.contentType
    : (CONTENT_TYPES.has(metadataContentType) ? metadataContentType : 'article');
  const document = {
    schemaVersion: CREATION_DOCUMENT_SCHEMA_VERSION,
    id: safeCreationId(source.id || sourceMetadata.id || `creation-${title}`),
    revision: boundedInteger(source.revision, 1, 1),
    title,
    canonicalFormat: 'markdown',
    contentType,
    canonicalMarkdown,
    blocks,
    assets: (Array.isArray(source.assets) ? source.assets : (Array.isArray(source.attachments) ? source.attachments : [])).map((asset, index) => normalizeAsset(asset, index, resolveSourceRefId)),
    sourceRefs,
    groundingLedger,
    layout: normalizeLayout(source.layout, source.creationStudio || source.studioState || source),
    metadata: normalizeMetadata(sourceMetadata, parsed.attributes, canonicalMarkdown, source),
    publishing: normalizePublishing(source.publishing, title),
    provenance: normalizeProvenance(source.provenance, legacyDetected, resolveSourceRefId),
    validationReceipt: normalizeValidationReceipt(source.validationReceipt, now, Boolean(canonicalMarkdown.trim())),
    readiness: isRecord(source.readiness) ? jsonSafe(source.readiness) : null,
  };
  return options.compatibilityAliases === false ? document : attachCompatibilityAliases(document);
}

export function creationStudioStateFromDocument(value, fallback = {}) {
  const document = normalizeCreationDocument(value, { compatibilityAliases: false });
  const properties = document.metadata.properties || {};
  const fontFamily = String(document.layout.typography.fontFamily || '').toLowerCase();
  const font = /kaiti|stkaiti|kaiti sc|楷/u.test(fontFamily)
    ? 'kaiti'
    : /serif|songti|stsong|宋/u.test(fontFamily)
      ? 'serif'
      : 'sans';
  const optionalProperty = (key) => properties[key] == null || properties[key] === '' ? fallback[key] : properties[key];
  return {
    ...fallback,
    theme: document.layout.themeId,
    font,
    fontSize: document.layout.typography.fontSize,
    lineHeight: Math.round(document.layout.typography.lineHeight * 10),
    contentType: properties.requestedContentType || document.contentType || optionalProperty('contentType'),
    writingPatternId: optionalProperty('writingPatternId'),
    writingVoiceId: optionalProperty('writingVoiceId'),
    purposePresetId: optionalProperty('purposePresetId'),
    fixPunctuation: optionalProperty('fixPunctuation'),
    rewriteMode: optionalProperty('rewriteMode'),
    rewriteStrength: optionalProperty('rewriteStrength'),
    rewriteSpoken: optionalProperty('rewriteSpoken'),
    rewriteRhythm: optionalProperty('rewriteRhythm'),
    rewriteScope: optionalProperty('rewriteScope'),
  };
}

export function isCreationDocumentV2(value) {
  return isRecord(value)
    && value.schemaVersion === CREATION_DOCUMENT_SCHEMA_VERSION
    && value.canonicalFormat === 'markdown'
    && CONTENT_TYPES.has(value.contentType)
    && typeof value.canonicalMarkdown === 'string'
    && Array.isArray(value.blocks)
    && Array.isArray(value.assets)
    && Array.isArray(value.sourceRefs)
    && isRecord(value.layout)
    && isRecord(value.metadata)
    && isRecord(value.publishing)
    && isRecord(value.provenance)
    && isRecord(value.validationReceipt)
    && isRecord(value.groundingLedger);
}

export function validateCreationGroundingRelations(value) {
  const document = normalizeCreationDocument(value, { compatibilityAliases: false });
  const sourceRefIds = new Set(document.sourceRefs.map((source) => source.id));
  const blockIds = new Set(document.blocks.map((block) => block.id));
  const issues = [];
  for (const sourceRefId of document.provenance.sourceIds) {
    if (!sourceRefIds.has(sourceRefId)) issues.push({ code: 'provenance.missing-source', sourceRefId });
  }
  if (!document.groundingLedger) return issues;
  for (const block of document.groundingLedger.blocks) {
    if (!blockIds.has(block.id)) issues.push({ code: 'grounding.missing-block', blockId: block.id });
    for (const sourceRefId of block.sourceRefIds) {
      if (!sourceRefIds.has(sourceRefId)) issues.push({ code: 'grounding.missing-source', blockId: block.id, sourceRefId });
    }
    for (const evidence of block.evidence) {
      if (!sourceRefIds.has(evidence.sourceRefId)) issues.push({ code: 'grounding.missing-evidence-source', blockId: block.id, sourceRefId: evidence.sourceRefId });
      if (!block.sourceRefIds.includes(evidence.sourceRefId)) issues.push({ code: 'grounding.undeclared-evidence-source', blockId: block.id, sourceRefId: evidence.sourceRefId });
    }
    if (block.verdict === 'supported' && (!block.sourceRefIds.length || !block.evidence.length)) {
      issues.push({ code: 'grounding.support-without-evidence', blockId: block.id });
    }
  }
  return issues;
}

export function serializeCreationDocumentMarkdown(value, options = {}) {
  const document = normalizeCreationDocument(value, { ...options, compatibilityAliases: false });
  if (options.frontmatter === false) return document.canonicalMarkdown;
  const excluded = new Set(options.excludeMetadata || ['createdAt', 'updatedAt']);
  const attributes = Object.fromEntries(Object.entries(document.metadata.properties).filter(([key, item]) => !excluded.has(key) && item !== undefined));
  attributes.title = document.title;
  if (document.metadata.author) attributes.author = document.metadata.author;
  if (document.metadata.tags.length) attributes.tags = document.metadata.tags;
  attributes.language = document.metadata.language;
  attributes.yunspire_creation_id = document.id;
  attributes.yunspire_creation_revision = document.revision;
  attributes.yunspire_theme = document.layout.themeId;
  const frontmatter = serializeCreationFrontmatter(attributes, options);
  return [frontmatter, document.canonicalMarkdown].filter(Boolean).join('\n\n');
}

export function creationDocumentToJSON(value, spacing = 2) {
  return JSON.stringify(normalizeCreationDocument(value, { compatibilityAliases: false }), null, spacing);
}

export function updateCreationDocument(value, changes = {}, options = {}) {
  const current = normalizeCreationDocument(value, { compatibilityAliases: false });
  const now = validDateTime(options.now, new Date().toISOString());
  const changeSource = isRecord(changes) ? changes : {};
  const metadataChanges = isRecord(changeSource.metadata) ? changeSource.metadata : {};
  return normalizeCreationDocument({
    ...current,
    ...jsonSafe(changeSource),
    id: current.id,
    revision: current.revision + 1,
    metadata: {
      ...current.metadata,
      ...metadataChanges,
      properties: {
        ...current.metadata.properties,
        ...(isRecord(metadataChanges.properties) ? metadataChanges.properties : {}),
        createdAt: current.metadata.properties.createdAt || now,
        updatedAt: now,
      },
    },
    provenance: {
      ...current.provenance,
      ...(isRecord(changeSource.provenance) ? changeSource.provenance : {}),
      derivation: 'revised',
    },
  }, { ...options, now });
}
