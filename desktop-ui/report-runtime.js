const REPORT_STATES = new Set([
  'preview',
  'awaiting_approval',
  'writing',
  'persisted',
  'failed',
  'cancelled',
]);

export function normalizeReportState(value, fallback = 'preview') {
  return REPORT_STATES.has(value) ? value : fallback;
}

export function reportStatePresentation(value) {
  const state = normalizeReportState(value);
  return {
    preview: ['预览', 'neutral'],
    awaiting_approval: ['等待确认', 'warning'],
    writing: ['写入中', 'info'],
    persisted: ['已归档', 'success'],
    failed: ['失败', 'danger'],
    cancelled: ['已取消', 'neutral'],
  }[state];
}

export function compactReportBodyAsset(value) {
  if (!value?.assetId) return null;
  return {
    assetId: String(value.assetId),
    ownerType: String(value.ownerType || 'report'),
    ownerId: String(value.ownerId || ''),
    role: String(value.role || 'markdown'),
    fileName: String(value.fileName || 'report.md'),
    mimeType: String(value.mimeType || 'text/markdown;charset=utf-8'),
    state: String(value.state || ''),
    byteLength: Math.max(0, Number(value.byteLength || 0)),
    sha256: value.sha256 || null,
    createdAt: value.createdAt || null,
    updatedAt: value.updatedAt || null,
    finalizedAt: value.finalizedAt || null,
  };
}

export function compactReportRecord(report) {
  if (!report?.id) throw new TypeError('报告记录缺少 id');
  const bodyAsset = compactReportBodyAsset(report.bodyAsset || report.durableAsset);
  if (!bodyAsset || bodyAsset.state !== 'ready') throw new TypeError('报告记录缺少已就绪的耐久正文');
  const compact = {
    ...report,
    state: normalizeReportState(report.state),
    bodyAsset,
    durableAsset: undefined,
    markdown: undefined,
    content: undefined,
  };
  delete compact.durableAsset;
  delete compact.markdown;
  delete compact.content;
  return compact;
}

export function preserveCommittedReportAfterFailure(report, commitResult, error, now = new Date()) {
  if (!report?.id) throw new TypeError('报告记录缺少 id');
  if (!commitResult?.relativePath && !report.committedAt && report.state !== 'persisted') {
    throw new TypeError('报告尚未提交，不能应用提交后告警');
  }
  const committedAt = commitResult?.committedAt || report.committedAt || (now instanceof Date ? now : new Date(now)).toISOString();
  return {
    ...report,
    localDestination: commitResult?.relativePath || report.localDestination || '',
    committedAt,
    state: 'persisted',
    lastError: '',
    stateSyncError: String(error || '提交后的状态同步失败'),
    updatedAt: committedAt,
  };
}

export async function loadAllNativeResourcePages(invoke, command, pageSize = 128) {
  if (typeof invoke !== 'function') throw new TypeError('原生分页缺少命令调用器');
  const items = [];
  let cursorUpdatedAt = null;
  let cursorId = null;
  const seenCursors = new Set();
  while (true) {
    const page = await invoke(command, {
      cursorUpdatedAt,
      cursorId,
      limit: Math.min(512, Math.max(1, Math.trunc(Number(pageSize) || 128))),
    });
    if (!page || !Array.isArray(page.items)) throw new Error(`${command} 返回了无效分页`);
    items.push(...page.items);
    if (!page.nextCursorUpdatedAt || !page.nextCursorId) break;
    const cursorKey = `${page.nextCursorUpdatedAt}\u0000${page.nextCursorId}`;
    if (seenCursors.has(cursorKey)) throw new Error(`${command} 返回了重复游标`);
    seenCursors.add(cursorKey);
    cursorUpdatedAt = page.nextCursorUpdatedAt;
    cursorId = page.nextCursorId;
  }
  return items;
}

export async function loadAllReportSourcePages(invoke, sourceKind, range, pageSize = 128) {
  if (typeof invoke !== 'function') throw new TypeError('报告数据分页缺少命令调用器');
  const startAt = new Date(range?.start).toISOString();
  const endAt = new Date(range?.end).toISOString();
  const items = [];
  let cursorOccurredAt = null;
  let cursorId = null;
  const seenCursors = new Set();
  while (true) {
    const page = await invoke('read_report_source_page', {
      sourceKind,
      startAt,
      endAt,
      cursorOccurredAt,
      cursorId,
      limit: Math.min(512, Math.max(1, Math.trunc(Number(pageSize) || 128))),
    });
    if (!page || !Array.isArray(page.items)) throw new Error('read_report_source_page 返回了无效分页');
    items.push(...page.items);
    if (!page.nextCursorOccurredAt || !page.nextCursorId) break;
    const cursorKey = `${page.nextCursorOccurredAt}\u0000${page.nextCursorId}`;
    if (seenCursors.has(cursorKey)) throw new Error('read_report_source_page 返回了重复游标');
    seenCursors.add(cursorKey);
    cursorOccurredAt = page.nextCursorOccurredAt;
    cursorId = page.nextCursorId;
  }
  return items;
}

export function reportOccurrenceId(subscriptionId, scheduledFor) {
  const id = String(subscriptionId || '').trim();
  const timestamp = new Date(scheduledFor).toISOString();
  if (!id) throw new TypeError('报告订阅缺少 id');
  return `${id}:${timestamp}`;
}

export function reportRetryDelayMs(attemptValue) {
  const attempt = Math.max(1, Math.trunc(Number(attemptValue) || 1));
  return Math.min(6 * 60 * 60 * 1000, 60_000 * (2 ** Math.min(16, attempt - 1)));
}

export function reportRetryAt(attempt, now = new Date()) {
  const base = now instanceof Date ? now : new Date(now);
  if (!Number.isFinite(base.getTime())) throw new TypeError('报告重试基准时间无效');
  return new Date(base.getTime() + reportRetryDelayMs(attempt)).toISOString();
}
