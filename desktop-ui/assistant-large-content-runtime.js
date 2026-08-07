const DEFAULT_STREAM_CHUNK_BYTES = 1024 * 1024;

function positiveInteger(value) {
  const number = Number(value);
  return Number.isFinite(number) && number > 0 ? Math.trunc(number) : null;
}

function firstPositive(values) {
  for (const value of values) {
    const parsed = positiveInteger(value);
    if (parsed) return parsed;
  }
  return null;
}

export function modelContextCapacity(model = {}) {
  const metadata = model?.metadata && typeof model.metadata === 'object' ? model.metadata : {};
  const contextWindowTokens = firstPositive([
    model.contextWindowTokens,
    model.context_window_tokens,
    model.contextLength,
    model.context_length,
    model.maxContextTokens,
    model.max_context_tokens,
    metadata.contextWindowTokens,
    metadata.context_window_tokens,
    metadata.contextLength,
    metadata.context_length,
  ]);
  if (!contextWindowTokens) return null;
  const configuredReserve = firstPositive([
    model.reservedOutputTokens,
    model.reserved_output_tokens,
    model.maxOutputTokens,
    model.max_output_tokens,
    metadata.reservedOutputTokens,
    metadata.reserved_output_tokens,
    metadata.maxOutputTokens,
    metadata.max_output_tokens,
  ]);
  const reservedOutputTokens = Math.min(
    Math.max(1, contextWindowTokens - 1),
    configuredReserve || Math.max(2_048, Math.round(contextWindowTokens * 0.08)),
  );
  const usableInputTokens = Math.max(1, contextWindowTokens - reservedOutputTokens);
  return {
    contextWindowTokens,
    reservedOutputTokens,
    usableInputTokens,
    // Compact before the provider hard limit so system prompts, tools, and
    // token-estimation error still have room. The threshold scales with the
    // selected model instead of imposing a product-wide token ceiling.
    compactionThresholdTokens: Math.max(1, Math.floor(usableInputTokens * 0.88)),
    recentTokenBudget: Math.max(1, Math.floor(usableInputTokens * 0.24)),
    chunkTokenBudget: Math.max(1, Math.floor(usableInputTokens * 0.2)),
  };
}

export async function* streamBlobText(blobValue, options = {}) {
  const blob = blobValue instanceof Blob ? blobValue : new Blob([blobValue || '']);
  const requested = positiveInteger(options.chunkSize) || DEFAULT_STREAM_CHUNK_BYTES;
  const chunkSize = Math.min(4 * 1024 * 1024, Math.max(64 * 1024, requested));
  const decoder = new TextDecoder(String(options.encoding || 'utf-8'), { fatal: options.fatal === true });
  let offset = 0;
  let index = 0;
  while (offset < blob.size) {
    const end = Math.min(blob.size, offset + chunkSize);
    const bytes = new Uint8Array(await blob.slice(offset, end).arrayBuffer());
    if (!bytes.length) throw new Error('附件分块读取没有取得任何字节');
    const text = decoder.decode(bytes, { stream: end < blob.size });
    offset = end;
    index += 1;
    if (typeof options.onProgress === 'function') {
      options.onProgress({ index, loaded: offset, total: blob.size, percent: blob.size ? Math.round((offset / blob.size) * 100) : 100 });
    }
    if (text) yield text;
  }
  const tail = decoder.decode();
  if (tail) yield tail;
}

export function streamedChunkCount(byteLength, chunkSize = DEFAULT_STREAM_CHUNK_BYTES) {
  const total = Math.max(0, Number(byteLength) || 0);
  const requested = positiveInteger(chunkSize) || DEFAULT_STREAM_CHUNK_BYTES;
  const bounded = Math.min(4 * 1024 * 1024, Math.max(64 * 1024, requested));
  return total ? Math.ceil(total / bounded) : 0;
}
