const ACTIVE_STATES = new Set(['running', 'pausePending', 'paused']);

function clampProgress(value, fallback = 0) {
  const candidate = Number(value);
  return Number.isFinite(candidate) ? Math.max(0, Math.min(100, Math.trunc(candidate))) : fallback;
}

function snapshotOf(state) {
  return Object.freeze({ ...state });
}

export function beautifyTaskActions(value) {
  const state = value?.state || value;
  if (state === 'running') return ['pause', 'details'];
  if (state === 'pausePending' || state === 'paused') return ['resume', 'details'];
  if (state === 'succeeded') return ['result'];
  if (state === 'failed') return ['error'];
  return ['details'];
}

export function createBeautifyTaskController(initial = {}) {
  let state = {
    state: 'running',
    progress: clampProgress(initial.progress, 0),
    detail: String(initial.detail || '正在准备排版任务'),
    result: '',
    error: '',
  };
  const listeners = new Set();
  const resumeWaiters = new Set();

  const emit = () => {
    const snapshot = snapshotOf(state);
    listeners.forEach((listener) => listener(snapshot));
    return snapshot;
  };
  const update = (patch = {}) => {
    if (!ACTIVE_STATES.has(state.state)) return snapshotOf(state);
    state = {
      ...state,
      ...patch,
      progress: clampProgress(patch.progress, state.progress),
      detail: patch.detail == null ? state.detail : String(patch.detail),
    };
    return emit();
  };
  const releaseWaiters = () => {
    const waiters = [...resumeWaiters];
    resumeWaiters.clear();
    waiters.forEach((resolve) => resolve(snapshotOf(state)));
  };

  return {
    snapshot() {
      return snapshotOf(state);
    },
    subscribe(listener) {
      if (typeof listener !== 'function') throw new TypeError('美化任务监听器必须是函数');
      listeners.add(listener);
      listener(snapshotOf(state));
      return () => listeners.delete(listener);
    },
    update,
    pause() {
      if (state.state !== 'running') return snapshotOf(state);
      return update({
        state: 'pausePending',
        detail: `${state.detail}；将在当前阶段完成后的检查点暂停`,
      });
    },
    resume() {
      if (state.state !== 'pausePending' && state.state !== 'paused') return snapshotOf(state);
      state = { ...state, state: 'running', detail: state.detail.replace(/；将在当前阶段完成后的检查点暂停$/u, '') };
      const snapshot = emit();
      releaseWaiters();
      return snapshot;
    },
    async checkpoint(detail) {
      if (state.state === 'pausePending') {
        state = {
          ...state,
          state: 'paused',
          detail: String(detail || '任务已在阶段检查点暂停'),
        };
        emit();
      }
      if (state.state !== 'paused') return snapshotOf(state);
      return new Promise((resolve) => resumeWaiters.add(resolve));
    },
    succeed(result = {}) {
      if (!ACTIVE_STATES.has(state.state)) return snapshotOf(state);
      state = {
        ...state,
        state: 'succeeded',
        progress: 100,
        detail: String(result.detail || '排版任务已完成'),
        result: String(result.result || result.detail || '排版结果已应用到未保存草稿'),
        error: '',
      };
      const snapshot = emit();
      releaseWaiters();
      return snapshot;
    },
    fail(error) {
      if (!ACTIVE_STATES.has(state.state)) return snapshotOf(state);
      const message = String(error || '排版任务失败');
      state = { ...state, state: 'failed', detail: message, result: '', error: message };
      const snapshot = emit();
      releaseWaiters();
      return snapshot;
    },
  };
}
