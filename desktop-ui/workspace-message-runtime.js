const MAX_NATIVE_MESSAGE_PAGE = 512;
const DEFAULT_MESSAGE_PAGE = 256;
const MAX_WORKSPACE_MESSAGE_SEARCH_QUERY_CHARS = 512;
const MAX_WORKSPACE_MESSAGE_SEARCH_LIMIT = 100;
const DEFAULT_WORKSPACE_MESSAGE_SEARCH_LIMIT = 50;

function assertInvoke(invoke) {
  if (typeof invoke !== 'function') throw new TypeError('工作区消息操作缺少原生命令调用器');
}

function normalizedPageSize(value) {
  const requested = Math.trunc(Number(value) || DEFAULT_MESSAGE_PAGE);
  return Math.min(MAX_NATIVE_MESSAGE_PAGE, Math.max(1, requested));
}

function normalizedWorkspaceMessageSearchQuery(value) {
  const query = String(value ?? '').trim();
  if (!query) throw new TypeError('工作区消息搜索缺少 query');
  if (Array.from(query).length > MAX_WORKSPACE_MESSAGE_SEARCH_QUERY_CHARS) {
    throw new RangeError(`工作区消息搜索 query 不能超过 ${MAX_WORKSPACE_MESSAGE_SEARCH_QUERY_CHARS} 个字符`);
  }
  return query;
}

function normalizedWorkspaceMessageSearchLimit(value) {
  const requested = value === undefined || value === null
    ? DEFAULT_WORKSPACE_MESSAGE_SEARCH_LIMIT
    : Math.trunc(Number(value));
  if (!Number.isFinite(requested)) return DEFAULT_WORKSPACE_MESSAGE_SEARCH_LIMIT;
  return Math.min(MAX_WORKSPACE_MESSAGE_SEARCH_LIMIT, Math.max(1, requested));
}

function validateWorkspaceMessageSearchResult(result, index) {
  if (!result || typeof result !== 'object' || Array.isArray(result)) {
    throw new Error(`search_workspace_messages 返回了无效结果 #${index + 1}`);
  }
  for (const field of ['conversationId', 'messageId', 'role', 'createdAt', 'snippet']) {
    if (typeof result[field] !== 'string') {
      throw new Error(`search_workspace_messages 结果 #${index + 1} 的 ${field} 必须是字符串`);
    }
  }
  if (!Number.isFinite(result.score)) {
    throw new Error(`search_workspace_messages 结果 #${index + 1} 的 score 必须是有限数字`);
  }
  return result;
}

function chunks(values, pageSize) {
  const pages = [];
  for (let offset = 0; offset < values.length; offset += pageSize) {
    pages.push(values.slice(offset, offset + pageSize));
  }
  return pages;
}

export async function listAllWorkspaceMessagePages(invoke, options = {}) {
  assertInvoke(invoke);
  const pageSize = normalizedPageSize(options.pageSize);
  const conversationId = String(options.conversationId || '').trim() || null;
  const items = [];
  const seenCursors = new Set();
  let cursorCreatedAt = null;
  let cursorId = null;
  while (true) {
    const page = await invoke('list_workspace_messages_page', {
      conversationId,
      cursorCreatedAt,
      cursorId,
      limit: pageSize,
    });
    if (!page || !Array.isArray(page.items)) throw new Error('list_workspace_messages_page 返回了无效分页');
    items.push(...page.items);
    const nextCreatedAt = page.nextCursorCreatedAt || null;
    const nextId = page.nextCursorId || null;
    if (!nextCreatedAt && !nextId) return items;
    if (!nextCreatedAt || !nextId) throw new Error('list_workspace_messages_page 返回了不完整游标');
    const cursorKey = `${nextCreatedAt}\u0000${nextId}`;
    if (seenCursors.has(cursorKey)) throw new Error('list_workspace_messages_page 返回了重复游标');
    seenCursors.add(cursorKey);
    cursorCreatedAt = nextCreatedAt;
    cursorId = nextId;
  }
}

export async function searchWorkspaceMessages(invoke, options = {}) {
  assertInvoke(invoke);
  const query = normalizedWorkspaceMessageSearchQuery(options?.query);
  const limit = normalizedWorkspaceMessageSearchLimit(options?.limit);
  const results = await invoke('search_workspace_messages', { query, limit });
  if (!Array.isArray(results)) throw new Error('search_workspace_messages 返回了无效结果列表');
  return results.map(validateWorkspaceMessageSearchResult);
}

export function createWorkspaceMessageSearchCoordinator(invoke) {
  assertInvoke(invoke);
  let generation = 0;
  return {
    async search(options = {}) {
      const requestGeneration = ++generation;
      try {
        const results = await searchWorkspaceMessages(invoke, options);
        return requestGeneration === generation ? results : null;
      } catch (error) {
        if (requestGeneration !== generation) return null;
        throw error;
      }
    },
    invalidate() {
      generation += 1;
    },
  };
}

export async function upsertWorkspaceMessagePages(invoke, messages, options = {}) {
  assertInvoke(invoke);
  const records = Array.isArray(messages) ? messages : [];
  const pageSize = normalizedPageSize(options.pageSize);
  for (const page of chunks(records, pageSize)) {
    await invoke('upsert_workspace_messages_page', { messages: page });
  }
  return records.length;
}

export async function deleteWorkspaceMessagePages(invoke, messageIds, options = {}) {
  assertInvoke(invoke);
  const ids = [...new Set((Array.isArray(messageIds) ? messageIds : []).map((value) => String(value || '').trim()).filter(Boolean))];
  const pageSize = normalizedPageSize(options.pageSize);
  let deleted = 0;
  for (const page of chunks(ids, pageSize)) {
    deleted += Number(await invoke('delete_workspace_messages', { messageIds: page }) || 0);
  }
  return deleted;
}

export async function deleteWorkspaceConversationMessages(invoke, conversationId) {
  assertInvoke(invoke);
  const normalizedId = String(conversationId || '').trim();
  if (!normalizedId) throw new TypeError('待删除消息的对话缺少 id');
  return Number(await invoke('delete_workspace_conversation_messages', { conversationId: normalizedId }) || 0);
}

export function cloneWorkspaceConversationMessages(messages, conversationId, createId) {
  const normalizedConversationId = String(conversationId || '').trim();
  if (!normalizedConversationId) throw new TypeError('复制消息缺少目标对话 id');
  if (typeof createId !== 'function') throw new TypeError('复制消息缺少 id 生成器');
  return (Array.isArray(messages) ? messages : []).map((message) => ({
    ...structuredClone(message),
    id: String(createId(message)),
    conversationId: normalizedConversationId,
  }));
}
