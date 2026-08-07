const IDENTIFIER_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,179}$/u;
const HASH_PATTERN = /^sha256:[a-f0-9]{64}$/iu;
const TASK_STATES = new Set([
  'created',
  'queued',
  'running',
  'paused',
  'awaiting_approval',
  'succeeded',
  'failed',
  'cancelled',
]);
const STEP_STATES = new Set(['pending', 'claimed', 'running', 'succeeded', 'failed', 'cancelled']);
const STEP_RECEIPT_STATES = new Set(['succeeded', 'failed', 'cancelled', 'expired']);
const STEP_KINDS = new Set(['model', 'capability', 'approval', 'verification', 'checkpoint', 'schedule_dispatch']);
const EFFECT_CLASSES = new Set(['read_only', 'effectful']);
const EVIDENCE_SOURCE_KINDS = new Set([
  'runtime',
  'operation_event',
  'inbound_content',
  'vault_commit',
  'model_receipt',
  'user_approval',
  'scheduler',
  'verification',
]);

function clone(value) {
  if (value === undefined) return undefined;
  return structuredClone(value);
}

function assertInvoke(invoke) {
  if (typeof invoke !== 'function') throw new TypeError('Task runtime requires an invoke function');
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
  if (!Number.isSafeInteger(number) || number < minimum || number > maximum) {
    throw new TypeError(`${label} is invalid`);
  }
  return number;
}

function boundedNumber(value, label, { minimum = 0, maximum = Number.MAX_VALUE, fallback = minimum } = {}) {
  if (value === undefined || value === null || value === '') return fallback;
  const number = Number(value);
  if (!Number.isFinite(number) || number < minimum || number > maximum) {
    throw new TypeError(`${label} is invalid`);
  }
  return number;
}

function optionalBoundedInteger(value, label, options = {}) {
  if (value === undefined || value === null || value === '') return null;
  return boundedInteger(value, label, options);
}

function optionalBoundedNumber(value, label, options = {}) {
  if (value === undefined || value === null || value === '') return null;
  return boundedNumber(value, label, options);
}

function assertObject(value, label) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new TypeError(`${label} must be an object`);
  return value;
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

function resolveAvailability(available) {
  return typeof available === 'function' ? Boolean(available()) : available !== false;
}

function validateTaskState(value) {
  const state = String(value || '').trim();
  if (!TASK_STATES.has(state)) throw new TypeError(`Runtime task state is invalid: ${state}`);
  return state;
}

export function normalizeRuntimeTask(value) {
  const source = assertObject(value, 'Runtime task');
  const id = assertIdentifier(source.id, 'Runtime task id');
  const state = validateTaskState(source.state);
  const progress = boundedInteger(source.progress, 'Runtime task progress', { maximum: 100, fallback: 0 });
  const title = String(source.title || '').trim();
  if (!title || title.length > 240) throw new TypeError('Runtime task title is invalid');
  return {
    ...clone(source),
    id,
    state,
    title,
    traceId: optionalIdentifier(source.traceId, 'Runtime task trace id'),
    progress,
    payload: source.payload && typeof source.payload === 'object' && !Array.isArray(source.payload)
      ? clone(source.payload)
      : {},
    createdAt: String(source.createdAt || ''),
    updatedAt: String(source.updatedAt || ''),
  };
}

export function normalizeRuntimeTaskList(value) {
  if (!Array.isArray(value)) throw new TypeError('Runtime task list must be an array');
  return value.map(normalizeRuntimeTask);
}

export function normalizeRuntimeTaskPlanInput(value) {
  const source = assertObject(value, 'Runtime task plan');
  if (source.schemaVersion !== '1.0') throw new TypeError('Runtime task plan schema version is invalid');
  const goal = String(source.goal || '').trim();
  if (!goal || goal.length > 4_000) throw new TypeError('Runtime task plan goal is invalid');
  if (!Array.isArray(source.steps) || source.steps.length === 0 || source.steps.length > 128) {
    throw new TypeError('Runtime task plan steps are invalid');
  }
  const ids = new Set();
  const steps = source.steps.map((step) => {
    const item = assertObject(step, 'Runtime task plan step');
    const id = assertIdentifier(item.id, 'Runtime task step id');
    if (ids.has(id)) throw new TypeError(`Runtime task step id is duplicated: ${id}`);
    ids.add(id);
    const kind = String(item.kind || '').trim();
    if (!STEP_KINDS.has(kind)) throw new TypeError(`Runtime task step kind is invalid: ${kind}`);
    const title = String(item.title || '').trim();
    if (!title || title.length > 240) throw new TypeError('Runtime task step title is invalid');
    const dependsOn = item.dependsOn === undefined ? [] : item.dependsOn;
    if (!Array.isArray(dependsOn)) throw new TypeError('Runtime task step dependencies are invalid');
    return {
      id,
      kind,
      title,
      dependsOn: dependsOn.map((dependency) => assertIdentifier(dependency, 'Runtime task dependency')),
      parameters: clone(item.parameters === undefined ? {} : assertObject(item.parameters, 'Runtime task step parameters')),
    };
  });
  for (const step of steps) {
    if (new Set(step.dependsOn).size !== step.dependsOn.length
      || step.dependsOn.some((dependency) => dependency === step.id || !ids.has(dependency))) {
      throw new TypeError(`Runtime task step dependency is invalid: ${step.id}`);
    }
  }
  const visiting = new Set();
  const visited = new Set();
  const stepById = new Map(steps.map((step) => [step.id, step]));
  const visit = (stepId) => {
    if (visiting.has(stepId)) throw new TypeError('Runtime task plan contains a dependency cycle');
    if (visited.has(stepId)) return;
    visiting.add(stepId);
    for (const dependency of stepById.get(stepId).dependsOn) visit(dependency);
    visiting.delete(stepId);
    visited.add(stepId);
  };
  for (const step of steps) visit(step.id);
  const completion = assertObject(source.completionContract, 'Runtime task completion contract');
  if (completion.mode !== 'all_of') throw new TypeError('Runtime task completion mode is invalid');
  if (!Array.isArray(completion.requirements) || completion.requirements.length === 0 || completion.requirements.length > 128) {
    throw new TypeError('Runtime task completion requirements are invalid');
  }
  const requirementIds = new Set();
  const requirements = completion.requirements.map((requirement) => {
    const item = assertObject(requirement, 'Runtime task completion requirement');
    const id = assertIdentifier(item.id, 'Runtime task completion requirement id');
    if (requirementIds.has(id)) throw new TypeError(`Runtime task completion requirement is duplicated: ${id}`);
    requirementIds.add(id);
    const stepId = optionalIdentifier(item.stepId, 'Runtime task completion step id');
    if (stepId && !ids.has(stepId)) throw new TypeError('Runtime task completion step is invalid');
    const evidenceType = assertIdentifier(item.evidenceType, 'Runtime task evidence type');
    const description = String(item.description || '').trim();
    if (!description || description.length > 500) throw new TypeError('Runtime task completion description is invalid');
    return {
      id,
      stepId,
      evidenceType,
      minimumCount: boundedInteger(item.minimumCount, 'Runtime task completion minimum count', { minimum: 1, maximum: 2_048, fallback: 1 }),
      description,
    };
  });
  return {
    schemaVersion: '1.0',
    goal,
    steps,
    completionContract: { mode: 'all_of', requirements },
    metadata: clone(source.metadata === undefined ? {} : assertObject(source.metadata, 'Runtime task plan metadata')),
  };
}

export function normalizeRuntimeTaskContract(value) {
  if (value === null || value === undefined) return null;
  const source = assertObject(value, 'Runtime task contract');
  const taskId = assertIdentifier(source.taskId, 'Runtime task contract task id');
  const plan = assertObject(source.plan, 'Runtime task contract plan');
  const revision = boundedInteger(plan.revision, 'Runtime task plan revision', { minimum: 1 });
  const planBody = assertObject(plan.plan, 'Runtime task plan body');
  if (!Array.isArray(planBody.steps) || planBody.steps.length === 0) throw new TypeError('Runtime task plan steps are invalid');
  const steps = planBody.steps.map((step) => {
    const item = assertObject(step, 'Runtime task plan step');
    const id = assertIdentifier(item.id, 'Runtime task step id');
    const state = item.state === undefined ? undefined : String(item.state);
    if (state !== undefined && !STEP_STATES.has(state)) throw new TypeError(`Runtime task step state is invalid: ${state}`);
    return {
      ...clone(item),
      id,
      ...(state ? { state } : {}),
      dependsOn: Array.isArray(item.dependsOn) ? item.dependsOn.map((dependency) => assertIdentifier(dependency, 'Runtime task dependency')) : [],
      parameters: item.parameters && typeof item.parameters === 'object' && !Array.isArray(item.parameters) ? clone(item.parameters) : {},
    };
  });
  if (!HASH_PATTERN.test(String(plan.contentHash || ''))) throw new TypeError('Runtime task plan content hash is invalid');
  const completion = assertObject(source.completion, 'Runtime task completion');
  if (typeof completion.satisfied !== 'boolean') throw new TypeError('Runtime task completion status is invalid');
  return {
    ...clone(source),
    taskId,
    plan: {
      ...clone(plan),
      revision,
      plan: { ...clone(planBody), steps },
      contentHash: String(plan.contentHash),
    },
    completion: {
      ...clone(completion),
      planRevision: boundedInteger(completion.planRevision, 'Runtime task completion plan revision', { minimum: 1, fallback: revision }),
      satisfied: completion.satisfied,
      requirements: Array.isArray(completion.requirements) ? clone(completion.requirements) : [],
    },
    evidence: Array.isArray(source.evidence) ? clone(source.evidence) : [],
  };
}

export function normalizeRuntimeTaskStepClaim(value) {
  const source = assertObject(value, 'Runtime task step claim');
  const claimId = assertIdentifier(source.claimId, 'Runtime task step claim id');
  const taskId = assertIdentifier(source.runtimeTaskId, 'Runtime task step claim task id');
  const stepId = assertIdentifier(source.stepId, 'Runtime task step claim step id');
  const planRevision = boundedInteger(source.planRevision, 'Runtime task step claim plan revision', { minimum: 1 });
  const stepKind = String(source.stepKind || '').trim();
  if (!STEP_KINDS.has(stepKind)) throw new TypeError('Runtime task step claim kind is invalid');
  const effectClass = String(source.effectClass || '').trim();
  if (!EFFECT_CLASSES.has(effectClass)) throw new TypeError('Runtime task step claim effect class is invalid');
  const title = String(source.title || '').trim();
  if (!title || title.length > 240) throw new TypeError('Runtime task step claim title is invalid');
  return {
    ...clone(source),
    claimId,
    runtimeTaskId: taskId,
    stepId,
    stepKind,
    title,
    planRevision,
    attempt: boundedInteger(source.attempt, 'Runtime task step attempt', { minimum: 1, fallback: 1 }),
    leaseOwner: assertIdentifier(source.leaseOwner, 'Runtime task step lease owner'),
    leaseExpiresAt: String(source.leaseExpiresAt || ''),
    dependsOn: Array.isArray(source.dependsOn) ? source.dependsOn.map((dependency) => assertIdentifier(dependency, 'Runtime task dependency')) : [],
    parameters: clone(source.parameters === undefined ? {} : assertObject(source.parameters, 'Runtime task step claim parameters')),
    effectClass,
    reservedToolCalls: boundedInteger(source.reservedToolCalls, 'Runtime task reserved tool calls', { maximum: 2_048, fallback: 0 }),
    reservedRuntimeSeconds: boundedInteger(source.reservedRuntimeSeconds, 'Runtime task reserved runtime seconds', { maximum: 86_400, fallback: 0 }),
    reservedTokens: optionalBoundedInteger(source.reservedTokens, 'Runtime task reserved tokens', { minimum: 0 }),
    reservedCost: optionalBoundedNumber(source.reservedCost, 'Runtime task reserved cost', { minimum: 0 }),
    cancellationFence: boundedInteger(source.cancellationFence, 'Runtime task cancellation fence', { fallback: 0 }),
    claimedAt: String(source.claimedAt || ''),
  };
}

export function normalizeRuntimeTaskStepReceipt(value) {
  const source = assertObject(value, 'Runtime task step receipt');
  return {
    ...clone(source),
    receiptId: assertIdentifier(source.receiptId, 'Runtime task step receipt id'),
    stepClaimId: assertIdentifier(source.stepClaimId, 'Runtime task step claim id'),
    runtimeTaskId: assertIdentifier(source.runtimeTaskId, 'Runtime task step receipt task id'),
    planRevision: boundedInteger(source.planRevision, 'Runtime task step receipt plan revision', { minimum: 1 }),
    stepId: assertIdentifier(source.stepId, 'Runtime task step receipt step id'),
    state: STEP_RECEIPT_STATES.has(source.state) ? source.state : (() => { throw new TypeError('Runtime task step receipt state is invalid'); })(),
    output: source.output && typeof source.output === 'object' && !Array.isArray(source.output) ? clone(source.output) : {},
    error: source.error === null || source.error === undefined ? null : String(source.error),
    consumedToolCalls: boundedInteger(source.consumedToolCalls, 'Runtime task receipt tool calls', { maximum: 2_048, fallback: 0 }),
    consumedRuntimeSeconds: boundedInteger(source.consumedRuntimeSeconds, 'Runtime task receipt runtime seconds', { maximum: 86_400, fallback: 0 }),
    consumedTokens: boundedInteger(source.consumedTokens, 'Runtime task receipt tokens', { fallback: 0 }),
    consumedCost: boundedNumber(source.consumedCost, 'Runtime task receipt cost', { fallback: 0 }),
    contentHash: HASH_PATTERN.test(String(source.contentHash || ''))
      ? String(source.contentHash)
      : (() => { throw new TypeError('Runtime task step receipt content hash is invalid'); })(),
    createdAt: String(source.createdAt || ''),
  };
}

export function normalizeRuntimeTaskEvidence(value) {
  const source = assertObject(value, 'Runtime task evidence');
  const evidenceType = assertIdentifier(source.evidenceType, 'Runtime task evidence type');
  const sourceKind = String(source.sourceKind || '').trim();
  if (!EVIDENCE_SOURCE_KINDS.has(sourceKind)) throw new TypeError('Runtime task evidence source kind is invalid');
  const contentHash = String(source.contentHash || '').trim();
  if (!HASH_PATTERN.test(contentHash)) throw new TypeError('Runtime task evidence content hash is invalid');
  return {
    ...clone(source),
    taskId: assertIdentifier(source.taskId, 'Runtime task evidence task id'),
    evidenceId: assertIdentifier(source.evidenceId, 'Runtime task evidence id'),
    planRevision: boundedInteger(source.planRevision, 'Runtime task evidence plan revision', { minimum: 1 }),
    requirementId: assertIdentifier(source.requirementId, 'Runtime task evidence requirement id'),
    stepId: optionalIdentifier(source.stepId, 'Runtime task evidence step id'),
    evidenceType,
    sourceKind,
    sourceRef: String(source.sourceRef || ''),
    payload: clone(assertObject(source.payload, 'Runtime task evidence payload')),
    contentHash,
    createdAt: String(source.createdAt || ''),
  };
}

function normalizeRuntimeTaskStepFrontier(value) {
  if (!Array.isArray(value)) throw new TypeError('Runtime task step frontier must be an array');
  return value.map((entry) => {
    const source = assertObject(entry, 'Runtime task step frontier item');
    const stepKind = String(source.stepKind || '').trim();
    const effectClass = String(source.effectClass || '').trim();
    if (!STEP_KINDS.has(stepKind) || !EFFECT_CLASSES.has(effectClass)) throw new TypeError('Runtime task step frontier item is invalid');
    return {
      ...clone(source),
      runtimeTaskId: assertIdentifier(source.runtimeTaskId, 'Runtime task frontier task id'),
      planRevision: boundedInteger(source.planRevision, 'Runtime task frontier plan revision', { minimum: 1 }),
      stepId: assertIdentifier(source.stepId, 'Runtime task frontier step id'),
      stepKind,
      title: String(source.title || ''),
      dependsOn: Array.isArray(source.dependsOn) ? source.dependsOn.map((dependency) => assertIdentifier(dependency, 'Runtime task dependency')) : [],
      parameters: clone(source.parameters === undefined ? {} : assertObject(source.parameters, 'Runtime task frontier parameters')),
      effectClass,
      ready: Boolean(source.ready),
      active: Boolean(source.active),
    };
  });
}

function normalizeClaimBatch(value) {
  const source = assertObject(value, 'Runtime task step claim batch');
  if (!Array.isArray(source.claims)) throw new TypeError('Runtime task step claims must be an array');
  if (!source.budget || typeof source.budget !== 'object' || Array.isArray(source.budget)) throw new TypeError('Runtime task budget is invalid');
  const claims = source.claims.map(normalizeRuntimeTaskStepClaim);
  const claimIds = new Set();
  const stepIds = new Set();
  for (const claim of claims) {
    if (claimIds.has(claim.claimId) || stepIds.has(claim.stepId)) {
      throw new Error('Runtime task step claim batch contains duplicate claims');
    }
    claimIds.add(claim.claimId);
    stepIds.add(claim.stepId);
  }
  return {
    ...clone(source),
    claims,
    budget: clone(source.budget),
  };
}

function normalizeReceiptList(value) {
  if (!Array.isArray(value)) throw new TypeError('Runtime task step receipt list must be an array');
  const receipts = value.map(normalizeRuntimeTaskStepReceipt);
  const receiptIds = new Set();
  for (const receipt of receipts) {
    if (receiptIds.has(receipt.receiptId)) {
      throw new Error('Runtime task step receipt list contains duplicate receipts');
    }
    receiptIds.add(receipt.receiptId);
  }
  return receipts;
}

function normalizeRecoveryList(value) {
  if (!Array.isArray(value)) throw new TypeError('Runtime recovery list must be an array');
  return clone(value);
}

function validateTransitionInput(taskId, action, detail, progress, checkpoint) {
  const normalizedTaskId = assertIdentifier(taskId, 'Runtime task id');
  const allowedActions = new Set(['queue', 'start', 'pause', 'resume', 'cancel', 'retry', 'checkpoint', 'succeed', 'fail']);
  const normalizedAction = String(action || '').trim();
  if (!allowedActions.has(normalizedAction)) throw new TypeError(`Runtime task action is invalid: ${normalizedAction}`);
  const normalizedProgress = progress === null || progress === undefined
    ? null
    : boundedInteger(progress, 'Runtime task progress', { maximum: 100 });
  if (checkpoint !== null && checkpoint !== undefined) assertObject(checkpoint, 'Runtime task checkpoint');
  return {
    taskId: normalizedTaskId,
    action: normalizedAction,
    detail: String(detail || '').slice(0, 4_000),
    progress: normalizedProgress,
    checkpoint: checkpoint === null || checkpoint === undefined ? null : clone(checkpoint),
  };
}

export function taskControlAvailability(task, nativeAvailable = true) {
  const state = String(task?.state || '').trim();
  const canControl = nativeAvailable && Boolean(task?.nativeRuntime !== false);
  return {
    canPause: canControl && ['queued', 'running', 'awaiting_approval'].includes(state),
    canResume: canControl && ['paused', 'failed'].includes(state),
    canCancel: canControl && ['created', 'queued', 'running', 'paused', 'awaiting_approval', 'failed'].includes(state),
    canRetry: canControl && ['failed', 'cancelled'].includes(state),
  };
}

export function mergeRuntimeTask(localTask, nativeTask) {
  if (!nativeTask) return clone(localTask);
  const native = normalizeRuntimeTask(nativeTask);
  return {
    ...(localTask && typeof localTask === 'object' ? clone(localTask) : {}),
    ...native,
    id: localTask?.id || native.id,
    runtimeTaskId: native.id,
    nativeRuntime: true,
    nativeState: native.state,
    state: native.state,
    progress: native.progress,
    traceId: native.traceId || localTask?.traceId || null,
    nativePayload: clone(native.payload),
  };
}

export function createTaskRuntime(invoke, { available = true, fallbackTasks = [] } = {}) {
  assertInvoke(invoke);
  let localFallbackTasks = normalizeRuntimeTaskList(fallbackTasks);
  const isAvailable = () => resolveAvailability(available);
  const browserResult = (operation, value = null) => unavailable(operation, value);

  async function list({ state = null, limit = 200 } = {}) {
    const normalizedState = state === null || state === undefined || state === '' ? null : validateTaskState(state);
    const normalizedLimit = boundedInteger(limit, 'Runtime task list limit', { minimum: 1, maximum: 500, fallback: 200 });
    if (!isAvailable()) return browserResult('list_runtime_tasks', localFallbackTasks);
    const tasks = normalizeRuntimeTaskList(await invoke('list_runtime_tasks', { state: normalizedState, limit: normalizedLimit }));
    localFallbackTasks = tasks;
    return nativeValue('list_runtime_tasks', tasks);
  }

  async function get(taskId, fallback = null) {
    const id = assertIdentifier(taskId, 'Runtime task id');
    if (!isAvailable()) return browserResult('get_runtime_task', fallback || localFallbackTasks.find((task) => task.id === id) || null);
    return nativeValue('get_runtime_task', normalizeRuntimeTask(await invoke('get_runtime_task', { taskId: id })));
  }

  async function getContract(taskId, fallback = null) {
    const id = assertIdentifier(taskId, 'Runtime task id');
    if (!isAvailable()) return browserResult('get_runtime_task_contract', fallback);
    const response = await invoke('get_runtime_task_contract', { taskId: id });
    return nativeValue('get_runtime_task_contract', normalizeRuntimeTaskContract(response));
  }

  async function definePlan(input) {
    const request = assertObject(input, 'Runtime task plan binding');
    const taskId = assertIdentifier(request.taskId, 'Runtime task plan task id');
    const plan = normalizeRuntimeTaskPlanInput(request.plan);
    if (!isAvailable()) return browserResult('define_runtime_task_plan');
    return nativeValue('define_runtime_task_plan', normalizeRuntimeTaskContract(await invoke('define_runtime_task_plan', {
      input: { taskId, plan },
    })));
  }

  async function getFrontier(taskId, options = {}) {
    const id = assertIdentifier(taskId, 'Runtime task id');
    const planRevision = optionalBoundedInteger(options.planRevision, 'Runtime task plan revision', { minimum: 1 });
    if (!isAvailable()) return browserResult('get_runtime_task_step_frontier', []);
    const value = await invoke('get_runtime_task_step_frontier', { taskId: id, planRevision });
    return nativeValue('get_runtime_task_step_frontier', normalizeRuntimeTaskStepFrontier(value));
  }

  async function claimSteps(input) {
    const request = assertObject(input, 'Runtime task step claim input');
    const taskId = assertIdentifier(request.taskId, 'Runtime task step claim task id');
    const workerId = assertIdentifier(request.workerId, 'Runtime task step worker id');
    const planRevision = request.planRevision === undefined || request.planRevision === null
      ? null
      : boundedInteger(request.planRevision, 'Runtime task step claim plan revision', { minimum: 1 });
    const maxClaims = boundedInteger(request.maxClaims, 'Runtime task step max claims', { minimum: 1, maximum: 32, fallback: 1 });
    const leaseSeconds = boundedInteger(request.leaseSeconds, 'Runtime task step lease seconds', { minimum: 1, maximum: 3_600, fallback: 300 });
    const reservationSource = request.reservation === undefined ? {} : assertObject(request.reservation, 'Runtime task budget reservation');
    const reservation = {
      maxToolCalls: boundedInteger(reservationSource.maxToolCalls, 'Runtime task reserved tool calls', { maximum: 2_048, fallback: 0 }),
      maxRuntimeSeconds: boundedInteger(reservationSource.maxRuntimeSeconds, 'Runtime task reserved runtime seconds', { maximum: 86_400, fallback: 0 }),
      maxTokens: optionalBoundedInteger(reservationSource.maxTokens, 'Runtime task reserved tokens', { minimum: 0 }),
      maxCost: optionalBoundedNumber(reservationSource.maxCost, 'Runtime task reserved cost', { minimum: 0 }),
    };
    if (!isAvailable()) return browserResult('claim_runtime_task_plan_steps');
    const result = await invoke('claim_runtime_task_plan_steps', {
      input: { taskId, planRevision, workerId, maxClaims, leaseSeconds, reservation: clone(reservation) },
    });
    return nativeValue('claim_runtime_task_plan_steps', normalizeClaimBatch(result));
  }

  async function completeStep(input) {
    const request = assertObject(input, 'Runtime task step completion input');
    const taskId = assertIdentifier(request.taskId, 'Runtime task step task id');
    const stepClaimId = assertIdentifier(request.stepClaimId, 'Runtime task step claim id');
    const receiptId = assertIdentifier(request.receiptId, 'Runtime task step receipt id');
    if (!isAvailable()) return browserResult('complete_runtime_task_plan_step');
    const result = await invoke('complete_runtime_task_plan_step', {
      input: {
        taskId,
        stepClaimId,
        receiptId,
        consumedToolCalls: boundedInteger(request.consumedToolCalls, 'Runtime task tool calls', { maximum: 2_048, fallback: 0 }),
        consumedRuntimeSeconds: boundedInteger(request.consumedRuntimeSeconds, 'Runtime task runtime seconds', { maximum: 86_400, fallback: 0 }),
        consumedTokens: boundedInteger(request.consumedTokens, 'Runtime task tokens', { fallback: 0 }),
        consumedCost: boundedNumber(request.consumedCost, 'Runtime task cost', { fallback: 0 }),
        output: clone(assertObject(request.output || {}, 'Runtime task step output')),
      },
    });
    return nativeValue('complete_runtime_task_plan_step', normalizeRuntimeTaskStepReceipt(result));
  }

  async function failStep(input) {
    const request = assertObject(input, 'Runtime task step failure input');
    const taskId = assertIdentifier(request.taskId, 'Runtime task step task id');
    const stepClaimId = assertIdentifier(request.stepClaimId, 'Runtime task step claim id');
    const receiptId = assertIdentifier(request.receiptId, 'Runtime task step receipt id');
    const error = String(request.error || '').trim();
    if (!error || error.length > 4_000) throw new TypeError('Runtime task step failure error is invalid');
    if (!isAvailable()) return browserResult('fail_runtime_task_plan_step');
    const result = await invoke('fail_runtime_task_plan_step', {
      input: {
        taskId,
        stepClaimId,
        receiptId,
        error,
        consumedToolCalls: boundedInteger(request.consumedToolCalls, 'Runtime task tool calls', { maximum: 2_048, fallback: 0 }),
        consumedRuntimeSeconds: boundedInteger(request.consumedRuntimeSeconds, 'Runtime task runtime seconds', { maximum: 86_400, fallback: 0 }),
        consumedTokens: boundedInteger(request.consumedTokens, 'Runtime task tokens', { fallback: 0 }),
        consumedCost: boundedNumber(request.consumedCost, 'Runtime task cost', { fallback: 0 }),
        output: clone(assertObject(request.output || {}, 'Runtime task step output')),
      },
    });
    return nativeValue('fail_runtime_task_plan_step', normalizeRuntimeTaskStepReceipt(result));
  }

  async function transition({ taskId, action, detail = '', progress = null, checkpoint = null }) {
    const input = validateTransitionInput(taskId, action, detail, progress, checkpoint);
    if (!isAvailable()) return browserResult('transition_runtime_task');
    return nativeValue('transition_runtime_task', normalizeRuntimeTask(await invoke('transition_runtime_task', { input })));
  }

  async function recover() {
    if (!isAvailable()) return browserResult('recover_interrupted_runtime_tasks', []);
    return nativeValue('recover_interrupted_runtime_tasks', normalizeRecoveryList(await invoke('recover_interrupted_runtime_tasks')));
  }

  async function listStepReceipts(taskId, options = {}) {
    const id = assertIdentifier(taskId, 'Runtime task id');
    const planRevision = optionalBoundedInteger(options.planRevision, 'Runtime task plan revision', { minimum: 1 });
    const limit = boundedInteger(options.limit, 'Runtime task receipt limit', { minimum: 1, maximum: 500, fallback: 200 });
    if (!isAvailable()) return browserResult('list_runtime_task_step_receipts', []);
    const result = await invoke('list_runtime_task_step_receipts', { taskId: id, planRevision, limit });
    return nativeValue('list_runtime_task_step_receipts', normalizeReceiptList(result));
  }

  async function appendEvidence(input) {
    const request = assertObject(input, 'Runtime task evidence input');
    const evidenceType = assertIdentifier(request.evidenceType, 'Runtime task evidence type');
    const sourceKind = String(request.sourceKind || '').trim();
    if (!EVIDENCE_SOURCE_KINDS.has(sourceKind)) throw new TypeError('Runtime task evidence source kind is invalid');
    if (evidenceType.startsWith('runtime.') || evidenceType === 'schedule.dispatch_ack') {
      throw new TypeError('Runtime task evidence type is reserved for the native runtime');
    }
    if (sourceKind === 'runtime' || sourceKind === 'scheduler') {
      throw new TypeError('Runtime task evidence source is reserved for the native runtime');
    }
    const sourceRef = String(request.sourceRef || '').trim();
    if (!sourceRef || sourceRef.length > 2_048) throw new TypeError('Runtime task evidence source reference is invalid');
    const normalized = {
      taskId: assertIdentifier(request.taskId, 'Runtime task evidence task id'),
      evidenceId: assertIdentifier(request.evidenceId, 'Runtime task evidence id'),
      planRevision: optionalBoundedInteger(request.planRevision, 'Runtime task evidence plan revision', { minimum: 1 }),
      requirementId: assertIdentifier(request.requirementId, 'Runtime task evidence requirement id'),
      evidenceType,
      sourceKind,
      sourceRef,
      payload: clone(assertObject(request.payload, 'Runtime task evidence payload')),
    };
    if (evidenceType === 'verification.result' && normalized.payload.valid !== true) {
      throw new TypeError('Runtime task verification evidence must declare valid=true');
    }
    if (!isAvailable()) return browserResult('append_runtime_task_evidence');
    return nativeValue('append_runtime_task_evidence', normalizeRuntimeTaskEvidence(await invoke('append_runtime_task_evidence', { input: normalized })));
  }

  return Object.freeze({
    mode: () => isAvailable() ? 'native' : 'browser',
    isNativeAvailable: isAvailable,
    list,
    get,
    getContract,
    definePlan,
    getFrontier,
    claimSteps,
    completeStep,
    failStep,
    transition,
    cancel: (taskId, detail = 'User cancelled runtime task') => transition({ taskId, action: 'cancel', detail }),
    recover,
    listStepReceipts,
    appendEvidence,
    fallbackTasks: () => clone(localFallbackTasks),
  });
}
