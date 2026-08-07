import { safeCreationId } from './document.js';

export const CREATION_AGENT_CAPABILITIES = Object.freeze(['creation.generate', 'creation.edit']);
export const LOCAL_NORMALIZATION_CONTENT_CHUNK_BYTES = 512 * 1024;
export const AGENT_STREAM_EVENT_TYPES = Object.freeze([
  'streamStarted',
  'contentDelta',
  'contentSnapshot',
  'progress',
  'artifact',
  'diagnostic',
  'streamCompleted',
  'streamFailed',
  'streamCancelled',
  'heartbeat',
]);

const EVENT_ALIASES = Object.freeze({
  start: 'streamStarted',
  started: 'streamStarted',
  stream_start: 'streamStarted',
  token: 'contentDelta',
  delta: 'contentDelta',
  content_delta: 'contentDelta',
  snapshot: 'contentSnapshot',
  content: 'contentSnapshot',
  content_snapshot: 'contentSnapshot',
  progress: 'progress',
  artifact: 'artifact',
  diagnostic: 'diagnostic',
  warning: 'diagnostic',
  complete: 'streamCompleted',
  completed: 'streamCompleted',
  done: 'streamCompleted',
  finish: 'streamCompleted',
  error: 'streamFailed',
  failed: 'streamFailed',
  cancel: 'streamCancelled',
  cancelled: 'streamCancelled',
  ping: 'heartbeat',
  heartbeat: 'heartbeat',
});

function isRecord(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

function stringValue(value, fallback = '', maximum = 4096) {
  const candidate = typeof value === 'string' || typeof value === 'number' ? String(value).trim() : '';
  return (candidate || fallback).slice(0, maximum);
}

function integer(value, fallback, minimum, maximum = Number.MAX_SAFE_INTEGER) {
  const candidate = Number(value);
  return Number.isFinite(candidate) ? Math.max(minimum, Math.min(maximum, Math.trunc(candidate))) : fallback;
}

function validDateTime(value, fallback) {
  return typeof value === 'string' && Number.isFinite(Date.parse(value)) ? new Date(value).toISOString() : fallback;
}

function validHash(value) {
  const candidate = stringValue(value).toLowerCase();
  return /^sha256:[a-f0-9]{64}$/u.test(candidate) ? candidate : null;
}

function uniqueStrings(value, maximum = 200) {
  return [...new Set((Array.isArray(value) ? value : []).map((item) => stringValue(item, '', 160)).filter(Boolean))].slice(0, maximum);
}

function uniqueCreationIds(value, prefix = 'creation') {
  const identifiers = (Array.isArray(value) ? value : [])
    .map((item) => stringValue(item, '', 240))
    .filter(Boolean)
    .map((item) => safeCreationId(item, prefix));
  return [...new Set(identifiers)];
}

function eventTypeFrom(value, sseEvent) {
  const candidate = stringValue(value || sseEvent);
  if (AGENT_STREAM_EVENT_TYPES.includes(candidate)) return candidate;
  return EVENT_ALIASES[candidate.toLowerCase()] || 'contentDelta';
}

function normalizeCode(value, fallback) {
  const candidate = stringValue(value, fallback, 100).toLowerCase().replace(/[^a-z0-9._-]+/gu, '-').replace(/^[^a-z]+/u, 'agent.');
  return candidate || fallback;
}

function normalizedPayload(eventType, value, context) {
  const source = isRecord(value) ? value : {};
  if (eventType === 'streamStarted') {
    return {
      agentId: stringValue(source.agentId || context.agentId, 'local-creation-agent', 160),
      protocolVersion: '1.0',
    };
  }
  if (eventType === 'contentDelta' || eventType === 'contentSnapshot') {
    const channel = ['html', 'css', 'javascript', 'text'].includes(source.channel || context.channel) ? (source.channel || context.channel) : 'html';
    const content = source.content ?? source.delta ?? source.text ?? context.content ?? '';
    const normalizedContent = String(content);
    if (new TextEncoder().encode(normalizedContent).byteLength > 1024 * 1024) {
      throw new RangeError('单个 Agent Stream 内容事件超过 1 MB；请拆分为连续事件');
    }
    return {
      channel,
      content: normalizedContent,
      replaceFrom: source.replaceFrom == null ? null : integer(source.replaceFrom, 0, 0),
      replaceTo: source.replaceTo == null ? null : integer(source.replaceTo, 0, 0),
    };
  }
  if (eventType === 'progress') {
    return {
      stage: normalizeCode(source.stage, 'generating').slice(0, 80),
      percent: integer(source.percent ?? source.progress, 0, 0, 100),
      message: stringValue(source.message, '', 1000),
    };
  }
  if (eventType === 'artifact') {
    const contentHash = validHash(source.contentHash || context.contentHash);
    if (!contentHash) throw new TypeError('Artifact stream event requires a verified SHA-256 contentHash');
    const kind = ['html', 'css', 'javascript', 'image', 'data', 'archive', 'report'].includes(source.kind) ? source.kind : 'data';
    return {
      artifactId: safeCreationId(source.artifactId || source.id || `${kind}-${source.relativePath || ''}`, 'artifact'),
      kind,
      relativePath: stringValue(source.relativePath || source.path, `${kind}/artifact`, 2048),
      contentHash,
    };
  }
  if (eventType === 'diagnostic') {
    return {
      code: normalizeCode(source.code, 'agent.diagnostic'),
      severity: ['info', 'warning', 'error'].includes(source.severity) ? source.severity : (context.sseEvent === 'warning' ? 'warning' : 'info'),
      message: stringValue(source.message || source.detail || source.content, '本地 Agent 返回了一条诊断信息。', 4000),
      file: stringValue(source.file, '', 2048) || null,
      line: source.line == null ? null : integer(source.line, 1, 1),
      column: source.column == null ? null : integer(source.column, 1, 1),
    };
  }
  if (eventType === 'streamCompleted') {
    const readinessReportId = stringValue(source.readinessReportId, '', 240);
    return {
      resultId: safeCreationId(source.resultId || source.id || `${context.operationId}-result`, 'result'),
      artifactIds: uniqueCreationIds(source.artifactIds, 'artifact'),
      readinessReportId: readinessReportId ? safeCreationId(readinessReportId, 'readiness') : null,
    };
  }
  if (eventType === 'streamFailed') {
    return {
      code: normalizeCode(source.code, 'agent.stream-failed'),
      message: stringValue(source.message || source.error, '本地 Agent 生成失败。', 4000),
      retryable: source.retryable === true,
    };
  }
  if (eventType === 'streamCancelled') return { reason: stringValue(source.reason, '用户取消', 1000) };
  return { elapsedMs: integer(source.elapsedMs, 0, 0) };
}

export function splitAgentStreamContent(value, maximumBytes = LOCAL_NORMALIZATION_CONTENT_CHUNK_BYTES) {
  const content = String(value ?? '');
  if (!Number.isSafeInteger(maximumBytes) || maximumBytes < 4 || maximumBytes > 1024 * 1024) {
    throw new RangeError('Agent Stream 内容分块边界必须是 4 字节到 1 MB 之间的安全整数');
  }
  if (!content) return [''];
  const chunks = [];
  let chunkStart = 0;
  let chunkBytes = 0;
  let index = 0;
  while (index < content.length) {
    const first = content.charCodeAt(index);
    const pairedSurrogate = first >= 0xd800
      && first <= 0xdbff
      && index + 1 < content.length
      && content.charCodeAt(index + 1) >= 0xdc00
      && content.charCodeAt(index + 1) <= 0xdfff;
    const codeUnitLength = pairedSurrogate ? 2 : 1;
    // Count the UTF-8 bytes that JSON.stringify will emit for the content
    // string. Escaped quotes, slashes, and control characters cost more than
    // their source UTF-8 representation, so this also protects the envelope
    // size checked by the native runtime.
    const codePointBytes = pairedSurrogate
      ? 4
      : first === 0x22 || first === 0x5c || first === 0x08 || first === 0x09 || first === 0x0a || first === 0x0c || first === 0x0d
        ? 2
        : first <= 0x1f || (first >= 0xd800 && first <= 0xdfff)
          ? 6
          : first <= 0x7f
            ? 1
            : first <= 0x7ff
              ? 2
              : 3;
    if (chunkBytes > 0 && chunkBytes + codePointBytes > maximumBytes) {
      chunks.push(content.slice(chunkStart, index));
      chunkStart = index;
      chunkBytes = 0;
    }
    chunkBytes += codePointBytes;
    index += codeUnitLength;
  }
  chunks.push(content.slice(chunkStart));
  return chunks;
}

// Provider chunks remain contentDelta events. This helper is only for a local
// parse/normalization correction: the first event establishes a replayable
// snapshot baseline and later chunks append to that baseline without allowing
// one large document to exceed the per-event protocol boundary.
export function createLocalNormalizationSnapshotEvents(value, options = {}) {
  const channel = ['html', 'css', 'javascript', 'text'].includes(options.channel) ? options.channel : 'text';
  return splitAgentStreamContent(value, options.maximumBytes).map((content, index) => ({
    eventType: index === 0 ? 'contentSnapshot' : 'contentDelta',
    payload: {
      channel,
      content,
      replaceFrom: null,
      replaceTo: null,
    },
  }));
}

function unwrapEvent(input, context) {
  if (isRecord(input)) {
    if (isRecord(input.data) && !input.payload) return { ...input, ...input.data, payload: input.data.payload || input.data };
    return input;
  }
  const source = String(input || '').trim();
  if (!source) return {};
  if (source === '[DONE]') return { eventType: 'streamCompleted', payload: {} };
  try {
    const parsed = JSON.parse(source);
    return isRecord(parsed) ? parsed : { content: String(parsed) };
  } catch {
    return { eventType: context.sseEvent || 'contentDelta', content: source };
  }
}

export function normalizeAgentStreamEvent(input, context = {}) {
  const source = unwrapEvent(input, context);
  const eventType = eventTypeFrom(source.eventType || source.type || source.event, context.sseEvent);
  const sequence = integer(source.sequence, integer(context.sequence, 0, 0), 0);
  const now = validDateTime(context.now, new Date().toISOString());
  const streamId = safeCreationId(source.streamId || context.streamId || 'creation-stream', 'stream');
  const operationId = safeCreationId(source.operationId || context.operationId || streamId, 'operation');
  const capability = CREATION_AGENT_CAPABILITIES.includes(source.capability || context.capability)
    ? (source.capability || context.capability)
    : 'creation.generate';
  const payloadSource = isRecord(source.payload) ? source.payload : source;
  return {
    schemaVersion: '1.0',
    streamId,
    eventId: safeCreationId(source.eventId || context.eventId || `${streamId}-${sequence}-${eventType}`, 'event'),
    sequence,
    timestamp: validDateTime(source.timestamp, now),
    operationId,
    capability,
    eventType,
    payload: normalizedPayload(eventType, payloadSource, { ...context, operationId }),
  };
}

export function parseSseEventBlock(block, context = {}) {
  const record = { event: '', id: '', data: [] };
  for (const line of String(block || '').split(/\r?\n/u)) {
    if (!line || line.startsWith(':')) continue;
    const separator = line.indexOf(':');
    const field = separator < 0 ? line : line.slice(0, separator);
    const value = separator < 0 ? '' : line.slice(separator + 1).replace(/^\s/u, '');
    if (field === 'event') record.event = value;
    else if (field === 'id') record.id = value;
    else if (field === 'data') record.data.push(value);
  }
  if (!record.data.length && !record.event) return null;
  return normalizeAgentStreamEvent(record.data.join('\n'), {
    ...context,
    sseEvent: record.event,
    eventId: record.id || context.eventId,
  });
}

export function parseSseStreamChunk(chunk, context = {}) {
  const source = `${context.remainder || ''}${String(chunk || '')}`.replace(/\r\n/gu, '\n');
  const blocks = source.split('\n\n');
  const endsWithBoundary = source.endsWith('\n\n');
  const remainder = endsWithBoundary ? '' : blocks.pop() || '';
  let sequence = integer(context.sequence, 0, 0);
  const events = [];
  for (const block of blocks) {
    const event = parseSseEventBlock(block, { ...context, sequence });
    if (!event) continue;
    events.push(event);
    sequence = event.sequence + 1;
  }
  return { events, remainder, nextSequence: sequence };
}

export function createSseEventDecoder(context = {}) {
  let remainder = '';
  let sequence = integer(context.sequence, 0, 0);
  return {
    push(chunk) {
      const result = parseSseStreamChunk(chunk, { ...context, remainder, sequence });
      remainder = result.remainder;
      sequence = result.nextSequence;
      return result.events;
    },
    finish() {
      if (!remainder.trim()) return [];
      const event = parseSseEventBlock(remainder, { ...context, sequence });
      remainder = '';
      if (!event) return [];
      sequence = event.sequence + 1;
      return [event];
    },
    snapshot() {
      return { remainder, nextSequence: sequence };
    },
  };
}

export function createHtmlStudioStreamState(value = {}) {
  const source = isRecord(value) ? value : {};
  return {
    streamId: stringValue(source.streamId),
    operationId: stringValue(source.operationId),
    capability: CREATION_AGENT_CAPABILITIES.includes(source.capability) ? source.capability : 'creation.generate',
    status: ['idle', 'running', 'completed', 'failed', 'cancelled'].includes(source.status) ? source.status : 'idle',
    lastSequence: integer(source.lastSequence, -1, -1),
    receivedEventIds: uniqueStrings(source.receivedEventIds, 2000),
    channels: {
      html: String(source.channels?.html || ''),
      css: String(source.channels?.css || ''),
      javascript: String(source.channels?.javascript || ''),
      text: String(source.channels?.text || ''),
    },
    progress: isRecord(source.progress) ? { ...source.progress } : { stage: 'idle', percent: 0, message: '' },
    artifacts: Array.isArray(source.artifacts) ? [...source.artifacts] : [],
    diagnostics: Array.isArray(source.diagnostics) ? [...source.diagnostics] : [],
    result: isRecord(source.result) ? { ...source.result } : null,
    error: isRecord(source.error) ? { ...source.error } : null,
    cancellation: isRecord(source.cancellation) ? { ...source.cancellation } : null,
    updatedAt: validDateTime(source.updatedAt, new Date().toISOString()),
  };
}

function applyContent(current, payload, snapshot) {
  if (snapshot) return payload.content;
  if (payload.replaceFrom == null && payload.replaceTo == null) return `${current}${payload.content}`;
  const from = integer(payload.replaceFrom, current.length, 0, current.length);
  const to = integer(payload.replaceTo, from, from, current.length);
  return `${current.slice(0, from)}${payload.content}${current.slice(to)}`;
}

export function reduceHtmlStudioStreamEvent(stateValue, eventValue, context = {}) {
  const state = createHtmlStudioStreamState(stateValue);
  const event = normalizeAgentStreamEvent(eventValue, {
    streamId: state.streamId || context.streamId,
    operationId: state.operationId || context.operationId,
    capability: state.capability || context.capability,
    ...context,
  });
  if (state.receivedEventIds.includes(event.eventId) || event.sequence <= state.lastSequence) return state;
  if (state.streamId && event.streamId !== state.streamId) throw new Error('HTML Studio stream event belongs to another stream');
  state.streamId = event.streamId;
  state.operationId = event.operationId;
  state.capability = event.capability;
  state.lastSequence = event.sequence;
  state.receivedEventIds = [...state.receivedEventIds, event.eventId].slice(-2000);
  state.updatedAt = event.timestamp;
  if (event.eventType === 'streamStarted') state.status = 'running';
  else if (event.eventType === 'contentDelta' || event.eventType === 'contentSnapshot') {
    state.status = 'running';
    const channel = event.payload.channel;
    state.channels[channel] = applyContent(state.channels[channel], event.payload, event.eventType === 'contentSnapshot');
  } else if (event.eventType === 'progress') {
    state.status = 'running';
    state.progress = { ...event.payload };
  } else if (event.eventType === 'artifact') {
    state.artifacts = [...state.artifacts.filter((item) => item.artifactId !== event.payload.artifactId), event.payload];
  } else if (event.eventType === 'diagnostic') {
    state.diagnostics = [...state.diagnostics, event.payload].slice(-500);
  } else if (event.eventType === 'streamCompleted') {
    state.status = 'completed';
    state.progress = { stage: 'completed', percent: 100, message: '生成已完成' };
    state.result = { ...event.payload };
  } else if (event.eventType === 'streamFailed') {
    state.status = 'failed';
    state.error = { ...event.payload };
  } else if (event.eventType === 'streamCancelled') {
    state.status = 'cancelled';
    state.cancellation = { ...event.payload };
  }
  return state;
}

export function normalizeLocalCreationAgent(value = {}) {
  const source = isRecord(value) ? value : {};
  const capabilities = uniqueStrings(source.capabilities).filter((capability) => CREATION_AGENT_CAPABILITIES.includes(capability));
  return {
    id: stringValue(source.id || source.agentId, 'local-creation-agent', 160),
    name: stringValue(source.name, '本地创作 Agent', 160),
    executable: stringValue(source.executable || source.command, '', 2048),
    protocolVersion: source.protocolVersion === '1.0' ? '1.0' : '1.0',
    capabilities,
    available: source.available === true && capabilities.length > 0,
    reason: stringValue(source.reason, capabilities.length ? '' : '未检测到允许的创作能力。', 1000),
  };
}

function escapeHtml(value) {
  return String(value || '').replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;');
}

export function buildHtmlStudioPreview(value = {}, options = {}) {
  const source = isRecord(value.channels) ? value.channels : value;
  const html = String(source.html || '');
  const css = String(source.css || '');
  const javascript = options.allowScripts === true ? String(source.javascript || '') : '';
  const scriptPolicy = javascript ? "script-src 'unsafe-inline'" : "script-src 'none'";
  const contentSecurityPolicy = `default-src 'none'; img-src data: blob:; media-src data: blob:; style-src 'unsafe-inline'; font-src data:; ${scriptPolicy}; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'`;
  const sourceDocument = `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta http-equiv="Content-Security-Policy" content="${escapeHtml(contentSecurityPolicy)}"><style>${css.replace(/<\/style/giu, '<\\/style')}</style></head><body>${html}${javascript ? `<script>${javascript.replace(/<\/script/giu, '<\\/script')}</script>` : ''}</body></html>`;
  return {
    srcdoc: sourceDocument,
    sandbox: javascript ? 'allow-scripts' : '',
    networkAllowed: false,
    scriptsAllowed: Boolean(javascript),
    allowTopNavigation: false,
    allowPopups: false,
  };
}
