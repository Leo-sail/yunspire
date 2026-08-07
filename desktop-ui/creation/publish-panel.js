import { normalizeCreationDocument, safeCreationId } from './document.js';

const TARGETS = new Set(['wechat', 'html', 'markdown', 'pdf', 'image']);
const CHECK_CATEGORIES = new Set(['content', 'structure', 'citation', 'asset', 'layout', 'compatibility', 'safety', 'brand', 'export']);
const CHECK_STATUSES = new Set(['pass', 'warn', 'fail', 'skip']);

function isRecord(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

function stringValue(value, fallback = '', maximum = 2000) {
  const candidate = typeof value === 'string' || typeof value === 'number' ? String(value).trim() : '';
  return (candidate || fallback).slice(0, maximum);
}

function uniqueStrings(value) {
  return [...new Set((Array.isArray(value) ? value : []).map((item) => stringValue(item)).filter(Boolean))];
}

function validDateTime(value, fallback) {
  return typeof value === 'string' && Number.isFinite(Date.parse(value)) ? new Date(value).toISOString() : fallback;
}

function check(id, category, status, detail, evidenceRefs = []) {
  return {
    id,
    category: CHECK_CATEGORIES.has(category) ? category : 'content',
    status: CHECK_STATUSES.has(status) ? status : 'fail',
    deterministic: true,
    detail: stringValue(detail, '未提供检查说明。'),
    evidenceRefs: uniqueStrings(evidenceRefs),
  };
}

function targetFromDocument(document, requestedTarget) {
  if (TARGETS.has(requestedTarget)) return requestedTarget;
  const target = document.publishing?.target;
  if (TARGETS.has(target)) return target;
  if (String(target || '').includes('wechat')) return 'wechat';
  if (document.publishing?.targets?.includes('wechat')) return 'wechat';
  if (document.publishing?.targets?.includes('html')) return 'html';
  return 'markdown';
}

function assetState(asset) {
  return stringValue(asset?.state || asset?.metadata?.state, asset?.source || asset?.relativePath ? 'local' : 'draft');
}

function summarizeAssets(assets) {
  const list = Array.isArray(assets) ? assets : [];
  const readyStates = new Set(['ready', 'localized', 'local']);
  const uploadStates = new Set(['upload_required', 'draft']);
  const imageKinds = new Set(['image', 'cover', 'infographic', 'gallery', 'longImage']);
  return {
    total: list.length,
    ready: list.filter((asset) => readyStates.has(assetState(asset))).length,
    uploadRequired: list.filter((asset) => uploadStates.has(assetState(asset))).length,
    failed: list.filter((asset) => assetState(asset) === 'failed').length,
    missingAlt: list.filter((asset) => imageKinds.has(asset?.kind) && !stringValue(asset?.alt)).length,
  };
}

function validationFromDocument(document, target, options, assets) {
  const receipt = isRecord(document.validationReceipt) ? document.validationReceipt : {};
  const publishing = isRecord(document.publishing) ? document.publishing : {};
  const coverId = stringValue(publishing.coverAssetId);
  const cover = document.assets.find((asset) => asset.id === coverId || asset.kind === 'cover');
  const htmlResult = isRecord(options.htmlValidation) ? options.htmlValidation : {};
  const citationResult = isRecord(options.citationValidation) ? options.citationValidation : {};
  return {
    schemaValid: options.schemaValid ?? receipt.schemaValid ?? true,
    astValid: options.astValid ?? receipt.astValid ?? (document.blocks.length > 0 || !document.canonicalMarkdown.trim()),
    htmlValid: target === 'markdown' ? true : (options.htmlValid ?? htmlResult.valid ?? receipt.htmlValid ?? false),
    cjkSpacingValid: options.cjkSpacingValid ?? true,
    citationsResolved: options.citationsResolved
      ?? citationResult.valid
      ?? (citationResult.unresolved == null ? true : citationResult.unresolved === 0),
    titleSelected: options.titleSelected ?? Boolean(stringValue(publishing.selectedTitle || document.title)),
    coverReady: target !== 'wechat' || options.coverRequired === false || Boolean(cover && ['ready', 'localized', 'local'].includes(assetState(cover))),
    assetSummary: assets,
  };
}

function checksFromValidation(document, target, validation, options) {
  const checks = [
    check('content.not-empty', 'content', document.canonicalMarkdown.trim() ? 'pass' : 'fail', document.canonicalMarkdown.trim() ? '正文已包含可导出内容。' : '正文为空，无法导出。'),
    check('validation.schema', 'structure', validation.schemaValid ? 'pass' : 'fail', validation.schemaValid ? '文档数据契约检查通过。' : '文档数据契约检查失败。'),
    check('validation.ast', 'structure', validation.astValid ? 'pass' : 'fail', validation.astValid ? 'Markdown AST 结构检查通过。' : 'Markdown AST 结构检查失败。'),
    check('validation.html', 'compatibility', target === 'markdown' ? 'skip' : (validation.htmlValid ? 'pass' : 'fail'), target === 'markdown' ? 'Markdown 目标不需要 HTML 检查。' : (validation.htmlValid ? '最终 HTML 兼容检查通过。' : '最终 HTML 尚未通过兼容检查。')),
    check('layout.cjk-spacing', 'layout', validation.cjkSpacingValid ? 'pass' : 'warn', validation.cjkSpacingValid ? 'CJK 间距策略已检查。' : '尚未确认完整 CJK 间距修复。'),
    check('citation.resolution', 'citation', validation.citationsResolved ? 'pass' : 'fail', validation.citationsResolved ? '引用与无来源声明分流已完成。' : '仍有未处理的引用或无来源声明。'),
    check('publishing.title', 'export', validation.titleSelected ? 'pass' : 'fail', validation.titleSelected ? '已有可用的发布标题。' : '尚未选择发布标题。'),
    check('publishing.cover', 'asset', target !== 'wechat' ? 'skip' : (validation.coverReady ? 'pass' : 'warn'), target !== 'wechat' ? '当前目标不强制封面。' : (validation.coverReady ? '封面素材已就绪。' : '微信导出前建议补充封面素材。')),
    check('asset.failed', 'asset', validation.assetSummary.failed ? 'fail' : 'pass', validation.assetSummary.failed ? `${validation.assetSummary.failed} 个素材处理失败。` : '没有处理失败的素材。'),
    check('asset.upload', 'asset', validation.assetSummary.uploadRequired ? 'warn' : 'pass', validation.assetSummary.uploadRequired ? `${validation.assetSummary.uploadRequired} 个素材仍需上传或本地化。` : '所有引用素材均已就绪。'),
    check('asset.alt', 'asset', validation.assetSummary.missingAlt ? 'warn' : 'pass', validation.assetSummary.missingAlt ? `${validation.assetSummary.missingAlt} 个视觉素材缺少替代文本。` : '视觉素材替代文本已完整。'),
  ];
  if (Array.isArray(options.additionalChecks)) checks.push(...options.additionalChecks.map(normalizeCheck));
  return checks;
}

function normalizeCheck(value, index = 0) {
  const source = isRecord(value) ? value : {};
  return check(
    stringValue(source.id, `custom.${index + 1}`, 100).replace(/[^a-z0-9._-]+/gu, '-').replace(/^[^a-z]+/u, 'custom.'),
    source.category,
    source.status,
    source.detail || source.message,
    source.evidenceRefs,
  );
}

export function createReadinessReport(value, options = {}) {
  const document = normalizeCreationDocument(value, { compatibilityAliases: false });
  const now = validDateTime(options.generatedAt, new Date().toISOString());
  const target = targetFromDocument(document, options.target);
  const assets = summarizeAssets(document.assets);
  const validation = validationFromDocument(document, target, options, assets);
  const checks = checksFromValidation(document, target, validation, options);
  const blockers = checks.filter((item) => item.status === 'fail').map((item) => item.detail);
  const warnings = checks.filter((item) => item.status === 'warn').map((item) => item.detail);
  const previousExport = document.publishing?.status === 'exported' || options.exported === true;
  const status = blockers.length ? 'blocked' : (warnings.length ? 'reviewRequired' : (previousExport ? 'exported' : 'readyForExport'));
  return {
    schemaVersion: '1.0',
    id: safeCreationId(options.id || `readiness-${document.id}-${document.revision}`, 'readiness'),
    documentId: document.id,
    documentRevision: document.revision,
    target,
    status,
    publicationClaim: 'exportOnly',
    checks,
    blockers,
    warnings,
    assets,
    validation: {
      schemaValid: Boolean(validation.schemaValid),
      astValid: Boolean(validation.astValid),
      htmlValid: Boolean(validation.htmlValid),
      cjkSpacingValid: Boolean(validation.cjkSpacingValid),
      citationsResolved: Boolean(validation.citationsResolved),
      titleSelected: Boolean(validation.titleSelected),
      coverReady: Boolean(validation.coverReady),
    },
    output: options.output || null,
    generatedAt: now,
    engineVersion: stringValue(options.engineVersion, '0.3.0', 40),
  };
}

export function normalizeReadinessReport(value, document = {}, options = {}) {
  const source = isRecord(value) ? value : {};
  if (!Array.isArray(source.checks) || !source.checks.length) return createReadinessReport(document, { ...options, ...source });
  const normalizedDocument = normalizeCreationDocument(document, { compatibilityAliases: false });
  const checks = source.checks.map(normalizeCheck);
  const blockers = uniqueStrings(source.blockers?.length ? source.blockers : checks.filter((item) => item.status === 'fail').map((item) => item.detail));
  const warnings = uniqueStrings(source.warnings?.length ? source.warnings : checks.filter((item) => item.status === 'warn').map((item) => item.detail));
  const status = ['blocked', 'reviewRequired', 'readyForExport', 'exported'].includes(source.status)
    ? source.status
    : (blockers.length ? 'blocked' : (warnings.length ? 'reviewRequired' : 'readyForExport'));
  const assets = isRecord(source.assets) ? source.assets : summarizeAssets(normalizedDocument.assets);
  const validation = isRecord(source.validation) ? source.validation : {};
  return {
    schemaVersion: '1.0',
    id: safeCreationId(source.id || `readiness-${normalizedDocument.id}-${normalizedDocument.revision}`, 'readiness'),
    documentId: safeCreationId(source.documentId || normalizedDocument.id),
    documentRevision: Math.max(1, Math.trunc(Number(source.documentRevision || normalizedDocument.revision || 1))),
    target: TARGETS.has(source.target) ? source.target : targetFromDocument(normalizedDocument, options.target),
    status,
    publicationClaim: 'exportOnly',
    checks,
    blockers,
    warnings,
    assets: {
      total: Math.max(0, Math.trunc(Number(assets.total || 0))),
      ready: Math.max(0, Math.trunc(Number(assets.ready || 0))),
      uploadRequired: Math.max(0, Math.trunc(Number(assets.uploadRequired || 0))),
      failed: Math.max(0, Math.trunc(Number(assets.failed || 0))),
      missingAlt: Math.max(0, Math.trunc(Number(assets.missingAlt || 0))),
    },
    validation: {
      schemaValid: validation.schemaValid === true,
      astValid: validation.astValid === true,
      htmlValid: validation.htmlValid === true,
      cjkSpacingValid: validation.cjkSpacingValid === true,
      citationsResolved: validation.citationsResolved === true,
      titleSelected: validation.titleSelected === true,
      coverReady: validation.coverReady === true,
    },
    output: isRecord(source.output) ? source.output : null,
    generatedAt: validDateTime(source.generatedAt, new Date().toISOString()),
    engineVersion: stringValue(source.engineVersion, '0.3.0', 40),
  };
}

export function createReadinessViewModel(reportOrDocument, options = {}) {
  const hasReport = isRecord(reportOrDocument) && Array.isArray(reportOrDocument.checks) && reportOrDocument.documentId;
  const report = hasReport
    ? normalizeReadinessReport(reportOrDocument, options.document || {}, options)
    : createReadinessReport(reportOrDocument, options);
  const pass = report.checks.filter((item) => item.status === 'pass').length;
  const warn = report.checks.filter((item) => item.status === 'warn').length;
  const applicable = report.checks.filter((item) => item.status !== 'skip').length || 1;
  const score = Math.round(((pass + warn * 0.5) / applicable) * 100);
  const labels = {
    blocked: '存在阻断项',
    reviewRequired: '需人工复核',
    readyForExport: '可复制/导出',
    exported: '已导出',
  };
  return {
    report,
    status: report.status,
    label: labels[report.status],
    score,
    ready: report.status === 'readyForExport' || report.status === 'exported',
    canCopy: report.status === 'readyForExport' || report.status === 'exported',
    canExport: report.status === 'readyForExport' || report.status === 'exported',
    published: false,
    publicationClaim: '仅表示可复制或导出，不代表已发布到微信或其他平台。',
    counts: {
      pass,
      warn,
      fail: report.checks.filter((item) => item.status === 'fail').length,
      skip: report.checks.filter((item) => item.status === 'skip').length,
    },
    blockers: report.blockers,
    warnings: report.warnings,
    checks: report.checks,
    assets: report.assets,
  };
}

export const buildReadinessViewModel = createReadinessViewModel;

export function toRuntimeReadinessReport(value, document = {}) {
  const report = normalizeReadinessReport(value, document);
  return {
    ready: report.status === 'readyForExport' || report.status === 'exported',
    target: report.target,
    score: createReadinessViewModel(report, { document }).score,
    checks: report.checks.map((item) => ({
      id: item.id,
      status: item.status,
      severity: item.status === 'fail' ? 'error' : (item.status === 'warn' ? 'warning' : 'info'),
      message: item.detail,
    })),
  };
}
