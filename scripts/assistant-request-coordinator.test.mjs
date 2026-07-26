import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

import { AssistantRequestCoordinator } from '../desktop-ui/assistant-request-coordinator.js';

const appSource = await readFile(new URL('../desktop-ui/app.js', import.meta.url), 'utf8');

function deferred() {
  let resolve;
  const promise = new Promise((complete) => { resolve = complete; });
  return { promise, resolve };
}

test('serializes submissions in one conversation', async () => {
  const coordinator = new AssistantRequestCoordinator();
  const firstGate = deferred();
  const firstStarted = deferred();
  const events = [];

  const first = coordinator.enqueue('conversation-a', { id: 'first' }, async () => {
    events.push('first:start');
    firstStarted.resolve();
    await firstGate.promise;
    events.push('first:end');
  });
  const second = coordinator.enqueue('conversation-a', { id: 'second' }, async () => {
    events.push('second:start');
  });

  await firstStarted.promise;
  assert.deepEqual(events, ['first:start']);
  assert.deepEqual(coordinator.pendingForConversation('conversation-a').map(({ id }) => id), ['second']);

  firstGate.resolve();
  await Promise.all([first, second]);
  assert.deepEqual(events, ['first:start', 'first:end', 'second:start']);
  assert.equal(coordinator.hasConversationWork('conversation-a'), false);
});

test('runs different conversations independently', async () => {
  const coordinator = new AssistantRequestCoordinator();
  const firstGate = deferred();
  const events = [];

  const first = coordinator.enqueue('conversation-a', { id: 'first' }, async () => {
    events.push('a:start');
    await firstGate.promise;
  });
  const second = coordinator.enqueue('conversation-b', { id: 'second' }, async () => {
    events.push('b:start');
  });

  await second;
  assert.deepEqual(events, ['a:start', 'b:start']);
  firstGate.resolve();
  await first;
});

test('tracks active requests by request and conversation', () => {
  const coordinator = new AssistantRequestCoordinator();
  const first = { id: 'request-a', conversationId: 'conversation-a' };
  const second = { id: 'request-b', conversationId: 'conversation-b' };

  coordinator.register(first);
  coordinator.register(second);
  assert.equal(coordinator.request(first.id), first);
  assert.equal(coordinator.activeForConversation(second.conversationId), second);

  coordinator.finish(first.id);
  assert.equal(coordinator.request(first.id), undefined);
  assert.equal(coordinator.activeForConversation(first.conversationId), undefined);
  assert.equal(coordinator.activeForConversation(second.conversationId), second);
});

test('continues a conversation queue after a rejected operation', async () => {
  const coordinator = new AssistantRequestCoordinator();
  const events = [];

  const first = coordinator.enqueue('conversation-a', { id: 'first' }, async () => {
    events.push('first');
    throw new Error('failed request');
  });
  const second = coordinator.enqueue('conversation-a', { id: 'second' }, async () => {
    events.push('second');
  });

  await assert.rejects(first, /failed request/u);
  await second;
  assert.deepEqual(events, ['first', 'second']);
});

test('clear prevents queued submissions from starting', async () => {
  const coordinator = new AssistantRequestCoordinator();
  const firstGate = deferred();
  const firstStarted = deferred();
  const events = [];

  const first = coordinator.enqueue('conversation-a', { id: 'first' }, async () => {
    events.push('first');
    firstStarted.resolve();
    await firstGate.promise;
  });
  const second = coordinator.enqueue('conversation-a', { id: 'second' }, async () => {
    events.push('second');
  });

  await firstStarted.promise;
  coordinator.clear();
  firstGate.resolve();
  await Promise.all([first, second]);
  assert.deepEqual(events, ['first']);
});

test('assistant composer stays sendable and delegates submissions to the coordinator', () => {
  const submitSource = appSource.match(/async function submitSecretaryTask\([\s\S]*?(?=\nasync function processSecretarySubmission)/u)?.[0] || '';
  assert.match(submitSource, /assistantRequestCoordinator\.enqueue\(conversation\.id, submission, processSecretarySubmission\)/u);
  assert.doesNotMatch(submitSource, /button\.disabled\s*=\s*true/u);
  assert.doesNotMatch(submitSource, /send-button[^\n]*disabled/u);
  assert.match(appSource, /assistantRequestCoordinator\.request\(payload\.requestId\)/u);
  assert.match(appSource, /assistantRequestCoordinator\.activeForConversation\(activeConversationId\)/u);
});
