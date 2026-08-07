import {
  createHtmlStudioStreamState,
  reduceHtmlStudioStreamEvent,
} from './html-studio.js';

function assertInvoke(invoke) {
  if (typeof invoke !== 'function') throw new TypeError('Creation 原生运行时缺少命令调用器');
}

function assertObject(value, label) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new TypeError(`${label} 必须是对象`);
  return value;
}

function assertIdentifier(value, label) {
  const normalized = String(value || '').trim();
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,159}$/u.test(normalized)) throw new TypeError(`${label} 无效`);
  return normalized;
}

function assertRunRecord(value) {
  const record = assertObject(value, 'Creation 原生 WritingRun 回执');
  const run = assertObject(record.writingRun, 'Creation 原生 WritingRun');
  assertIdentifier(run.id, 'WritingRun ID');
  assertIdentifier(run.documentId, 'WritingRun documentId');
  return record;
}

function assertSequence(value, label, minimum = -1) {
  const sequence = Number(value);
  if (!Number.isSafeInteger(sequence) || sequence < minimum) throw new TypeError(`${label} 无效`);
  return sequence;
}

function assertEventPage(value, runId) {
  const page = assertObject(value, 'Creation 原生事件页');
  if (page.runId !== runId || !Array.isArray(page.events)) throw new TypeError('Creation 原生事件页身份或事件列表无效');
  assertSequence(page.lastSequence, 'Creation 原生事件页末序号');
  assertSequence(page.runLastSequence, 'Creation 原生 WritingRun 末序号');
  if (page.nextSequence !== null && page.nextSequence !== undefined) {
    assertSequence(page.nextSequence, 'Creation 原生事件页下一序号', 0);
  }
  return page;
}

function journalIsComplete(record) {
  const lastSequence = assertSequence(record.lastSequence ?? -1, 'Creation 原生 WritingRun 末序号');
  const events = Array.isArray(record.events) ? record.events : [];
  if (lastSequence === -1) return events.length === 0;
  return events.length === lastSequence + 1
    && events.every((event, index) => Number(event?.sequence) === index);
}

function usageNumber(value) {
  const number = Number(value || 0);
  return Number.isFinite(number) && number >= 0 ? Math.trunc(number) : 0;
}

export function normalizeCreationUsage(value = {}) {
  const usage = value && typeof value === 'object' ? value : {};
  return {
    promptTokens: usageNumber(usage.promptTokens ?? usage.prompt_tokens),
    completionTokens: usageNumber(usage.completionTokens ?? usage.completion_tokens),
    totalTokens: usageNumber(usage.totalTokens ?? usage.total_tokens),
    estimatedCostUsd: Number.isFinite(Number(usage.estimatedCostUsd ?? usage.estimated_cost_usd))
      ? Math.max(0, Number(usage.estimatedCostUsd ?? usage.estimated_cost_usd))
      : null,
    durationMs: usageNumber(usage.durationMs ?? usage.duration_ms),
  };
}

export function creationCheckpointFromNativeRecord(recordValue) {
  const record = assertRunRecord(recordValue);
  if (!record.latestCheckpoint || typeof record.latestCheckpoint !== 'object' || Array.isArray(record.latestCheckpoint)) return null;
  const checkpoint = structuredClone(record.latestCheckpoint);
  let streamState = createHtmlStudioStreamState(checkpoint.streamState);
  const storedLastSequence = Number(record.lastSequence ?? -1);
  if (!Number.isSafeInteger(storedLastSequence) || storedLastSequence < -1) throw new TypeError('Creation 原生流序号无效');
  if (streamState.lastSequence > storedLastSequence) throw new Error('Creation checkpoint 序号领先于原生 Agent Stream');
  const events = Array.isArray(record.events) ? record.events : [];
  for (const event of events) {
    const sequence = Number(event?.sequence);
    if (!Number.isSafeInteger(sequence) || sequence < 0) throw new TypeError('Creation 原生 Agent Stream 事件序号无效');
    if (sequence <= streamState.lastSequence) continue;
    if (sequence !== streamState.lastSequence + 1) throw new Error(`Creation 原生 Agent Stream 恢复序列不连续：期望 ${streamState.lastSequence + 1}，实际 ${sequence}`);
    streamState = reduceHtmlStudioStreamEvent(streamState, event);
  }
  if (streamState.lastSequence !== storedLastSequence) {
    throw new Error(`Creation 原生 Agent Stream 恢复不完整：期望末序号 ${storedLastSequence}，实际 ${streamState.lastSequence}`);
  }
  return {
    ...checkpoint,
    writingRun: structuredClone(record.writingRun),
    streamState,
  };
}

export function createCreationNativeRuntime(invoke) {
  assertInvoke(invoke);
  async function readEvents(runIdValue, options = {}) {
    const runId = assertIdentifier(runIdValue, 'WritingRun ID');
    const afterSequence = assertSequence(options.afterSequence ?? -1, 'Creation 事件页游标');
    return assertEventPage(await invoke('read_creation_stream_events_page', {
      input: {
        runId,
        afterSequence,
        ...(options.pageSize ? { pageSize: Number(options.pageSize) } : {}),
        ...(options.maxBytes ? { maxBytes: Number(options.maxBytes) } : {}),
      },
    }), runId);
  }

  async function attachEventJournal(recordValue) {
    const record = assertRunRecord(recordValue);
    if (journalIsComplete(record)) return record;
    const targetLastSequence = assertSequence(record.lastSequence ?? -1, 'Creation 原生 WritingRun 末序号');
    const events = [];
    let afterSequence = -1;
    while (afterSequence < targetLastSequence) {
      const page = await readEvents(record.writingRun.id, { afterSequence });
      if (!page.events.length) throw new Error('Creation 原生事件分页恢复没有向前推进');
      for (const event of page.events) {
        const sequence = assertSequence(event?.sequence, 'Creation 原生 Agent Stream 事件序号', 0);
        if (sequence > targetLastSequence) break;
        if (sequence !== events.length) {
          throw new Error(`Creation 原生 Agent Stream 分页序列不连续：期望 ${events.length}，实际 ${sequence}`);
        }
        events.push(event);
        afterSequence = sequence;
      }
      if (afterSequence < targetLastSequence && page.hasMore !== true) {
        throw new Error(`Creation 原生事件分页提前结束：期望 ${targetLastSequence}，实际 ${afterSequence}`);
      }
    }
    return { ...record, events };
  }

  async function recoverHeaders() {
    const records = await invoke('recover_creation_runs');
    if (!Array.isArray(records)) throw new TypeError('Creation 恢复命令没有返回列表');
    return records.map(assertRunRecord);
  }

  return Object.freeze({
    async begin({ run, document, capability = 'creation.edit', streamId = null, operationId = null }) {
      assertObject(run, 'WritingRun');
      assertObject(document, 'CreationDocument');
      return assertRunRecord(await invoke('begin_creation_run', {
        input: { run, document, capability, streamId, operationId },
      }));
    },

    async get(runId) {
      return assertRunRecord(await invoke('get_creation_run', { runId: assertIdentifier(runId, 'WritingRun ID') }));
    },

    readEvents,

    async loadForReplay(runId, header = null) {
      const normalizedRunId = assertIdentifier(runId, 'WritingRun ID');
      const suppliedHeader = header === null ? null : assertRunRecord(header);
      const record = assertRunRecord(suppliedHeader && Object.prototype.hasOwnProperty.call(suppliedHeader, 'baseDocument')
        ? suppliedHeader
        : await invoke('get_creation_run', { runId: normalizedRunId }));
      if (record.writingRun.id !== normalizedRunId) throw new Error('Creation 恢复 header 与 WritingRun ID 不一致');
      return attachEventJournal(record);
    },

    async append(runId, event) {
      assertObject(event, 'Creation Agent Stream 事件');
      return assertRunRecord(await invoke('append_creation_stream_event', {
        runId: assertIdentifier(runId, 'WritingRun ID'),
        event,
      }));
    },

    async checkpoint(runId, checkpoint, candidateDocument = null) {
      assertObject(checkpoint, 'Creation checkpoint');
      if (candidateDocument !== null) assertObject(candidateDocument, 'Creation candidate document');
      return assertRunRecord(await invoke('checkpoint_creation_run', {
        input: {
          runId: assertIdentifier(runId, 'WritingRun ID'),
          checkpoint,
          candidateDocument,
        },
      }));
    },

    async recordUsage(input) {
      const source = assertObject(input, 'Creation 模型用量');
      const usage = normalizeCreationUsage(source.usage);
      return assertRunRecord(await invoke('record_creation_run_usage', {
        input: {
          runId: assertIdentifier(source.runId, 'WritingRun ID'),
          requestId: assertIdentifier(source.requestId, '模型 requestId'),
          traceId: assertIdentifier(source.traceId, '模型 traceId'),
          operation: source.operation || 'creation.edit',
          provider: String(source.provider || '').trim(),
          model: String(source.model || '').trim(),
          state: source.state,
          ...usage,
          error: source.error ? String(source.error).slice(0, 4000) : null,
        },
      }));
    },

    recoverHeaders,

    async recover() {
      return recoverHeaders();
    },

    async reverify(document, verificationTraceId = null) {
      assertObject(document, 'Creation grounding 文稿');
      const result = assertObject(await invoke('reverify_creation_grounding', {
        document,
        verificationTraceId: verificationTraceId || null,
      }), 'Creation grounding 回执');
      assertObject(result.document, 'Creation grounding 文稿回执');
      return result;
    },

    async accept({ runId, expectedDocumentRevision, expectedInputHash, candidateDocument, verificationTraceId = null }) {
      assertObject(candidateDocument, 'Creation candidate document');
      const receipt = assertObject(await invoke('accept_creation_candidate', {
        input: {
          runId: assertIdentifier(runId, 'WritingRun ID'),
          expectedDocumentRevision: Number(expectedDocumentRevision),
          expectedInputHash: String(expectedInputHash || ''),
          candidateDocument,
          verificationTraceId: verificationTraceId || null,
        },
      }), 'Creation candidate 接受回执');
      receipt.run = assertRunRecord(receipt.run);
      assertObject(receipt.grounding, 'Creation candidate grounding 回执');
      return receipt;
    },

    async cancel(runId, reason = '用户取消') {
      return assertRunRecord(await invoke('cancel_creation_run', {
        runId: assertIdentifier(runId, 'WritingRun ID'),
        reason: String(reason || '用户取消').slice(0, 4000),
      }));
    },
  });
}
