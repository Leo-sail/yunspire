import { renderPrompt } from './prompt-registry.js';

const CLASSIFICATION_EVIDENCE_KINDS = new Set([
  'source_type',
  'title',
  'source',
  'content',
  'folder',
  'similar_note',
  'tag',
]);

function isRecord(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

function text(value, fallback = '', maximum = 2_000) {
  const candidate = typeof value === 'string' || typeof value === 'number' ? String(value).trim() : '';
  return (candidate || fallback).slice(0, maximum);
}

function uniqueStrings(values, maximum = 100) {
  return [...new Set((Array.isArray(values) ? values : []).map((item) => text(item, '', 240)).filter(Boolean))].slice(0, maximum);
}

function normalizedFolder(value) {
  const candidate = text(value, '', 1_024).replaceAll('\\', '/').replace(/^\/+|\/+$/gu, '');
  if (!candidate || candidate.split('/').some((part) => !part || part === '.' || part === '..') || /[\0\r\n]/u.test(candidate)) return '';
  return candidate;
}

export function normalizeClassificationTargetPath(value, fallback = '原子库/分析') {
  return normalizedFolder(value) || normalizedFolder(fallback) || '原子库/分析';
}

function folderPaths(values) {
  return uniqueStrings((Array.isArray(values) ? values : []).map((item) => (
    typeof item === 'string' ? item : item?.relativePath
  )), 240).map(normalizedFolder).filter(Boolean);
}

function stripEnvelope(value) {
  const source = text(value, '', 100_000);
  const unfenced = source.replace(/^```(?:json)?\s*\n?/iu, '').replace(/\n?```$/u, '').trim();
  const start = unfenced.indexOf('{');
  const end = unfenced.lastIndexOf('}');
  return start >= 0 && end > start ? unfenced.slice(start, end + 1) : unfenced;
}

function classificationTags(category, tags) {
  return uniqueStrings([category, ...(Array.isArray(tags) ? tags : []), '待审查'], 12);
}

function defaultClassificationFolder(category, folders) {
  const paths = folderPaths(folders);
  const preferences = category === '视觉素材'
    ? [/视觉|图片|素材/u, /原子库\/分析/u]
    : category === '媒体采集'
      ? [/媒体|视频|采集/u, /原子库\/分析/u]
      : category === '文档资料'
        ? [/文档|资料|来源/u, /原子库\/分析/u]
        : [/原子库\/分析/u, /分析/u, /资料|来源/u];
  return preferences.flatMap((pattern) => paths.filter((path) => pattern.test(path)))[0]
    || paths[0]
    || '原子库/分析';
}

export function buildInboundClassificationRequest(input = {}) {
  const title = text(input.title, '未命名内容', 500);
  const source = text(input.source, '本地来源', 1_000);
  const sourceType = text(input.sourceType, 'link', 40);
  const content = text(input.content, '', 8_000);
  const vaultId = text(input.vaultId, '', 160);
  const vaultName = text(input.vaultName, vaultId || '本地 Obsidian', 240);
  const folders = folderPaths(input.folders);
  const allowedFolders = folders.length ? folders : ['原子库/分析'];
  const similarNotes = (Array.isArray(input.similarNotes) ? input.similarNotes : []).slice(0, 16).map((item) => ({
    title: text(item?.title, '未命名笔记', 240),
    relativePath: text(item?.relativePath, '', 1_024),
    tags: uniqueStrings(item?.tags, 20),
    score: Number.isFinite(Number(item?.score)) ? Number(item.score) : null,
  })).filter((item) => item.relativePath);
  const availableEvidenceKinds = [
    'source_type',
    'title',
    'source',
    ...(content ? ['content'] : []),
    ...(folders.length ? ['folder'] : []),
    ...(similarNotes.length ? ['similar_note'] : []),
    ...(similarNotes.some((item) => item.tags.length) ? ['tag'] : []),
  ];
  const evidencePayload = {
    sourceType,
    title,
    source,
    content: content || null,
    vault: { id: vaultId || null, name: vaultName },
    allowedTargetFolders: allowedFolders,
    availableEvidenceKinds,
    similarNotes,
  };
  return {
    allowedFolders,
    availableEvidenceKinds,
    prompt: renderPrompt('capture.inbound-classification', {
      AVAILABLE_EVIDENCE_KINDS: availableEvidenceKinds.join('|'),
      EVIDENCE_PAYLOAD_JSON: JSON.stringify(evidencePayload),
    }),
  };
}

export function parseInboundClassificationReply(value, options = {}) {
  let parsed;
  try {
    parsed = JSON.parse(stripEnvelope(value));
  } catch {
    throw new Error('分类模型没有返回有效 JSON');
  }
  if (!isRecord(parsed)) throw new Error('分类模型结果必须是 JSON 对象');
  const category = text(parsed.category, '', 80);
  if (!category) throw new Error('分类模型没有返回 category');
  const confidence = parsed.confidence;
  if (typeof confidence !== 'number' || !Number.isFinite(confidence) || confidence < 0 || confidence > 1) throw new Error('分类模型 confidence 必须是 0 到 1 之间的数字');
  const availableEvidenceKinds = new Set((Array.isArray(options.availableEvidenceKinds) ? options.availableEvidenceKinds : [...CLASSIFICATION_EVIDENCE_KINDS]).filter((kind) => CLASSIFICATION_EVIDENCE_KINDS.has(kind)));
  const evidence = (Array.isArray(parsed.evidence) ? parsed.evidence : []).map((item) => ({
    kind: text(item?.kind, '', 40),
    detail: text(item?.detail, '', 500),
  })).filter((item) => availableEvidenceKinds.has(item.kind) && item.detail);
  if (!evidence.length) throw new Error('分类模型没有返回可复核 evidence');
  const allowedFolders = folderPaths(options.allowedFolders);
  const targetPath = normalizedFolder(parsed.targetPath);
  if (!targetPath || (allowedFolders.length && !allowedFolders.includes(targetPath))) {
    throw new Error('分类模型返回了不在真实目录清单中的 targetPath');
  }
  return {
    method: 'model',
    category,
    confidence,
    evidence,
    targetPath,
    tags: classificationTags(category, parsed.tags),
  };
}

export function buildLocalClassificationFallback(input = {}) {
  const sourceType = text(input.sourceType, 'link', 40);
  const title = text(input.title, '未命名内容', 500);
  const source = text(input.source, '本地来源', 1_000);
  const category = sourceType === 'image'
    ? '视觉素材'
    : sourceType === 'file'
      ? '文档资料'
      : /视频|抖音|小红书/iu.test(`${title} ${source}`)
        ? '媒体采集'
        : '来源资料';
  const targetPath = defaultClassificationFolder(category, input.folders);
  return {
    method: 'local_rule',
    category,
    confidence: null,
    evidence: [{
      kind: 'source_type',
      detail: `本地规则仅依据来源类型“${sourceType}”${category === '媒体采集' ? '及标题或来源中的媒体关键词' : ''}生成建议`,
    }],
    targetPath,
    tags: classificationTags(category, []),
  };
}

export function classificationConfidenceLabel(classification) {
  return classification?.method === 'model' && Number.isFinite(Number(classification.confidence))
    ? `${Math.round(Number(classification.confidence) * 100)}%（模型返回）`
    : '不适用（本地规则）';
}
