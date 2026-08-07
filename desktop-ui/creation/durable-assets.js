const DEFAULT_UPLOAD_CHUNK_BYTES = 3 * 1024 * 1024;
const DEFAULT_READ_CHUNK_BYTES = 3 * 1024 * 1024;
const DEFAULT_TEXT_CHUNK_BYTES = 1024 * 1024;

function assertInvoke(invoke) {
  if (typeof invoke !== 'function') throw new TypeError('耐久资产操作缺少原生命令调用器');
}

function assertDescriptorInput(value) {
  if (!value || typeof value !== 'object') throw new TypeError('耐久资产描述不能为空');
  for (const key of ['ownerType', 'ownerId', 'fileName', 'mimeType']) {
    if (!String(value[key] || '').trim()) throw new TypeError(`耐久资产缺少 ${key}`);
  }
}

function positiveChunkSize(value, fallback) {
  const number = Number(value);
  return Number.isFinite(number) && number > 0 ? Math.min(4 * 1024 * 1024, Math.max(64 * 1024, Math.trunc(number))) : fallback;
}

export function bytesToBase64(bytesValue) {
  const bytes = bytesValue instanceof Uint8Array ? bytesValue : new Uint8Array(bytesValue || 0);
  let binary = '';
  for (let offset = 0; offset < bytes.length; offset += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(offset, Math.min(bytes.length, offset + 0x8000)));
  }
  return globalThis.btoa(binary);
}

export function base64ToBytes(value) {
  const binary = globalThis.atob(String(value || ''));
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
  return bytes;
}

function progress(callback, payload) {
  if (typeof callback === 'function') callback(payload);
}

async function resolveReadyDescriptor(invoke, descriptorOrId) {
  const descriptor = typeof descriptorOrId === 'string'
    ? await invoke('get_durable_asset', { assetId: descriptorOrId })
    : descriptorOrId;
  if (!descriptor?.assetId || descriptor.state !== 'ready') throw new Error('耐久资产尚未就绪');
  return descriptor;
}

export async function uploadDurableBlob(invoke, blobValue, descriptorInput, options = {}) {
  assertInvoke(invoke);
  assertDescriptorInput(descriptorInput);
  const blob = blobValue instanceof Blob ? blobValue : new Blob([blobValue], { type: descriptorInput.mimeType });
  if (!blob.size) throw new Error('耐久资产内容不能为空');
  let assetId = '';
  try {
    const descriptor = await invoke('begin_durable_asset_upload', {
      input: {
        assetId: descriptorInput.assetId || null,
        ownerType: String(descriptorInput.ownerType).trim(),
        ownerId: String(descriptorInput.ownerId).trim(),
        role: String(descriptorInput.role || 'source').trim(),
        fileName: String(descriptorInput.fileName).trim(),
        mimeType: String(descriptorInput.mimeType).trim(),
        expectedSha256: descriptorInput.expectedSha256 || null,
        metadata: descriptorInput.metadata && typeof descriptorInput.metadata === 'object' ? descriptorInput.metadata : {},
      },
    });
    assetId = descriptor?.assetId;
    const stagedId = descriptor?.stagedId;
    if (!assetId) throw new Error('原生耐久资产没有返回 assetId');
    if (descriptor?.state === 'ready') {
      if (Number(descriptor.byteLength) !== blob.size) throw new Error('已存在耐久资产的字节数与当前内容不一致');
      return descriptor;
    }
    if (!stagedId) throw new Error('原生耐久资产没有返回 stagedId');
    const chunkSize = positiveChunkSize(options.chunkSize, DEFAULT_UPLOAD_CHUNK_BYTES);
    let offset = Number(descriptor.byteLength || 0);
    if (offset < 0 || offset > blob.size) throw new Error('原生耐久资产恢复偏移无效');
    while (offset < blob.size) {
      const nextOffset = Math.min(blob.size, offset + chunkSize);
      const bytes = new Uint8Array(await blob.slice(offset, nextOffset).arrayBuffer());
      const next = await invoke('append_durable_asset_chunk', {
        assetId,
        stagedId,
        offset,
        chunkBase64: bytesToBase64(bytes),
      });
      const acknowledged = Number(next?.byteLength ?? nextOffset);
      if (acknowledged !== nextOffset) throw new Error(`耐久资产分块回执偏移不一致：expected=${nextOffset}, actual=${acknowledged}`);
      offset = nextOffset;
      progress(options.onProgress, { assetId, loaded: offset, total: blob.size, percent: Math.round((offset / blob.size) * 100) });
    }
    const completed = await invoke('finish_durable_asset_upload', { assetId, stagedId });
    if (completed?.state !== 'ready' || Number(completed.byteLength) !== blob.size) {
      throw new Error('耐久资产完成回执无效');
    }
    return completed;
  } catch (error) {
    if (options.cleanupOnError === true && assetId) {
      try {
        await invoke('delete_durable_asset', { assetId });
      } catch (cleanupError) {
        console.error('耐久资产上传失败后无法清理暂存数据', assetId, cleanupError);
      }
    }
    throw error;
  }
}

export function uploadDurableText(invoke, text, descriptorInput, options = {}) {
  const mimeType = descriptorInput?.mimeType || 'text/plain;charset=utf-8';
  return uploadDurableBlob(invoke, new Blob([String(text ?? '')], { type: mimeType }), { ...descriptorInput, mimeType }, options);
}

export async function prepareDurableTextNoteWrite(invoke, text, descriptorInput, writeInput, options = {}) {
  assertInvoke(invoke);
  const durableAsset = await uploadDurableText(invoke, text, descriptorInput, { ...options, cleanupOnError: true });
  try {
    const preview = await invoke('prepare_note_write_from_durable_asset', {
      vaultId: String(writeInput?.vaultId || '').trim(),
      relativePath: String(writeInput?.relativePath || '').trim(),
      durableAssetId: durableAsset.assetId,
      analysisReceipt: String(writeInput?.analysisReceipt || '').trim(),
      expectedHash: writeInput?.expectedHash || null,
      operationContext: writeInput?.operationContext || null,
    });
    return { durableAsset, preview };
  } catch (error) {
    try {
      await invoke('delete_durable_asset', { assetId: durableAsset.assetId });
    } catch (cleanupError) {
      console.error('准备 Vault 写入失败后无法清理临时耐久正文', cleanupError);
    }
    throw error;
  }
}

export async function readDurableAssetBlob(invoke, descriptorOrId, options = {}) {
  assertInvoke(invoke);
  const descriptor = await resolveReadyDescriptor(invoke, descriptorOrId);
  const total = Number(descriptor.byteLength || 0);
  const chunkSize = positiveChunkSize(options.chunkSize, DEFAULT_READ_CHUNK_BYTES);
  const parts = [];
  let offset = 0;
  while (offset < total) {
    const chunk = await invoke('read_durable_asset_chunk', {
      assetId: descriptor.assetId,
      offset,
      length: Math.min(chunkSize, total - offset),
    });
    if (Number(chunk?.offset) !== offset) throw new Error('耐久资产读取回执偏移不一致');
    const bytes = base64ToBytes(chunk.contentBase64);
    const nextOffset = Number(chunk.nextOffset);
    if (!bytes.length || nextOffset !== offset + bytes.length || nextOffset > total) {
      throw new Error('耐久资产读取回执长度无效');
    }
    parts.push(bytes);
    offset = nextOffset;
    progress(options.onProgress, { assetId: descriptor.assetId, loaded: offset, total, percent: total ? Math.round((offset / total) * 100) : 100 });
  }
  return new Blob(parts, { type: descriptor.mimeType || 'application/octet-stream' });
}

export async function readDurableAssetSlice(invoke, descriptorOrId, offsetValue = 0, lengthValue = null, options = {}) {
  assertInvoke(invoke);
  const descriptor = await resolveReadyDescriptor(invoke, descriptorOrId);
  const total = Number(descriptor.byteLength || 0);
  const offset = Math.max(0, Math.min(total, Math.trunc(Number(offsetValue) || 0)));
  const requestedLength = lengthValue == null ? total - offset : Math.max(0, Math.trunc(Number(lengthValue) || 0));
  const end = Math.min(total, offset + requestedLength);
  const chunkSize = positiveChunkSize(options.chunkSize, DEFAULT_READ_CHUNK_BYTES);
  const parts = [];
  let cursor = offset;
  while (cursor < end) {
    const chunk = await invoke('read_durable_asset_chunk', {
      assetId: descriptor.assetId,
      offset: cursor,
      length: Math.min(chunkSize, end - cursor),
    });
    if (Number(chunk?.offset) !== cursor) throw new Error('耐久资产读取回执偏移不一致');
    const bytes = base64ToBytes(chunk.contentBase64);
    const nextOffset = Number(chunk.nextOffset);
    if (!bytes.length || nextOffset !== cursor + bytes.length || nextOffset > end) {
      throw new Error('耐久资产读取回执长度无效');
    }
    parts.push(bytes);
    cursor = nextOffset;
    progress(options.onProgress, { assetId: descriptor.assetId, loaded: cursor - offset, total: end - offset, percent: end === offset ? 100 : Math.round(((cursor - offset) / (end - offset)) * 100) });
  }
  return new Blob(parts, { type: descriptor.mimeType || 'application/octet-stream' });
}

export async function readDurableAssetText(invoke, descriptorOrId, options = {}) {
  const blob = await readDurableAssetBlob(invoke, descriptorOrId, options);
  return blob.text();
}

/**
 * Decode one durable text asset incrementally. Each yielded string is backed by
 * only one bounded native chunk; callers can analyze and release it before the
 * next chunk is requested. TextDecoder streaming preserves UTF-8 code points
 * split across native chunk boundaries without ever rebuilding the whole file.
 */
export async function* streamDurableAssetText(invoke, descriptorOrId, options = {}) {
  assertInvoke(invoke);
  const descriptor = await resolveReadyDescriptor(invoke, descriptorOrId);
  const total = Number(descriptor.byteLength || 0);
  const chunkSize = positiveChunkSize(options.chunkSize, DEFAULT_TEXT_CHUNK_BYTES);
  const decoder = new TextDecoder(String(options.encoding || 'utf-8'), { fatal: options.fatal === true });
  let offset = 0;
  let index = 0;
  while (offset < total) {
    const chunk = await invoke('read_durable_asset_chunk', {
      assetId: descriptor.assetId,
      offset,
      length: Math.min(chunkSize, total - offset),
    });
    if (Number(chunk?.offset) !== offset) throw new Error('耐久资产读取回执偏移不一致');
    const bytes = base64ToBytes(chunk.contentBase64);
    const nextOffset = Number(chunk.nextOffset);
    if (!bytes.length || nextOffset !== offset + bytes.length || nextOffset > total) {
      throw new Error('耐久资产读取回执长度无效');
    }
    const text = decoder.decode(bytes, { stream: nextOffset < total });
    index += 1;
    offset = nextOffset;
    progress(options.onProgress, {
      assetId: descriptor.assetId,
      index,
      loaded: offset,
      total,
      percent: total ? Math.round((offset / total) * 100) : 100,
    });
    if (text) yield text;
  }
  const tail = decoder.decode();
  if (tail) yield tail;
}

function missingNativeCommand(error, command) {
  const message = String(error || '').toLowerCase();
  return message.includes(String(command).toLowerCase())
    && (message.includes('not found') || message.includes('unknown') || message.includes('不存在') || message.includes('未找到'));
}

/**
 * Read a durable-asset registry without an aggregate item limit. New runtimes
 * are consumed page by page; old runtimes remain readable while migrations are
 * rolling out, but only a genuinely missing page command triggers fallback.
 */
export async function listAllDurableAssetPages(invoke, filters = {}, options = {}) {
  assertInvoke(invoke);
  const pageCommand = String(options.pageCommand || 'list_durable_assets_page');
  const pageSize = Math.min(512, Math.max(1, Math.trunc(Number(options.pageSize) || 128)));
  const items = [];
  const seenCursors = new Set();
  let cursorUpdatedAt = null;
  let cursorId = null;
  try {
    while (true) {
      const page = await invoke(pageCommand, {
        ownerType: filters.ownerType ?? null,
        ownerId: filters.ownerId ?? null,
        cursorUpdatedAt,
        cursorId,
        // Transitional native builds used cursorAssetId. Sending both names is
        // harmless for Tauri named arguments and keeps rolling migrations
        // readable while cursorId becomes canonical.
        cursorAssetId: cursorId,
        limit: pageSize,
      });
      if (!page || !Array.isArray(page.items)) throw new Error(`${pageCommand} 返回了无效分页`);
      items.push(...page.items);
      const nextCursorId = page.nextCursorId || page.nextCursorAssetId || null;
      if (!page.nextCursorUpdatedAt || !nextCursorId) return items;
      const cursorKey = `${page.nextCursorUpdatedAt}\u0000${nextCursorId}`;
      if (seenCursors.has(cursorKey)) throw new Error(`${pageCommand} 返回了重复游标`);
      seenCursors.add(cursorKey);
      cursorUpdatedAt = page.nextCursorUpdatedAt;
      cursorId = nextCursorId;
    }
  } catch (error) {
    if (!missingNativeCommand(error, pageCommand)) throw error;
    const legacy = await invoke('list_durable_assets', {
      ownerType: filters.ownerType ?? null,
      ownerId: filters.ownerId ?? null,
    });
    if (!Array.isArray(legacy)) throw new Error('list_durable_assets 返回了无效结果');
    return legacy;
  }
}

export async function createDurableAssetObjectUrl(invoke, descriptorOrId, options = {}) {
  const blob = await readDurableAssetBlob(invoke, descriptorOrId, options);
  const url = URL.createObjectURL(blob);
  return { url, blob, revoke: () => URL.revokeObjectURL(url) };
}
