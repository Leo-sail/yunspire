import {
  createHtmlStudioStreamState,
  reduceHtmlStudioStreamEvent,
} from './html-studio.js';

function isRecord(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

function clone(value) {
  if (typeof structuredClone === 'function') return structuredClone(value);
  return JSON.parse(JSON.stringify(value));
}

function integer(value, fallback = -1) {
  const candidate = Number(value);
  return Number.isSafeInteger(candidate) ? candidate : fallback;
}

function nonNegativeInteger(value, fallback = 0) {
  const candidate = integer(value, fallback);
  return candidate >= 0 ? candidate : fallback;
}

function validCheckpoint(value) {
  return isRecord(value)
    && value.schemaVersion === '1.0'
    && value.kind === 'creationExecutionCheckpoint';
}

export function compactCreationWritingRunReference(value) {
  if (!isRecord(value) || !value.id || !value.documentId) return null;
  const run = {
    schemaVersion: value.schemaVersion || '1.0',
    id: value.id || null,
    documentId: value.documentId || null,
    documentRevision: value.documentRevision ?? null,
    state: value.state || null,
    capability: value.capability || null,
    action: value.action || null,
    scope: value.scope || null,
    selection: value.selection || null,
    iteration: value.iteration ?? null,
    maxIterations: value.maxIterations ?? null,
    inputHash: value.inputHash || null,
    outputHash: value.outputHash || null,
    evaluation: isRecord(value.evaluation)
      ? {
        status: value.evaluation.status || 'pending',
        score: value.evaluation.score ?? 0,
        gateCount: Array.isArray(value.evaluation.gates) ? value.evaluation.gates.length : 0,
      }
      : null,
    factCount: Array.isArray(value.factLedger) ? value.factLedger.length : 0,
    citationRouteCount: Array.isArray(value.citationRouting) ? value.citationRouting.length : 0,
    annotationCount: Array.isArray(value.annotations) ? value.annotations.length : 0,
    reportArtifactCount: Array.isArray(value.reportArtifacts) ? value.reportArtifacts.length : 0,
    startedAt: value.startedAt || null,
    completedAt: value.completedAt || null,
    failureReason: value.failureReason || null,
  };
  return run;
}

/**
 * Build the native/client checkpoint shape used for recovery metadata.
 *
 * The stream event table is the content journal. A checkpoint therefore keeps
 * the replay cursor and execution metadata, but deliberately does not carry
 * accumulated `channels.*`, `receivedEventIds`, `source`, `outputs`, or review
 * bodies. `streamState.lastSequence` is reset to -1 and
 * `execution.completedSequence` records the last fully verified batch so
 * recovery always replays the native event journal from the beginning.
 */
export function createLightweightCreationCheckpoint(checkpointValue, options = {}) {
  const checkpoint = isRecord(checkpointValue) ? checkpointValue : {};
  if (!validCheckpoint(checkpoint)) throw new TypeError('Creation execution checkpoint is invalid');
  const stream = createHtmlStudioStreamState(checkpoint.streamState);
  const execution = isRecord(checkpoint.execution) ? checkpoint.execution : {};
  const run = isRecord(checkpoint.writingRun) ? checkpoint.writingRun : null;
  const candidate = isRecord(execution.candidate) ? execution.candidate : null;
  const compactExecution = {
    documentId: execution.documentId || run?.documentId || null,
    documentRevision: execution.documentRevision ?? run?.documentRevision ?? null,
    documentTitle: execution.documentTitle || null,
    documentInputHash: execution.documentInputHash || null,
    sourceHash: execution.sourceHash || run?.inputHash || null,
    sourceAsset: clone(execution.sourceAsset || options.sourceAsset || null),
    scope: execution.scope || run?.scope || null,
    nextChunkIndex: nonNegativeInteger(execution.nextChunkIndex, 0),
    chunkCount: nonNegativeInteger(execution.chunkCount, 0),
    completedSequence: integer(execution.completedSequence, -1),
    protectedBlockCount: nonNegativeInteger(
      execution.protectedBlockCount,
      Array.isArray(execution.protectedBlocks) ? execution.protectedBlocks.length : 0,
    ),
    preserve: isRecord(execution.preserve) ? clone(execution.preserve) : {},
    configuration: isRecord(execution.configuration) ? clone(execution.configuration) : {},
    label: execution.label || '',
    instruction: execution.instruction || '',
    // Active checkpoints must stay constant-size. Model trace IDs are already
    // durable diagnostic events; copy the full list only once a final review
    // candidate exists (or when explicitly requested by the caller).
    traceIds: candidate || options.includeTraceIds === true
      ? (Array.isArray(execution.traceIds) ? [...execution.traceIds] : [])
      : [],
    traceCount: Array.isArray(execution.traceIds) ? execution.traceIds.length : 0,
    selection: execution.selection ? clone(execution.selection) : null,
    capability: execution.capability || stream.capability || run?.capability || null,
    recoverable: execution.recoverable === true,
    candidate: candidate ? compactCandidate(candidate) : null,
  };
  const lightweight = {
    schemaVersion: '1.0',
    kind: 'creationExecutionCheckpoint',
    checkpointId: checkpoint.checkpointId || null,
    ...(options.includeWritingRun === true ? {
      writingRun: options.compactWritingRun === true
        ? compactCreationWritingRunReference(run)
        : clone(run),
    } : {}),
    streamState: {
      streamId: stream.streamId || null,
      operationId: stream.operationId || null,
      capability: stream.capability || null,
      status: 'idle',
      // Native events are the content journal. Always replay them from the
      // beginning and use execution.completedSequence as the durable batch
      // boundary so a partially received next batch can never be committed.
      lastSequence: -1,
    },
    execution: compactExecution,
    checkpointedAt: checkpoint.checkpointedAt || new Date().toISOString(),
  };
  return lightweight;
}

function compactCandidate(candidate) {
  return {
    kind: candidate.kind || null,
    grounded: candidate.grounded === true,
    scope: candidate.scope || null,
    chunkCount: nonNegativeInteger(candidate.chunkCount, 0),
    createdAt: candidate.createdAt || null,
    documentId: candidate.documentId || null,
    documentRevision: candidate.documentRevision ?? null,
    documentInputHash: candidate.documentInputHash || null,
    runId: candidate.runId || null,
    traceCount: Array.isArray(candidate.traceIds) ? candidate.traceIds.length : 0,
    allowIteration: candidate.allowIteration !== false,
  };
}

function eventSequence(event, fallback = -1) {
  const sequence = integer(event?.sequence, fallback);
  if (sequence < 0) throw new TypeError('Creation native event sequence is invalid');
  return sequence;
}

function validTraceId(value) {
  const candidate = String(value || '').trim();
  return /^[A-Za-z0-9][A-Za-z0-9._-]{0,159}$/u.test(candidate) ? candidate : null;
}

export function extractCreationTraceIds(eventsValue) {
  const traceIds = [];
  const seen = new Set();
  for (const event of Array.isArray(eventsValue) ? eventsValue : []) {
    if (event?.eventType !== 'diagnostic' || event?.payload?.code !== 'writing.model-trace') continue;
    const message = String(event.payload.traceId || event.payload.message || '');
    const explicit = validTraceId(event.payload.traceId);
    const matched = message.match(/trace-[A-Za-z0-9._-]{1,153}/u)?.[0] || null;
    const traceId = explicit || validTraceId(matched) || validTraceId(message);
    if (!traceId || seen.has(traceId)) continue;
    seen.add(traceId);
    traceIds.push(traceId);
  }
  return traceIds;
}

function replayEvents(events, initialState, checkpointSequence, storedLastSequence) {
  let state = createHtmlStudioStreamState(initialState);
  let completedState = checkpointSequence === -1 ? state : null;
  for (const rawEvent of events) {
    const sequence = eventSequence(rawEvent);
    if (sequence > storedLastSequence) break;
    if (sequence !== state.lastSequence + 1) {
      throw new Error(`Creation native event replay sequence is not contiguous: expected ${state.lastSequence + 1}, received ${sequence}`);
    }
    state = reduceHtmlStudioStreamEvent(state, rawEvent);
    if (sequence === checkpointSequence) completedState = state;
  }
  return { completedState, fullState: state };
}

/**
 * Replays a full native CreationRunRecord without retaining a second full
 * stream in the checkpoint. `completedMarkdown` is the text through the
 * durable checkpoint cursor; `partialBatchMarkdown` is any append-only model
 * response received after that cursor before interruption.
 */
export function replayCreationNativeRecord(recordValue) {
  const record = isRecord(recordValue) ? recordValue : {};
  if (!isRecord(record.writingRun)) throw new TypeError('Creation native record is missing writingRun');
  const events = Array.isArray(record.events) ? record.events : [];
  const checkpoint = isRecord(record.latestCheckpoint) ? record.latestCheckpoint : null;
  const checkpointStream = createHtmlStudioStreamState(checkpoint?.streamState);
  const checkpointSequence = integer(
    checkpoint?.execution?.completedSequence,
    checkpointStream.lastSequence,
  );
  const storedLastSequence = integer(record.lastSequence, -1);
  if (storedLastSequence < -1) throw new TypeError('Creation native record lastSequence is invalid');
  if (checkpointSequence > storedLastSequence) throw new Error('Creation checkpoint sequence is ahead of native stream');

  const identity = {
    streamId: checkpointStream.streamId || record.streamId || '',
    operationId: checkpointStream.operationId || record.operationId || '',
    capability: checkpoint?.streamState?.capability || record.capability || 'creation.generate',
    lastSequence: -1,
    status: 'idle',
  };
  const replayed = replayEvents(events, identity, checkpointSequence, storedLastSequence);
  const completedState = replayed.completedState;
  const fullState = replayed.fullState;
  if (!completedState || completedState.lastSequence !== checkpointSequence) {
    throw new Error(`Creation native event replay is missing checkpoint events: expected ${checkpointSequence}, received ${completedState?.lastSequence ?? -1}`);
  }
  if (fullState.lastSequence !== storedLastSequence) {
    throw new Error(`Creation native event replay is incomplete: expected ${storedLastSequence}, received ${fullState.lastSequence}`);
  }

  const completedMarkdown = String(completedState.channels.text || '');
  const fullMarkdown = String(fullState.channels.text || '');
  const appendOnly = fullMarkdown.startsWith(completedMarkdown);
  const partialBatchMarkdown = appendOnly ? fullMarkdown.slice(completedMarkdown.length) : null;
  const traceIds = extractCreationTraceIds(events);
  const completedCheckpoint = checkpoint
    ? {
      ...clone(checkpoint),
      writingRun: clone(record.writingRun),
      streamState: completedState,
    }
    : {
      schemaVersion: '1.0',
      kind: 'creationExecutionCheckpoint',
      writingRun: clone(record.writingRun),
      streamState: completedState,
      execution: {},
      checkpointedAt: new Date().toISOString(),
    };
  const restoredCheckpoint = {
    ...clone(completedCheckpoint),
    streamState: fullState,
  };
  return {
    checkpoint: restoredCheckpoint,
    completedCheckpoint,
    completedState,
    fullState,
    completedMarkdown,
    fullMarkdown,
    partialBatchMarkdown,
    traceIds,
    appendOnly,
    checkpointSequence,
    lastSequence: storedLastSequence,
    // These already belong to the supplied native record and can be very large;
    // avoid making another full in-memory copy during replay.
    baseDocument: record.baseDocument || null,
    candidateDocument: record.candidateDocument || null,
    usage: record.usage ? clone(record.usage) : null,
  };
}

/**
 * Backwards-compatible name for callers that previously consumed a native
 * record as a checkpoint. This returns the replayed checkpoint only; callers
 * that need partial-batch diagnostics should use replayCreationNativeRecord.
 */
export function creationCheckpointFromNativeRecord(recordValue) {
  return replayCreationNativeRecord(recordValue).checkpoint;
}
