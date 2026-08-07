const FIRST_PARTY_SOURCE = Object.freeze({
  policy: 'yunspire_first_party',
  authoredBy: 'Yunspire',
  repository: 'https://github.com/Leo-sail/yunspire',
  upstreamCodeCopied: false,
  researchBoundary: '云枢原创主题；只借鉴公开产品的能力边界，不复制上游代码、模板、素材或提示词。',
});

const FIRST_PARTY_LICENSE = Object.freeze({
  scope: 'yunspire_first_party_project_asset',
  notice: '本主题清单和渲染令牌由云枢项目原创维护。',
  thirdPartyAssets: [],
});

const DEFAULT_PALETTE = Object.freeze({
  accent: '#31536F',
  accentSoft: '#EDF3F6',
  text: '#202B33',
  muted: '#66727A',
  border: '#DBE2E7',
  quote: '#F4F7F9',
  heading: '#17232C',
  background: '#FFFFFF',
});

const DEFAULT_TYPOGRAPHY = Object.freeze({
  defaultFamily: 'sans',
  fallbackStack: '-apple-system,BlinkMacSystemFont,"PingFang SC","Microsoft YaHei",sans-serif',
  baseSize: 16,
  lineHeight: 1.8,
  headingWeight: 700,
  bodyWeight: 400,
});

const LEGACY_THEMES = Object.freeze([
  { id: 'ink', displayName: '云墨', description: '适合深度长文和知识解读。', category: 'longform', accent: '#31536F', accentSoft: '#EDF3F6', text: '#202B33', muted: '#66727A', border: '#DBE2E7', quote: '#F4F7F9', heading: '#17232C', tags: ['推荐', '长文'] },
  { id: 'jade', displayName: '青序', description: '适合教程、清单和方法论。', category: 'tutorial', accent: '#0F766E', accentSoft: '#ECF7F5', text: '#23312F', muted: '#61706D', border: '#D6E5E1', quote: '#F1F8F6', heading: '#153F3A', tags: ['推荐', '教程'] },
  { id: 'vermilion', displayName: '朱简', description: '适合观点、评论和态度表达。', category: 'commentary', accent: '#B42318', accentSoft: '#FFF1EE', text: '#352724', muted: '#786A66', border: '#EADBD7', quote: '#FFF7F5', heading: '#631D17', tags: ['推荐', '观点'] },
  { id: 'graphite', displayName: '素刊', description: '适合专业报告和数据分析。', category: 'report', accent: '#52525B', accentSoft: '#F1F1F3', text: '#27272A', muted: '#71717A', border: '#E1E1E5', quote: '#F7F7F8', heading: '#18181B', tags: ['推荐', '报告'] },
]);

function isRecord(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

function stringValue(value, fallback = '', maximum = 1000) {
  const candidate = typeof value === 'string' || typeof value === 'number' ? String(value).trim() : '';
  return (candidate || fallback).slice(0, maximum);
}

function uniqueStrings(value, maximum = 100) {
  return [...new Set((Array.isArray(value) ? value : []).map((item) => stringValue(item)).filter(Boolean))].slice(0, maximum);
}

function identifier(value, fallback) {
  const candidate = stringValue(value).toLowerCase().replace(/[^a-z0-9-]+/gu, '-').replace(/^-+|-+$/gu, '');
  return /^[a-z]/u.test(candidate) ? candidate.slice(0, 80) : fallback;
}

function boundedInteger(value, fallback, minimum, maximum) {
  const candidate = Number(value);
  return Number.isFinite(candidate) ? Math.max(minimum, Math.min(maximum, Math.round(candidate))) : fallback;
}

function boundedNumber(value, fallback, minimum, maximum) {
  const candidate = Number(value);
  return Number.isFinite(candidate) ? Math.max(minimum, Math.min(maximum, candidate)) : fallback;
}

function color(value, fallback) {
  const candidate = stringValue(value).toUpperCase();
  return /^#[0-9A-F]{6}$/u.test(candidate) ? candidate : fallback;
}

function normalizePalette(source) {
  const palette = isRecord(source.palette) ? source.palette : (isRecord(source.tokens) ? source.tokens : source);
  return Object.fromEntries(Object.entries(DEFAULT_PALETTE).map(([key, fallback]) => [key, color(palette[key], fallback)]));
}

function normalizeTypography(source) {
  const typography = isRecord(source.typography) ? source.typography : {};
  const family = ['sans', 'serif', 'kaiti'].includes(typography.defaultFamily) ? typography.defaultFamily : DEFAULT_TYPOGRAPHY.defaultFamily;
  return {
    defaultFamily: family,
    fallbackStack: stringValue(typography.fallbackStack, DEFAULT_TYPOGRAPHY.fallbackStack, 500),
    baseSize: boundedInteger(typography.baseSize, DEFAULT_TYPOGRAPHY.baseSize, 10, 32),
    lineHeight: boundedNumber(typography.lineHeight, DEFAULT_TYPOGRAPHY.lineHeight, 1, 3),
    headingWeight: boundedInteger(typography.headingWeight, DEFAULT_TYPOGRAPHY.headingWeight, 100, 900),
    bodyWeight: boundedInteger(typography.bodyWeight, DEFAULT_TYPOGRAPHY.bodyWeight, 100, 900),
  };
}

function attachThemeAliases(theme, source) {
  Object.defineProperties(theme, {
    name: { enumerable: false, configurable: true, get: () => theme.displayName },
    aliases: { enumerable: false, configurable: true, get: () => theme.legacyIds },
    wechatCertified: { enumerable: false, configurable: true, get: () => theme.compatibility.wechatCertification === 'certified' },
    tokens: { enumerable: false, configurable: true, get: () => ({ ...theme.palette, ...theme.spacing }) },
    recommended: { enumerable: false, configurable: true, get: () => source.recommended === true || theme.tags.includes('推荐') || theme.tags.includes('recommended') },
  });
  return theme;
}

export function normalizeThemeManifest(value, index = 0) {
  const source = isRecord(value) ? value : {};
  const id = identifier(source.id, `theme-${index + 1}`);
  const tokens = isRecord(source.tokens) ? source.tokens : {};
  const features = isRecord(source.features) ? source.features : {};
  const certification = source.wechatCertified === true ? 'certified' : stringValue(source.compatibility?.wechatCertification, ['ink', 'jade', 'vermilion', 'graphite'].includes(id) ? 'legacyCompatible' : 'candidate');
  const manifest = {
    schemaVersion: '1.0',
    manifestType: 'theme',
    catalogVersion: stringValue(source.catalogVersion, '0.3.0', 40),
    id,
    version: stringValue(source.version, '1.0.0', 40).includes('.') ? stringValue(source.version, '1.0.0', 40) : `${stringValue(source.version, '1')}.0.0`,
    displayName: stringValue(source.displayName || source.name, id, 80),
    description: stringValue(source.description, '云枢原创创作主题。', 500),
    status: ['active', 'experimental', 'deprecated'].includes(source.status) ? source.status : 'active',
    category: ['longform', 'tutorial', 'commentary', 'report', 'lifestyle', 'brand', 'visual'].includes(source.category) ? source.category : 'longform',
    tags: uniqueStrings(source.tags, 30),
    legacyIds: uniqueStrings(source.legacyIds || source.aliases, 20).map((item) => identifier(item, '')).filter(Boolean),
    palette: normalizePalette(source),
    typography: normalizeTypography(source),
    spacing: {
      paragraph: boundedInteger(source.spacing?.paragraph ?? tokens.paragraphSpacing, 16, 0, 100),
      section: boundedInteger(source.spacing?.section ?? tokens.sectionSpacing, 28, 0, 200),
      pageX: boundedInteger(source.spacing?.pageX ?? tokens.pageX, 18, 0, 100),
      pageY: boundedInteger(source.spacing?.pageY ?? tokens.pageY, 24, 0, 100),
    },
    features: {
      autoNumbering: features.autoNumbering === true,
      keywordUnderline: features.keywordUnderline === true,
      tableOfContents: features.tableOfContents === true,
      introduction: features.introduction === true,
      signature: features.signature === true,
      spanLeaf: features.spanLeaf === true,
      cjkSpacing: features.cjkSpacing !== false,
      externalLinkFootnotes: features.externalLinkFootnotes === true,
    },
    supportedComponentIds: uniqueStrings(source.supportedComponentIds || source.supportedComponents, 100).map((item) => identifier(item, '')).filter(Boolean),
    renderers: {
      markdown: stringValue(source.renderers?.markdown, 'creation.theme.markdown.v1', 120),
      html: stringValue(source.renderers?.html, 'creation.theme.html.v1', 120),
      wechatRichText: stringValue(source.renderers?.wechatRichText, 'creation.theme.wechat.v1', 120),
    },
    compatibility: {
      targets: uniqueStrings(source.compatibility?.targets, 3).filter((item) => ['markdown', 'html', 'wechatRichText'].includes(item)).length
        ? uniqueStrings(source.compatibility.targets, 3).filter((item) => ['markdown', 'html', 'wechatRichText'].includes(item))
        : ['markdown', 'html', 'wechatRichText'],
      wechatCertification: ['notApplicable', 'legacyCompatible', 'candidate', 'certified'].includes(certification) ? certification : 'candidate',
      minRuntimeVersion: stringValue(source.compatibility?.minRuntimeVersion, '0.3.0', 40),
    },
    source: { ...FIRST_PARTY_SOURCE, ...(isRecord(source.source) ? source.source : {}) },
    license: { ...FIRST_PARTY_LICENSE, ...(isRecord(source.license) ? source.license : {}) },
  };
  return attachThemeAliases(manifest, source);
}

export const legacyThemeManifests = Object.freeze(LEGACY_THEMES.map(normalizeThemeManifest));

export function normalizeThemeCatalog(value, { includeLegacyFallback = true } = {}) {
  const source = Array.isArray(value) ? value : (Array.isArray(value?.themes) ? value.themes : (Array.isArray(value?.items) ? value.items : []));
  const manifests = source.map(normalizeThemeManifest);
  const merged = manifests.length || !includeLegacyFallback ? manifests : [...legacyThemeManifests];
  const byId = new Map();
  for (const theme of merged) if (!byId.has(theme.id)) byId.set(theme.id, theme);
  return [...byId.values()];
}

async function resolveThemeSource(source, options) {
  let resolved = typeof source === 'function' ? await source() : await source;
  if (typeof resolved === 'string') {
    const fetchImplementation = options.fetch || globalThis.fetch;
    if (typeof fetchImplementation !== 'function') throw new Error('Theme catalog URL requires a fetch implementation');
    const response = await fetchImplementation(resolved);
    if (!response?.ok) throw new Error(`Unable to load theme catalog (${response?.status || 'network error'})`);
    resolved = await response.json();
  } else if (resolved && typeof resolved.json === 'function') {
    resolved = await resolved.json();
  }
  return resolved;
}

export async function loadThemeManifests(source, options = {}) {
  const resolved = await resolveThemeSource(source, options);
  if (Array.isArray(resolved) && resolved.every((item) => typeof item === 'string')) {
    const catalogs = await Promise.all(resolved.map((item) => resolveThemeSource(item, options)));
    return normalizeThemeCatalog(catalogs.flatMap((catalog) => Array.isArray(catalog) ? catalog : (catalog?.themes || [catalog])), options);
  }
  return normalizeThemeCatalog(resolved, options);
}

export function resolveThemeManifest(catalog, id) {
  const themes = normalizeThemeCatalog(catalog);
  const candidate = stringValue(id).toLowerCase();
  return themes.find((theme) => theme.id === candidate || theme.legacyIds.includes(candidate)) || themes[0] || null;
}

export function createThemeGalleryViewModel(catalog, options = {}) {
  const selectedId = stringValue(options.selectedId, '');
  const query = stringValue(options.query).toLocaleLowerCase('zh-CN');
  const category = stringValue(options.category);
  const recommendationLimit = boundedInteger(options.recommendationLimit, 6, 1, 12);
  const themes = normalizeThemeCatalog(catalog).filter((theme) => {
    if (options.includeDeprecated !== true && theme.status === 'deprecated') return false;
    if (category && category !== 'all' && theme.category !== category) return false;
    if (!query) return true;
    return [theme.id, theme.displayName, theme.description, theme.category, ...theme.tags].join(' ').toLocaleLowerCase('zh-CN').includes(query);
  }).map((theme) => ({
    manifest: theme,
    id: theme.id,
    name: theme.displayName,
    description: theme.description,
    category: theme.category,
    accent: theme.palette.accent,
    selected: theme.id === selectedId || theme.legacyIds.includes(selectedId),
    recommended: theme.recommended,
    certified: theme.wechatCertified,
    disabled: theme.status === 'deprecated',
  }));
  const recommended = themes.filter((theme) => theme.recommended).slice(0, recommendationLimit);
  if (recommended.length < recommendationLimit) {
    recommended.push(...themes.filter((theme) => !recommended.includes(theme)).slice(0, recommendationLimit - recommended.length));
  }
  return {
    total: themes.length,
    selected: themes.find((theme) => theme.selected) || null,
    recommended,
    all: themes,
    categories: [...new Set(themes.map((theme) => theme.category))],
    empty: themes.length === 0,
  };
}

export function toRuntimeThemeManifest(value) {
  return JSON.parse(JSON.stringify(normalizeThemeManifest(value)));
}
