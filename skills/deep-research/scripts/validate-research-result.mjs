#!/usr/bin/env node

import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import process from "node:process";
import { pathToFileURL } from "node:url";

const outputSchema = JSON.parse(
  await readFile(new URL("../output.schema.json", import.meta.url), "utf8"),
);

function childPath(path, key) {
  return path === "/" ? `/${key}` : `${path}/${key}`;
}

function schemaReference(reference) {
  if (typeof reference !== "string" || !reference.startsWith("#/")) {
    throw new Error(`不支持的 JSON Schema 引用：${reference}`);
  }
  return reference.slice(2).split("/").reduce((value, segment) => (
    value?.[segment.replaceAll("~1", "/").replaceAll("~0", "~")]
  ), outputSchema);
}

function matchesType(value, type) {
  if (type === "null") return value === null;
  if (type === "array") return Array.isArray(value);
  if (type === "object") return value !== null && typeof value === "object" && !Array.isArray(value);
  if (type === "integer") return Number.isInteger(value);
  if (type === "number") return typeof value === "number" && Number.isFinite(value);
  return typeof value === type;
}

function sameJsonValue(left, right) {
  return JSON.stringify(left) === JSON.stringify(right);
}

function validateSchemaValue(value, schema, path, errors) {
  if (!schema || typeof schema !== "object") {
    errors.push(`${path} 的 schema 无效`);
    return;
  }
  if (schema.$ref) {
    validateSchemaValue(value, schemaReference(schema.$ref), path, errors);
    return;
  }
  if (Array.isArray(schema.oneOf)) {
    const matched = schema.oneOf.filter((candidate) => {
      const candidateErrors = [];
      validateSchemaValue(value, candidate, path, candidateErrors);
      return candidateErrors.length === 0;
    });
    if (matched.length !== 1) errors.push(`${path} 必须且只能匹配一个 oneOf 分支`);
    return;
  }

  const acceptedTypes = Array.isArray(schema.type) ? schema.type : [schema.type].filter(Boolean);
  if (acceptedTypes.length && !acceptedTypes.some((type) => matchesType(value, type))) {
    errors.push(`${path} 类型必须为 ${acceptedTypes.join(" 或 ")}`);
    return;
  }
  if (Object.hasOwn(schema, "const") && !sameJsonValue(value, schema.const)) {
    errors.push(`${path} 必须等于 ${JSON.stringify(schema.const)}`);
  }
  if (Array.isArray(schema.enum) && !schema.enum.some((item) => sameJsonValue(value, item))) {
    errors.push(`${path} 不在允许的枚举值中`);
  }

  if (typeof value === "string") {
    if (Number.isInteger(schema.minLength) && value.length < schema.minLength) {
      errors.push(`${path} 长度不能小于 ${schema.minLength}`);
    }
    if (Number.isInteger(schema.maxLength) && value.length > schema.maxLength) {
      errors.push(`${path} 长度不能大于 ${schema.maxLength}`);
    }
    if (schema.pattern && !new RegExp(schema.pattern, "u").test(value)) {
      errors.push(`${path} 不符合格式 ${schema.pattern}`);
    }
    if (schema.format === "uri") {
      try {
        const parsed = new URL(value);
        if (!parsed.protocol) throw new Error("missing protocol");
      } catch {
        errors.push(`${path} 必须是有效 URI`);
      }
    }
    if (schema.format === "date-time"
      && (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/u.test(value)
        || !Number.isFinite(Date.parse(value)))) {
      errors.push(`${path} 必须是有效 date-time`);
    }
  }

  if (typeof value === "number" && Number.isFinite(value)) {
    if (typeof schema.minimum === "number" && value < schema.minimum) {
      errors.push(`${path} 不能小于 ${schema.minimum}`);
    }
    if (typeof schema.maximum === "number" && value > schema.maximum) {
      errors.push(`${path} 不能大于 ${schema.maximum}`);
    }
  }

  if (Array.isArray(value)) {
    if (Number.isInteger(schema.minItems) && value.length < schema.minItems) {
      errors.push(`${path} 数量不能少于 ${schema.minItems}`);
    }
    if (Number.isInteger(schema.maxItems) && value.length > schema.maxItems) {
      errors.push(`${path} 数量不能多于 ${schema.maxItems}`);
    }
    if (schema.uniqueItems) {
      const identities = value.map((item) => JSON.stringify(item));
      if (new Set(identities).size !== identities.length) errors.push(`${path} 不能包含重复项`);
    }
    if (schema.items) {
      value.forEach((item, index) => {
        validateSchemaValue(item, schema.items, childPath(path, index), errors);
      });
    }
  }

  if (value !== null && typeof value === "object" && !Array.isArray(value)) {
    for (const required of schema.required || []) {
      if (!Object.hasOwn(value, required)) errors.push(`${path} 缺少必填字段 ${required}`);
    }
    const properties = schema.properties || {};
    for (const [key, propertySchema] of Object.entries(properties)) {
      if (Object.hasOwn(value, key)) {
        validateSchemaValue(value[key], propertySchema, childPath(path, key), errors);
      }
    }
    if (schema.additionalProperties === false) {
      for (const key of Object.keys(value)) {
        if (!Object.hasOwn(properties, key)) errors.push(`${childPath(path, key)} 是未声明字段`);
      }
    }
  }

  for (const conjunct of schema.allOf || []) {
    validateSchemaValue(value, conjunct, path, errors);
  }
  if (schema.if) {
    const conditionErrors = [];
    validateSchemaValue(value, schema.if, path, conditionErrors);
    if (conditionErrors.length === 0 && schema.then) {
      validateSchemaValue(value, schema.then, path, errors);
    }
  }
}

const workflowStages = [
  "plan",
  "evidence_collection",
  "contradiction_check",
  "synthesis",
  "citations",
  "reflection",
];

function indexUnique(items, key, label, errors) {
  const index = new Map();
  for (const item of items) {
    const value = item?.[key];
    if (typeof value !== "string" || value.length === 0) {
      errors.push(`${label} 缺少 ${key}`);
    } else if (index.has(value)) {
      errors.push(`${label} ${key} 重复：${value}`);
    } else {
      index.set(value, item);
    }
  }
  return index;
}

function requireReference(index, id, context, errors) {
  if (!index.has(id)) errors.push(`${context} 引用了不存在的 ${id}`);
}

export function validateResearchResult(result) {
  if (!result || typeof result !== "object" || Array.isArray(result)) {
    return { valid: false, errors: ["研究结果必须是 JSON 对象"] };
  }
  const schemaErrors = [];
  validateSchemaValue(result, outputSchema, "/", schemaErrors);
  if (schemaErrors.length) {
    return {
      valid: false,
      errors: schemaErrors.map((error) => `output.schema.json ${error}`),
    };
  }

  const errors = [];

  const sources = Array.isArray(result.sources) ? result.sources : [];
  const evidence = Array.isArray(result.evidence) ? result.evidence : [];
  const claims = Array.isArray(result.claims) ? result.claims : [];
  const citations = Array.isArray(result.citations) ? result.citations : [];
  const checkpoints = Array.isArray(result.checkpoints) ? result.checkpoints : [];
  const sourceById = indexUnique(sources, "source_id", "来源", errors);
  const evidenceById = indexUnique(evidence, "evidence_id", "证据", errors);
  const claimById = indexUnique(claims, "claim_id", "主张", errors);
  const citationByMarker = indexUnique(citations, "marker", "引用", errors);

  for (const item of evidence) {
    requireReference(sourceById, item.source_id, `证据 ${item.evidence_id}`, errors);
    const source = sourceById.get(item.source_id);
    if (source && item.source_content_hash !== source.content_hash) {
      errors.push(`证据 ${item.evidence_id} 的内容哈希与来源不一致`);
    }
  }

  for (const claim of claims) {
    if (!Array.isArray(claim.evidence_ids) || claim.evidence_ids.length === 0) {
      errors.push(`主张 ${claim.claim_id} 缺少证据`);
    }
    if (!Array.isArray(claim.citation_markers) || claim.citation_markers.length === 0) {
      errors.push(`主张 ${claim.claim_id} 缺少引用`);
    }
    for (const evidenceId of claim.evidence_ids || []) {
      requireReference(evidenceById, evidenceId, `主张 ${claim.claim_id}`, errors);
    }
    for (const marker of claim.citation_markers || []) {
      requireReference(citationByMarker, marker, `主张 ${claim.claim_id}`, errors);
      const citation = citationByMarker.get(marker);
      if (citation && !(citation.claim_ids || []).includes(claim.claim_id)) {
        errors.push(`引用 ${marker} 未回指主张 ${claim.claim_id}`);
      }
    }
  }

  for (const citation of citations) {
    requireReference(sourceById, citation.source_id, `引用 ${citation.marker}`, errors);
    const citationSource = sourceById.get(citation.source_id);
    if (citationSource && citationSource.retrieval_status !== "accepted") {
      errors.push(`引用 ${citation.marker} 使用了未接受的来源`);
    }
    if (!Array.isArray(citation.evidence_ids) || citation.evidence_ids.length === 0) {
      errors.push(`引用 ${citation.marker} 缺少证据`);
    }
    if (!Array.isArray(citation.claim_ids) || citation.claim_ids.length === 0) {
      errors.push(`引用 ${citation.marker} 缺少主张`);
    }
    for (const evidenceId of citation.evidence_ids || []) {
      requireReference(evidenceById, evidenceId, `引用 ${citation.marker}`, errors);
      const item = evidenceById.get(evidenceId);
      if (item && item.source_id !== citation.source_id) {
        errors.push(`引用 ${citation.marker} 的证据 ${evidenceId} 不属于其来源`);
      }
    }
    for (const claimId of citation.claim_ids || []) {
      requireReference(claimById, claimId, `引用 ${citation.marker}`, errors);
      const claim = claimById.get(claimId);
      if (claim && !(claim.citation_markers || []).includes(citation.marker)) {
        errors.push(`主张 ${claimId} 未回指引用 ${citation.marker}`);
      }
      if (claim && !(citation.evidence_ids || []).some((evidenceId) => (claim.evidence_ids || []).includes(evidenceId))) {
        errors.push(`引用 ${citation.marker} 未覆盖主张 ${claimId} 的证据`);
      }
    }
    if (typeof result.answer_markdown === "string" && !result.answer_markdown.includes(citation.marker)) {
      errors.push(`引用 ${citation.marker} 未出现在答案中`);
    }
  }

  const answerMarkers = typeof result.answer_markdown === "string"
    ? new Set(result.answer_markdown.match(/\[S[1-9][0-9]*\]/g) || [])
    : new Set();
  for (const marker of answerMarkers) {
    requireReference(citationByMarker, marker, "答案", errors);
  }

  for (const conflict of result.contradictions || []) {
    for (const evidenceId of conflict.evidence_ids || []) {
      requireReference(evidenceById, evidenceId, `矛盾 ${conflict.contradiction_id}`, errors);
    }
  }

  for (const checkpoint of checkpoints) {
    for (const sourceId of checkpoint.accepted_source_ids || []) {
      requireReference(sourceById, sourceId, `检查点 ${checkpoint.checkpoint_ref}`, errors);
    }
  }

  if (result.status === "completed") {
    const completedStages = new Set(checkpoints
      .filter((checkpoint) => checkpoint.state === "complete")
      .map((checkpoint) => checkpoint.stage));
    for (const stage of workflowStages) {
      if (!completedStages.has(stage)) errors.push(`完成态缺少 ${stage} 检查点`);
    }
    if (result.last_stage !== "reflection") errors.push("完成态的最后阶段必须是 reflection");
    if (result.termination !== null) errors.push("完成态不能包含终止原因");
    if (result.budget_usage?.limit_reached !== null) errors.push("完成态不能标记预算耗尽");
    const audit = result.reflection?.citation_audit;
    if (!audit || audit.total_claims !== claims.length || audit.cited_claims !== claims.length
      || audit.unsupported_claims !== 0 || audit.orphan_citations !== 0) {
      errors.push("完成态的引用审计必须覆盖全部主张且不能包含孤立引用");
    }
  } else if (typeof result.status === "string" && !result.termination) {
    errors.push("非完成态必须包含终止原因");
  }

  if (result.status === "budget_exhausted" && result.budget_usage?.limit_reached === null) {
    errors.push("预算耗尽状态必须指出触发的预算项");
  }
  if (result.status === "policy_denied" && result.policy_result?.decision !== "deny") {
    errors.push("策略拒绝状态必须包含 deny 决策");
  }
  if (result.status === "awaiting_checkpoint" && result.policy_result?.decision !== "requires_approval") {
    errors.push("等待检查点状态必须包含 requires_approval 决策");
  }
  if (result.policy_result?.decision === "requires_approval" && result.status !== "awaiting_checkpoint") {
    errors.push("requires_approval 决策必须返回 awaiting_checkpoint 状态");
  }

  return { valid: errors.length === 0, errors };
}

async function readInput(path) {
  if (path) return readFile(path, "utf8");
  const chunks = [];
  for await (const chunk of process.stdin) chunks.push(chunk);
  return Buffer.concat(chunks).toString("utf8");
}

async function main() {
  try {
    const source = await readInput(process.argv[2]);
    const result = validateResearchResult(JSON.parse(source));
    process.stdout.write(`${JSON.stringify(result)}\n`);
    if (!result.valid) process.exitCode = 1;
  } catch (error) {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  }
}

const invokedPath = process.argv[1] ? pathToFileURL(resolve(process.argv[1])).href : "";
if (import.meta.url === invokedPath) await main();
