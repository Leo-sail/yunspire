const IDENTIFIER_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,199}$/u;
const HASH_PATTERN = /^sha256:[a-f0-9]{64}$/iu;
const REFLECTION_STATES = new Set([
  'queued',
  'running',
  'awaiting_review',
  'completed',
  'failed',
  'cancelled',
]);

function clone(value) {
  if (value === undefined) return undefined;
  return structuredClone(value);
}

function assertInvoke(invoke) {
  if (typeof invoke !== 'function') throw new TypeError('Reflection runtime requires an invoke function');
}

function assertObject(value, label) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new TypeError(`${label} must be an object`);
  return value;
}

function assertIdentifier(value, label) {
  const normalized = String(value || '').trim();
  if (!IDENTIFIER_PATTERN.test(normalized)) throw new TypeError(`${label} is invalid`);
  return normalized;
}

function optionalIdentifier(value, label) {
  if (value === undefined || value === null || value === '') return null;
  return assertIdentifier(value, label);
}

function boundedInteger(value, label, { minimum = 0, maximum = Number.MAX_SAFE_INTEGER, fallback = minimum } = {}) {
  if (value === undefined || value === null || value === '') return fallback;
  const number = Number(value);
  if (!Number.isSafeInteger(number) || number < minimum || number > maximum) throw new TypeError(`${label} is invalid`);
  return number;
}

function resolveAvailability(available) {
  return typeof available === 'function' ? Boolean(available()) : available !== false;
}

function unavailable(operation, fallback = null) {
  return {
    mode: 'browser',
    available: false,
    readOnly: true,
    operation,
    value: clone(fallback),
  };
}

function nativeValue(operation, value) {
  return {
    mode: 'native',
    available: true,
    readOnly: false,
    operation,
    value,
  };
}

function normalizeState(value) {
  const state = String(value || '').trim();
  if (!REFLECTION_STATES.has(state)) throw new TypeError(`Reflection state is invalid: ${state}`);
  return state;
}

function normalizeScope(value) {
  const scope = assertObject(value, 'Reflection scope');
  const required = ['userId', 'agentId', 'appId', 'projectId', 'sessionId'];
  const normalized = {};
  for (const key of required) {
    const entry = String(scope[key] || '').trim();
    if (!entry || entry.length > 160) throw new TypeError(`Reflection scope ${key} is invalid`);
    normalized[key] = entry;
  }
  return normalized;
}

export function normalizeReflectionJob(value) {
  const source = assertObject(value, 'Reflection job');
  const id = assertIdentifier(source.id, 'Reflection job id');
  const state = normalizeState(source.state);
  const sourceDocIds = Array.isArray(source.sourceDocIds) ? source.sourceDocIds : [];
  if (!sourceDocIds.length || sourceDocIds.length > 256) throw new TypeError('Reflection source document ids are invalid');
  return {
    ...clone(source),
    id,
    idempotencyKey: String(source.idempotencyKey || '').trim(),
    taskId: optionalIdentifier(source.taskId, 'Reflection task id'),
    scope: normalizeScope(source.scope),
    sourceDocIds: sourceDocIds.map((entry) => assertIdentifier(entry, 'Reflection source document id')),
    sourceContentHash: HASH_PATTERN.test(String(source.sourceContentHash || ''))
      ? String(source.sourceContentHash)
      : (() => { throw new TypeError('Reflection source content hash is invalid'); })(),
    sourceSnapshot: source.sourceSnapshot && typeof source.sourceSnapshot === 'object' && !Array.isArray(source.sourceSnapshot)
      ? clone(source.sourceSnapshot)
      : {},
    sourceSnapshotHash: HASH_PATTERN.test(String(source.sourceSnapshotHash || ''))
      ? String(source.sourceSnapshotHash)
      : (() => { throw new TypeError('Reflection source snapshot hash is invalid'); })(),
    metrics: source.metrics && typeof source.metrics === 'object' && !Array.isArray(source.metrics) ? clone(source.metrics) : {},
    state,
    proposalMemoryId: optionalIdentifier(source.proposalMemoryId, 'Reflection proposal memory id'),
    optimizationCandidateId: optionalIdentifier(source.optimizationCandidateId, 'Reflection optimization candidate id'),
    attemptCount: boundedInteger(source.attemptCount, 'Reflection attempt count', { fallback: 0 }),
    lastError: source.lastError === null || source.lastError === undefined ? null : String(source.lastError),
    claimedBy: source.claimedBy === null || source.claimedBy === undefined ? null : String(source.claimedBy),
    claimToken: source.claimToken === null || source.claimToken === undefined ? null : String(source.claimToken),
    leaseExpiresAtMs: source.leaseExpiresAtMs === null || source.leaseExpiresAtMs === undefined
      ? null
      : boundedInteger(source.leaseExpiresAtMs, 'Reflection lease expiry', { minimum: 0 }),
  };
}

export function normalizeReflectionJobList(value) {
  if (!Array.isArray(value)) throw new TypeError('Reflection job list must be an array');
  return value.map(normalizeReflectionJob);
}

function normalizeClaimResult(value) {
  if (value === null || value === undefined) return null;
  if (value && typeof value === 'object' && !Array.isArray(value) && Object.hasOwn(value, 'job')) {
    if (value.job === null || value.job === undefined) return null;
    const job = normalizeReflectionJob(value.job);
    const claimToken = String(value.claimToken || job.claimToken || '').trim();
    if (!claimToken || claimToken.length > 200) throw new TypeError('Reflection claim token is invalid');
    return { ...job, claimToken };
  }
  return normalizeReflectionJob(value);
}

function normalizeBeginInput(input) {
  const source = assertObject(input, 'Reflection begin input');
  const idempotencyKey = String(source.idempotencyKey || '').trim();
  if (!idempotencyKey || idempotencyKey.length > 200) throw new TypeError('Reflection idempotency key is invalid');
  const sourceDocIds = Array.isArray(source.sourceDocIds) ? source.sourceDocIds : [];
  if (!sourceDocIds.length || sourceDocIds.length > 256) throw new TypeError('Reflection source document ids are invalid');
  const sourceEffectIds = source.sourceEffectIds === undefined ? [] : source.sourceEffectIds;
  if (!Array.isArray(sourceEffectIds) || sourceEffectIds.length > 512) throw new TypeError('Reflection source effect ids are invalid');
  const sourceContentHash = String(source.sourceContentHash || '').trim();
  if (!HASH_PATTERN.test(sourceContentHash)) throw new TypeError('Reflection source content hash is invalid');
  const metrics = source.metrics === undefined ? {} : assertObject(source.metrics, 'Reflection metrics');
  const sourceSnapshot = source.sourceSnapshot === undefined ? {} : assertObject(source.sourceSnapshot, 'Reflection source snapshot');
  const sourceSnapshotHash = source.sourceSnapshotHash === undefined || source.sourceSnapshotHash === null
    ? null
    : String(source.sourceSnapshotHash || '').trim();
  if (sourceSnapshotHash !== null && !HASH_PATTERN.test(sourceSnapshotHash)) throw new TypeError('Reflection source snapshot hash is invalid');
  return {
    idempotencyKey,
    taskId: optionalIdentifier(source.taskId, 'Reflection task id'),
    scope: normalizeScope(source.scope),
    sourceDocIds: sourceDocIds.map((entry) => assertIdentifier(entry, 'Reflection source document id')),
    sourceContentHash,
    metrics: clone(metrics),
    sourceEffectIds: sourceEffectIds.map((entry) => assertIdentifier(entry, 'Reflection source effect id')),
    sourceSnapshot: clone(sourceSnapshot),
    sourceSnapshotHash,
  };
}

function normalizeListRequest(input = {}) {
  const request = assertObject(input, 'Reflection list request');
  const states = request.states === undefined ? [] : request.states;
  if (!Array.isArray(states)) throw new TypeError('Reflection states must be an array');
  return {
    states: states.map(normalizeState),
    limit: boundedInteger(request.limit, 'Reflection list limit', { minimum: 1, maximum: 500, fallback: 100 }),
  };
}

function fallbackList(fallbackJobs, states) {
  return fallbackJobs.filter((job) => !states.length || states.includes(job.state));
}

export function createReflectionRuntime(invoke, { available = true, fallbackJobs = [] } = {}) {
  assertInvoke(invoke);
  let localFallbackJobs = normalizeReflectionJobList(fallbackJobs);
  const isAvailable = () => resolveAvailability(available);

  async function begin(input) {
    const request = normalizeBeginInput(input);
    if (!isAvailable()) return unavailable('begin_memory_reflection');
    return nativeValue('begin_memory_reflection', normalizeReflectionJob(await invoke('begin_memory_reflection', { input: request })));
  }

  async function get(jobId, fallback = null) {
    const id = assertIdentifier(jobId, 'Reflection job id');
    if (!isAvailable()) return unavailable('get_memory_reflection', fallback || localFallbackJobs.find((job) => job.id === id) || null);
    return nativeValue('get_memory_reflection', normalizeReflectionJob(await invoke('get_memory_reflection', { jobId: id })));
  }

  async function list(input = {}) {
    const request = normalizeListRequest(input);
    if (!isAvailable()) return unavailable('list_memory_reflections', fallbackList(localFallbackJobs, request.states));
    const jobs = normalizeReflectionJobList(await invoke('list_memory_reflections', { request }));
    localFallbackJobs = jobs;
    return nativeValue('list_memory_reflections', jobs);
  }

  async function claim({ workerId, leaseSeconds = 300 } = {}) {
    const input = {
      workerId: assertIdentifier(workerId, 'Reflection worker id'),
      leaseSeconds: boundedInteger(leaseSeconds, 'Reflection lease seconds', { minimum: 5, maximum: 900, fallback: 300 }),
    };
    if (!isAvailable()) return unavailable('claim_memory_reflection');
    return nativeValue('claim_memory_reflection', normalizeClaimResult(await invoke('claim_memory_reflection', { input })));
  }

  async function renew({ jobId, claimToken, leaseSeconds = 300 }) {
    const id = assertIdentifier(jobId, 'Reflection job id');
    const token = String(claimToken || '').trim();
    if (!token || token.length > 200) throw new TypeError('Reflection claim token is invalid');
    const lease = boundedInteger(leaseSeconds, 'Reflection lease seconds', { minimum: 5, maximum: 900, fallback: 300 });
    if (!isAvailable()) return unavailable('renew_memory_reflection_lease');
    return nativeValue('renew_memory_reflection_lease', normalizeReflectionJob(await invoke('renew_memory_reflection_lease', {
      jobId: id,
      claimToken: token,
      leaseSeconds: lease,
    })));
  }

  async function complete({ jobId, claimToken, proposal, candidateId = null }) {
    const id = assertIdentifier(jobId, 'Reflection job id');
    const token = String(claimToken || '').trim();
    if (!token || token.length > 200) throw new TypeError('Reflection claim token is invalid');
    assertObject(proposal, 'Reflection proposal');
    const normalizedCandidateId = optionalIdentifier(candidateId, 'Reflection optimization candidate id');
    if (!isAvailable()) return unavailable('complete_memory_reflection');
    return nativeValue('complete_memory_reflection', normalizeReflectionJob(await invoke('complete_memory_reflection', {
      jobId: id,
      claimToken: token,
      proposal: clone(proposal),
      candidateId: normalizedCandidateId,
    })));
  }

  async function review({ jobId, decision }) {
    const id = assertIdentifier(jobId, 'Reflection job id');
    const normalizedDecision = String(decision || '').trim();
    if (!['approve', 'reject', 'revise'].includes(normalizedDecision)) throw new TypeError('Reflection review decision is invalid');
    if (!isAvailable()) return unavailable('review_memory_reflection');
    return nativeValue('review_memory_reflection', normalizeReflectionJob(await invoke('review_memory_reflection', {
      jobId: id,
      decision: normalizedDecision,
    })));
  }

  async function fail({ jobId, claimToken, error }) {
    const id = assertIdentifier(jobId, 'Reflection job id');
    const token = String(claimToken || '').trim();
    const reason = String(error || '').trim();
    if (!token || token.length > 200 || !reason || reason.length > 2_000) throw new TypeError('Reflection failure input is invalid');
    if (!isAvailable()) return unavailable('fail_memory_reflection');
    return nativeValue('fail_memory_reflection', normalizeReflectionJob(await invoke('fail_memory_reflection', {
      jobId: id,
      claimToken: token,
      error: reason,
    })));
  }

  async function cancel({ jobId, reason = 'User cancelled reflection' }) {
    const id = assertIdentifier(jobId, 'Reflection job id');
    const normalizedReason = String(reason || '').trim();
    if (!normalizedReason || normalizedReason.length > 2_000) throw new TypeError('Reflection cancellation reason is invalid');
    if (!isAvailable()) return unavailable('cancel_memory_reflection');
    return nativeValue('cancel_memory_reflection', normalizeReflectionJob(await invoke('cancel_memory_reflection', {
      jobId: id,
      reason: normalizedReason,
    })));
  }

  return Object.freeze({
    mode: () => isAvailable() ? 'native' : 'browser',
    isNativeAvailable: isAvailable,
    begin,
    get,
    list,
    claim,
    renew,
    complete,
    review,
    fail,
    cancel,
    fallbackJobs: () => clone(localFallbackJobs),
  });
}

export function createReflectionWorker(runtime, { workerId, leaseSeconds = 300, onLeaseLost = null } = {}) {
  if (!runtime || typeof runtime.claim !== 'function') throw new TypeError('Reflection worker requires a reflection runtime');
  const normalizedWorkerId = assertIdentifier(workerId, 'Reflection worker id');
  const normalizedLeaseSeconds = boundedInteger(leaseSeconds, 'Reflection lease seconds', { minimum: 5, maximum: 900, fallback: 300 });
  let inFlight = null;

  async function runOnce(processJob) {
    if (typeof processJob !== 'function') throw new TypeError('Reflection worker requires a process function');
    if (inFlight) return inFlight;
    inFlight = (async () => {
      const claim = await runtime.claim({ workerId: normalizedWorkerId, leaseSeconds: normalizedLeaseSeconds });
      if (!claim.available || !claim.value) return claim;
      const job = claim.value;
      const token = String(job.claimToken || '').trim();
      if (!token) throw new Error('Claimed reflection job is missing a claim token');
      try {
        const proposal = await processJob(clone(job));
        return await runtime.complete({ jobId: job.id, claimToken: token, proposal });
      } catch (error) {
        const reason = String(error?.message || error || 'Reflection worker failed').slice(0, 2_000);
        try {
          await runtime.fail({ jobId: job.id, claimToken: token, error: reason });
        } catch (failureError) {
          if (typeof onLeaseLost === 'function') onLeaseLost(failureError, job);
          const failure = error instanceof Error ? error : new Error(String(error));
          failure.reflectionFailureRecordError = failureError;
          error = failure;
        }
        throw error instanceof Error ? error : new Error(String(error));
      }
    })().finally(() => {
      inFlight = null;
    });
    return inFlight;
  }

  return Object.freeze({
    runOnce,
    isRunning: () => Boolean(inFlight),
  });
}

export function createReflectionTimer(run, {
  delayMs = 15_000,
  intervalMs = 6 * 60 * 60 * 1_000,
  setTimeoutFn = globalThis.setTimeout,
  clearTimeoutFn = globalThis.clearTimeout,
  onError = null,
} = {}) {
  if (typeof run !== 'function') throw new TypeError('Reflection timer requires a run function');
  if (typeof setTimeoutFn !== 'function' || typeof clearTimeoutFn !== 'function') throw new TypeError('Reflection timer requires timeout functions');
  const initialDelay = boundedInteger(delayMs, 'Reflection timer delay', { minimum: 0 });
  const repeatDelay = boundedInteger(intervalMs, 'Reflection timer interval', { minimum: 1 });
  let timer = null;
  let started = false;
  let inFlight = null;

  function schedule(delay) {
    if (!started || timer !== null) return;
    timer = setTimeoutFn(() => {
      timer = null;
      void trigger().catch((error) => {
        if (typeof onError === 'function') onError(error);
      });
    }, delay);
  }

  function trigger() {
    if (inFlight) return inFlight;
    inFlight = Promise.resolve()
      .then(run)
      .finally(() => {
        inFlight = null;
        schedule(repeatDelay);
      });
    return inFlight;
  }

  function start() {
    if (started) return;
    started = true;
    schedule(initialDelay);
  }

  function stop() {
    started = false;
    if (timer !== null) clearTimeoutFn(timer);
    timer = null;
  }

  return Object.freeze({
    start,
    stop,
    trigger,
    snapshot: () => ({ started, scheduled: timer !== null, running: Boolean(inFlight) }),
  });
}
