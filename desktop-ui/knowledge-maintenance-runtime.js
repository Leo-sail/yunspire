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
  const notes = [];
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
    });
    if (!page || !Array.isArray(page.notes) || typeof page.hasMore !== 'boolean') {
      throw new Error('知识库分页回执无效');
    }
    notes.push(...page.notes);
    if (typeof options.onPage === 'function') {
      await options.onPage({
        notes: page.notes,
        loadedCount: notes.length,
        returnedBytes: Math.max(0, Number(page.returnedBytes || 0)),
        hasMore: page.hasMore,
      });
    }
    if (!page.hasMore) return notes;
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
