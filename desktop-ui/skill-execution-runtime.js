const IDENTIFIER_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,199}$/u;
const SKILL_ID_PATTERN = /^[a-z][a-z0-9-]{0,63}$/u;
const HASH_PATTERN = /^sha256:[a-f0-9]{64}$/iu;
const EFFECT_OUTCOMES = new Set(['started', 'succeeded', 'failed', 'cancelled']);
const EFFECT_RELATIONS = new Set(['correction', 'acceptance']);
const MAX_EXECUTION_CACHE = 512;

function clone(value) {
  if (value === undefined) return undefined;
  return structuredClone(value);
}

function assertInvoke(invoke) {
  if (typeof invoke !== 'function') throw new TypeError('Skill execution runtime requires an invoke function');
}

function assertObject(value, label) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new TypeError(`${label} must be an object`);
  return value;
}

function assertIdentifier(value, label, pattern = IDENTIFIER_PATTERN) {
  const normalized = String(value || '').trim();
  if (!pattern.test(normalized)) throw new TypeError(`${label} is invalid`);
  return normalized;
}

function optionalIdentifier(value, label) {
  if (value === undefined || value === null || value === '') return null;
  return assertIdentifier(value, label);
}

function normalizeOperationContext(value) {
  if (value === undefined || value === null) return null;
  const source = assertObject(value, 'Skill operation context');
  return {
    taskId: optionalIdentifier(source.taskId, 'Skill operation task id'),
    traceId: optionalIdentifier(source.traceId, 'Skill operation trace id'),
    executionTicket: optionalIdentifier(source.executionTicket, 'Skill operation execution ticket'),
  };
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

function nativeValue(operation, value, metadata = {}) {
  return {
    mode: 'native',
    available: true,
    readOnly: false,
    operation,
    value,
    ...metadata,
  };
}

function normalizeWarnings(value) {
  if (!Array.isArray(value) || value.length > 64) throw new TypeError('Skill execution warnings are invalid');
  return value.map((warning) => {
    const normalized = String(warning || '').trim();
    if (!normalized || normalized.length > 2_000) throw new TypeError('Skill execution warning is invalid');
    return normalized;
  });
}

function normalizeUsage(value) {
  const usage = assertObject(value, 'Skill execution usage');
  const result = clone(usage);
  for (const field of ['promptTokens', 'completionTokens', 'totalTokens', 'durationMs']) {
    if (usage[field] === undefined) continue;
    result[field] = boundedInteger(usage[field], `Skill execution ${field}`, { fallback: 0 });
  }
  if (usage.estimatedCostUsd !== undefined && usage.estimatedCostUsd !== null) {
    const cost = Number(usage.estimatedCostUsd);
    if (!Number.isFinite(cost) || cost < 0) throw new TypeError('Skill execution estimated cost is invalid');
    result.estimatedCostUsd = cost;
  }
  return result;
}

function executionInputSize(value) {
  try {
    return new TextEncoder().encode(JSON.stringify(value)).byteLength;
  } catch {
    throw new TypeError('Skill execution input is not serializable');
  }
}

function normalizeExecutionInput(input) {
  const source = assertObject(input, 'Skill execution input');
  const skillId = assertIdentifier(source.skillId, 'Skill id', SKILL_ID_PATTERN);
  const expectedVersion = boundedInteger(source.expectedVersion, 'Skill version', { minimum: 1 });
  const expectedPayloadHash = String(source.expectedPayloadHash || '').trim().toLowerCase();
  if (!HASH_PATTERN.test(expectedPayloadHash)) throw new TypeError('Skill payload hash is invalid');
  const requestId = assertIdentifier(source.requestId, 'Skill request id');
  const traceId = assertIdentifier(source.traceId, 'Skill trace id');
  const taskId = optionalIdentifier(source.taskId, 'Skill task id');
  const operationContext = normalizeOperationContext(source.operationContext);
  if (source.input === undefined) throw new TypeError('Skill execution input payload is required');
  if (executionInputSize(source.input) > 512 * 1_024) throw new TypeError('Skill execution input exceeds 512 KB');
  return {
    skillId,
    expectedVersion,
    expectedPayloadHash,
    input: clone(source.input),
    requestId,
    taskId,
    traceId,
    ...(operationContext ? { operationContext } : {}),
  };
}

function canonicalJson(value) {
  try {
    return JSON.stringify(value, (_key, nested) => {
      if (!nested || typeof nested !== 'object' || Array.isArray(nested)) return nested;
      const canonical = Object.create(null);
      for (const key of Object.keys(nested).sort()) canonical[key] = nested[key];
      return canonical;
    });
  } catch {
    throw new TypeError('Skill execution request is not serializable');
  }
}

function executionRequestFingerprint(request) {
  return canonicalJson({
    skillId: request.skillId,
    expectedVersion: request.expectedVersion,
    expectedPayloadHash: request.expectedPayloadHash,
    input: request.input,
    taskId: request.taskId,
    traceId: request.traceId,
    operationContext: request.operationContext,
  });
}

export function normalizeSkillExecutionResult(value, expected = null) {
  const source = assertObject(value, 'Skill execution result');
  const skill = assertObject(source.skill, 'Skill execution identity');
  const trace = assertObject(source.trace, 'Skill execution trace');
  const model = assertObject(source.model, 'Skill execution model');
  const normalized = {
    ...clone(source),
    outputText: String(source.outputText || ''),
    outputData: clone(source.outputData),
    warnings: normalizeWarnings(source.warnings || []),
    skill: {
      ...clone(skill),
      id: assertIdentifier(skill.id, 'Skill result id', SKILL_ID_PATTERN),
      name: String(skill.name || '').trim(),
      version: boundedInteger(skill.version, 'Skill result version', { minimum: 1 }),
      payloadHash: String(skill.payloadHash || '').trim().toLowerCase(),
    },
    trace: {
      ...clone(trace),
      traceId: assertIdentifier(trace.traceId, 'Skill result trace id'),
      requestId: assertIdentifier(trace.requestId, 'Skill result request id'),
      startedAt: String(trace.startedAt || ''),
      completedAt: String(trace.completedAt || ''),
    },
    model: {
      ...clone(model),
      provider: String(model.provider || '').trim(),
      model: String(model.model || '').trim(),
    },
    usage: normalizeUsage(source.usage || {}),
  };
  if (!normalized.skill.name || !HASH_PATTERN.test(normalized.skill.payloadHash)) throw new TypeError('Skill execution identity is incomplete');
  if (!normalized.trace.startedAt || !normalized.trace.completedAt || !normalized.model.provider || !normalized.model.model) {
    throw new TypeError('Skill execution receipt is incomplete');
  }
  if (expected) {
    if (normalized.skill.id !== expected.skillId
      || normalized.skill.version !== expected.expectedVersion
      || normalized.skill.payloadHash !== expected.expectedPayloadHash
      || normalized.trace.requestId !== expected.requestId
      || normalized.trace.traceId !== expected.traceId) {
      throw new Error('Skill execution receipt does not match the frozen request identity');
    }
  }
  return normalized;
}

function normalizeEffectFeedback(value) {
  const source = assertObject(value, 'Skill execution effect feedback');
  const relationKind = String(source.relationKind || '').trim();
  if (!EFFECT_RELATIONS.has(relationKind)) throw new TypeError('Skill execution effect feedback relation is invalid');
  return {
    ...clone(source),
    id: assertIdentifier(source.id, 'Skill execution effect feedback id'),
    relationKind,
    referenceId: assertIdentifier(source.referenceId, 'Skill execution effect feedback reference id'),
    note: String(source.note || ''),
    createdAt: String(source.createdAt || ''),
  };
}

export function normalizeSkillExecutionEffect(value) {
  const source = assertObject(value, 'Skill execution effect');
  const inputHash = String(source.inputHash || '').trim().toLowerCase();
  const outputHash = source.outputHash === null || source.outputHash === undefined
    ? null
    : String(source.outputHash).trim().toLowerCase();
  const outcome = String(source.outcome || '').trim();
  if (!HASH_PATTERN.test(inputHash) || (outputHash && !HASH_PATTERN.test(outputHash))) throw new TypeError('Skill execution effect hash is invalid');
  if (!EFFECT_OUTCOMES.has(outcome)) throw new TypeError('Skill execution effect outcome is invalid');
  if (outcome === 'succeeded' && !outputHash) throw new TypeError('Successful Skill execution effect requires an output hash');
  return {
    ...clone(source),
    id: assertIdentifier(source.id, 'Skill execution effect id'),
    executionId: assertIdentifier(source.executionId, 'Skill execution id'),
    skillId: assertIdentifier(source.skillId, 'Skill execution effect skill id', SKILL_ID_PATTERN),
    skillVersion: boundedInteger(source.skillVersion, 'Skill execution effect version', { minimum: 1 }),
    requestId: assertIdentifier(source.requestId, 'Skill execution effect request id'),
    taskId: optionalIdentifier(source.taskId, 'Skill execution effect task id'),
    traceId: assertIdentifier(source.traceId, 'Skill execution effect trace id'),
    inputHash,
    outputHash,
    outcome,
    startedAt: String(source.startedAt || ''),
    completedAt: source.completedAt === null || source.completedAt === undefined ? null : String(source.completedAt),
    warnings: normalizeWarnings(source.warnings || []),
    error: source.error === null || source.error === undefined ? null : String(source.error),
    createdAt: String(source.createdAt || ''),
    feedback: Array.isArray(source.feedback) ? source.feedback.map(normalizeEffectFeedback) : [],
  };
}

export function normalizeSkillExecutionEffects(value) {
  if (!Array.isArray(value)) throw new TypeError('Skill execution effect list must be an array');
  const unique = new Map();
  for (const effect of value.map(normalizeSkillExecutionEffect)) {
    const key = `${effect.executionId}\u0000${effect.outcome}`;
    const current = unique.get(key);
    if (current && current.id !== effect.id) throw new Error('Skill execution effect list contains conflicting duplicate outcomes');
    if (!current) unique.set(key, effect);
  }
  return [...unique.values()];
}

function normalizeEffectQuery(query = {}) {
  const source = assertObject(query, 'Skill execution effect query');
  const outcomes = source.outcomes === undefined ? [] : source.outcomes;
  if (!Array.isArray(outcomes)) throw new TypeError('Skill execution effect outcomes must be an array');
  return {
    skillId: source.skillId ? assertIdentifier(source.skillId, 'Skill effect query skill id', SKILL_ID_PATTERN) : null,
    requestId: source.requestId ? assertIdentifier(source.requestId, 'Skill effect query request id') : null,
    taskId: source.taskId ? assertIdentifier(source.taskId, 'Skill effect query task id') : null,
    traceId: source.traceId ? assertIdentifier(source.traceId, 'Skill effect query trace id') : null,
    outcomes: outcomes.map((outcome) => {
      const normalized = String(outcome || '').trim();
      if (!EFFECT_OUTCOMES.has(normalized)) throw new TypeError('Skill execution effect outcome is invalid');
      return normalized;
    }),
    limit: boundedInteger(source.limit, 'Skill execution effect query limit', { minimum: 1, maximum: 500, fallback: 100 }),
  };
}

function remember(cache, key, value) {
  cache.set(key, value);
  if (cache.size > MAX_EXECUTION_CACHE) cache.delete(cache.keys().next().value);
}

export function createSkillExecutionRuntime(invoke, { available = true, fallbackEffects = [] } = {}) {
  assertInvoke(invoke);
  const isAvailable = () => resolveAvailability(available);
  const pendingExecutions = new Map();
  const completedExecutions = new Map();
  const pendingFeedback = new Map();
  const completedFeedback = new Map();
  let localFallbackEffects = normalizeSkillExecutionEffects(fallbackEffects);

  async function execute(input) {
    const request = normalizeExecutionInput(input);
    const fingerprint = executionRequestFingerprint(request);
    if (!isAvailable()) return unavailable('execute_skill');
    const completed = completedExecutions.get(request.requestId);
    if (completed) {
      if (completed.fingerprint !== fingerprint) {
        throw new Error('Skill execution request id is already bound to a different request');
      }
      return nativeValue('execute_skill', clone(completed.result), { deduplicated: true });
    }
    const pending = pendingExecutions.get(request.requestId);
    if (pending) {
      if (pending.fingerprint !== fingerprint) {
        throw new Error('Skill execution request id is already bound to a different request');
      }
      return pending.promise;
    }
    const execution = (async () => {
      const result = normalizeSkillExecutionResult(await invoke('execute_skill', { input: request }), request);
      remember(completedExecutions, request.requestId, { fingerprint, result });
      return nativeValue('execute_skill', clone(result), { deduplicated: false });
    })().finally(() => {
      pendingExecutions.delete(request.requestId);
    });
    pendingExecutions.set(request.requestId, { fingerprint, promise: execution });
    return execution;
  }

  async function listEffects(query = {}) {
    const normalizedQuery = normalizeEffectQuery(query);
    if (!isAvailable()) {
      const filtered = localFallbackEffects.filter((effect) => (
        (!normalizedQuery.skillId || effect.skillId === normalizedQuery.skillId)
        && (!normalizedQuery.requestId || effect.requestId === normalizedQuery.requestId)
        && (!normalizedQuery.taskId || effect.taskId === normalizedQuery.taskId)
        && (!normalizedQuery.traceId || effect.traceId === normalizedQuery.traceId)
        && (!normalizedQuery.outcomes.length || normalizedQuery.outcomes.includes(effect.outcome))
      )).slice(0, normalizedQuery.limit);
      return unavailable('list_skill_execution_effects', filtered);
    }
    const effects = normalizeSkillExecutionEffects(await invoke('list_skill_execution_effects', { query: normalizedQuery }));
    localFallbackEffects = effects;
    return nativeValue('list_skill_execution_effects', effects);
  }

  async function recordFeedback(input) {
    const source = assertObject(input, 'Skill execution feedback input');
    const effectId = assertIdentifier(source.effectId, 'Skill execution effect id');
    const relationKind = String(source.relationKind || '').trim();
    if (!EFFECT_RELATIONS.has(relationKind)) throw new TypeError('Skill execution feedback relation is invalid');
    const referenceId = assertIdentifier(source.referenceId, 'Skill execution feedback reference id');
    const note = String(source.note || '').trim().slice(0, 2_000);
    const feedbackKey = `${effectId}\u0000${relationKind}\u0000${referenceId}`;
    if (!isAvailable()) return unavailable('record_skill_execution_effect_feedback');
    if (completedFeedback.has(feedbackKey)) {
      return nativeValue('record_skill_execution_effect_feedback', clone(completedFeedback.get(feedbackKey)), { deduplicated: true });
    }
    if (pendingFeedback.has(feedbackKey)) return pendingFeedback.get(feedbackKey);
    const persistence = (async () => {
      const effect = normalizeSkillExecutionEffect(await invoke('record_skill_execution_effect_feedback', {
        input: { effectId, relationKind, referenceId, note },
      }));
      remember(completedFeedback, feedbackKey, effect);
      return nativeValue('record_skill_execution_effect_feedback', clone(effect), { deduplicated: false });
    })().finally(() => {
      pendingFeedback.delete(feedbackKey);
    });
    pendingFeedback.set(feedbackKey, persistence);
    return persistence;
  }

  return Object.freeze({
    mode: () => isAvailable() ? 'native' : 'browser',
    isNativeAvailable: isAvailable,
    execute,
    listEffects,
    recordFeedback,
    fallbackEffects: () => clone(localFallbackEffects),
  });
}
