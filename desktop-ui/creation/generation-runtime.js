import { sha256Text } from './writing-panel.js';

function isRecord(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

function gate(id, status, detail) {
  return { id, status, deterministic: true, detail };
}

function annotation(gateValue, revisedLength, index) {
  return {
    id: `annotation-${index + 1}-${gateValue.id}`,
    code: `writing.${gateValue.id}`,
    severity: gateValue.status === 'fail' ? 'error' : 'warning',
    message: gateValue.detail,
    range: { start: 0, end: revisedLength },
    suggestion: null,
  };
}

export async function evaluateCreationGenerationCandidate(runValue, candidate = {}, options = {}) {
  if (!isRecord(runValue) || !runValue.id || !runValue.documentId || !runValue.inputHash) {
    throw new TypeError('Creation generation evaluation requires a valid WritingRun');
  }
  const original = String(candidate.original || '');
  const revised = String(candidate.revised || '');
  const grounded = candidate.grounded === true;
  const sourceRefs = Array.isArray(candidate.sourceRefs) ? candidate.sourceRefs : [];
  const ledger = isRecord(candidate.groundingLedger) ? candidate.groundingLedger : { status: 'unverified', blocks: [] };
  const blocks = Array.isArray(ledger.blocks) ? ledger.blocks : [];
  const sourceIds = new Set(sourceRefs.map((source) => source?.id).filter(Boolean));
  const invalidGrounding = blocks.some((block) => (
    block?.verdict !== 'supported'
    || !Array.isArray(block.sourceRefIds)
    || !block.sourceRefIds.length
    || block.sourceRefIds.some((sourceId) => !sourceIds.has(sourceId))
    || !Array.isArray(block.evidence)
    || !block.evidence.length
    || block.evidence.some((evidence) => !sourceIds.has(evidence?.sourceRefId) || !String(evidence?.quote || '').trim())
  ));
  const groundingVerified = grounded
    && ledger.status === 'verified'
    && blocks.length > 0
    && sourceRefs.length > 0
    && !invalidGrounding;
  const minimumCharacters = Math.max(1, Number(options.minimumCharacters || 40));
  const gates = [
    gate('output.nonempty', revised.trim().length >= minimumCharacters ? 'pass' : 'fail', revised.trim().length >= minimumCharacters ? '模型返回了完整候选正文。' : `候选正文少于 ${minimumCharacters} 个字符。`),
    gate('output.changed', revised !== original ? 'pass' : 'fail', revised !== original ? '候选正文与当前草稿不同。' : '候选正文与当前草稿完全相同。'),
    gate('structure.heading', /^#\s+\S+/mu.test(revised) ? 'pass' : 'fail', /^#\s+\S+/mu.test(revised) ? '候选包含一级标题。' : '候选缺少一级标题。'),
    gate('security.embedded-binary', /data:image\/[a-z0-9.+-]+;base64,/iu.test(revised) ? 'fail' : 'pass', /data:image\/[a-z0-9.+-]+;base64,/iu.test(revised) ? '候选包含内嵌 Base64 图片，必须先转为耐久资产。' : '候选没有内嵌二进制图片。'),
    grounded
      ? gate('grounding.verified', groundingVerified ? 'pass' : 'fail', groundingVerified ? `已逐块核验 ${blocks.length} 个正文块并绑定 ${sourceRefs.length} 条本地来源。` : '候选没有通过完整的逐块本地证据核验。')
      : gate('grounding.unavailable', 'warn', '本地知识库没有匹配证据；接受前必须人工复核具体事实，模型不得把待核实内容表述为已证实。'),
  ];
  const failures = gates.filter((item) => item.status === 'fail');
  const warnings = gates.filter((item) => item.status === 'warn');
  const completedAt = options.completedAt || new Date().toISOString();
  return {
    ...structuredClone(runValue),
    state: failures.length ? 'failed' : 'awaitingReview',
    outputHash: revised.trim() ? await sha256Text(revised, options) : null,
    annotations: [...failures, ...warnings].map((item, index) => annotation(item, revised.length, index)),
    evaluation: {
      status: failures.length ? 'failed' : 'passed',
      gates,
      score: Math.max(0, 100 - failures.length * 30 - warnings.length * 10),
    },
    completedAt: failures.length ? completedAt : null,
    failureReason: failures.length ? failures.map((item) => item.detail).join('；').slice(0, 4000) : null,
  };
}
