export const CAPTURE_NETWORK_BATCH_SIZE = 32;
export const MAX_CAPTURE_NETWORK_BATCH_SIZE = 128;
const EMBEDDED_LINK_SUMMARY_PREVIEW = 20;

function embeddedLinksFromResult(result) {
  if (Array.isArray(result?.embedded_links)) return result.embedded_links;
  if (Array.isArray(result?.embeddedLinks)) return result.embeddedLinks;
  return [];
}
export function normalizedCapturedEmbeddedLinks(result) {
  const linkIdOccurrences = new Map();
  return embeddedLinksFromResult(result).flatMap((link, index) => {
    if (link?.policy?.capture_candidate === false || link?.policy?.captureCandidate === false) return [];
    const target = String(link?.target || '').trim();
    if (!/^https?:\/\//iu.test(target)) return [];
    const provenance = link.provenance && typeof link.provenance === 'object' ? link.provenance : {};
    const sourceLinkId = String(link.link_id || link.linkId || '').trim();
    const baseLinkId = sourceLinkId || `embedded-link-${index + 1}`;
    const occurrence = (linkIdOccurrences.get(baseLinkId) || 0) + 1;
    linkIdOccurrences.set(baseLinkId, occurrence);
    return [{
      linkId: occurrence === 1 ? baseLinkId : `${baseLinkId}-occurrence-${occurrence}`,
      sourceLinkId: sourceLinkId || null,
      occurrenceIndex: index,
      target,
      displayText: String(link.display_text || link.displayText || ''),
      source: String(link.source || 'document'),
      provenance,
      policy: {
        contentRole: 'untrusted_data',
        autoOpen: false,
        autoFetch: false,
        captureRequiresExplicitUserRequest: true,
      },
    }];
  });
}

export function embeddedLinkResultSummary(links) {
  if (!Array.isArray(links) || !links.length) return '';
  const listed = links.slice(0, EMBEDDED_LINK_SUMMARY_PREVIEW)
    .map((link) => `- \`${link.linkId}\` · \`${String(link.target).replace(/`/gu, '\\`')}\``)
    .join('\n');
  const remainder = links.length > EMBEDDED_LINK_SUMMARY_PREVIEW
    ? `\n- 另有 ${links.length - EMBEDDED_LINK_SUMMARY_PREVIEW} 条，全部保存在结构化附件中，可按稳定 linkId 继续采集`
    : '';
  return `\n\n## 文件内链接\n\n已完整保留 ${links.length} 条可采集链接，解析过程没有打开或访问它们。明确要求“继续采集文件内链接”后，AI助手会为选定目标创建新的分批采集命令。\n\n${listed}${remainder}`;
}

export function partitionDeterministicCaptureRequests(requests, batchSize = CAPTURE_NETWORK_BATCH_SIZE) {
  if (!Array.isArray(requests)) throw new TypeError('文件内链接采集请求必须是数组');
  const normalizedBatchSize = Number(batchSize);
  if (!Number.isInteger(normalizedBatchSize) || normalizedBatchSize < 1 || normalizedBatchSize > MAX_CAPTURE_NETWORK_BATCH_SIZE) {
    throw new RangeError(`单次文件内链接采集批次必须在 1 到 ${MAX_CAPTURE_NETWORK_BATCH_SIZE} 条之间`);
  }
  const batches = [];
  for (let offset = 0; offset < requests.length; offset += normalizedBatchSize) {
    batches.push(requests.slice(offset, offset + normalizedBatchSize));
  }
  return batches;
}
