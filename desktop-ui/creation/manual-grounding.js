import {
  deriveCreationBlocks,
  normalizeCreationDocument,
  safeCreationId,
} from './document.js';

function text(value, fallback = '', maximum = 4_000) {
  const candidate = typeof value === 'string' || typeof value === 'number' ? String(value).trim() : '';
  return (candidate || fallback).slice(0, maximum);
}
function normalizedWhitespace(value) {
  return String(value || '').replace(/\s+/gu, ' ').trim();
}

function normalizedHash(value) {
  const candidate = text(value, '', 80).toLowerCase();
  if (/^sha256:[a-f0-9]{64}$/u.test(candidate)) return candidate;
  if (/^[a-f0-9]{64}$/u.test(candidate)) return `sha256:${candidate}`;
  return null;
}

function validCapturedAt(value) {
  if (!Number.isFinite(Date.parse(value))) throw new TypeError('SourceRef capturedAt 必须是有效时间');
  return new Date(value).toISOString();
}

function exactExcerptCandidate(searchExcerpt, normalizedContent) {
  return String(searchExcerpt || '')
    .split(/…|\.\.\./u)
    .map(normalizedWhitespace)
    .filter((candidate) => candidate.length >= 4 && normalizedContent.includes(candidate))
    .sort((left, right) => right.length - left.length)[0]
    || '';
}

export function selectManualCitationQuote(searchResult = {}, note = {}) {
  const normalizedContent = normalizedWhitespace(note.content);
  if (!normalizedContent) throw new Error('引用来源没有可复核正文');
  const indexedExcerpt = exactExcerptCandidate(searchResult.excerpt, normalizedContent);
  if (indexedExcerpt) return indexedExcerpt.slice(0, 1_200);
  const body = String(note.content || '')
    .replace(/^---\s*\n[\s\S]*?\n---\s*(?:\n|$)/u, '')
    .split(/\r?\n/u)
    .map((line) => line.replace(/^#{1,6}\s+/u, '').replace(/^>\s?/u, '').trim())
    .filter((line) => line && !/^!\[|^---$|^```/u.test(line))
    .join(' ');
  const fallback = normalizedWhitespace(body || normalizedContent).slice(0, 1_200);
  if (fallback.length < 4) throw new Error('引用来源正文过短，无法建立逐字证据');
  return fallback;
}

export async function createManualVaultSourceRef(searchResult, note, options = {}) {
  const searchVaultId = text(searchResult?.vaultId, '', 160);
  const searchPath = text(searchResult?.relativePath, '', 2_048).replaceAll('\\', '/');
  const noteVaultId = text(note?.vaultId, '', 160);
  const notePath = text(note?.relativePath, '', 2_048).replaceAll('\\', '/');
  if (!searchVaultId || !searchPath) throw new Error('搜索结果缺少 vaultId 或 relativePath');
  if (noteVaultId !== searchVaultId || notePath !== searchPath) throw new Error('读取到的笔记身份与搜索结果不一致');
  const quote = selectManualCitationQuote(searchResult, note);
  const hashText = options.hashText;
  if (typeof hashText !== 'function') throw new TypeError('创建 SourceRef 需要 SHA-256 实现');
  const contentHash = normalizedHash(note.contentHash) || normalizedHash(await hashText(String(note.content || '')));
  if (!contentHash) throw new Error('引用来源没有有效 SHA-256 contentHash');
  const excerptHash = normalizedHash(await hashText(quote));
  if (!excerptHash) throw new Error('无法计算引用摘录 SHA-256');
  const capturedAt = validCapturedAt(options.capturedAt || new Date().toISOString());
  const sourceRef = {
    id: safeCreationId(`vault-note-${searchVaultId}-${searchPath}`, 'source'),
    kind: 'vaultNote',
    ref: `${searchVaultId}:${searchPath}`,
    vaultId: searchVaultId,
    relativePath: searchPath,
    title: text(searchResult?.title || note?.title, searchPath.replace(/\.md$/iu, '').split('/').at(-1), 240),
    excerpt: quote,
    contentHash,
    excerptHash,
    capturedAt,
    trust: 'direct',
  };
  return { sourceRef, quote };
}

function blockMarkdown(document, block) {
  return document.canonicalMarkdown.slice(block.sourceRange.start, block.sourceRange.end).trim();
}

export function bindManualSourceToCreationDocument(value, binding) {
  const sourceRef = binding?.sourceRef;
  const quote = normalizedWhitespace(binding?.quote);
  const citationMarkdown = text(binding?.citationMarkdown, '', 4_000);
  if (!sourceRef?.id || !quote || !citationMarkdown) throw new TypeError('手工引用绑定缺少 SourceRef、quote 或 citationMarkdown');
  const canonicalMarkdown = String(value?.canonicalMarkdown || '');
  const blocks = deriveCreationBlocks(canonicalMarkdown);
  const block = [...blocks].reverse().find((candidate) => blockMarkdown({ canonicalMarkdown }, candidate).includes(citationMarkdown));
  if (!block) throw new Error('无法在当前创作文稿中定位刚插入的引用块');
  const previous = normalizeCreationDocument({ ...value, blocks }, { compatibilityAliases: false });
  const sourceRefs = [
    ...previous.sourceRefs.filter((item) => item.id !== sourceRef.id && !(item.vaultId === sourceRef.vaultId && item.relativePath === sourceRef.relativePath)),
    sourceRef,
  ];
  const previousLedgerBlock = previous.groundingLedger.blocks.find((item) => item.id === block.id);
  const ledgerBlock = {
    id: block.id,
    sourceRefIds: [...new Set([...(previousLedgerBlock?.sourceRefIds || []), sourceRef.id])],
    verdict: 'supported',
    evidence: [
      ...(previousLedgerBlock?.evidence || []).filter((item) => item.sourceRefId !== sourceRef.id),
      { sourceRefId: sourceRef.id, quote },
    ],
  };
  const groundingBlocks = [
    ...previous.groundingLedger.blocks.filter((item) => item.id !== block.id),
    ledgerBlock,
  ];
  const now = sourceRef.capturedAt;
  const document = normalizeCreationDocument({
    ...previous,
    revision: previous.revision + 1,
    blocks,
    sourceRefs,
    groundingLedger: {
      ...previous.groundingLedger,
      status: 'unverified',
      blocks: groundingBlocks,
      verifiedAt: null,
      contentHash: null,
      verificationTraceId: null,
    },
    metadata: {
      ...previous.metadata,
      properties: {
        ...previous.metadata.properties,
        groundingVerified: false,
        groundingStatus: 'unverified',
        lastManualSourceCapturedAt: now,
      },
    },
    provenance: {
      ...previous.provenance,
      sourceIds: [...new Set([...previous.provenance.sourceIds, sourceRef.id])],
      derivation: 'revised',
    },
    readiness: null,
  }, { compatibilityAliases: false });
  return { document, sourceRef, quote, blockId: block.id };
}
