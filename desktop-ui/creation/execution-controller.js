import {
  createHtmlStudioStreamState,
  normalizeAgentStreamEvent,
  reduceHtmlStudioStreamEvent,
} from './html-studio.js';

const TERMINAL_STREAM_STATUSES = new Set(['completed', 'failed', 'cancelled']);
const TERMINAL_WRITING_STATES = new Set(['awaitingReview', 'succeeded', 'failed', 'cancelled']);

function isRecord(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

function clone(value) {
  if (typeof structuredClone === 'function') return structuredClone(value);
  return JSON.parse(JSON.stringify(value));
}

function validDateTime(value, fallback = new Date().toISOString()) {
  return typeof value === 'string' && Number.isFinite(Date.parse(value)) ? new Date(value).toISOString() : fallback;
}

function clockValue(now) {
  return validDateTime(typeof now === 'function' ? now() : now);
}

function ensureWritingRun(value) {
  if (!isRecord(value) || !value.id || !value.documentId || !value.inputHash) {
    throw new TypeError('Creation execution requires a valid WritingRun');
  }
  return clone(value);
}

function eventIdentityMatches(stream, event) {
  return (!stream.streamId || stream.streamId === event.streamId)
    && (!stream.operationId || stream.operationId === event.operationId)
    && (!stream.capability || stream.capability === event.capability);
}

function applyWritingRunEvent(runValue, event) {
  const run = clone(runValue);
  if (event.eventType === 'streamStarted') {
    run.state = 'running';
    run.failureReason = null;
    run.completedAt = null;
  } else if (event.eventType === 'streamCompleted') {
    run.state = 'awaitingReview';
    run.failureReason = null;
    run.completedAt = null;
  } else if (event.eventType === 'streamFailed') {
    run.state = 'failed';
    run.failureReason = event.payload.message;
    run.completedAt = event.timestamp;
  } else if (event.eventType === 'streamCancelled') {
    run.state = 'cancelled';
    run.failureReason = event.payload.reason;
    run.completedAt = event.timestamp;
  } else if (run.state === 'queued') {
    run.state = 'running';
  }
  return run;
}

function terminalState(writingRun, streamState) {
  return TERMINAL_STREAM_STATUSES.has(streamState.status) || TERMINAL_WRITING_STATES.has(writingRun.state);
}

export function createCreationExecutionController({
  writingRun,
  streamState = {},
  abort = () => {},
  now = () => new Date().toISOString(),
} = {}) {
  let run = ensureWritingRun(writingRun);
  let stream = createHtmlStudioStreamState({
    ...streamState,
    capability: streamState.capability || 'creation.edit',
  });
  let abortCalled = false;

  const snapshot = () => ({ writingRun: clone(run), streamState: clone(stream) });
  const reject = (reason, event = null) => ({ accepted: false, reason, event, snapshot: snapshot() });

  function accept(input, context = {}) {
    if (terminalState(run, stream)) return reject('terminal');
    let event;
    try {
      event = normalizeAgentStreamEvent(input, {
        streamId: stream.streamId || context.streamId,
        operationId: stream.operationId || context.operationId,
        capability: stream.capability || context.capability || 'creation.edit',
        now: context.now || clockValue(now),
        ...context,
      });
    } catch (error) {
      return reject('invalid_event', { error: String(error) });
    }
    if (!eventIdentityMatches(stream, event)) return reject('identity_mismatch', event);
    if (stream.receivedEventIds.includes(event.eventId)) return reject('duplicate_event', event);
    if (event.sequence <= stream.lastSequence) return reject('stale_sequence', event);
    if (event.sequence !== stream.lastSequence + 1) return reject('sequence_gap', event);
    if (stream.lastSequence === -1 && event.eventType !== 'streamStarted') return reject('invalid_transition', event);
    if (stream.lastSequence >= 0 && event.eventType === 'streamStarted') return reject('invalid_transition', event);

    try {
      stream = reduceHtmlStudioStreamEvent(stream, event);
    } catch (error) {
      return reject('invalid_event', { ...event, error: String(error) });
    }
    run = applyWritingRunEvent(run, event);
    return { accepted: true, reason: null, event, snapshot: snapshot() };
  }

  function cancel(reason = '用户取消') {
    if (terminalState(run, stream)) return { cancelled: false, reason: 'terminal', snapshot: snapshot() };
    if (!abortCalled) {
      abortCalled = true;
      try {
        abort(reason);
      } catch {
        // Local cancellation remains authoritative even if transport cancellation fails.
      }
    }
    if (stream.lastSequence === -1) {
      const started = accept({
        streamId: stream.streamId || `stream-${run.id}`,
        operationId: stream.operationId || `operation-${run.id}`,
        capability: stream.capability || 'creation.edit',
        eventId: `event-0-${run.id}`,
        eventType: 'streamStarted',
        sequence: 0,
        timestamp: clockValue(now),
        payload: { agentId: 'local-creation-agent', protocolVersion: '1.0' },
      });
      if (!started.accepted) return { cancelled: false, reason: started.reason, snapshot: started.snapshot };
    }
    const sequence = stream.lastSequence + 1;
    const cancelled = accept({
      streamId: stream.streamId,
      operationId: stream.operationId,
      capability: stream.capability,
      eventId: `event-${sequence}-${run.id}`,
      eventType: 'streamCancelled',
      sequence,
      timestamp: clockValue(now),
      payload: { reason },
    });
    return { cancelled: cancelled.accepted, reason: cancelled.reason, snapshot: cancelled.snapshot };
  }

  function updateWritingRun(changes = {}) {
    if (!isRecord(changes)) throw new TypeError('WritingRun changes must be an object');
    run = { ...run, ...clone(changes) };
    return snapshot();
  }

  function checkpoint(execution = {}) {
    return {
      schemaVersion: '1.0',
      kind: 'creationExecutionCheckpoint',
      writingRun: clone(run),
      streamState: clone(stream),
      execution: isRecord(execution) ? clone(execution) : {},
      checkpointedAt: clockValue(now),
    };
  }

  return {
    accept,
    cancel,
    checkpoint,
    snapshot,
    updateWritingRun,
  };
}

export function restoreCreationExecutionController(checkpointValue, context = {}, options = {}) {
  const checkpoint = isRecord(checkpointValue) ? checkpointValue : {};
  if (checkpoint.schemaVersion !== '1.0' || checkpoint.kind !== 'creationExecutionCheckpoint') {
    throw new Error('Creation execution checkpoint is invalid');
  }
  const run = ensureWritingRun(checkpoint.writingRun);
  const expected = {
    documentId: String(context.documentId || ''),
    documentRevision: Number(context.documentRevision || 0),
    inputHash: String(context.inputHash || ''),
  };
  if (expected.documentId && run.documentId !== expected.documentId) throw new Error('WritingRun checkpoint belongs to another document');
  if (expected.documentRevision && Number(run.documentRevision) !== expected.documentRevision) throw new Error('WritingRun checkpoint revision is stale');
  if (expected.inputHash && run.inputHash !== expected.inputHash) throw new Error('WritingRun checkpoint input hash is stale');
  const stream = createHtmlStudioStreamState(checkpoint.streamState);
  if (terminalState(run, stream)) throw new Error('Terminal WritingRun checkpoints cannot resume execution');
  return createCreationExecutionController({
    writingRun: run,
    streamState: stream,
    abort: options.abort,
    now: options.now,
  });
}
