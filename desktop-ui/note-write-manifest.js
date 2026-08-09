const MANIFEST_VERSION = 'yunspire.note-write-manifest.v1';
const HASH_PATTERN = /^[a-f0-9]{64}$/u;
const textEncoder = new TextEncoder();

function compareUtf8Bytes(left, right) {
  const leftBytes = textEncoder.encode(String(left));
  const rightBytes = textEncoder.encode(String(right));
  const sharedLength = Math.min(leftBytes.length, rightBytes.length);
  for (let index = 0; index < sharedLength; index += 1) {
    if (leftBytes[index] !== rightBytes[index]) return leftBytes[index] - rightBytes[index];
  }
  return leftBytes.length - rightBytes.length;
}

export function canonicalNoteWriteManifest(entries) {
  const writes = (Array.isArray(entries) ? entries : []).map((entry) => {
    const vaultId = String(entry?.vaultId || '').trim();
    const relativePath = String(entry?.relativePath || '').trim();
    const nextContentHash = String(entry?.nextContentHash || '').trim().toLowerCase();
    const expectedAbsent = entry?.expectedAbsent === true;
    const expectedHash = String(entry?.expectedHash || '').replace(/^sha256:/iu, '').trim().toLowerCase();
    if (!vaultId || !relativePath || !HASH_PATTERN.test(nextContentHash)) {
      throw new Error('灵感整理写入清单包含无效目标或正文哈希');
    }
    if (!expectedAbsent && !HASH_PATTERN.test(expectedHash)) {
      throw new Error(`灵感整理无法确认“${relativePath}”的旧正文哈希`);
    }
    return {
      vaultId,
      relativePath,
      previous: expectedAbsent ? 'absent' : `sha256:${expectedHash}`,
      nextContentHash,
    };
  }).sort((left, right) => compareUtf8Bytes(left.vaultId, right.vaultId)
    || compareUtf8Bytes(left.relativePath, right.relativePath));
  if (!writes.length) throw new Error('灵感整理写入清单不能为空');
  writes.forEach((entry, index) => {
    const previous = writes[index - 1];
    if (previous?.vaultId === entry.vaultId && previous.relativePath === entry.relativePath) {
      throw new Error(`灵感整理写入清单包含重复目标“${entry.relativePath}”`);
    }
  });
  return JSON.stringify({ version: MANIFEST_VERSION, writes });
}
