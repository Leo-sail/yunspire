import {
  deriveCreationBlocks,
  normalizeCreationDocument,
} from './document.js';
import { markdownToEditorHtml } from './editor-adapter.js';

const CONTENT_TYPES = new Set(['article', 'wechat', 'xiaohongshu', 'contract', 'paper']);
const CHECK_CATEGORIES = new Set(['content', 'structure', 'citation', 'asset', 'layout', 'compatibility', 'safety', 'brand', 'export']);
const ALLOWED_ADAPTER_TAGS = new Set([
  'a', 'blockquote', 'br', 'code', 'em', 'figcaption', 'figure', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6',
  'hr', 'img', 'li', 'ol', 'p', 'pre', 'strong', 'table', 'tbody', 'td', 'th', 'thead', 'tr', 'ul',
]);
const VOID_TAGS = new Set(['br', 'hr', 'img']);
const LINK_SCHEMES = new Set(['http', 'https', 'mailto', 'obsidian', 'tel']);
const IMAGE_SCHEMES = new Set(['asset', 'blob', 'http', 'https', 'tauri', 'yunspire-draft']);

export const CREATION_RUNTIME_CONTENT_TYPES = Object.freeze([...CONTENT_TYPES]);

export const CONTENT_TYPE_RUNTIME_DEFINITIONS = Object.freeze({
  article: Object.freeze({ label: '文章', outputTarget: 'html', rootRole: 'article' }),
  wechat: Object.freeze({ label: '微信公众号', outputTarget: 'wechat', rootRole: 'article' }),
  xiaohongshu: Object.freeze({ label: '小红书', outputTarget: 'image', rootRole: 'article' }),
  contract: Object.freeze({ label: '合同', outputTarget: 'pdf', rootRole: 'document' }),
  paper: Object.freeze({ label: '论文', outputTarget: 'pdf', rootRole: 'document' }),
});

function normalizeContentType(value, fallback = 'article') {
  if (CONTENT_TYPES.has(value)) return value;
  return CONTENT_TYPES.has(fallback) ? fallback : 'article';
}

function normalizedRuntimeDocument(value, options = {}) {
  const requestedType = CONTENT_TYPES.has(options.contentType) ? options.contentType : null;
  if (typeof value === 'string') {
    return normalizeCreationDocument({
      canonicalMarkdown: value,
      contentType: requestedType,
    }, { ...options, compatibilityAliases: false });
  }
  const source = value && typeof value === 'object' && !Array.isArray(value) ? value : {};
  return normalizeCreationDocument({
    ...source,
    contentType: requestedType || normalizeContentType(source.contentType),
  }, { ...options, compatibilityAliases: false });
}

function blockMarkdown(document, block) {
  const start = Math.max(0, Math.min(document.canonicalMarkdown.length, Number(block?.sourceRange?.start || 0)));
  const end = Math.max(start, Math.min(document.canonicalMarkdown.length, Number(block?.sourceRange?.end || start)));
  return document.canonicalMarkdown.slice(start, end);
}

function plainMarkdown(value) {
  return String(value || '')
    .replace(/^```[^\n]*\n?|```$/gmu, '')
    .replace(/!\[([^\]]*)\]\([^)]+\)/gu, '$1')
    .replace(/!\[\[([^\]|]+)(?:\|[^\]]+)?\]\]/gu, '$1')
    .replace(/\[([^\]]+)\]\([^)]+\)/gu, '$1')
    .replace(/\[\[([^\]|]+)(?:\|([^\]]+))?\]\]/gu, (_, target, label) => label || target)
    .replace(/^#{1,6}\s+/gmu, '')
    .replace(/^>\s?/gmu, '')
    .replace(/^\s*(?:[-*+]\s+|\d+[.)]\s+)/gmu, '')
    .replace(/<[^>]*>/gu, ' ')
    .replace(/(?:\*\*|__|~~|`)/gu, '')
    .replace(/(?<!\*)\*(?!\*)|(?<!_)_(?!_)/gu, '')
    .replace(/\[\^[^\]]+\]/gu, '')
    .replace(/[|]/gu, ' ')
    .replace(/\s+/gu, ' ')
    .trim();
}

function headingText(markdown) {
  return String(markdown || '').match(/^#{1,6}\s+(.+)$/mu)?.[1]?.trim() || '';
}

function headingLevel(markdown) {
  return String(markdown || '').match(/^(#{1,6})\s+/mu)?.[1]?.length || 0;
}

function runtimeDescriptors(document) {
  // Canonical Markdown is authoritative; derive fresh ranges instead of trusting
  // blocks persisted by an older editor revision.
  return deriveCreationBlocks(document.canonicalMarkdown).map((block, index) => {
    const markdown = blockMarkdown(document, block);
    const text = plainMarkdown(markdown);
    return {
      ...block,
      index,
      markdown,
      text,
      headingText: block.kind === 'heading' ? headingText(markdown) : '',
      headingLevel: block.kind === 'heading' ? headingLevel(markdown) : 0,
      listItemCount: block.kind === 'list'
        ? markdown.split(/\r?\n/u).filter((line) => /^\s*(?:[-*+]\s+|\d+[.)]\s+)/u.test(line)).length
        : 0,
      orderedListItemCount: block.kind === 'list'
        ? markdown.split(/\r?\n/u).filter((line) => /^\s*\d+[.)]\s+/u.test(line)).length
        : 0,
    };
  });
}

function headingOrderValid(headings) {
  let previous = 0;
  for (const heading of headings) {
    if (previous && heading.headingLevel > previous + 1) return false;
    previous = heading.headingLevel;
  }
  return true;
}

function matchesHeading(context, pattern) {
  return context.headings.some((heading) => pattern.test(heading.headingText));
}

function hashtagValues(markdown) {
  return [...String(markdown || '').matchAll(/(?:^|[\s，,。！？!?；;])#([\p{L}\p{N}_-]{2,30})(?=$|[\s，,。！？!?；;])/gmu)]
    .map((match) => match[1]);
}

function isHashtagOnlyBlock(descriptor) {
  const source = descriptor.markdown.trim();
  if (!source || !hashtagValues(source).length) return false;
  return source.split(/\s+/u).every((token) => /^#[\p{L}\p{N}_-]{2,30}$/u.test(token));
}

function markdownDestinations(markdown) {
  const source = String(markdown || '');
  const destinations = [];
  const opener = /(!?)\[[^\]]*\]\(/gu;
  for (const match of source.matchAll(opener)) {
    const start = Number(match.index) + match[0].length;
    let depth = 1;
    let escaped = false;
    let end = start;
    for (; end < source.length; end += 1) {
      const character = source[end];
      if (escaped) {
        escaped = false;
      } else if (character === '\\') {
        escaped = true;
      } else if (character === '(') {
        depth += 1;
      } else if (character === ')') {
        depth -= 1;
        if (depth === 0) break;
      }
    }
    if (depth !== 0) continue;
    const raw = source.slice(start, end).trim();
    const destination = raw.startsWith('<')
      ? raw.slice(1, raw.indexOf('>') >= 0 ? raw.indexOf('>') : undefined)
      : raw.match(/^\S+/u)?.[0] || '';
    if (destination) destinations.push({ destination, kind: match[1] ? 'image' : 'link' });
  }
  return destinations;
}

function createContext(value, options = {}) {
  const document = normalizedRuntimeDocument(value, options);
  const descriptors = runtimeDescriptors(document);
  const headings = descriptors.filter((block) => block.kind === 'heading');
  const titleHeadings = headings.filter((heading) => heading.headingLevel === 1);
  const narrativeBlocks = descriptors.filter((block) => ['paragraph', 'quote', 'list'].includes(block.kind) && block.text);
  const paragraphs = descriptors.filter((block) => ['paragraph', 'quote'].includes(block.kind) && block.text);
  const bodyDescriptors = descriptors.filter((block) => block !== titleHeadings[0]);
  const bodyText = bodyDescriptors.map((block) => block.text).filter(Boolean).join('\n');
  const markdown = document.canonicalMarkdown;
  const hashtags = hashtagValues(markdown);
  const unsafeDestinations = markdownDestinations(markdown)
    .filter((item) => !safeUrl(item.destination, item.kind))
    .map((item) => item.destination.slice(0, 240));
  const contractClauseCount = Math.max(
    [...markdown.matchAll(/(?:^|\n)\s*(?:#{1,6}\s*)?第[一二三四五六七八九十百千万零〇两0-9]+条(?:\s|[：:、.])/gu)].length,
    descriptors.filter((block) => block.kind === 'list').reduce((total, block) => total + block.orderedListItemCount, 0),
  );
  const contractCoreTerms = {
    obligations: /(?:权利.{0,16}义务|义务.{0,16}权利|甲方[\s\S]{0,120}(?:应当|应|负责)[\s\S]{0,240}乙方|rights?\s+and\s+obligations?)/iu.test(markdown),
    liability: /(?:违约(?:责任|金)?|损害赔偿|赔偿责任|breach|liabilit|indemn)/iu.test(markdown),
    dispute: /(?:争议解决|适用法律|管辖|仲裁|诉讼|dispute|governing\s+law|jurisdiction|arbitration)/iu.test(markdown),
  };
  const paper = {
    abstract: matchesHeading({ headings }, /^(?:摘要|abstract)$/iu) || /(?:^|\n)\s*(?:\*\*)?(?:摘要|abstract)(?:\*\*)?\s*[：:]/iu.test(markdown),
    keywords: matchesHeading({ headings }, /^(?:关键词|关键字|keywords?)$/iu) || /(?:^|\n)\s*(?:\*\*)?(?:关键词|关键字|keywords?)(?:\*\*)?\s*[：:]/iu.test(markdown),
    introduction: matchesHeading({ headings }, /(?:引言|绪论|研究背景|introduction)/iu),
    method: matchesHeading({ headings }, /(?:研究方法|方法论|材料与方法|methodology|methods?)/iu),
    conclusion: matchesHeading({ headings }, /(?:结论|结语|总结与展望|conclusions?)/iu),
    references: matchesHeading({ headings }, /(?:参考文献|引用文献|bibliography|references?)/iu),
    citations: document.sourceRefs.length > 0
      || /(?:\[\^[^\]]+\]|\[[0-9]{1,3}\]|obsidian:\/\/open\?|\([^)]*,\s*\d{4}[a-z]?\))/iu.test(markdown),
  };
  return {
    document,
    contentType: document.contentType,
    descriptors,
    headings,
    titleHeadings,
    narrativeBlocks,
    paragraphs,
    bodyDescriptors,
    bodyText,
    markdown,
    metrics: {
      blockCount: descriptors.length,
      bodyBlockCount: bodyDescriptors.filter((block) => block.text).length,
      bodyCharacterCount: bodyText.length,
      headingCount: headings.length,
      paragraphCount: paragraphs.length,
      longestParagraph: Math.max(0, ...paragraphs.map((block) => block.text.length)),
      rawHtmlBlockCount: descriptors.filter((block) => block.kind === 'html').length,
      listItemCount: descriptors.reduce((total, block) => total + block.listItemCount, 0),
    },
    features: {
      headingOrderValid: headingOrderValid(headings),
      firstNarrativeBlock: narrativeBlocks[0] || null,
      lastNarrativeBlock: narrativeBlocks.at(-1) || null,
      hashtags,
      contractClauseCount,
      contractCoreTerms,
      contractParties: (/甲方/u.test(markdown) && /乙方/u.test(markdown))
        || (/party\s+a/iu.test(markdown) && /party\s+b/iu.test(markdown)),
      contractExecution: /(?:签署|签字|盖章|生效日期|签订日期|signature|executed\s+by|effective\s+date)/iu.test(markdown),
      contractPlaceholders: [...markdown.matchAll(/(?:待确认|待填写|待补充|TBD|TODO|【待[^】]*】|\[(?:待确认|待填写|待定)[^\]]*\])/giu)].map((match) => match[0]),
      paper,
      unsafeDestinations,
    },
  };
}

function readinessCheck(id, category, passed, failureStatus, passDetail, failDetail, evidenceRefs = []) {
  return {
    id,
    category: CHECK_CATEGORIES.has(category) ? category : 'content',
    status: passed ? 'pass' : failureStatus,
    deterministic: true,
    detail: passed ? passDetail : failDetail,
    evidenceRefs: [...new Set(evidenceRefs.filter(Boolean))],
  };
}

function baseChecks(context) {
  const type = context.contentType;
  return [
    readinessCheck(
      `content-type.${type}.single-title`,
      'structure',
      context.titleHeadings.length === 1,
      'fail',
      '正文包含且仅包含一个一级标题。',
      context.titleHeadings.length ? '正文存在多个一级标题，请保留一个主标题。' : '正文缺少一级标题。',
      context.titleHeadings.map((block) => block.id),
    ),
    readinessCheck(
      `content-type.${type}.body`,
      'content',
      context.metrics.bodyCharacterCount > 0,
      'fail',
      '主标题之后存在正文内容。',
      '主标题之后没有可发布的正文内容。',
    ),
    readinessCheck(
      `content-type.${type}.heading-order`,
      'structure',
      context.features.headingOrderValid,
      'warn',
      '标题层级连续。',
      '标题层级存在跨级，需要复核目录结构。',
      context.headings.map((block) => block.id),
    ),
    readinessCheck(
      `content-type.${type}.canonical-markdown`,
      'safety',
      context.metrics.rawHtmlBlockCount === 0,
      'warn',
      '正文使用规范 Markdown，不含原始 HTML 块。',
      '检测到原始 HTML；预览会将其转义为文本，不会直接执行。',
      context.descriptors.filter((block) => block.kind === 'html').map((block) => block.id),
    ),
    readinessCheck(
      `content-type.${type}.safe-links`,
      'safety',
      context.features.unsafeDestinations.length === 0,
      'fail',
      'Markdown 中的链接和图片地址均使用允许的协议。',
      `检测到 ${context.features.unsafeDestinations.length} 个不安全的链接或图片地址；预览已移除，但定稿前必须修正原文。`,
    ),
  ];
}

function articleChecks(context) {
  const longArticleNeedsSections = context.metrics.bodyCharacterCount <= 600
    || context.headings.some((heading) => heading.headingLevel >= 2);
  return [
    readinessCheck(
      'content-type.article.sectioning',
      'structure',
      longArticleNeedsSections,
      'warn',
      '文章长度与分节结构匹配。',
      '长文章没有二级分节，不利于持续阅读。',
    ),
    readinessCheck(
      'content-type.article.paragraph-length',
      'content',
      context.metrics.longestParagraph <= 1200,
      'warn',
      '段落长度处于可阅读范围。',
      '存在超过 1200 字的单段，建议拆分。',
    ),
  ];
}

function wechatChecks(context) {
  const first = context.features.firstNarrativeBlock;
  const last = context.features.lastNarrativeBlock;
  const hasOpening = Boolean(first && ['paragraph', 'quote'].includes(first.kind) && first.text.length >= 8);
  const hasClosing = Boolean(last && ['paragraph', 'quote'].includes(last.kind)
    && (last !== first || context.narrativeBlocks.length > 1)
    && /(?:最后|总之|因此|结语|写在最后|欢迎|期待|一起|以上|感谢|愿)/u.test(last.text));
  return [
    readinessCheck(
      'content-type.wechat.opening',
      'structure',
      hasOpening,
      'fail',
      '主标题后有明确的开场段。',
      '微信公众号文章需要在主标题后提供开场段。',
      first ? [first.id] : [],
    ),
    readinessCheck(
      'content-type.wechat.sections',
      'structure',
      context.headings.some((heading) => heading.headingLevel === 2),
      'fail',
      '正文已使用二级标题分节。',
      '微信公众号文章至少需要一个二级标题来组织正文。',
    ),
    readinessCheck(
      'content-type.wechat.closing',
      'content',
      hasClosing,
      'warn',
      '正文包含可识别的收束段。',
      '没有识别到明确收束语，请人工确认文章结尾。',
      last ? [last.id] : [],
    ),
  ];
}

function xiaohongshuChecks(context) {
  const first = context.features.firstNarrativeBlock;
  const scanable = context.narrativeBlocks.length >= 3 || context.metrics.listItemCount >= 3;
  return [
    readinessCheck(
      'content-type.xiaohongshu.value-opening',
      'content',
      Boolean(first && first.text.length >= 4 && first.text.length <= 160),
      'fail',
      '首个正文块直接且简短。',
      '小红书首个正文块应直接给出价值，并控制在 160 字以内。',
      first ? [first.id] : [],
    ),
    readinessCheck(
      'content-type.xiaohongshu.scanability',
      'structure',
      scanable,
      'fail',
      '正文具备至少三个可扫描内容单元。',
      '小红书正文需要至少三个短内容块或三个清单项。',
    ),
    readinessCheck(
      'content-type.xiaohongshu.paragraph-length',
      'content',
      context.metrics.longestParagraph <= 220,
      'warn',
      '正文段落适合快速扫描。',
      '存在超过 220 字的长段，建议拆分为短段或清单。',
    ),
    readinessCheck(
      'content-type.xiaohongshu.hashtags',
      'export',
      context.features.hashtags.length > 0,
      'warn',
      `已识别 ${context.features.hashtags.length} 个话题标签。`,
      '未识别到话题标签，发布前可补充与正文直接相关的标签。',
    ),
  ];
}

function contractChecks(context) {
  const terms = context.features.contractCoreTerms;
  const completeCoreTerms = terms.obligations && terms.liability && terms.dispute;
  return [
    readinessCheck(
      'content-type.contract.parties',
      'structure',
      context.features.contractParties,
      'fail',
      '合同明确列出双方主体。',
      '合同必须明确列出甲方与乙方（或 Party A 与 Party B）。',
    ),
    readinessCheck(
      'content-type.contract.numbered-clauses',
      'structure',
      context.features.contractClauseCount >= 3,
      'fail',
      `已识别 ${context.features.contractClauseCount} 个合同条款。`,
      '正式合同至少需要三个可识别的编号条款或条款章节。',
    ),
    readinessCheck(
      'content-type.contract.core-terms',
      'content',
      completeCoreTerms,
      'fail',
      '权利义务、违约责任和争议解决条款齐全。',
      `合同核心条款不完整：${[
        !terms.obligations ? '权利义务' : '',
        !terms.liability ? '违约责任' : '',
        !terms.dispute ? '争议解决' : '',
      ].filter(Boolean).join('、')}。`,
    ),
    readinessCheck(
      'content-type.contract.execution',
      'structure',
      context.features.contractExecution,
      'fail',
      '合同包含签署或生效安排。',
      '合同缺少签署、盖章或生效日期安排。',
    ),
    readinessCheck(
      'content-type.contract.placeholders',
      'export',
      context.features.contractPlaceholders.length === 0,
      'fail',
      '合同不存在未填写占位符。',
      `合同仍有 ${context.features.contractPlaceholders.length} 个待确认字段，完成前不能视为可导出定稿。`,
    ),
  ];
}

function paperChecks(context) {
  const paper = context.features.paper;
  return [
    readinessCheck('content-type.paper.abstract', 'structure', paper.abstract, 'fail', '论文包含摘要。', '论文缺少摘要。'),
    readinessCheck('content-type.paper.keywords', 'structure', paper.keywords, 'fail', '论文包含关键词。', '论文缺少关键词。'),
    readinessCheck('content-type.paper.introduction', 'structure', paper.introduction, 'fail', '论文包含引言或研究背景。', '论文缺少引言或研究背景章节。'),
    readinessCheck('content-type.paper.method', 'structure', paper.method, 'warn', '论文明确说明研究方法。', '没有识别到研究方法章节，请确认当前论文类型是否适用。'),
    readinessCheck('content-type.paper.conclusion', 'structure', paper.conclusion, 'fail', '论文包含结论。', '论文缺少结论章节。'),
    readinessCheck('content-type.paper.references', 'citation', paper.references, 'fail', '论文包含参考文献章节。', '论文缺少参考文献章节。'),
    readinessCheck('content-type.paper.citations', 'citation', paper.citations, 'fail', '论文存在可审计的来源或文内引用。', '论文没有可审计的来源或文内引用。'),
  ];
}

function checksForContext(context) {
  const typeChecks = {
    article: articleChecks,
    wechat: wechatChecks,
    xiaohongshu: xiaohongshuChecks,
    contract: contractChecks,
    paper: paperChecks,
  }[context.contentType](context);
  return [...baseChecks(context), ...typeChecks];
}

function publicAnalysis(context, checks) {
  return {
    contentType: context.contentType,
    documentId: context.document.id,
    valid: checks.every((item) => item.status !== 'fail'),
    title: context.titleHeadings[0]?.headingText || '',
    metrics: { ...context.metrics },
    headings: context.headings.map((heading) => ({
      blockId: heading.id,
      level: heading.headingLevel,
      text: heading.headingText,
    })),
    features: {
      headingOrderValid: context.features.headingOrderValid,
      hashtags: [...context.features.hashtags],
      contractClauseCount: context.features.contractClauseCount,
      contractParties: context.features.contractParties,
      contractExecution: context.features.contractExecution,
      contractPlaceholders: [...context.features.contractPlaceholders],
      contractCoreTerms: { ...context.features.contractCoreTerms },
      paper: { ...context.features.paper },
      unsafeDestinations: [...context.features.unsafeDestinations],
    },
    issues: checks.filter((item) => item.status !== 'pass').map((item) => ({
      code: item.id,
      severity: item.status === 'fail' ? 'error' : 'warning',
      message: item.detail,
      blockIds: [...item.evidenceRefs],
    })),
  };
}

export function analyzeCreationContentStructure(value, options = {}) {
  const context = createContext(value, options);
  const checks = checksForContext(context);
  return publicAnalysis(context, checks);
}

export function createContentTypeReadinessChecks(value, options = {}) {
  return checksForContext(createContext(value, options));
}

function decodeAttributeEntities(value) {
  return String(value || '')
    .replace(/&#x([0-9a-f]+);/giu, (_, code) => String.fromCodePoint(Number.parseInt(code, 16)))
    .replace(/&#(\d+);/gu, (_, code) => String.fromCodePoint(Number(code)))
    .replace(/&tab;/giu, '\t')
    .replace(/&newline;/giu, '\n')
    .replace(/&colon;/giu, ':')
    .replace(/&quot;/giu, '"')
    .replace(/&apos;|&#39;/giu, "'")
    .replace(/&lt;/giu, '<')
    .replace(/&gt;/giu, '>')
    .replace(/&amp;/giu, '&');
}

function escapeAttribute(value) {
  return String(value || '')
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;');
}

function safeUrl(value, kind) {
  const decoded = decodeAttributeEntities(value).trim();
  if (!decoded) return null;
  const schemeProbe = decoded
    .replace(/[\u0000-\u0020\u007f]+/gu, '')
    .toLowerCase();
  if (/^(?:#|\/|\.\/|\.\.\/)/u.test(schemeProbe)) return decoded;
  const scheme = schemeProbe.match(/^([a-z][a-z0-9+.-]*):/u)?.[1];
  if (!scheme) return decoded;
  if (kind === 'link' && scheme === 'obsidian' && !/^obsidian:\/\/open(?:[/?#]|$)/iu.test(schemeProbe)) return null;
  return (kind === 'image' ? IMAGE_SCHEMES : LINK_SCHEMES).has(scheme) ? decoded : null;
}

function allowedAttributes(tagName) {
  if (tagName === 'a') return new Set(['href', 'title']);
  if (tagName === 'img') return new Set(['alt', 'data-attachment-id', 'data-attachment-name', 'src', 'title']);
  if (tagName === 'pre') return new Set(['data-language']);
  if (tagName === 'ol') return new Set(['start']);
  if (tagName === 'th' || tagName === 'td') return new Set(['data-align']);
  return new Set();
}

function sanitizeAdapterHtml(value, audit) {
  const source = String(value || '').replace(/<!--[\s\S]*?-->|<![^>]*>/gu, '');
  return source.replace(/<\/?([A-Za-z][A-Za-z0-9-]*)([^>]*)>/gu, (token, rawName, rawAttributes) => {
    const tagName = rawName.toLowerCase();
    if (!ALLOWED_ADAPTER_TAGS.has(tagName)) {
      audit.removedTags += 1;
      return '';
    }
    if (token.startsWith('</')) return VOID_TAGS.has(tagName) ? '' : `</${tagName}>`;
    const allowed = allowedAttributes(tagName);
    const attributes = [];
    const attributePattern = /([A-Za-z_:][A-Za-z0-9:._-]*)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+)))?/gu;
    for (const match of rawAttributes.matchAll(attributePattern)) {
      const name = match[1].toLowerCase();
      if (!allowed.has(name)) {
        audit.removedAttributes += 1;
        continue;
      }
      const decoded = decodeAttributeEntities(match[2] ?? match[3] ?? match[4] ?? '');
      if (name === 'href' || name === 'src') {
        const url = safeUrl(decoded, name === 'src' ? 'image' : 'link');
        if (!url) {
          audit.blockedUrls += 1;
          continue;
        }
        attributes.push(`${name}="${escapeAttribute(url)}"`);
        continue;
      }
      attributes.push(`${name}="${escapeAttribute(decoded)}"`);
    }
    if (tagName === 'a' && attributes.some((item) => item.startsWith('href="'))) {
      attributes.push('rel="noopener noreferrer"');
    }
    const suffix = attributes.length ? ` ${attributes.join(' ')}` : '';
    return `<${tagName}${suffix}>`;
  });
}

function splitMarkdownTableRow(row) {
  const source = String(row || '').trim().replace(/^\|/u, '').replace(/\|$/u, '');
  const cells = [];
  let cell = '';
  let escaped = false;
  for (const character of source) {
    if (escaped) {
      cell += character;
      escaped = false;
    } else if (character === '\\') {
      escaped = true;
    } else if (character === '|') {
      cells.push(cell.trim());
      cell = '';
    } else {
      cell += character;
    }
  }
  cells.push(cell.trim());
  return cells;
}

function inlineCellHtml(markdown, audit) {
  const rendered = sanitizeAdapterHtml(markdownToEditorHtml(String(markdown || '').replace(/\r?\n/gu, ' ')), audit);
  return rendered.match(/^<p>([\s\S]*)<\/p>$/u)?.[1] || escapeAttribute(plainMarkdown(markdown));
}

function renderMarkdownTable(markdown, audit) {
  const rows = String(markdown || '').split(/\r?\n/u).filter((line) => line.trim());
  if (rows.length < 2) return sanitizeAdapterHtml(markdownToEditorHtml(markdown), audit);
  const headers = splitMarkdownTableRow(rows[0]);
  const separators = splitMarkdownTableRow(rows[1]);
  const aligns = separators.map((cell) => {
    const value = cell.trim();
    if (/^:-+:$/u.test(value)) return 'center';
    if (/^-+:$/u.test(value)) return 'right';
    if (/^:-+$/u.test(value)) return 'left';
    return '';
  });
  const headerHtml = headers.map((cell, index) => `<th${aligns[index] ? ` data-align="${aligns[index]}"` : ''}>${inlineCellHtml(cell, audit)}</th>`).join('');
  const bodyHtml = rows.slice(2).map((row) => {
    const cells = splitMarkdownTableRow(row);
    return `<tr>${headers.map((_, index) => `<td${aligns[index] ? ` data-align="${aligns[index]}"` : ''}>${inlineCellHtml(cells[index] || '', audit)}</td>`).join('')}</tr>`;
  }).join('');
  return `<table><thead><tr>${headerHtml}</tr></thead><tbody>${bodyHtml}</tbody></table>`;
}

function renderDescriptor(descriptor, audit) {
  if (descriptor.kind === 'table') return renderMarkdownTable(descriptor.markdown, audit);
  return sanitizeAdapterHtml(markdownToEditorHtml(descriptor.markdown), audit);
}

function renderedBlock(descriptor, audit) {
  return `<div data-creation-block-id="${escapeAttribute(descriptor.id)}">${renderDescriptor(descriptor, audit)}</div>`;
}

function splitTitle(context) {
  const title = context.titleHeadings[0] || null;
  return {
    title,
    body: context.descriptors.filter((descriptor) => descriptor !== title),
  };
}

function renderArticle(context, audit) {
  const { title, body } = splitTitle(context);
  const header = title ? `<header data-article-part="title">${renderedBlock(title, audit)}</header>` : '';
  return `${header}<section data-article-part="body">${body.map((block) => renderedBlock(block, audit)).join('')}</section>`;
}

function renderWechat(context, audit) {
  const { title, body } = splitTitle(context);
  const lead = body.find((block) => ['paragraph', 'quote'].includes(block.kind)) || null;
  const closing = [...body].reverse().find((block) => ['paragraph', 'quote'].includes(block.kind) && block !== lead) || null;
  const middle = body.filter((block) => block !== lead && block !== closing);
  return [
    title ? `<header data-wechat-part="title">${renderedBlock(title, audit)}</header>` : '',
    lead ? `<section data-wechat-part="lead">${renderedBlock(lead, audit)}</section>` : '',
    `<section data-wechat-part="body">${middle.map((block) => renderedBlock(block, audit)).join('')}</section>`,
    closing ? `<footer data-wechat-part="closing">${renderedBlock(closing, audit)}</footer>` : '',
  ].join('');
}

function renderXiaohongshu(context, audit) {
  const { title, body } = splitTitle(context);
  const tagBlocks = body.filter(isHashtagOnlyBlock);
  const contentBlocks = body.filter((block) => !tagBlocks.includes(block));
  return [
    title ? `<header data-xiaohongshu-part="title">${renderedBlock(title, audit)}</header>` : '',
    `<section data-xiaohongshu-part="content">${contentBlocks.map((block, index) => `<section data-xiaohongshu-block="${index + 1}">${renderedBlock(block, audit)}</section>`).join('')}</section>`,
    tagBlocks.length ? `<footer data-xiaohongshu-part="hashtags">${tagBlocks.map((block) => renderedBlock(block, audit)).join('')}</footer>` : '',
  ].join('');
}

function contractGroupKind(descriptor) {
  if (descriptor.kind !== 'heading' || descriptor.headingLevel < 2) return '';
  if (/(?:签署|签字|盖章|生效|execution|signature)/iu.test(descriptor.headingText)) return 'execution';
  return 'clause';
}

function renderContract(context, audit) {
  const { title, body } = splitTitle(context);
  const groups = [];
  let active = { kind: 'preamble', blocks: [] };
  groups.push(active);
  for (const descriptor of body) {
    const kind = contractGroupKind(descriptor);
    if (kind) {
      active = { kind, blocks: [] };
      groups.push(active);
    }
    active.blocks.push(descriptor);
  }
  let clauseNumber = 0;
  const groupedHtml = groups.filter((group) => group.blocks.length).map((group) => {
    if (group.kind === 'execution') {
      return `<footer data-contract-part="execution">${group.blocks.map((block) => renderedBlock(block, audit)).join('')}</footer>`;
    }
    if (group.kind === 'clause') clauseNumber += 1;
    const attribute = group.kind === 'clause'
      ? ` data-contract-clause="${clauseNumber}"`
      : ' data-contract-part="preamble"';
    return `<section${attribute}>${group.blocks.map((block) => renderedBlock(block, audit)).join('')}</section>`;
  }).join('');
  return `${title ? `<header data-contract-part="title">${renderedBlock(title, audit)}</header>` : ''}${groupedHtml}`;
}

function paperSectionKind(descriptor) {
  const label = descriptor.headingText || descriptor.text.slice(0, 80);
  if (/^(?:摘要|abstract)(?:\s|[：:]|$)/iu.test(label)) return 'abstract';
  if (/^(?:关键词|关键字|keywords?)(?:\s|[：:]|$)/iu.test(label)) return 'keywords';
  if (/(?:参考文献|引用文献|bibliography|references?)/iu.test(label)) return 'references';
  if (/(?:引言|绪论|研究背景|introduction)/iu.test(label)) return 'introduction';
  if (/(?:研究方法|方法论|材料与方法|methodology|methods?)/iu.test(label)) return 'method';
  if (/(?:结果|研究发现|results?)/iu.test(label)) return 'results';
  if (/(?:讨论|discussion)/iu.test(label)) return 'discussion';
  if (/(?:结论|结语|总结与展望|conclusions?)/iu.test(label)) return 'conclusion';
  return 'body';
}

function renderPaper(context, audit) {
  const { title, body } = splitTitle(context);
  const groups = [];
  let active = { kind: 'body', blocks: [] };
  groups.push(active);
  for (const descriptor of body) {
    const kind = paperSectionKind(descriptor);
    const startsSection = descriptor.kind === 'heading'
      || (descriptor.kind === 'paragraph' && ['abstract', 'keywords'].includes(kind));
    if (startsSection) {
      active = { kind, blocks: [] };
      groups.push(active);
    }
    active.blocks.push(descriptor);
  }
  const groupedHtml = groups.filter((group) => group.blocks.length).map((group) => {
    const tag = group.kind === 'references' ? 'footer' : (group.kind === 'keywords' ? 'aside' : 'section');
    return `<${tag} data-paper-section="${group.kind}">${group.blocks.map((block) => renderedBlock(block, audit)).join('')}</${tag}>`;
  }).join('');
  return `${title ? `<header data-paper-section="title">${renderedBlock(title, audit)}</header>` : ''}${groupedHtml}`;
}

function renderContext(context) {
  const audit = { blockedUrls: 0, removedAttributes: 0, removedTags: 0 };
  const body = {
    article: renderArticle,
    wechat: renderWechat,
    xiaohongshu: renderXiaohongshu,
    contract: renderContract,
    paper: renderPaper,
  }[context.contentType](context, audit);
  const definition = CONTENT_TYPE_RUNTIME_DEFINITIONS[context.contentType];
  const html = `<article data-yunspire-content-type="${context.contentType}" data-document-id="${escapeAttribute(context.document.id)}" role="${definition.rootRole}">${body}</article>`;
  const validation = validateRenderedCreationHtml(html, { contentType: context.contentType });
  if (!validation.valid) throw new Error(`内容类型渲染器生成了不安全的 HTML：${validation.violations.join('；')}`);
  return { html, audit, validation };
}

export function validateRenderedCreationHtml(value, options = {}) {
  const html = String(value || '');
  const expectedType = options.contentType ? normalizeContentType(options.contentType) : null;
  const violations = [];
  const rootType = html.match(/^<article\s+[^>]*data-yunspire-content-type="([a-z]+)"[^>]*>/u)?.[1] || '';
  if (!CONTENT_TYPES.has(rootType)) violations.push('缺少有效的内容类型根节点');
  if (expectedType && rootType !== expectedType) violations.push(`渲染内容类型应为 ${expectedType}`);
  if (!/<\/article>$/u.test(html)) violations.push('内容类型根节点没有正确闭合');
  if (/<\/?(?:script|style|iframe|object|embed|form|input|meta|link|base|svg|math)(?:\s|>)/iu.test(html)) violations.push('HTML 包含禁止执行的标签');
  if (/\son[a-z]+\s*=/iu.test(html)) violations.push('HTML 包含事件处理属性');
  if (/\s(?:style|srcdoc)\s*=/iu.test(html)) violations.push('HTML 包含未允许的样式或嵌入文档属性');
  for (const match of html.matchAll(/\s(href|src)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))/giu)) {
    const url = match[2] ?? match[3] ?? match[4] ?? '';
    if (!safeUrl(url, match[1].toLowerCase() === 'src' ? 'image' : 'link')) {
      violations.push(`${match[1].toLowerCase()} 包含不安全的 URL`);
    }
  }
  return { valid: violations.length === 0, contentType: rootType || null, violations: [...new Set(violations)] };
}

export function renderCreationContent(value, options = {}) {
  const context = createContext(value, options);
  const checks = checksForContext(context);
  const rendered = renderContext(context);
  return {
    contentType: context.contentType,
    outputTarget: CONTENT_TYPE_RUNTIME_DEFINITIONS[context.contentType].outputTarget,
    document: context.document,
    analysis: publicAnalysis(context, checks),
    checks,
    html: rendered.html,
    sanitization: { ...rendered.audit },
    validation: rendered.validation,
  };
}

export function renderCreationContentHtml(value, options = {}) {
  return renderCreationContent(value, options).html;
}

export function evaluateCreationContentTypeRuntime(value, options = {}) {
  return renderCreationContent(value, options);
}
