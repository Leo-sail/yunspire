function isRecord(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

function validHash(value) {
  return /^sha256:[a-f0-9]{64}$/u.test(String(value || '')) ? String(value) : null;
}

function stringValue(value, fallback = '') {
  const candidate = typeof value === 'string' || typeof value === 'number' ? String(value).trim() : '';
  return candidate || fallback;
}

function attachmentAssetId(value) {
  if (!isRecord(value)) return '';
  return stringValue(value.assetId || value.asset?.assetId || value.durableAsset?.assetId || value.id);
}

function descriptorUpdatedAt(value) {
  const timestamp = Date.parse(value?.updatedAt || value?.finalizedAt || value?.createdAt || '');
  return Number.isFinite(timestamp) ? timestamp : 0;
}

function isDescriptorForDocument(value, documentId) {
  if (!isRecord(value) || !stringValue(value.assetId) || value.ownerType !== 'creation_asset') return false;
  return !documentId || stringValue(value.ownerId) === documentId;
}

function documentAssetAttachment(assetValue, existingValue = {}) {
  const asset = isRecord(assetValue) ? assetValue : {};
  const existing = isRecord(existingValue) ? existingValue : {};
  const id = stringValue(asset.id || attachmentAssetId(existing));
  const contentHash = validHash(asset.contentHash || existing.contentHash || existing.sha256);
  return {
    ...existing,
    id,
    name: stringValue(asset.name, stringValue(existing.name, id || 'asset')),
    mimeType: stringValue(asset.mimeType, stringValue(existing.mimeType)),
    byteLength: Math.max(0, Number(existing.byteLength || 0)),
    alt: stringValue(asset.alt, stringValue(existing.alt, asset.name || existing.name || '')),
    caption: stringValue(asset.caption, stringValue(existing.caption)),
    relativePath: stringValue(asset.relativePath, stringValue(existing.relativePath)) || null,
    state: ['draft', 'local', 'localized', 'upload_required', 'ready', 'failed'].includes(asset.state)
      ? asset.state
      : existing.state || 'draft',
    ...(contentHash ? { contentHash, sha256: contentHash } : {}),
    ...(asset.width == null ? {} : { width: Math.max(1, Number(asset.width)) }),
    ...(asset.height == null ? {} : { height: Math.max(1, Number(asset.height)) }),
  };
}

export function applyCreationDurableDescriptor(attachmentValue, descriptorValue) {
  const attachment = isRecord(attachmentValue) ? attachmentValue : {};
  const descriptor = isRecord(descriptorValue) ? descriptorValue : {};
  if (!descriptor.assetId) throw new TypeError('Creation durable attachment descriptor requires assetId');
  const ready = descriptor.state === 'ready';
  const failed = ['deleted', 'failed', 'source_missing'].includes(descriptor.state);
  return {
    ...attachment,
    assetId: String(descriptor.assetId),
    assetState: String(descriptor.state || 'draft'),
    byteLength: Math.max(0, Number(descriptor.byteLength ?? attachment.byteLength ?? 0)),
    sha256: validHash(descriptor.sha256),
    relativePath: String(descriptor.relativePath || attachment.relativePath || '').trim() || null,
    state: ready ? 'ready' : (failed ? 'failed' : attachment.state || 'draft'),
    asset: descriptor,
    durableAsset: descriptor,
  };
}

/**
 * Rebuild the attachment records for one persisted CreationDocument revision.
 *
 * The document asset manifest is the version authority: descriptors that are
 * no longer referenced by that revision are deliberately excluded. The native
 * durable registry is the byte/state authority and is filtered by both owner
 * type and document identity. No aggregate item cap is applied.
 */
export function rebuildCreationAttachments({
  documentId: documentIdValue = '',
  documentAssets: documentAssetsValue = [],
  durableDescriptors: durableDescriptorsValue,
  existingAttachments: existingAttachmentsValue = [],
} = {}) {
  const documentId = stringValue(documentIdValue);
  const documentAssets = Array.isArray(documentAssetsValue) ? documentAssetsValue.filter(isRecord) : [];
  const existingAttachments = Array.isArray(existingAttachmentsValue) ? existingAttachmentsValue.filter(isRecord) : [];
  const registryProvided = Array.isArray(durableDescriptorsValue);
  const durableDescriptors = registryProvided ? durableDescriptorsValue.filter(isRecord) : [];

  const existingById = new Map();
  for (const attachment of existingAttachments) {
    const assetId = attachmentAssetId(attachment);
    if (assetId && !existingById.has(assetId)) existingById.set(assetId, attachment);
  }

  const descriptorById = new Map();
  for (const descriptor of durableDescriptors) {
    if (!isDescriptorForDocument(descriptor, documentId)) continue;
    const assetId = stringValue(descriptor.assetId);
    const previous = descriptorById.get(assetId);
    if (!previous || descriptorUpdatedAt(descriptor) >= descriptorUpdatedAt(previous)) descriptorById.set(assetId, descriptor);
  }

  const manifest = documentAssets.length
    ? documentAssets
    : existingAttachments.map((attachment) => ({
      id: attachmentAssetId(attachment),
      name: attachment.name,
      mimeType: attachment.mimeType,
      relativePath: attachment.relativePath,
      contentHash: attachment.contentHash || attachment.sha256,
      alt: attachment.alt,
      caption: attachment.caption,
      state: attachment.state,
      width: attachment.width,
      height: attachment.height,
    }));
  const seen = new Set();
  const attachments = [];
  for (const asset of manifest) {
    const assetId = stringValue(asset.id);
    if (!assetId || seen.has(assetId)) continue;
    seen.add(assetId);
    const existing = existingById.get(assetId) || {};
    let attachment = documentAssetAttachment(asset, existing);
    const descriptor = descriptorById.get(assetId)
      || (!registryProvided && isDescriptorForDocument(existing.asset || existing.durableAsset, documentId)
        ? (existing.asset || existing.durableAsset)
        : null);
    if (descriptor) {
      attachment = applyCreationDurableDescriptor(attachment, descriptor);
      // CreationDocument.assets owns version-specific semantic placement. The
      // durable descriptor relativePath points at the private asset store and
      // must not overwrite a localized Vault path recorded by the document.
      const semanticPath = stringValue(asset.relativePath, stringValue(existing.relativePath));
      if (semanticPath) attachment.relativePath = semanticPath;
    } else {
      delete attachment.asset;
      delete attachment.durableAsset;
      delete attachment.assetId;
      delete attachment.assetState;
    }
    attachments.push(attachment);
  }
  return attachments;
}

export function creationAssetFromAttachment(attachmentValue, descriptorValue = null) {
  const attachment = isRecord(attachmentValue) ? attachmentValue : {};
  const descriptor = isRecord(descriptorValue)
    ? descriptorValue
    : isRecord(attachment.asset) ? attachment.asset
      : isRecord(attachment.durableAsset) ? attachment.durableAsset
        : null;
  const descriptorReady = descriptor?.state === 'ready';
  const relativePath = String(attachment.relativePath || descriptor?.relativePath || '').trim() || null;
  const state = descriptorReady
    ? 'ready'
    : ['draft', 'local', 'localized', 'upload_required', 'ready', 'failed'].includes(attachment.state)
      ? attachment.state
      : relativePath ? 'local' : 'draft';
  return {
    id: String(attachment.assetId || descriptor?.assetId || attachment.id || ''),
    kind: String(attachment.mimeType || descriptor?.mimeType || '').startsWith('image/') ? 'image' : 'file',
    name: String(attachment.name || descriptor?.fileName || attachment.id || 'asset'),
    mimeType: String(attachment.mimeType || descriptor?.mimeType || ''),
    relativePath,
    contentHash: validHash(descriptor?.sha256 || attachment.sha256),
    alt: String(attachment.alt || attachment.name || ''),
    caption: String(attachment.caption || ''),
    state,
  };
}
