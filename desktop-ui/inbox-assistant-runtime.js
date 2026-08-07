function durableDescriptor(value) {
  if (!value || typeof value !== 'object') return null;
  const descriptor = value.asset?.assetId
    ? value.asset
    : value.durableAsset?.assetId
      ? value.durableAsset
      : value.assetId
        ? {
          assetId: value.assetId,
          state: value.assetState || 'ready',
          byteLength: value.byteLength || value.size || 0,
          mimeType: value.mimeType || value.type || 'application/octet-stream',
          fileName: value.name || 'asset',
        }
        : null;
  return descriptor?.assetId ? descriptor : null;
}

function safePathSegment(value, fallback = '未命名附件') {
  return String(value || fallback)
    .replace(/[\\/:*?"<>|#%{}\[\]\u0000-\u001f]/gu, '-')
    .replace(/\s+/gu, ' ')
    .trim()
    .slice(-160) || fallback;
}

function yamlString(value) {
  return JSON.stringify(String(value ?? ''));
}

export function inboxOriginalAssetRelativePath(item, storageStem, inboxOnly = false) {
  const descriptor = durableDescriptor(item);
  if (!descriptor?.assetId || descriptor.state !== 'ready') return '';
  const folder = inboxOnly ? '收件箱/入站/附件' : '资料库/附件/收件箱';
  const assetPrefix = String(descriptor.assetId).replace(/[^a-z0-9._-]/giu, '-').slice(0, 24) || 'asset';
  const fileName = safePathSegment(item?.name || item?.title || descriptor.fileName);
  return `${folder}/${String(storageStem || '').replace(/^\/+|\/+$/gu, '')}.assets/${assetPrefix}-${fileName}`;
}

export function buildInboxFaithfulOriginalMarkdown({
  title,
  source,
  sourceType,
  receivedAt,
  content,
  contentHash,
  assetRelativePath = '',
  assetMimeType = '',
  assetId = '',
  assetSha256 = '',
} = {}) {
  const normalizedTitle = String(title || '收件箱内容').replace(/[\r\n]+/gu, ' ').trim() || '收件箱内容';
  const body = String(content ?? '');
  const attachmentLink = assetRelativePath
    ? `${String(assetMimeType).startsWith('image/') ? '!' : ''}[[${assetRelativePath}]]`
    : '';
  return [
    '---',
    'yunspire_faithful_original: true',
    `source_type: ${yamlString(sourceType || 'text')}`,
    `source: ${yamlString(source || '本地入站')}`,
    `received_at: ${yamlString(receivedAt || '')}`,
    `content_hash: ${yamlString(contentHash || '')}`,
    ...(assetId ? [`asset_id: ${yamlString(assetId)}`] : []),
    ...(assetSha256 ? [`asset_sha256: ${yamlString(assetSha256)}`] : []),
    '---',
    '',
    `# ${normalizedTitle}`,
    '',
    '## 忠实提取内容',
    '',
    body || '该入站项没有可提取的文本；原始附件仍按字节保存在下方路径。',
    ...(attachmentLink ? ['', '## 原始附件', '', attachmentLink] : []),
    '',
  ].join('\n');
}

export function assertInboxDualVaultTargets(rawTarget, agentTarget) {
  const rawVaultId = String(rawTarget?.id || '').trim();
  const agentVaultId = String(agentTarget?.id || '').trim();
  if (!rawVaultId || !agentVaultId) throw new Error('收件箱入库缺少可写的原文库或 Agent 分析库');
  if (rawVaultId === agentVaultId) {
    throw new Error('收件箱双库入库需要独立的用户原文 Vault；当前只有 Agent 库可用，请连接或选择一个可读写的用户 Vault');
  }
  return { rawTarget, agentTarget };
}

export function inboxAtomicCommitInput(pendingWrite) {
  const noteApprovalIds = (pendingWrite?.previews || [pendingWrite])
    .map((preview) => String(preview?.approvalId || '').trim())
    .filter(Boolean);
  const assetApprovalIds = (pendingWrite?.assetPreviews || [])
    .map((preview) => String(preview?.approvalId || '').trim())
    .filter(Boolean);
  if (noteApprovalIds.length < 2) throw new Error('收件箱双库入库缺少忠实原文或 Agent 分析稿预览');
  return { noteApprovalIds, assetApprovalIds, batchKind: 'capture' };
}

export function inboxAnalysisSourceReference(rawTarget, rawPath) {
  const vaultName = String(rawTarget?.name || '').trim();
  const relativePath = String(rawPath || '').trim();
  if (!vaultName || !relativePath) throw new Error('Agent 分析稿缺少忠实原文的 Vault 或路径');
  return `${vaultName} · ${relativePath}`;
}

export function markInboxPostCommitCleanupWarning(item, cleanupSucceeded) {
  if (cleanupSucceeded) return { ...item };
  return {
    ...item,
    status: 'processed',
    captureState: 'committed',
    stateSyncWarning: '忠实原文和分析稿已提交，但临时正文耐久资产清理失败，系统将在后续维护中重试',
  };
}

export function assistantAttachmentsHaveVolatileContent(attachments = []) {
  return (Array.isArray(attachments) ? attachments : []).some((attachment) => {
    const descriptor = durableDescriptor(attachment);
    return !descriptor?.assetId || descriptor.state !== 'ready';
  });
}

export function createInboxItemFromAssistantAttachment(attachment, message, receivedAt, createId) {
  const descriptor = durableDescriptor(attachment);
  return {
    id: createId(),
    title: attachment.name,
    source: `AI助手附件 · ${attachment.name}`,
    type: attachment.kind === 'screenshot' || String(attachment.type || '').startsWith('image/') ? 'image' : 'file',
    categories: [],
    classificationPath: '',
    status: 'pending',
    receivedAt,
    content: message || '',
    attachmentId: attachment.id,
    ...(descriptor ? {
      assetId: descriptor.assetId,
      assetState: descriptor.state || 'ready',
      asset: descriptor,
      durableAsset: descriptor,
    } : {}),
  };
}

export function captureDiagnostics(values = [], perEntryCharacters = 800) {
  const normalized = (Array.isArray(values) ? values : [])
    .map((value) => String(value).trim())
    .filter(Boolean);
  const entries = normalized.map((value) => value.slice(0, perEntryCharacters));
  return {
    entries,
    entryCount: normalized.length,
    truncatedEntryCount: normalized.reduce(
      (total, value) => total + Number(value.length > perEntryCharacters),
      0,
    ),
  };
}
