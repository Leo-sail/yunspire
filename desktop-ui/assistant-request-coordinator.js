export class AssistantRequestCoordinator {
  constructor(runRequest, { onChange = () => {}, onError = () => {}, onCancel = () => {} } = {}) {
    if (typeof runRequest !== 'function') throw new TypeError('runRequest must be a function');
    this.runRequest = runRequest;
    this.onChange = onChange;
    this.onError = onError;
    this.onCancel = onCancel;
    this.lanes = new Map();
    this.requests = new Map();
  }

  enqueue(request) {
    if (!request?.id || !request?.conversationId) {
      return Promise.reject(new Error('AI assistant request requires id and conversationId'));
    }
    if (this.requests.has(request.id)) {
      return Promise.reject(new Error(`Duplicate AI assistant request: ${request.id}`));
    }
    const lane = this.#lane(request.conversationId);
    request.state = 'queued';
    request.cancelled = false;
    request.cancelReason = '';
    const completion = new Promise((resolve, reject) => {
      request.resolve = resolve;
      request.reject = reject;
    });
    lane.queue.push(request);
    this.requests.set(request.id, request);
    this.#notify(request.conversationId);
    void this.#pump(request.conversationId, lane);
    return completion;
  }

  active(conversationId) {
    return this.lanes.get(conversationId)?.active || null;
  }

  queued(conversationId) {
    return [...(this.lanes.get(conversationId)?.queue || [])];
  }

  get(requestId) {
    return this.requests.get(requestId) || null;
  }

  cancel(requestId, reason = 'cancelled') {
    const request = this.requests.get(requestId);
    if (!request || request.cancelled || request.state === 'completed' || request.state === 'cancelled') return null;
    request.cancelled = true;
    request.cancelReason = reason;
    const lane = this.lanes.get(request.conversationId);
    if (request.state === 'queued' && lane) {
      lane.queue = lane.queue.filter((item) => item.id !== requestId);
      request.state = 'cancelled';
      this.requests.delete(requestId);
      try {
        this.onCancel(request, reason);
      } catch {
        // Native cancellation is best effort; local queue ownership is already settled.
      }
      request.resolve({ status: 'cancelled', request });
      this.#notify(request.conversationId);
      this.#deleteIdleLane(request.conversationId, lane);
    } else {
      try {
        this.onCancel(request, reason);
      } catch {
        // Cancellation transport errors are reported by the owning UI layer.
      }
      this.#notify(request.conversationId);
    }
    return request;
  }

  cancelConversation(conversationId, reason = 'conversation_cancelled') {
    const lane = this.lanes.get(conversationId);
    if (!lane) return [];
    const requests = [lane.active, ...lane.queue].filter(Boolean);
    requests.forEach((request) => this.cancel(request.id, reason));
    return requests;
  }

  snapshot(conversationId) {
    const lane = this.lanes.get(conversationId);
    return {
      active: lane?.active || null,
      queued: [...(lane?.queue || [])],
    };
  }

  #lane(conversationId) {
    let lane = this.lanes.get(conversationId);
    if (!lane) {
      lane = { active: null, queue: [], pumping: false };
      this.lanes.set(conversationId, lane);
    }
    return lane;
  }

  async #pump(conversationId, lane) {
    if (lane.pumping) return;
    lane.pumping = true;
    try {
      while (lane.queue.length) {
        const request = lane.queue.shift();
        if (request.cancelled) continue;
        lane.active = request;
        request.state = 'running';
        this.#notify(conversationId);
        try {
          const value = await this.runRequest(request);
          request.state = request.cancelled ? 'cancelled' : 'completed';
          request.resolve({ status: request.state, request, value });
        } catch (error) {
          if (request.cancelled) {
            request.state = 'cancelled';
            request.resolve({ status: 'cancelled', request, error });
          } else {
            request.state = 'failed';
            request.reject(error);
            this.onError(error, request);
          }
        } finally {
          if (lane.active?.id === request.id) lane.active = null;
          this.requests.delete(request.id);
          this.#notify(conversationId);
        }
      }
    } finally {
      lane.pumping = false;
      this.#deleteIdleLane(conversationId, lane);
    }
  }

  #deleteIdleLane(conversationId, lane) {
    if (!lane.pumping && !lane.active && lane.queue.length === 0) this.lanes.delete(conversationId);
  }

  #notify(conversationId) {
    try {
      this.onChange(conversationId, this.snapshot(conversationId));
    } catch {
      // UI observation must not change request ordering.
    }
  }
}

export function clearOwnedProcessingStage(conversation, requestId) {
  if (conversation?.processingStage?.requestId !== requestId) return false;
  delete conversation.processingStage;
  return true;
}
