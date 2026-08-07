import { normalizeCreationDocument, safeCreationId } from './document.js';

export const WRITING_SCOPES = Object.freeze(['structural', 'bounded', 'in_place']);
export const WRITING_ACTIONS = Object.freeze(['annotate', 'rewrite']);
export const MAX_WRITING_ITERATIONS = 3;

function isRecord(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

function stringValue(value, fallback = '', maximum = 4000) {
  const candidate = typeof value === 'string' || typeof value === 'number' ? String(value).trim() : '';
  return (candidate || fallback).slice(0, maximum);
}

function nullableIdentifier(value) {
  const candidate = stringValue(value).toLowerCase().replace(/[^a-z0-9-]+/gu, '-').replace(/^-+|-+$/gu, '');
  return /^[a-z]/u.test(candidate) ? candidate.slice(0, 80) : null;
}

function boundedInteger(value, fallback, minimum, maximum) {
  const candidate = Number(value);
  return Number.isFinite(candidate) ? Math.max(minimum, Math.min(maximum, Math.trunc(candidate))) : fallback;
}

function uniqueStrings(value, maximum = Number.MAX_SAFE_INTEGER) {
  return [...new Set((Array.isArray(value) ? value : []).map((item) => stringValue(item)).filter(Boolean))].slice(0, maximum);
}

export function mergeWritingModelRunIds(...values) {
  return [...new Set(values.flatMap((value) => (Array.isArray(value) ? value : [value]))
    .map((value) => stringValue(value, '', 160))
    .filter(Boolean))];
}

function validHash(value) {
  const candidate = stringValue(value).toLowerCase();
  return /^sha256:[a-f0-9]{64}$/u.test(candidate) ? candidate : null;
}

function validDateTime(value, fallback) {
  return typeof value === 'string' && Number.isFinite(Date.parse(value)) ? new Date(value).toISOString() : fallback;
}

function normalizeFactQualifiers(value) {
  if (!isRecord(value)) return {};
  return Object.fromEntries(Object.entries(value).slice(0, 40).filter(([, qualifier]) => (
    qualifier === null
    || typeof qualifier === 'string'
    || typeof qualifier === 'boolean'
    || (typeof qualifier === 'number' && Number.isFinite(qualifier))
  )));
}

function normalizeScope(value, legacyScope) {
  if (WRITING_SCOPES.includes(value)) return value;
  if (value === 'selection' || legacyScope === 'selection') return 'bounded';
  if (value === 'document' || legacyScope === 'document') return 'structural';
  return 'in_place';
}

export function normalizeWritingPolicy(value = {}, { action = 'rewrite' } = {}) {
  const source = isRecord(value) ? value : {};
  const annotateOnly = action === 'annotate' || source.annotateOnly === true || source.onlyAnnotate === true || source.allowTextChanges === false;
  return {
    allowTextChanges: !annotateOnly,
    preserveFacts: source.preserveFacts !== false && source.facts !== false,
    preserveRelations: source.preserveRelations !== false && source.relations !== false,
    preserveNumbers: source.preserveNumbers !== false && source.numbers !== false,
    preserveCitations: source.preserveCitations !== false && source.references !== false && source.citations !== false,
    allowUnsupportedClaims: false,
  };
}

function normalizeSelection(value) {
  if (!isRecord(value)) return null;
  const start = boundedInteger(value.start, 0, 0, Number.MAX_SAFE_INTEGER);
  const end = boundedInteger(value.end, start, start, Number.MAX_SAFE_INTEGER);
  return {
    start,
    end,
    selectedHash: validHash(value.selectedHash),
  };
}

export function normalizeWritingRunStrategy(value = {}) {
  const source = isRecord(value) ? value : {};
  const annotateOnly = source.annotateOnly === true || source.onlyAnnotate === true || source.action === 'annotate';
  const action = annotateOnly ? 'annotate' : 'rewrite';
  const scope = normalizeScope(source.scope, source.rewriteScope);
  const maxIterations = boundedInteger(source.maxIterations, MAX_WRITING_ITERATIONS, 1, MAX_WRITING_ITERATIONS);
  return {
    action,
    scope,
    selection: normalizeSelection(source.selection || source.range),
    scenePackId: nullableIdentifier(source.scenePackId || source.sceneId),
    patternId: nullableIdentifier(source.patternId || source.rewriteMode),
    voiceId: nullableIdentifier(source.voiceId || source.voice),
    purposePresetId: nullableIdentifier(source.purposePresetId || source.purpose),
    brandProfileId: nullableIdentifier(source.brandProfileId),
    iteration: boundedInteger(source.iteration, 1, 1, maxIterations),
    maxIterations,
    policy: normalizeWritingPolicy(source.policy || source.preserve || source, { action }),
  };
}

export const createWritingRunPolicy = normalizeWritingPolicy;
export const createWritingStrategy = normalizeWritingRunStrategy;

function bytesToHex(bytes) {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('');
}

export async function sha256Text(value, options = {}) {
  const supplied = validHash(options.hash);
  if (supplied) return supplied;
  if (typeof options.digest === 'function') {
    const result = await options.digest(String(value || ''));
    const normalized = validHash(result) || validHash(`sha256:${result}`);
    if (!normalized) throw new Error('Custom digest must return a SHA-256 hex digest');
    return normalized;
  }
  const cryptoImplementation = options.crypto || globalThis.crypto;
  if (!cryptoImplementation?.subtle?.digest) throw new Error('SHA-256 requires Web Crypto or a custom digest implementation');
  const digest = await cryptoImplementation.subtle.digest('SHA-256', new TextEncoder().encode(String(value || '')));
  return `sha256:${bytesToHex(new Uint8Array(digest))}`;
}

function normalizeFact(value, index) {
  const source = isRecord(value) ? value : {};
  return {
    id: safeCreationId(source.id || `fact-${index + 1}-${source.subject || ''}`, 'fact'),
    subject: stringValue(source.subject, '未命名主体', 1000),
    predicate: stringValue(source.predicate, '关联', 500),
    object: stringValue(source.object, '未命名客体', 4000),
    qualifiers: normalizeFactQualifiers(source.qualifiers),
    status: ['sourced', 'unsourced', 'inferred', 'conflicted'].includes(source.status) ? source.status : 'unsourced',
    protected: source.protected !== false,
    evidenceRefIds: uniqueStrings(source.evidenceRefIds),
  };
}

function normalizeCitationRoute(value, index) {
  const source = isRecord(value) ? value : {};
  const route = ['cited', 'needsSource', 'labelAsInference', 'remove', 'reviewConflict'].includes(source.route) ? source.route : 'needsSource';
  return {
    claimId: safeCreationId(source.claimId || source.id || `claim-${index + 1}-${source.claim || ''}`, 'claim'),
    claim: stringValue(source.claim, '未命名声明', 4000),
    route,
    sourceRefIds: uniqueStrings(source.sourceRefIds),
    reason: stringValue(source.reason, '', 1000) || null,
  };
}

function normalizeAnnotation(value, index) {
  const source = isRecord(value) ? value : {};
  const range = isRecord(source.range) ? source.range : {};
  const start = boundedInteger(range.start, 0, 0, Number.MAX_SAFE_INTEGER);
  return {
    id: safeCreationId(source.id || `annotation-${index + 1}-${source.code || ''}`, 'annotation'),
    code: stringValue(source.code, 'writing.review', 100).toLowerCase().replace(/[^a-z0-9._-]+/gu, '-').replace(/^[^a-z]+/u, 'writing.'),
    severity: ['info', 'warning', 'error'].includes(source.severity) ? source.severity : 'warning',
    message: stringValue(source.message, '需要复核此处内容。', 2000),
    range: {
      start,
      end: boundedInteger(range.end, start, start, Number.MAX_SAFE_INTEGER),
    },
    suggestion: stringValue(source.suggestion, '', 4000) || null,
  };
}

function normalizeEvaluation(value) {
  const source = isRecord(value) ? value : {};
  return {
    status: ['pending', 'passed', 'failed'].includes(source.status) ? source.status : 'pending',
    gates: (Array.isArray(source.gates) ? source.gates : []).map((gate, index) => ({
      id: stringValue(gate?.id, `gate-${index + 1}`, 100).toLowerCase().replace(/[^a-z0-9._-]+/gu, '-').replace(/^[^a-z]+/u, 'gate-'),
      status: ['pass', 'warn', 'fail', 'skip'].includes(gate?.status) ? gate.status : 'skip',
      deterministic: gate?.deterministic !== false,
      detail: stringValue(gate?.detail, '', 2000),
    })),
    score: Math.max(0, Math.min(100, Number.isFinite(Number(source.score)) ? Number(source.score) : 0)),
  };
}

function normalizeReportArtifacts(value) {
  return (Array.isArray(value) ? value : []).flatMap((artifact) => {
    if (!isRecord(artifact) || !['json', 'markdown', 'sarif'].includes(artifact.format) || !stringValue(artifact.relativePath)) return [];
    return [{
      format: artifact.format,
      relativePath: stringValue(artifact.relativePath, '', 2048),
      contentHash: validHash(artifact.contentHash),
    }];
  });
}

export async function createWritingRun(documentValue, strategyValue = {}, options = {}) {
  const document = normalizeCreationDocument(documentValue, { compatibilityAliases: false });
  if (/data:image\/[a-z0-9.+-]+;base64,/iu.test(document.canonicalMarkdown)) {
    throw new Error('WritingRun 不接受内嵌 Base64 图片；请先将图片保存为耐久素材并使用相对路径引用');
  }
  const strategy = normalizeWritingRunStrategy(strategyValue);
  if (strategy.scope === 'bounded' && (!strategy.selection || strategy.selection.end <= strategy.selection.start)) {
    throw new Error('Bounded writing scope requires a non-empty selection');
  }
  if (strategy.selection && !strategy.selection.selectedHash) {
    strategy.selection.selectedHash = await sha256Text(
      document.canonicalMarkdown.slice(strategy.selection.start, strategy.selection.end),
      options,
    );
  }
  const now = validDateTime(options.startedAt, new Date().toISOString());
  const inputHash = await sha256Text(document.canonicalMarkdown, { ...options, hash: options.inputHash });
  const seed = `${document.id}:${document.revision}:${strategy.action}:${strategy.scope}:${now}`;
  return {
    schemaVersion: '1.0',
    id: safeCreationId(options.id || `writing-${seed}`, 'writing'),
    documentId: document.id,
    documentRevision: document.revision,
    state: ['queued', 'running', 'awaitingReview', 'succeeded', 'failed', 'cancelled'].includes(options.state) ? options.state : 'queued',
    action: strategy.action,
    scope: strategy.scope,
    selection: strategy.scope === 'bounded' ? strategy.selection : (strategy.selection || null),
    scenePackId: strategy.scenePackId,
    patternId: strategy.patternId,
    voiceId: strategy.voiceId,
    purposePresetId: strategy.purposePresetId,
    brandProfileId: strategy.brandProfileId,
    iteration: strategy.iteration,
    maxIterations: strategy.maxIterations,
    policy: strategy.policy,
    inputHash,
    outputHash: validHash(options.outputHash),
    factLedger: (Array.isArray(options.factLedger) ? options.factLedger : []).map(normalizeFact),
    citationRouting: (Array.isArray(options.citationRouting) ? options.citationRouting : []).map(normalizeCitationRoute),
    annotations: (Array.isArray(options.annotations) ? options.annotations : []).map(normalizeAnnotation),
    evaluation: normalizeEvaluation(options.evaluation),
    reportArtifacts: normalizeReportArtifacts(options.reportArtifacts),
    startedAt: now,
    completedAt: validDateTime(options.completedAt, null),
    failureReason: stringValue(options.failureReason, '', 4000) || null,
  };
}

export const buildWritingRunRequest = createWritingRun;

export function createWritingCandidateDocument(documentValue, options = {}) {
  const document = normalizeCreationDocument(documentValue, { compatibilityAliases: false });
  const canonicalMarkdown = typeof options.canonicalMarkdown === 'string'
    ? options.canonicalMarkdown
    : document.canonicalMarkdown;
  if (!canonicalMarkdown.trim()) throw new Error('Writing candidate document requires non-empty Markdown');
  return normalizeCreationDocument({
    ...document,
    title: stringValue(options.title, document.title, 240),
    revision: document.revision + 1,
    canonicalMarkdown,
    provenance: {
      ...document.provenance,
      createdBy: 'assistant',
      derivation: 'revised',
      modelRunIds: mergeWritingModelRunIds(document.provenance?.modelRunIds, options.traceIds),
    },
  }, { compatibilityAliases: false });
}

function writingTokenFacts(markdown) {
  const facts = new Map();
  const collect = (pattern, predicate, kind) => {
    for (const match of String(markdown || '').matchAll(pattern)) {
      const token = String(match[0] || '').trim();
      const key = `${kind}:${token}`;
      if (!token) continue;
      const existing = facts.get(key);
      if (existing) existing.occurrences += 1;
      else facts.set(key, { token, predicate, kind, occurrences: 1 });
    }
  };
  collect(/(?:v\d+(?:\.\d+){1,3}|\d+(?:\.\d+)?\s*(?:%|％|ms|毫秒|秒|分钟|小时|天|周|月|年|元|万元|亿元|KB|MB|GB|TB|倍|个|条|篇|人|次)?)/giu, '包含受保护数字', 'number');
  collect(/https?:\/\/[^\s)\]}>]+/giu, '包含受保护链接', 'reference');
  collect(/\[\[[^\]]+\]\]/gu, '包含受保护 Wiki Link', 'reference');
  collect(/\[\^[^\]]+\]|\[[0-9]{1,3}\]/gu, '包含受保护引用', 'reference');
  collect(/`[^`\n]+`/gu, '包含受保护代码标记', 'reference');
  return [...facts.values()];
}

function sourceRangeOverlaps(block, selection) {
  if (!selection) return true;
  return Number(block?.sourceRange?.end || 0) > selection.start
    && Number(block?.sourceRange?.start || 0) < selection.end;
}

function groundedStatus(verdict) {
  if (verdict === 'supported') return 'sourced';
  if (verdict === 'uncertain') return 'conflicted';
  return 'unsourced';
}

function citationRouteFor(block) {
  if (block.verdict === 'supported' && block.sourceRefIds.length) return 'cited';
  if (block.verdict === 'unsupported') return 'remove';
  if (block.verdict === 'uncertain') return block.sourceRefIds.length ? 'reviewConflict' : 'needsSource';
  return 'needsSource';
}

export function deriveWritingRunLedgers(documentValue, options = {}) {
  const document = normalizeCreationDocument(documentValue, { compatibilityAliases: false });
  const selection = isRecord(options.selection) ? normalizeSelection(options.selection) : null;
  const markdown = typeof options.markdown === 'string' ? options.markdown : document.canonicalMarkdown;
  const factLedger = writingTokenFacts(markdown).map((fact, index) => ({
    id: safeCreationId(`fact-token-${index + 1}`, 'fact'),
    subject: '创作正文',
    predicate: fact.predicate,
    object: fact.token,
    qualifiers: { kind: fact.kind, occurrences: fact.occurrences },
    status: document.sourceRefs.length ? 'sourced' : 'unsourced',
    protected: true,
    evidenceRefIds: [],
  }));
  const citationRouting = [];
  for (const [index, groundedBlock] of document.groundingLedger.blocks.entries()) {
    const block = document.blocks.find((item) => item.id === groundedBlock.id);
    if (block && !sourceRangeOverlaps(block, selection)) continue;
    const claim = block
      ? document.canonicalMarkdown.slice(block.sourceRange.start, block.sourceRange.end).trim().slice(0, 4000)
      : `正文块 ${groundedBlock.id}`;
    factLedger.push({
      id: safeCreationId(`fact-grounding-${index + 1}`, 'fact'),
      subject: claim || `正文块 ${groundedBlock.id}`,
      predicate: '由本地来源支撑',
      object: groundedBlock.sourceRefIds.join('、') || '尚无可复核来源',
      qualifiers: { kind: 'grounding-relation', blockId: groundedBlock.id, verdict: groundedBlock.verdict },
      status: groundedStatus(groundedBlock.verdict),
      protected: groundedBlock.verdict === 'supported',
      evidenceRefIds: groundedBlock.sourceRefIds,
    });
    const route = citationRouteFor(groundedBlock);
    citationRouting.push({
      claimId: safeCreationId(`claim-${groundedBlock.id}`, 'claim'),
      claim: claim || `正文块 ${groundedBlock.id}`,
      route,
      sourceRefIds: groundedBlock.sourceRefIds,
      reason: route === 'cited'
        ? '当前正文块已绑定可复核来源。'
        : route === 'remove'
          ? '当前正文块在证据核验中被判定为不受支持。'
          : route === 'reviewConflict'
            ? '来源与当前正文块存在不确定性，需要人工复核。'
            : '当前正文块需要补充可复核来源。',
    });
  }
  return {
    factLedger: factLedger.map(normalizeFact),
    citationRouting: citationRouting.map(normalizeCitationRoute),
  };
}

function evaluationGate(id, status, detail) {
  return { id, status, deterministic: true, detail };
}

function literalOccurrenceCount(value, token) {
  const source = String(value || '');
  const target = String(token || '');
  if (!target) return 0;
  let count = 0;
  let offset = 0;
  while (offset <= source.length - target.length) {
    const index = source.indexOf(target, offset);
    if (index < 0) break;
    count += 1;
    offset = index + target.length;
  }
  return count;
}

function missingProtectedFactOccurrences(factLedger, revised) {
  const requirements = new Map();
  for (const fact of factLedger || []) {
    const kind = fact?.qualifiers?.kind;
    const token = String(fact?.object || '');
    if (!fact?.protected || !['number', 'reference'].includes(kind) || !token) continue;
    const key = `${kind}:${token}`;
    const required = boundedInteger(fact.qualifiers?.occurrences, 1, 1, 5000);
    const current = requirements.get(key) || { kind, token, required: 0 };
    current.required += required;
    requirements.set(key, current);
  }
  return [...requirements.values()].flatMap((requirement) => {
    const missing = Math.max(0, requirement.required - literalOccurrenceCount(revised, requirement.token));
    return missing ? [{ ...requirement, missing }] : [];
  });
}

export async function evaluateWritingCandidate(runValue, candidate = {}, options = {}) {
  if (!isRecord(runValue)) throw new TypeError('Writing candidate evaluation requires a WritingRun');
  const original = String(candidate.original || '');
  const revised = String(candidate.revised || '');
  const missingFacts = missingProtectedFactOccurrences(runValue.factLedger, revised);
  const missingFactCount = missingFacts.reduce((total, fact) => total + fact.missing, 0);
  const protectedTokens = uniqueStrings(candidate.protectedTokens);
  const missingProtectedTokens = protectedTokens.filter((token) => !revised.includes(token));
  const unresolvedRoutes = (runValue.citationRouting || []).filter((route) => route.route !== 'cited');
  const groundingRelations = (runValue.factLedger || []).filter((fact) => fact.qualifiers?.kind === 'grounding-relation');
  const lengthRatio = original.length ? revised.length / original.length : 0;
  const gates = [
    evaluationGate('output.nonempty', revised.trim() ? 'pass' : 'fail', revised.trim() ? '模型返回了非空候选正文。' : '模型没有返回候选正文。'),
    evaluationGate('output.changed', revised !== original ? 'pass' : 'warn', revised !== original ? '候选正文与输入不同。' : '候选正文与输入完全相同。'),
    evaluationGate('facts.protected', missingFactCount ? 'fail' : 'pass', missingFactCount ? `候选缺少 ${missingFactCount} 处受保护数字或引用。` : '受保护数字与引用均按出现次数保留。'),
    evaluationGate('structure.protected', missingProtectedTokens.length ? 'fail' : 'pass', missingProtectedTokens.length ? `候选缺少 ${missingProtectedTokens.length} 个结构保护标记。` : '结构保护标记均保留。'),
    evaluationGate('length.bounded', revised.trim() && lengthRatio >= 0.2 && lengthRatio <= 3 ? 'pass' : 'fail', original.length ? `候选长度为输入的 ${(lengthRatio * 100).toFixed(1)}%。` : '缺少可比较的输入正文。'),
    evaluationGate('citations.routed', unresolvedRoutes.length ? 'warn' : 'pass', unresolvedRoutes.length ? `${unresolvedRoutes.length} 条声明仍需补来源、删除或人工复核。` : '引用分流没有未解决项。'),
    evaluationGate('grounding.reverify', groundingRelations.length ? 'warn' : 'skip', groundingRelations.length ? '改写候选尚未重新执行逐块证据核验；接受后原核验账本将失效。' : '当前正文没有已核验的 grounding 关系。'),
  ];
  const failed = gates.filter((gate) => gate.status === 'fail');
  const warnings = gates.filter((gate) => gate.status === 'warn');
  const score = Math.max(0, 100 - failed.length * 30 - warnings.length * 8);
  const completedAt = validDateTime(options.completedAt, new Date().toISOString());
  const annotations = gates.filter((gate) => ['warn', 'fail'].includes(gate.status)).map((gate, index) => normalizeAnnotation({
    id: `annotation-${index + 1}-${gate.id}`,
    code: `writing.${gate.id}`,
    severity: gate.status === 'fail' ? 'error' : 'warning',
    message: gate.detail,
    range: { start: 0, end: revised.length },
  }, index));
  const outputHash = revised.trim() ? await sha256Text(revised, options) : null;
  return {
    ...cloneWritingRun(runValue),
    state: failed.length ? 'failed' : 'awaitingReview',
    outputHash,
    annotations,
    evaluation: { status: failed.length ? 'failed' : 'passed', gates, score },
    completedAt: failed.length ? completedAt : null,
    failureReason: failed.length ? failed.map((gate) => gate.detail).join('；').slice(0, 4000) : null,
  };
}

function cloneWritingRun(value) {
  return JSON.parse(JSON.stringify(value));
}

export function completeWritingRunReview(runValue, { accepted, reason = '', completedAt = new Date().toISOString() } = {}) {
  if (!isRecord(runValue) || runValue.state !== 'awaitingReview') throw new Error('WritingRun is not awaiting review');
  return {
    ...cloneWritingRun(runValue),
    state: accepted ? 'succeeded' : 'cancelled',
    completedAt: validDateTime(completedAt, new Date().toISOString()),
    failureReason: accepted ? null : stringValue(reason, '用户放弃候选', 4000),
  };
}

export function writingRunCanIterate(value) {
  return isRecord(value)
    && boundedInteger(value.iteration, 1, 1, MAX_WRITING_ITERATIONS) < boundedInteger(value.maxIterations, MAX_WRITING_ITERATIONS, 1, MAX_WRITING_ITERATIONS)
    && !['failed', 'cancelled'].includes(value.state);
}

export function advanceWritingIteration(value, changes = {}) {
  if (!writingRunCanIterate(value)) throw new Error('Writing run reached its iteration limit');
  return {
    ...value,
    ...changes,
    iteration: Number(value.iteration) + 1,
    state: changes.state || 'queued',
    outputHash: validHash(changes.outputHash),
    completedAt: null,
    failureReason: null,
  };
}

export function createWritingStrategyViewModel(value = {}) {
  const strategy = normalizeWritingRunStrategy(value);
  const scopeLabels = {
    structural: '结构级',
    bounded: '限定范围',
    in_place: '原位改写',
  };
  return {
    strategy,
    scopeLabel: scopeLabels[strategy.scope],
    annotateOnly: strategy.action === 'annotate',
    allowsTextChanges: strategy.policy.allowTextChanges,
    iterationLabel: `${strategy.iteration}/${strategy.maxIterations}`,
    remainingIterations: strategy.maxIterations - strategy.iteration,
    preserved: [
      strategy.policy.preserveFacts && '事实',
      strategy.policy.preserveRelations && '关系',
      strategy.policy.preserveNumbers && '数字',
      strategy.policy.preserveCitations && '引用',
    ].filter(Boolean),
  };
}

export function writingRunContext(value) {
  const strategy = normalizeWritingRunStrategy(value);
  return {
    action: strategy.action,
    scope: strategy.scope,
    selection: strategy.selection,
    scenePackId: strategy.scenePackId,
    patternId: strategy.patternId,
    voiceId: strategy.voiceId,
    purposePresetId: strategy.purposePresetId,
    brandProfileId: strategy.brandProfileId,
    iteration: strategy.iteration,
    maxIterations: strategy.maxIterations,
    immutablePolicy: {
      ...strategy.policy,
      allowUnsupportedClaims: false,
    },
  };
}
