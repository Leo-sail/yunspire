import test from 'node:test';
import assert from 'node:assert/strict';
import { buildAgentAnalysisArtifact } from '../desktop-ui/agent-analysis-artifact.js';

test('builds one analysis-only artifact without copying source content or attachments', () => {
  const originalContent = 'DO-NOT-COPY-RAW-CONTENT-7f3d';
  const artifact = buildAgentAnalysisArtifact({
    title: '测试标题',
    sourceReference: 'https://example.com/source',
    sourceType: 'url',
    observedAt: '2026-07-29T12:00:00.000Z',
    analysisMarkdown: '这是模型分析，不是原文。',
    entities: ['云枢'],
    keyPoints: ['只保留分析产物'],
    originalContent,
    attachments: [{ name: 'source.pdf', bytes: originalContent }],
  });

  assert.equal(artifact.artifactKind, 'agent_analysis');
  assert.match(artifact.content, /^---\nartifact_kind: agent_analysis\n/u);
  assert.equal(artifact.content.includes(originalContent), false);
  assert.deepEqual(artifact.assetWrites, []);
  assert.equal(artifact.sourceReference, 'https://example.com/source');
  assert.match(artifact.content, /source: "https:\/\/example\.com\/source"/u);
});

test('normalizes Chinese, emoji, and combining characters to NFC', () => {
  const decomposed = 'Cafe\u0301';
  const artifact = buildAgentAnalysisArtifact({
    title: `中文 ${decomposed} 🧠`,
    sourceReference: `本地/${decomposed}.md`,
    analysisMarkdown: `分析 ${decomposed} ✅`,
    tags: [decomposed],
  });

  assert.equal(artifact.content, artifact.content.normalize('NFC'));
  assert.equal(artifact.sourceReference, artifact.sourceReference.normalize('NFC'));
  assert.equal(artifact.content.includes(decomposed), false);
  assert.match(artifact.content, /Café/u);
});

test('quotes source metadata and keeps it outside the analysis body', () => {
  const artifact = buildAgentAnalysisArtifact({
    title: '收件箱分析',
    sourceReference: '文件: 原文.md\nunsafe: value',
    sourceType: 'file',
    timestampField: 'received_at',
    categories: ['资料'],
    analysisMarkdown: '摘要内容',
  });

  assert.match(artifact.content, /source: "文件: 原文\.md\\nunsafe: value"/u);
  assert.match(artifact.content, /received_at:/u);
  assert.equal((artifact.content.match(/artifact_kind: agent_analysis/gu) || []).length, 1);
});
