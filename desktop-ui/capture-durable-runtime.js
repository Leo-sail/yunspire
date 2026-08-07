function durableDescriptor(value) {
  if (!value || typeof value !== 'object') return null;
  const descriptor = value.asset?.assetId
    ? value.asset
    : value.durableAsset?.assetId
      ? value.durableAsset
      : value.assetId
        ? value
        : null;
  if (!descriptor?.assetId || (descriptor.state && descriptor.state !== 'ready')) return null;
  return descriptor;
}

function missingNativeCommand(error, command) {
  const message = String(error || '').toLowerCase();
  return message.includes(String(command).toLowerCase())
    && (message.includes('not found') || message.includes('unknown') || message.includes('不存在') || message.includes('未找到'));
}

export function captureInputForDurableAttachment(value) {
  const descriptor = durableDescriptor(value);
  if (!descriptor) return null;
  const fileName = String(value?.name || value?.title || descriptor.fileName || '未命名附件');
  return {
    name: fileName,
    relativePath: String(value?.relativePath || fileName),
    durableAssetId: String(descriptor.assetId),
  };
}

export async function prepareDurableImageAnalysisInput(invoke, value, options = {}) {
  if (typeof invoke !== 'function') throw new TypeError('图片派生输入缺少原生命令调用器');
  const descriptor = durableDescriptor(value);
  if (!descriptor) return null;
  const command = 'prepare_capture_image_analysis_input';
  const mimeType = String(options.mimeType || value?.mimeType || value?.type || descriptor.mimeType || '').toLowerCase();
  if (!mimeType.startsWith('image/')) throw new Error('模型分析派生输入只接受图片附件');
  try {
    return await invoke(command, {
      stagedAttachmentId: null,
      durableAssetId: String(descriptor.assetId),
      mimeType,
      expectedSha256: descriptor.sha256 || options.expectedSha256 || null,
    });
  } catch (error) {
    if (!missingNativeCommand(error, command) || typeof options.fallback !== 'function') throw error;
    return options.fallback(descriptor);
  }
}
