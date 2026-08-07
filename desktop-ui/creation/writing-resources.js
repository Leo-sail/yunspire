const NORMALIZED_CATALOG = Symbol('yunspire.normalizedWritingResources');

export const WRITING_RESOURCE_COUNTS = Object.freeze({
  writingPatterns: 53,
  voices: 5,
  purposePresets: 9,
});

export const WRITING_CONTENT_TYPES = Object.freeze([
  'all',
  'article',
  'wechat',
  'xiaohongshu',
  'contract',
  'paper',
  'report',
  'tutorial',
  'memo',
  'speech',
  'brand',
]);

export const DEFAULT_WRITING_RESOURCES_URL = new URL(
  '../../resources/creation/catalog/writing-resources.json',
  import.meta.url,
).toString();

const CONTENT_TYPES = new Set(WRITING_CONTENT_TYPES);
const ID_PATTERN = /^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$/u;
const NUMBERED_PLACEHOLDER = /^(?:(?:writing\s*)?pattern|mode|voice|purpose|preset|模式|写作模式|语气|目的|预设)[\s_-]*0*\d+$/iu;

function isRecord(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

function fail(path, message) {
  throw new TypeError(`Invalid writing resources at ${path}: ${message}`);
}

function requiredString(value, path, { minimum = 1, maximum = 8_000 } = {}) {
  if (typeof value !== 'string') fail(path, 'expected a string');
  const result = value.trim();
  if (result.length < minimum) fail(path, `must contain at least ${minimum} characters`);
  if (result.length > maximum) fail(path, `must contain at most ${maximum} characters`);
  return result;
}

function resourceId(value, path) {
  const id = requiredString(value, path, { maximum: 80 }).toLowerCase();
  if (!ID_PATTERN.test(id)) fail(path, 'must be a lowercase kebab-case identifier');
  return id;
}

function resourceName(value, path) {
  const name = requiredString(value, path, { minimum: 2, maximum: 80 });
  if (NUMBERED_PLACEHOLDER.test(name)) fail(path, 'numbered placeholder names are not allowed');
  return name;
}

function stringList(value, path, { minimum = 1, maximum = 30, allowed = null } = {}) {
  if (!Array.isArray(value)) fail(path, 'expected an array');
  const result = value.map((item, index) => requiredString(item, `${path}[${index}]`, { maximum: 160 }));
  if (result.length < minimum) fail(path, `must contain at least ${minimum} item(s)`);
  if (result.length > maximum) fail(path, `must contain at most ${maximum} item(s)`);
  if (new Set(result).size !== result.length) fail(path, 'must not contain duplicate items');
  if (allowed) {
    const unknown = result.find((item) => !allowed.has(item));
    if (unknown) fail(path, `contains unsupported value "${unknown}"`);
  }
  return result;
}

function normalizePattern(value, index) {
  const path = `writingPatterns[${index}]`;
  if (!isRecord(value)) fail(path, 'expected an object');
  return {
    id: resourceId(value.id, `${path}.id`),
    name: resourceName(value.name, `${path}.name`),
    category: requiredString(value.category, `${path}.category`, { minimum: 2, maximum: 80 }),
    description: requiredString(value.description, `${path}.description`, { minimum: 8, maximum: 500 }),
    instruction: requiredString(value.instruction, `${path}.instruction`, { minimum: 24, maximum: 4_000 }),
    contentTypes: stringList(value.contentTypes, `${path}.contentTypes`, { allowed: CONTENT_TYPES }),
    purposes: stringList(value.purposes, `${path}.purposes`),
    signals: stringList(value.signals, `${path}.signals`, { minimum: 2, maximum: 20 }),
    avoid: stringList(value.avoid, `${path}.avoid`, { minimum: 1, maximum: 20 }),
  };
}

function normalizeVoice(value, index) {
  const path = `voices[${index}]`;
  if (!isRecord(value)) fail(path, 'expected an object');
  return {
    id: resourceId(value.id, `${path}.id`),
    name: resourceName(value.name, `${path}.name`),
    description: requiredString(value.description, `${path}.description`, { minimum: 8, maximum: 500 }),
    instruction: requiredString(value.instruction, `${path}.instruction`, { minimum: 24, maximum: 4_000 }),
    contentTypes: stringList(value.contentTypes, `${path}.contentTypes`, { allowed: CONTENT_TYPES }),
    signals: stringList(value.signals, `${path}.signals`, { minimum: 2, maximum: 20 }),
    avoid: stringList(value.avoid, `${path}.avoid`, { minimum: 1, maximum: 20 }),
  };
}

function normalizePurposePreset(value, index) {
  const path = `purposePresets[${index}]`;
  if (!isRecord(value)) fail(path, 'expected an object');
  return {
    id: resourceId(value.id, `${path}.id`),
    name: resourceName(value.name, `${path}.name`),
    description: requiredString(value.description, `${path}.description`, { minimum: 8, maximum: 500 }),
    instruction: requiredString(value.instruction, `${path}.instruction`, { minimum: 24, maximum: 4_000 }),
    contentTypes: stringList(value.contentTypes, `${path}.contentTypes`, { allowed: CONTENT_TYPES }),
    signals: stringList(value.signals, `${path}.signals`, { minimum: 2, maximum: 20 }),
    recommendedPatternIds: stringList(value.recommendedPatternIds, `${path}.recommendedPatternIds`, { minimum: 2, maximum: 12 }),
    recommendedVoiceIds: stringList(value.recommendedVoiceIds, `${path}.recommendedVoiceIds`, { minimum: 1, maximum: 5 }),
    successCriteria: stringList(value.successCriteria, `${path}.successCriteria`, { minimum: 2, maximum: 12 }),
  };
}

function assertExactCount(items, key) {
  const expected = WRITING_RESOURCE_COUNTS[key];
  if (!Array.isArray(items)) fail(key, 'expected an array');
  if (items.length !== expected) fail(key, `expected exactly ${expected} items, received ${items.length}`);
}

function assertUniqueIds(items, key) {
  const seen = new Set();
  for (const item of items) {
    if (seen.has(item.id)) fail(key, `duplicate id "${item.id}"`);
    seen.add(item.id);
  }
  return seen;
}

function assertReferences(catalog, patternIds, voiceIds, purposeIds) {
  for (const pattern of catalog.writingPatterns) {
    for (const purposeId of pattern.purposes) {
      if (!purposeIds.has(purposeId)) fail(`writingPatterns.${pattern.id}.purposes`, `unknown purpose preset "${purposeId}"`);
    }
  }
  for (const preset of catalog.purposePresets) {
    for (const patternId of preset.recommendedPatternIds) {
      if (!patternIds.has(patternId)) fail(`purposePresets.${preset.id}.recommendedPatternIds`, `unknown writing pattern "${patternId}"`);
    }
    for (const voiceId of preset.recommendedVoiceIds) {
      if (!voiceIds.has(voiceId)) fail(`purposePresets.${preset.id}.recommendedVoiceIds`, `unknown voice "${voiceId}"`);
    }
  }
}

function deepFreeze(value) {
  if (!value || typeof value !== 'object' || Object.isFrozen(value)) return value;
  for (const item of Object.values(value)) deepFreeze(item);
  return Object.freeze(value);
}

function normalizeSource(value) {
  if (!isRecord(value)) fail('source', 'expected an object');
  const kind = requiredString(value.kind, 'source.kind', { maximum: 80 });
  if (kind !== 'firstParty') fail('source.kind', 'must be "firstParty"');
  return {
    kind,
    authoredBy: requiredString(value.authoredBy, 'source.authoredBy', { minimum: 2, maximum: 120 }),
    license: requiredString(value.license, 'source.license', { minimum: 8, maximum: 500 }),
    researchBoundary: requiredString(value.researchBoundary, 'source.researchBoundary', { minimum: 24, maximum: 1_000 }),
  };
}

export function normalizeWritingResources(value) {
  if (value?.[NORMALIZED_CATALOG] === true) return value;
  if (!isRecord(value)) fail('$', 'expected a catalog object');
  assertExactCount(value.writingPatterns, 'writingPatterns');
  assertExactCount(value.voices, 'voices');
  assertExactCount(value.purposePresets, 'purposePresets');

  const catalog = {
    schemaVersion: requiredString(value.schemaVersion, 'schemaVersion', { maximum: 20 }),
    catalogVersion: requiredString(value.catalogVersion, 'catalogVersion', { maximum: 40 }),
    source: normalizeSource(value.source),
    writingPatterns: value.writingPatterns.map(normalizePattern),
    voices: value.voices.map(normalizeVoice),
    purposePresets: value.purposePresets.map(normalizePurposePreset),
  };
  if (catalog.schemaVersion !== '1.0') fail('schemaVersion', 'only schema version 1.0 is supported');

  const patternIds = assertUniqueIds(catalog.writingPatterns, 'writingPatterns');
  const voiceIds = assertUniqueIds(catalog.voices, 'voices');
  const purposeIds = assertUniqueIds(catalog.purposePresets, 'purposePresets');
  assertReferences(catalog, patternIds, voiceIds, purposeIds);
  Object.defineProperty(catalog, NORMALIZED_CATALOG, { value: true });
  return deepFreeze(catalog);
}

async function resolveCatalogSource(source, options) {
  let resolved = typeof source === 'function' ? await source() : await source;
  if (typeof resolved === 'string' && resolved.trim().startsWith('{')) {
    try {
      return JSON.parse(resolved);
    } catch (error) {
      throw new TypeError(`Unable to parse writing resources JSON: ${error.message}`);
    }
  }
  if (typeof URL !== 'undefined' && resolved instanceof URL) resolved = resolved.toString();
  if (typeof resolved === 'string') {
    const fetchImplementation = options.fetch || globalThis.fetch;
    if (typeof fetchImplementation !== 'function') throw new Error('Writing resources URL requires a fetch implementation');
    const response = await fetchImplementation(resolved);
    if (!response || response.ok === false) throw new Error(`Unable to load writing resources (${response?.status || 'network error'})`);
    if (typeof response.json !== 'function') throw new TypeError('Writing resources response does not provide json()');
    return response.json();
  }
  if (resolved && typeof resolved.json === 'function') return resolved.json();
  return resolved;
}

export async function loadWritingResources(source = DEFAULT_WRITING_RESOURCES_URL, options = {}) {
  return normalizeWritingResources(await resolveCatalogSource(source, options));
}

function catalogOf(value) {
  return value?.[NORMALIZED_CATALOG] === true ? value : normalizeWritingResources(value);
}

function lookupId(value) {
  const candidate = typeof value === 'string' ? value.trim().toLowerCase() : '';
  return ID_PATTERN.test(candidate) ? candidate : '';
}

function resolveById(resources, key, id) {
  const candidate = lookupId(id);
  if (!candidate) return null;
  return catalogOf(resources)[key].find((item) => item.id === candidate) || null;
}

export function resolveWritingPattern(resources, id) {
  return resolveById(resources, 'writingPatterns', id);
}

export function resolveWritingVoice(resources, id) {
  return resolveById(resources, 'voices', id);
}

export function resolvePurposePreset(resources, id) {
  return resolveById(resources, 'purposePresets', id);
}

function searchText(context) {
  return [context.text, context.requirement, context.intent, context.topic]
    .filter((item) => typeof item === 'string')
    .join('\n')
    .trim()
    .toLocaleLowerCase('zh-CN');
}

function requestedContentType(value) {
  const candidate = typeof value === 'string' ? value.trim().toLowerCase() : '';
  return CONTENT_TYPES.has(candidate) ? candidate : 'article';
}

function signalMatches(signals, source) {
  if (!source) return [];
  return signals.filter((signal) => source.includes(signal.toLocaleLowerCase('zh-CN')));
}

function ranked(items, scoreItem, limit) {
  return items.map((resource, index) => ({ resource, index, ...scoreItem(resource) }))
    .sort((left, right) => right.score - left.score || left.index - right.index || left.resource.id.localeCompare(right.resource.id))
    .slice(0, limit)
    .map(({ resource, score, reasons }) => deepFreeze({ resource, score, reasons }));
}

function contentTypeScore(resource, contentType) {
  if (resource.contentTypes.includes(contentType)) return { score: 12, reason: `适用于${contentType}` };
  if (resource.contentTypes.includes('all')) return { score: 5, reason: '适用于通用内容' };
  return { score: 0, reason: null };
}

export function recommendWritingResources(resources, context = {}) {
  const catalog = catalogOf(resources);
  const source = searchText(context);
  const contentType = requestedContentType(context.contentType);
  const explicitPurposeId = lookupId(context.purposeId || context.purposePresetId);
  const limit = Math.max(1, Math.min(12, Number.isFinite(Number(context.limit)) ? Math.trunc(Number(context.limit)) : 5));

  const purposes = ranked(catalog.purposePresets, (preset) => {
    const reasons = [];
    let score = 0;
    if (explicitPurposeId && preset.id === explicitPurposeId) {
      score += 100;
      reasons.push('用户明确指定此目的');
    }
    const typeMatch = contentTypeScore(preset, contentType);
    score += typeMatch.score;
    if (typeMatch.reason) reasons.push(typeMatch.reason);
    const matches = signalMatches(preset.signals, source);
    score += Math.min(matches.length, 5) * 8;
    if (matches.length) reasons.push(`命中意图：${matches.slice(0, 3).join('、')}`);
    if (source && (source.includes(preset.name.toLocaleLowerCase('zh-CN')) || source.includes(preset.id))) {
      score += 16;
      reasons.push('用户描述直接提及该目的');
    }
    return { score, reasons: reasons.length ? reasons : ['作为通用目的候选'] };
  }, catalog.purposePresets.length);

  const purpose = purposes[0];
  const recommendedPatternIds = new Set(purpose?.resource.recommendedPatternIds || []);
  const patterns = ranked(catalog.writingPatterns, (pattern) => {
    const reasons = [];
    let score = 0;
    if (recommendedPatternIds.has(pattern.id)) {
      score += 24;
      reasons.push(`匹配目的：${purpose.resource.name}`);
    } else if (purpose && pattern.purposes.includes(purpose.resource.id)) {
      score += 16;
      reasons.push(`可服务于目的：${purpose.resource.name}`);
    }
    const typeMatch = contentTypeScore(pattern, contentType);
    score += typeMatch.score;
    if (typeMatch.reason) reasons.push(typeMatch.reason);
    const matches = signalMatches(pattern.signals, source);
    score += Math.min(matches.length, 5) * 10;
    if (matches.length) reasons.push(`命中表达需求：${matches.slice(0, 3).join('、')}`);
    if (source && (source.includes(pattern.name.toLocaleLowerCase('zh-CN')) || source.includes(pattern.id))) {
      score += 20;
      reasons.push('用户直接提及此模式');
    }
    return { score, reasons: reasons.length ? reasons : ['通用写作候选'] };
  }, limit);

  const recommendedVoiceIds = new Set(purpose?.resource.recommendedVoiceIds || []);
  const voices = ranked(catalog.voices, (voice) => {
    const reasons = [];
    let score = 0;
    if (recommendedVoiceIds.has(voice.id)) {
      score += 22;
      reasons.push(`匹配目的：${purpose.resource.name}`);
    }
    const typeMatch = contentTypeScore(voice, contentType);
    score += typeMatch.score;
    if (typeMatch.reason) reasons.push(typeMatch.reason);
    const matches = signalMatches(voice.signals, source);
    score += Math.min(matches.length, 5) * 6;
    if (matches.length) reasons.push(`命中语气需求：${matches.slice(0, 3).join('、')}`);
    if (source && (source.includes(voice.name.toLocaleLowerCase('zh-CN')) || source.includes(voice.id))) {
      score += 20;
      reasons.push('用户直接提及此语气');
    }
    return { score, reasons: reasons.length ? reasons : ['通用语气候选'] };
  }, Math.min(limit, catalog.voices.length));

  return deepFreeze({
    contentType,
    purpose,
    purposeCandidates: purposes.slice(0, Math.min(limit, purposes.length)),
    patterns,
    voices,
  });
}

export function installWritingResourcesGlobal(target = globalThis) {
  if (!target || (typeof target !== 'object' && typeof target !== 'function')) throw new TypeError('A browser global target is required');
  const api = Object.freeze({
    WRITING_RESOURCE_COUNTS,
    WRITING_CONTENT_TYPES,
    DEFAULT_WRITING_RESOURCES_URL,
    normalizeWritingResources,
    loadWritingResources,
    resolveWritingPattern,
    resolveWritingVoice,
    resolvePurposePreset,
    recommendWritingResources,
  });
  Object.defineProperty(target, 'YunspireWritingResources', {
    configurable: true,
    enumerable: false,
    writable: false,
    value: api,
  });
  return api;
}

if (typeof window !== 'undefined' && window?.document) installWritingResourcesGlobal(window);
