function normalizedText(value) {
  return String(value ?? '').normalize('NFC');
}

function yamlString(value) {
  return JSON.stringify(normalizedText(value));
}

function yamlList(values, fallback) {
  const items = (Array.isArray(values) ? values : [])
    .map(normalizedText)
    .map((item) => item.trim())
    .filter(Boolean);
  return (items.length ? items : [fallback]).map((item) => `  - ${yamlString(item)}`).join('\n');
}

function markdownList(values, fallback) {
  const items = (Array.isArray(values) ? values : [])
    .map(normalizedText)
    .map((item) => item.trim())
    .filter(Boolean);
  return (items.length ? items : [fallback]).map((item) => `- ${item}`).join('\n');
}

export function buildAgentAnalysisArtifact({
  title,
  sourceReference = '',
  sourceField = 'source',
  sourceType = 'unknown',
  observedAt = new Date().toISOString(),
  timestampField = 'captured_at',
  categories = [],
  tags = [],
  analysisMarkdown = '',
  entities = [],
  keyPoints = [],
} = {}) {
  const safeSourceField = sourceField === 'source_url' ? 'source_url' : 'source';
  const safeTimestampField = timestampField === 'received_at' ? 'received_at' : 'captured_at';
  const categoryBlock = Array.isArray(categories) && categories.length
    ? `categories:\n${yamlList(categories, '待分类')}\n`
    : '';
  const content = [
    '---',
    'artifact_kind: agent_analysis',
    `${safeSourceField}: ${yamlString(sourceReference)}`,
    `source_type: ${yamlString(sourceType)}`,
    `${safeTimestampField}: ${yamlString(observedAt)}`,
    categoryBlock.trimEnd(),
    'tags:',
    yamlList(tags, '未分类'),
    '---',
    '',
    `# ${normalizedText(title)} · AI分析`,
    '',
    normalizedText(analysisMarkdown).trim() || '模型未返回分析正文',
    '',
    '## 实体',
    '',
    markdownList(entities, '未识别到明确实体'),
    '',
    '## 关键点',
    '',
    markdownList(keyPoints, '未返回关键点'),
    '',
  ].filter((line, index, lines) => line !== '' || lines[index - 1] !== '').join('\n').normalize('NFC');

  return {
    artifactKind: 'agent_analysis',
    content,
    assetWrites: [],
    sourceReference: normalizedText(sourceReference),
  };
}
