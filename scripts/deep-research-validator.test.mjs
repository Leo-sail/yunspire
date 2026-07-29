import test from 'node:test';
import assert from 'node:assert/strict';
import { validateResearchResult } from '../skills/deep-research/scripts/validate-research-result.mjs';

const contentHash = `sha256:${'a'.repeat(64)}`;
const stages = [
  'plan',
  'evidence_collection',
  'contradiction_check',
  'synthesis',
  'citations',
  'reflection',
];

function budgetUsage() {
  return {
    elapsed_seconds: 12,
    sources_considered: 1,
    sources_accepted: 1,
    network_requests: 0,
    model_calls: 2,
    input_tokens: 200,
    output_tokens: 100,
    download_bytes: 0,
    limit_reached: null,
  };
}

function completedResult() {
  return {
    status: 'completed',
    research_id: 'research-completed-001',
    query: '云枢如何验证研究结果？',
    last_stage: 'reflection',
    answer_markdown: '云枢会校验主张、证据和来源之间的引用关系。[S1]',
    claims: [{
      claim_id: 'claim-validation',
      statement: '云枢会验证研究结果的引用关系。',
      classification: 'fact',
      confidence: 0.95,
      evidence_ids: ['ev-validation'],
      citation_markers: ['[S1]'],
    }],
    sources: [{
      source_id: 'src-validation',
      kind: 'local_vault',
      title: '云枢研究验证说明',
      locator: 'docs/research-validation.md',
      canonical_url: null,
      vault_id: 'primary-vault',
      relative_path: 'docs/research-validation.md',
      publisher: null,
      published_at: null,
      retrieved_at: '2026-07-29T12:00:00.000Z',
      content_hash: contentHash,
      acquisition: 'policy_scoped_vault',
      retrieval_status: 'accepted',
      upstream_source_ids: [],
      transformations: ['none'],
    }],
    evidence: [{
      evidence_id: 'ev-validation',
      source_id: 'src-validation',
      locator: '段落 1',
      excerpt: '验证器会检查主张、证据、来源和引用标记。',
      source_content_hash: contentHash,
      relationship: 'supports',
    }],
    contradictions: [],
    citations: [{
      marker: '[S1]',
      source_id: 'src-validation',
      evidence_ids: ['ev-validation'],
      claim_ids: ['claim-validation'],
      rendered_reference: '云枢研究验证说明，段落 1',
    }],
    checkpoints: stages.map((stage) => ({
      checkpoint_ref: `checkpoint:${stage}`,
      stage,
      state: 'complete',
      recorded_at: '2026-07-29T12:00:00.000Z',
      request_revision: 1,
      input_hash: contentHash,
      result_hash: contentHash,
      accepted_source_ids: ['src-validation'],
      budget_usage: budgetUsage(),
    })),
    budget_usage: budgetUsage(),
    policy_result: {
      decision: 'allow',
      rules_checked: ['vault-read'],
      effective_network_origins: [],
      effective_vault_ids: ['primary-vault'],
      reasons: [],
    },
    reflection: {
      coverage: 1,
      confidence: 0.95,
      source_independence: 1,
      citation_audit: {
        total_claims: 1,
        cited_claims: 1,
        unsupported_claims: 0,
        orphan_citations: 0,
      },
      limitations: [],
      unanswered_questions: [],
      follow_up_queries: [],
    },
    termination: null,
    warnings: [],
    errors: [],
  };
}

function assertInvalidWith(result, expectedError) {
  const validation = validateResearchResult(result);
  assert.equal(validation.valid, false);
  assert.ok(
    validation.errors.includes(expectedError),
    `expected ${JSON.stringify(expectedError)} in ${JSON.stringify(validation.errors)}`,
  );
}

test('accepts a completed result with fully connected citations', () => {
  assert.deepEqual(validateResearchResult(completedResult()), { valid: true, errors: [] });
});

test('rejects a claim without a citation marker', () => {
  const result = completedResult();
  result.claims[0].citation_markers = [];

  assertInvalidWith(result, '主张 claim-validation 缺少引用');
});

test('rejects an orphan citation that no claim points back to', () => {
  const result = completedResult();
  result.citations.push({
    marker: '[S2]',
    source_id: 'src-validation',
    evidence_ids: ['ev-validation'],
    claim_ids: ['claim-validation'],
    rendered_reference: '未被主张采用的引用',
  });

  assertInvalidWith(result, '主张 claim-validation 未回指引用 [S2]');
});

test('rejects a citation marker that does not exist in the citation table', () => {
  const result = completedResult();
  result.claims[0].citation_markers = ['[S9]'];
  result.answer_markdown = '答案错误地引用了不存在的来源。[S9]';

  assertInvalidWith(result, '主张 claim-validation 引用了不存在的 [S9]');
  assertInvalidWith(result, '答案 引用了不存在的 [S9]');
});
