const DEFAULT_PAGE_SIZE = 128;
const DEFAULT_PAGE_BYTES = 8 * 1024 * 1024;

export function knowledgeMaintenanceLookupKey(vaultId, value) {
  const normalizedVaultId = String(vaultId || '').trim();
  const normalizedValue = String(value || '').trim().toLocaleLowerCase('zh-CN');
  return normalizedVaultId && normalizedValue ? `${normalizedVaultId}\u0000${normalizedValue}` : '';
}

function normalizedCursor(page) {
  const vaultId = typeof page?.nextAfterVaultId === 'string' ? page.nextAfterVaultId : '';
  const relativePath = typeof page?.nextAfterRelativePath === 'string' ? page.nextAfterRelativePath : '';
  if (!vaultId || !relativePath) return null;
  return { vaultId, relativePath, key: `${vaultId}\u0000${relativePath}` };
}

export async function readAllVaultNotes(invokeNative, options = {}) {
  if (typeof invokeNative !== 'function') throw new TypeError('读取知识库需要原生命令调用器');
  const vaultId = String(options.vaultId || 'all');
  const pageSize = Math.max(1, Math.min(512, Number(options.pageSize || DEFAULT_PAGE_SIZE)));
  const maxPageBytes = Math.max(64 * 1024, Math.min(32 * 1024 * 1024, Number(options.maxPageBytes || DEFAULT_PAGE_BYTES)));
  const folderPrefix = String(options.folderPrefix || '').trim() || null;
  const notes = [];
  const failures = [];
  let candidateCount = null;
  let afterVaultId = null;
  let afterRelativePath = null;
  let previousCursor = '';
  for (;;) {
    const page = await invokeNative('list_vault_notes_page', {
      vaultId,
      afterVaultId,
      afterRelativePath,
      limit: pageSize,
      maxBytes: maxPageBytes,
      folderPrefix,
    });
    if (!page || !Array.isArray(page.notes) || typeof page.hasMore !== 'boolean') {
      throw new Error('知识库分页回执无效');
    }
    notes.push(...page.notes);
    if (Array.isArray(page.failures)) failures.push(...page.failures);
    if (Number.isInteger(Number(page.candidateCount))) candidateCount = Number(page.candidateCount);
    if (typeof options.onPage === 'function') {
      await options.onPage({
        notes: page.notes,
        failures: Array.isArray(page.failures) ? page.failures : [],
        loadedCount: notes.length,
        candidateCount,
        returnedBytes: Math.max(0, Number(page.returnedBytes || 0)),
        hasMore: page.hasMore,
      });
    }
    if (!page.hasMore) {
      if (failures.length) {
        const preview = failures.slice(0, 6)
          .map((failure) => `${failure.vaultName || failure.vaultId}/${failure.relativePath}：${failure.reason}`)
          .join('；');
        const error = new Error(`有 ${failures.length} 篇 Markdown 无法完整读取，知识维护已停止：${preview}${failures.length > 6 ? '；其余错误已省略' : ''}`);
        error.failures = failures;
        throw error;
      }
      if (candidateCount !== null && notes.length !== candidateCount) {
        throw new Error(`知识库库存校验失败：候选 ${candidateCount} 篇，实际读取 ${notes.length} 篇`);
      }
      return notes;
    }
    const cursor = normalizedCursor(page);
    if (!cursor || cursor.key === previousCursor) throw new Error('知识库分页游标没有前进');
    previousCursor = cursor.key;
    afterVaultId = cursor.vaultId;
    afterRelativePath = cursor.relativePath;
  }
}

export function selectExecutableMaintenanceRepairs(repairs, vaults, options = {}) {
  const access = options.vaultAccess && typeof options.vaultAccess === 'object' ? options.vaultAccess : {};
  const scopedVaultId = options.vaultId && options.vaultId !== 'all' ? String(options.vaultId) : '';
  const writableVaultIds = new Set((Array.isArray(vaults) ? vaults : [])
    .filter((vault) => vault?.connectionState === 'connected'
      && (access[vault.id] || 'readwrite') === 'readwrite'
      && (!scopedVaultId || vault.id === scopedVaultId))
    .map((vault) => vault.id));
  return (Array.isArray(repairs) ? repairs : []).filter((repair) => writableVaultIds.has(repair?.note?.vaultId));
}
