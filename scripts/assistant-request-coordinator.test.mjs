import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import {
  AssistantRequestCoordinator,
  clearOwnedProcessingStage,
} from '../desktop-ui/assistant-request-coordinator.js';

const appSource = await readFile(new URL('../desktop-ui/app.js', import.meta.url), 'utf8');

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

async function flushMicrotasks() {
  await Promise.resolve();
  await Promise.resolve();
}

test('serializes requests in one conversation', async () => {
  const gates = new Map();
  const started = [];
  const coordinator = new AssistantRequestCoordinator(async (request) => {
    started.push(request.id);
    const gate = deferred();
    gates.set(request.id, gate);
    return gate.promise;
  });
  const first = coordinator.enqueue({ id: 'a', conversationId: 'one' });
  const second = coordinator.enqueue({ id: 'b', conversationId: 'one' });
  await flushMicrotasks();
  assert.deepEqual(started, ['a']);
  assert.equal(coordinator.queued('one')[0].id, 'b');
  gates.get('a').resolve('first');
  await first;
  await flushMicrotasks();
  assert.deepEqual(started, ['a', 'b']);
  gates.get('b').resolve('second');
  await second;
});

test('runs different conversations concurrently', async () => {
  const gates = new Map();
  const started = [];
  const coordinator = new AssistantRequestCoordinator(async (request) => {
    started.push(request.id);
    const gate = deferred();
    gates.set(request.id, gate);
    return gate.promise;
  });
  const first = coordinator.enqueue({ id: 'a', conversationId: 'one' });
  const second = coordinator.enqueue({ id: 'b', conversationId: 'two' });
  await flushMicrotasks();
  assert.deepEqual(new Set(started), new Set(['a', 'b']));
  gates.get('a').resolve();
  gates.get('b').resolve();
  await Promise.all([first, second]);
});

test('cancels one queued request without disturbing other lanes', async () => {
  const gates = new Map();
  const coordinator = new AssistantRequestCoordinator(async (request) => {
    const gate = deferred();
    gates.set(request.id, gate);
    return gate.promise;
  });
  const active = coordinator.enqueue({ id: 'a', conversationId: 'one' });
  const queued = coordinator.enqueue({ id: 'b', conversationId: 'one' });
  const other = coordinator.enqueue({ id: 'c', conversationId: 'two' });
  await flushMicrotasks();
  assert.equal(coordinator.cancel('b')?.state, 'cancelled');
  assert.equal((await queued).status, 'cancelled');
  assert.equal(coordinator.active('one').id, 'a');
  assert.equal(coordinator.active('two').id, 'c');
  gates.get('a').resolve();
  gates.get('c').resolve();
  await Promise.all([active, other]);
});

test('only the owning request can clear a processing stage', () => {
  const conversation = { processingStage: { requestId: 'newer', title: 'running' } };
  assert.equal(clearOwnedProcessingStage(conversation, 'older'), false);
  assert.equal(conversation.processingStage.requestId, 'newer');
  assert.equal(clearOwnedProcessingStage(conversation, 'newer'), true);
  assert.equal(conversation.processingStage, undefined);
});

test('active cancellation resolves as cancelled without reporting an error', async () => {
  const gates = new Map();
  const started = [];
  const errors = [];
  const cancellations = [];
  const coordinator = new AssistantRequestCoordinator(
    async (request) => {
      started.push(request.id);
      const gate = deferred();
      gates.set(request.id, gate);
      return gate.promise;
    },
    {
      onError: (error, request) => errors.push([request.id, error]),
      onCancel: (request) => cancellations.push(request.id),
    },
  );
  const first = coordinator.enqueue({ id: 'a', conversationId: 'one' });
  const second = coordinator.enqueue({ id: 'b', conversationId: 'one' });
  await flushMicrotasks();
  coordinator.cancel('a', 'user_cancelled');
  gates.get('a').reject(new Error('transport aborted'));
  assert.equal((await first).status, 'cancelled');
  await flushMicrotasks();
  assert.deepEqual(started, ['a', 'b']);
  assert.deepEqual(cancellations, ['a']);
  assert.deepEqual(errors, []);
  gates.get('b').resolve('done');
  assert.equal((await second).status, 'completed');
});

test('cancelConversation only cancels the selected conversation', async () => {
  const gates = new Map();
  const coordinator = new AssistantRequestCoordinator(async (request) => {
    const gate = deferred();
    gates.set(request.id, gate);
    return gate.promise;
  });
  const first = coordinator.enqueue({ id: 'a', conversationId: 'one' });
  const queued = coordinator.enqueue({ id: 'b', conversationId: 'one' });
  const other = coordinator.enqueue({ id: 'c', conversationId: 'two' });
  await flushMicrotasks();
  const cancelled = coordinator.cancelConversation('one', 'conversation_cleared');
  assert.deepEqual(cancelled.map((request) => request.id), ['a', 'b']);
  assert.equal((await queued).status, 'cancelled');
  gates.get('a').resolve('late');
  assert.equal((await first).status, 'cancelled');
  assert.equal(coordinator.active('two')?.id, 'c');
  gates.get('c').resolve('done');
  assert.equal((await other).status, 'completed');
});

test('queued cancellation is mirrored to the native owner', async () => {
  const gate = deferred();
  const cancelled = [];
  const coordinator = new AssistantRequestCoordinator(
    async () => gate.promise,
    { onCancel: (request, reason) => cancelled.push([request.id, reason]) },
  );
  const active = coordinator.enqueue({ id: 'a', conversationId: 'one' });
  const queued = coordinator.enqueue({ id: 'b', conversationId: 'one' });
  await flushMicrotasks();
  coordinator.cancel('b', 'user_cancelled');
  assert.equal((await queued).status, 'cancelled');
  assert.deepEqual(cancelled, [['b', 'user_cancelled']]);
  gate.resolve();
  await active;
});

test('assistant composer remains sendable and submits through the coordinator', () => {
  const submitSource = appSource.match(/async function submitSecretaryTask\(\)[\s\S]*?(?=\nasync function runSecretaryTaskRequest)/u)?.[0] || '';
  const runSource = appSource.match(/async function runSecretaryTaskRequest\(requestToken\)[\s\S]*?(?=\nfunction handleSecretaryClick)/u)?.[0] || '';
  const finishSource = appSource.match(/async function finishNativeAssistantRequest\(request, state, error = ''\)[\s\S]*?(?=\nasync function advanceNativeAssistantRevision)/u)?.[0] || '';
  const composerStateSource = appSource.match(/function syncAssistantComposerState\(conversation\)[\s\S]*?(?=\nfunction secretaryMessageMarkup)/u)?.[0] || '';
  assert.match(submitSource, /assistantRequestCoordinator\.enqueue\(requestToken\)/u);
  assert.match(submitSource, /persistNativeAssistantRequest\(requestToken\)/u);
  assert.doesNotMatch(submitSource, /\.disabled\s*=\s*true/u);
  assert.match(composerStateSource, /button\.disabled\s*=\s*false/u);
  assert.match(appSource, /assistantRequestCoordinator\.get\(payload\.requestId\)/u);
  assert.match(appSource, /assistantRequestCoordinator\.active\(conversation\?\.id\)/u);
  assert.match(appSource, /Number\(currentConversation\.requestRevision \|\| 0\) !== Number\(requestToken\.conversationRevision\)/u);
  assert.match(appSource, /assemble_assistant_request_context/u);
  assert.match(appSource, /claim_assistant_request/u);
  assert.match(appSource, /finish_assistant_request/u);
  assert.match(appSource, /recover_assistant_requests/u);
  assert.match(appSource, /modelConfig: request\.modelConfig \|\| null/u);
  assert.match(appSource, /assistantRequestModelConfiguration\(requestToken, modelSelection\)/u);
  assert.ok(submitSource.indexOf('await compactConversationContext(conversation, modelId)') >= 0);
  assert.ok(submitSource.indexOf('await compactConversationContext(conversation, modelId)') < submitSource.indexOf('requestToken.conversationMessages ='));
  assert.ok(submitSource.indexOf('requestToken.conversationMessages =') < submitSource.indexOf('persistNativeAssistantRequest(requestToken)'));
  assert.doesNotMatch(runSource, /await compactConversationContext\(conversation, modelId\)/u);
  assert.match(runSource, /requestToken\.assistantMessageIdsAtStart/u);
  assert.match(finishSource, /hasRequestMessageBoundary/u);
  assert.match(finishSource, /!priorAssistantMessageIds\.has\(message\.id\)/u);
});
