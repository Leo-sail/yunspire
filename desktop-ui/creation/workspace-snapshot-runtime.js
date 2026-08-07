import {
  compactCreationWritingRunReference,
  createLightweightCreationCheckpoint,
} from './checkpoint-runtime.js';

function isRecord(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

function stringValue(value, fallback = '') {
  return typeof value === 'string' || typeof value === 'number' ? String(value) : fallback;
}

function finiteNumber(value, fallback = 0) {
  const candidate = Number(value);
  return Number.isFinite(candidate) ? candidate : fallback;
}

function optional(value) {
  return value == null || value === '' ? undefined : value;
}

export function compactDurableAssetDescriptorForWorkspace(value) {
  if (!isRecord(value) || !value.assetId) return null;
  return {
    assetId: String(value.assetId),
    ownerType: stringValue(value.ownerType),
    ownerId: stringValue(value.ownerId),
    role: stringValue(value.role),
    fileName: stringValue(value.fileName),
    mimeType: stringValue(value.mimeType, 'application/octet-stream'),
    state: stringValue(value.state, 'ready'),
    byteLength: Math.max(0, finiteNumber(value.byteLength)),
    sha256: optional(value.sha256),
    relativePath: stringValue(value.relativePath),
    metadata: isRecord(value.metadata) ? compactMetadata(value.metadata) : {},
    createdAt: optional(value.createdAt),
    updatedAt: optional(value.updatedAt),
    finalizedAt: optional(value.finalizedAt),
  };
}

function compactMetadata(value, depth = 0) {
  if (depth > 12) return null;
  if (value == null || typeof value === 'boolean' || typeof value === 'number' || typeof value === 'string') return value;
  if (Array.isArray(value)) return value.map((item) => compactMetadata(item, depth + 1));
  if (!isRecord(value)) return null;
  const result = {};
  for (const [key, item] of Object.entries(value)) {
    const normalized = key.toLowerCase();
    if ([
      'contentbase64', 'database64', 'base64', 'dataurl', 'objecturl',
      'file', 'blob', 'canonicalmarkdown', 'creationdocument',
      'candidatedocument', 'basedocument',
    ].includes(normalized)) continue;
    result[key] = compactMetadata(item, depth + 1);
  }
  return result;
}

function compactMetadataOmitting(value, omittedKeys) {
  const source = isRecord(value) ? value : {};
  const filtered = Object.fromEntries(Object.entries(source).filter(([key]) => !omittedKeys.has(key)));
  return compactMetadata(filtered);
}

export function compactBeautifyRunForWorkspace(value) {
  if (!isRecord(value)) return null;
  return {
    title: stringValue(value.title),
    skill: stringValue(value.skill),
    completedAt: value.completedAt || null,
    status: stringValue(value.status),
    changed: value.changed === true,
  };
}

export function compactAttachmentForWorkspace(value) {
  const attachment = isRecord(value) ? value : {};
  const descriptor = compactDurableAssetDescriptorForWorkspace(attachment.asset || attachment.durableAsset);
  const assetId = descriptor?.assetId || stringValue(attachment.assetId);
  return {
    id: stringValue(attachment.id, assetId),
    name: stringValue(attachment.name, descriptor?.fileName || assetId || 'asset'),
    mimeType: stringValue(attachment.mimeType || attachment.type, descriptor?.mimeType || 'application/octet-stream'),
    byteLength: Math.max(0, finiteNumber(attachment.byteLength ?? attachment.size ?? descriptor?.byteLength)),
    alt: stringValue(attachment.alt),
    caption: stringValue(attachment.caption),
    state: stringValue(attachment.state, descriptor?.state || 'ready'),
    assetState: stringValue(attachment.assetState, descriptor?.state || ''),
    relativePath: optional(attachment.relativePath || descriptor?.relativePath),
    width: attachment.width == null ? undefined : Math.max(1, finiteNumber(attachment.width, 1)),
    height: attachment.height == null ? undefined : Math.max(1, finiteNumber(attachment.height, 1)),
    contentHash: optional(attachment.contentHash),
    sha256: optional(attachment.sha256 || descriptor?.sha256),
    assetId: optional(assetId),
    ...(descriptor ? { asset: descriptor } : {}),
  };
}

export function compactCreationVersionForWorkspace(value) {
  if (!isRecord(value)) return null;
  const assets = isRecord(value.assets)
    ? Object.fromEntries(Object.entries(value.assets).flatMap(([role, descriptor]) => {
      const compact = compactDurableAssetDescriptorForWorkspace(descriptor);
      return compact ? [[role, compact]] : [];
    }))
    : {};
  if (!Object.keys(assets).length) return null;
  return {
    createdAt: value.createdAt || null,
    revision: Math.max(1, finiteNumber(value.revision, 1)),
    documentId: stringValue(value.documentId),
    assets,
  };
}

export function compactCreationDocumentMetadataForWorkspace(value) {
  const metadata = isRecord(value) ? value : {};
  return {
    documentId: stringValue(metadata.documentId),
    vaultId: stringValue(metadata.vaultId),
    folder: stringValue(metadata.folder),
    updatedAt: metadata.updatedAt || null,
    createdAt: metadata.createdAt || null,
    lastSavedPath: optional(metadata.lastSavedPath),
    lastSavedAt: metadata.lastSavedAt || null,
    lastManualSourceRefId: optional(metadata.lastManualSourceRefId),
    lastManualSourceCapturedAt: metadata.lastManualSourceCapturedAt || null,
  };
}

export function compactCreationCheckpointForWorkspace(value) {
  if (!isRecord(value) || value.schemaVersion !== '1.0' || value.kind !== 'creationExecutionCheckpoint') return null;
  const checkpoint = createLightweightCreationCheckpoint(value, { includeWritingRun: true, compactWritingRun: true });
  const execution = isRecord(checkpoint.execution) ? checkpoint.execution : {};
  return {
    schemaVersion: checkpoint.schemaVersion,
    kind: checkpoint.kind,
    checkpointId: checkpoint.checkpointId || null,
    writingRun: checkpoint.writingRun,
    streamState: {
      streamId: checkpoint.streamState.streamId || null,
      operationId: checkpoint.streamState.operationId || null,
      capability: checkpoint.streamState.capability || null,
      status: checkpoint.streamState.status || 'idle',
      lastSequence: checkpoint.streamState.lastSequence,
      progress: checkpoint.streamState.progress || null,
    },
    execution: {
      documentId: execution.documentId || null,
      documentRevision: execution.documentRevision ?? null,
      documentTitle: execution.documentTitle || null,
      documentInputHash: execution.documentInputHash || null,
      sourceHash: execution.sourceHash || null,
      sourceAsset: execution.sourceAsset
        ? compactDurableAssetDescriptorForWorkspace(execution.sourceAsset)
        : null,
      scope: execution.scope || null,
      nextChunkIndex: execution.nextChunkIndex ?? 0,
      chunkCount: execution.chunkCount ?? 0,
      capability: execution.capability || null,
      recoverable: execution.recoverable === true,
      candidate: isRecord(execution.candidate) ? {
        kind: execution.candidate.kind || null,
        grounded: execution.candidate.grounded === true,
        scope: execution.candidate.scope || null,
        chunkCount: execution.candidate.chunkCount ?? null,
        createdAt: execution.candidate.createdAt || null,
        documentId: execution.candidate.documentId || null,
        documentRevision: execution.candidate.documentRevision ?? null,
        documentInputHash: execution.candidate.documentInputHash || null,
        runId: execution.candidate.runId || null,
        traceCount: Math.max(0, finiteNumber(execution.candidate.traceCount
          ?? (Array.isArray(execution.candidate.traceIds) ? execution.candidate.traceIds.length : 0))),
        allowIteration: execution.candidate.allowIteration !== false,
      } : null,
    },
    checkpointedAt: checkpoint.checkpointedAt,
  };
}

function compactInboxItem(value) {
  const item = isRecord(value) ? value : {};
  const safe = compactMetadata(item);
  const descriptor = compactDurableAssetDescriptorForWorkspace(item.asset || item.durableAsset);
  if (descriptor) {
    safe.assetId = descriptor.assetId;
    safe.asset = descriptor;
    safe.durableAsset = descriptor;
    safe.contentExcerpt = String(item.contentExcerpt || item.content || '').slice(0, 2_000);
    delete safe.content;
  }
  if (Array.isArray(item.attachments)) safe.attachments = item.attachments.map(compactAttachmentForWorkspace);
  return safe;
}

/**
 * Compacts the Creation-owned parts of a workspace `clientState` without
 * imposing an attachment-count or WritingRun-ledger-count limit. Full document
 * bodies and authoritative runtime records remain in durable assets/native
 * Creation tables instead of the 2 MiB clientState record.
 */
export function compactCreationClientStateForWorkspaceSnapshot(value, options = {}) {
  const source = isRecord(value) ? value : {};
  const documentMetadata = Object.fromEntries(Object.entries(isRecord(source.documentMetadata) ? source.documentMetadata : {})
    .map(([title, metadata]) => [title, compactCreationDocumentMetadataForWorkspace(metadata)]));
  const clientState = {
    ...compactMetadataOmitting(source, new Set([
      'documents',
      'creationDocuments',
      'documentMetadata',
      'documentVersions',
      'creationWritingRuns',
      'creationWritingCheckpoints',
      'inboxItems',
      'conversations',
      'lastBeautifyRun',
    ])),
    documents: {},
    creationDocuments: {},
    documentMetadata,
    // Native durable-asset indexes and Creation WritingRun tables are the
    // authoritative stores. Keeping these ledgers in clientState would copy
    // thousands of attachment descriptors and full run histories toward the
    // Rust 2 MiB safety boundary on every unrelated workspace save.
    documentVersions: {},
    creationWritingRuns: [],
    creationWritingCheckpoints: [],
    inboxItems: (Array.isArray(source.inboxItems) ? source.inboxItems : []).map(compactInboxItem),
  };
  const lastBeautifyRun = compactBeautifyRunForWorkspace(source.lastBeautifyRun);
  if (lastBeautifyRun) clientState.lastBeautifyRun = lastBeautifyRun;
  if (Array.isArray(source.conversations)) {
    clientState.conversations = source.conversations.map((conversation) => ({
      id: conversation?.id || '',
      title: conversation?.title || '',
      meta: conversation?.meta || '',
      context: conversation?.context || '',
      requestRevision: Math.max(0, finiteNumber(conversation?.requestRevision)),
    }));
  }
  delete clientState.externalOutbox;
  return clientState;
}
