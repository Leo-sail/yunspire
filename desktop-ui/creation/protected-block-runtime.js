function stringValue(value) {
  return value == null ? '' : String(value);
}

export function stableCreationBlockDigest(value) {
  const source = stringValue(value);
  let h1 = 0xdeadbeef;
  let h2 = 0x41c6ce57;
  let h3 = 0xc0decafe;
  let h4 = 0x9e3779b9;
  for (let index = 0; index < source.length; index += 1) {
    const code = source.charCodeAt(index);
    h1 = Math.imul(h1 ^ code, 2654435761);
    h2 = Math.imul(h2 ^ code, 1597334677);
    h3 = Math.imul(h3 ^ code, 2246822507);
    h4 = Math.imul(h4 ^ code, 3266489909);
  }
  h1 = Math.imul(h1 ^ (h1 >>> 16), 2246822507) ^ Math.imul(h2 ^ (h2 >>> 13), 3266489909);
  h2 = Math.imul(h2 ^ (h2 >>> 16), 2246822507) ^ Math.imul(h3 ^ (h3 >>> 13), 3266489909);
  h3 = Math.imul(h3 ^ (h3 >>> 16), 2246822507) ^ Math.imul(h4 ^ (h4 >>> 13), 3266489909);
  h4 = Math.imul(h4 ^ (h4 >>> 16), 2246822507) ^ Math.imul(h1 ^ (h1 >>> 13), 3266489909);
  return [h1, h2, h3, h4].map((hash) => (hash >>> 0).toString(16).padStart(8, '0')).join('');
}

export function stableCreationProtectedBlockToken({ attachmentId = '', blockIdentity = '', ordinal = 0 } = {}) {
  const stableOrdinal = Number.isSafeInteger(Number(ordinal)) && Number(ordinal) >= 0 ? Number(ordinal) : 0;
  const digest = stableCreationBlockDigest(`${stringValue(attachmentId)}|${stringValue(blockIdentity)}|${stableOrdinal}`);
  return `[[YUNSPIRE_BLOCK_${digest}]]`;
}

export function collectCreationProtectedSpans(sourceValue, preserve = {}) {
  const source = stringValue(sourceValue);
  const spans = [];
  const collect = (pattern) => [...source.matchAll(pattern)].forEach((match) => spans.push(match[0]));
  collect(/\[\[YUNSPIRE_BLOCK_[0-9a-f-]+\]\]/giu);
  collect(/<<YUNSPIRE_[A-Z0-9_]+>>/gu);
  if (preserve.numbers) {
    collect(/(?:v\d+(?:\.\d+){1,3}|\d+(?:\.\d+)?\s*(?:%|％|ms|毫秒|秒|分钟|小时|天|周|月|年|元|万元|亿元|KB|MB|GB|TB|倍|个|条|篇|人|次)?)/giu);
  }
  if (preserve.references) {
    collect(/https?:\/\/[^\s)\]}>]+/giu);
    collect(/\[\[[^\]]+\]\]/gu);
    collect(/`[^`\n]+`/gu);
  }
  return [...new Set(spans.map((item) => item.trim()).filter((item) => item.length > 1))];
}
