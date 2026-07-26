export class AssistantRequestCoordinator {
  constructor() {
    this.activeRequests = new Map();
    this.activeConversationRequests = new Map();
    this.conversationTails = new Map();
    this.pendingSubmissions = new Map();
    this.generation = 0;
  }

  register(request) {
    this.activeRequests.set(request.id, request);
    this.activeConversationRequests.set(request.conversationId, request.id);
  }

  request(requestId) {
    return this.activeRequests.get(requestId);
  }

  activeForConversation(conversationId) {
    const requestId = this.activeConversationRequests.get(conversationId);
    return requestId ? this.activeRequests.get(requestId) : undefined;
  }

  finish(requestId) {
    const request = this.activeRequests.get(requestId);
    if (!request) return;
    this.activeRequests.delete(requestId);
    if (this.activeConversationRequests.get(request.conversationId) === requestId) {
      this.activeConversationRequests.delete(request.conversationId);
    }
  }

  allActive() {
    return [...this.activeRequests.values()];
  }

  hasConversationWork(conversationId) {
    return this.conversationTails.has(conversationId);
  }

  pendingForConversation(conversationId) {
    return [...(this.pendingSubmissions.get(conversationId) || [])];
  }

  enqueue(conversationId, submission, operation) {
    const generation = this.generation;
    const pending = this.pendingSubmissions.get(conversationId) || [];
    pending.push(submission);
    this.pendingSubmissions.set(conversationId, pending);

    const previous = this.conversationTails.get(conversationId) || Promise.resolve();
    const execution = previous.catch(() => undefined).then(async () => {
      if (generation !== this.generation) return undefined;
      const queued = this.pendingSubmissions.get(conversationId) || [];
      const index = queued.indexOf(submission);
      if (index >= 0) queued.splice(index, 1);
      if (queued.length) this.pendingSubmissions.set(conversationId, queued);
      else this.pendingSubmissions.delete(conversationId);
      return operation(submission);
    });
    this.conversationTails.set(conversationId, execution);
    void execution.finally(() => {
      if (this.conversationTails.get(conversationId) === execution) {
        this.conversationTails.delete(conversationId);
      }
    }).catch(() => undefined);
    return execution;
  }

  clear() {
    this.generation += 1;
    this.activeRequests.clear();
    this.activeConversationRequests.clear();
    this.conversationTails.clear();
    this.pendingSubmissions.clear();
  }
}
