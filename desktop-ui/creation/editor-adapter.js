import {
  DEFAULT_CREATION_FOLDER,
  normalizeCreationDocument,
} from './document.js';

const VOID_ELEMENTS = new Set(['BR', 'HR', 'IMG', 'INPUT', 'META', 'LINK', 'SOURCE', 'WBR']);

function isRecord(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

function escapeHtml(value) {
  return String(value || '')
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;');
}

function decodeHtml(value) {
  return String(value || '')
    .replace(/&#(\d+);/gu, (_, code) => String.fromCodePoint(Number(code)))
    .replace(/&#x([0-9a-f]+);/giu, (_, code) => String.fromCodePoint(Number.parseInt(code, 16)))
    .replace(/&nbsp;/giu, '\u00a0')
    .replace(/&quot;/giu, '"')
    .replace(/&#39;|&apos;/giu, "'")
    .replace(/&lt;/giu, '<')
    .replace(/&gt;/giu, '>')
    .replace(/&amp;/giu, '&');
}

function createMiniElement(tagName, attributes = {}) {
  return { nodeType: 1, tagName, attributes, childNodes: [], parentNode: null };
}

function parseMiniHtml(html) {
  const root = createMiniElement('ROOT');
  const stack = [root];
  const tokens = String(html || '').match(/<!--[\s\S]*?-->|<![^>]*>|<\/?[A-Za-z][^>]*>|[^<]+|</gu) || [];
  for (const token of tokens) {
    if (token.startsWith('<!--') || /^<!/u.test(token)) continue;
    if (!token.startsWith('<')) {
      const node = { nodeType: 3, textContent: decodeHtml(token), parentNode: stack.at(-1) };
      stack.at(-1).childNodes.push(node);
      continue;
    }
    const closing = token.match(/^<\/\s*([A-Za-z0-9-]+)/u);
    if (closing) {
      const tagName = closing[1].toUpperCase();
      while (stack.length > 1) {
        const popped = stack.pop();
        if (popped.tagName === tagName) break;
      }
      continue;
    }
    const opening = token.match(/^<\s*([A-Za-z0-9-]+)([\s\S]*?)\/?\s*>$/u);
    if (!opening) {
      const node = { nodeType: 3, textContent: '<', parentNode: stack.at(-1) };
      stack.at(-1).childNodes.push(node);
      continue;
    }
    const attributes = {};
    const source = opening[2] || '';
    const attributePattern = /([^\s=/>]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'=<>`]+)))?/gu;
    for (const match of source.matchAll(attributePattern)) {
      attributes[match[1].toLowerCase()] = decodeHtml(match[2] ?? match[3] ?? match[4] ?? '');
    }
    const element = createMiniElement(opening[1].toUpperCase(), attributes);
    element.parentNode = stack.at(-1);
    stack.at(-1).childNodes.push(element);
    if (!VOID_ELEMENTS.has(element.tagName) && !/\/\s*>$/u.test(token)) stack.push(element);
  }
  return root;
}

function parseHtml(html, options = {}) {
  const Parser = options.DOMParser || globalThis.DOMParser;
  if (typeof Parser === 'function') return new Parser().parseFromString(String(html || ''), 'text/html').body;
  const documentObject = options.document || globalThis.document;
  if (documentObject?.implementation?.createHTMLDocument) {
    const parsed = documentObject.implementation.createHTMLDocument('Yunspire legacy draft');
    parsed.body.innerHTML = String(html || '');
    return parsed.body;
  }
  return parseMiniHtml(html);
}

function tagName(node) {
  return String(node?.tagName || node?.nodeName || '').toUpperCase();
}

function childNodes(node) {
  return Array.from(node?.childNodes || []);
}

function elementChildren(node) {
  return childNodes(node).filter((child) => child?.nodeType === 1);
}

function attribute(node, name) {
  if (typeof node?.getAttribute === 'function') return node.getAttribute(name);
  return node?.attributes?.[String(name).toLowerCase()] ?? null;
}

function hasClass(node, className) {
  if (node?.classList?.contains) return node.classList.contains(className);
  return String(attribute(node, 'class') || '').split(/\s+/u).includes(className);
}

function descendants(node, predicate, result = []) {
  for (const child of childNodes(node)) {
    if (child.nodeType !== 1) continue;
    if (predicate(child)) result.push(child);
    descendants(child, predicate, result);
  }
  return result;
}

function firstDescendant(node, predicate) {
  return descendants(node, predicate, [])[0] || null;
}

function nodeText(node) {
  if (!node) return '';
  if (node.nodeType === 3) return String(node.textContent || '');
  if (typeof node.textContent === 'string' && !node.childNodes) return node.textContent;
  return childNodes(node).map(nodeText).join('');
}

function cleanText(value) {
  return String(value || '').replace(/[\t\f\v ]+/gu, ' ').replace(/\s*\n\s*/gu, ' ').trim();
}

function inlineNodeToMarkdown(node) {
  if (!node) return '';
  if (node.nodeType === 3) return String(node.textContent || '').replace(/\u00a0/gu, ' ');
  if (node.nodeType !== 1) return '';
  const tag = tagName(node);
  const content = childNodes(node).map(inlineNodeToMarkdown).join('');
  if (tag === 'STRONG' || tag === 'B') return content.trim() ? `**${content}**` : '';
  const style = String(attribute(node, 'style') || '').toLowerCase();
  if (tag === 'EM' || tag === 'I' || (tag === 'SPAN' && /(?:^|;)\s*font-style\s*:\s*(?:italic|oblique)\b/u.test(style))) return content.trim() ? `*${content}*` : '';
  if (tag === 'U') return content.trim() ? `<u>${content}</u>` : '';
  if (tag === 'S' || tag === 'DEL') return content.trim() ? `~~${content}~~` : '';
  if (tag === 'CODE') return content.includes('`') ? `\`\`${content}\`\`` : `\`${content}\``;
  if (tag === 'BR') return '\n';
  if (tag === 'A') {
    const href = String(attribute(node, 'href') || '').trim();
    return href && !/^javascript:/iu.test(href) ? `[${content || href}](${href})` : content;
  }
  if (tag === 'SUP' && hasClass(node, 'citation-ref')) return `[^${cleanText(content)}]`;
  if (tag === 'IMG') {
    const alt = String(attribute(node, 'alt') || '').trim();
    const attachmentId = String(attribute(node, 'data-attachment-id') || '').trim();
    const src = attachmentId ? `yunspire-draft://${attachmentId}` : String(attribute(node, 'src') || '').trim();
    return src ? `![${alt}](${src})` : '';
  }
  return content;
}

function directElementsByTag(node, wantedTag) {
  const wanted = String(wantedTag).toUpperCase();
  return elementChildren(node).filter((child) => tagName(child) === wanted);
}

function componentNodeToMarkdown(node, type) {
  const directParagraphs = directElementsByTag(node, 'P').map((item) => cleanText(inlineNodeToMarkdown(item))).filter(Boolean);
  const directStrong = elementChildren(node).find((item) => ['STRONG', 'B', 'H3', 'H4'].includes(tagName(item)));
  const title = cleanText(nodeText(directStrong));
  if (type === 'divider') return '---';
  if (type === 'lead') return `> [!abstract] ${title || '导读'}\n> ${directParagraphs.join('\n> ') || cleanText(nodeText(node).replace(title, ''))}`;
  if (type === 'quote') return directParagraphs.map((item) => `> ${item}`).join('\n') || `> ${cleanText(nodeText(node))}`;
  if (type === 'notice') return `> [!note] ${title || '提示'}\n> ${directParagraphs.join('\n> ') || cleanText(nodeText(node).replace(title, ''))}`;
  if (type === 'steps' || type === 'timeline') {
    const listItems = descendants(node, (item) => tagName(item) === 'LI').map((item) => cleanText(inlineNodeToMarkdown(item))).filter(Boolean);
    return [`### ${title || (type === 'steps' ? '落地步骤' : '时间线')}`, ...listItems.map((item, index) => `${index + 1}. ${item}`)].join('\n');
  }
  if (type === 'metrics') {
    const cells = elementChildren(node).slice(0, 3).map((item) => {
      const valueNode = firstDescendant(item, (child) => ['STRONG', 'B'].includes(tagName(child)));
      const labelNode = firstDescendant(item, (child) => ['SPAN', 'SMALL', 'P'].includes(tagName(child)));
      return { value: cleanText(nodeText(valueNode)), label: cleanText(nodeText(labelNode)) };
    });
    return `| 指标 | 数值 |\n| --- | --- |\n${cells.map((cell) => `| ${cell.label || '指标'} | ${cell.value || '-'} |`).join('\n')}`;
  }
  if (type === 'compare') {
    const columns = elementChildren(node).slice(0, 2).map((item) => {
      const heading = firstDescendant(item, (child) => ['STRONG', 'B', 'H3', 'H4'].includes(tagName(child)));
      const paragraph = firstDescendant(item, (child) => tagName(child) === 'P');
      return { title: cleanText(nodeText(heading)), body: cleanText(inlineNodeToMarkdown(paragraph)) };
    });
    return `| ${columns[0]?.title || '方案 A'} | ${columns[1]?.title || '方案 B'} |\n| --- | --- |\n| ${columns[0]?.body || '-'} | ${columns[1]?.body || '-'} |`;
  }
  if (type === 'dialogue') return directParagraphs.map((item) => `> ${item}`).join('\n');
  if (type === 'cta') return `> [!tip] ${title || '下一步'}\n> ${directParagraphs.join('\n> ') || cleanText(nodeText(node).replace(title, ''))}`;
  return directParagraphs.join('\n\n') || cleanText(nodeText(node));
}

function tableNodeToMarkdown(node) {
  const rows = descendants(node, (item) => tagName(item) === 'TR').map((row) => elementChildren(row)
    .filter((cell) => ['TH', 'TD'].includes(tagName(cell)))
    .map((cell) => cleanText(inlineNodeToMarkdown(cell)).replaceAll('|', '\\|')));
  if (!rows.length) return '';
  const width = Math.max(...rows.map((row) => row.length));
  const normalized = rows.map((row) => [...row, ...Array(Math.max(0, width - row.length)).fill('')]);
  return [
    `| ${normalized[0].join(' | ')} |`,
    `| ${Array(width).fill('---').join(' | ')} |`,
    ...normalized.slice(1).map((row) => `| ${row.join(' | ')} |`),
  ].join('\n');
}

function blockNodeToMarkdown(node, attachmentPaths) {
  if (!node || node.nodeType !== 1) return '';
  const tag = tagName(node);
  const componentType = String(attribute(node, 'data-creation-block') || '').trim();
  if (componentType) return componentNodeToMarkdown(node, componentType);
  if (/^H[1-6]$/u.test(tag)) return `${'#'.repeat(Number(tag.slice(1)))} ${cleanText(inlineNodeToMarkdown(node)).replace(/^#{1,6}\s*/u, '')}`;
  if (tag === 'P') return cleanText(inlineNodeToMarkdown(node));
  if (tag === 'BLOCKQUOTE') return cleanText(inlineNodeToMarkdown(node)).split('\n').map((line) => `> ${line}`).join('\n');
  if (tag === 'UL' || tag === 'OL') {
    return directElementsByTag(node, 'LI').map((item, index) => `${tag === 'OL' ? `${index + 1}.` : '-'} ${cleanText(inlineNodeToMarkdown(item))}`).join('\n');
  }
  if (tag === 'PRE') {
    const code = firstDescendant(node, (item) => tagName(item) === 'CODE');
    const language = String(attribute(node, 'data-language') || attribute(code, 'data-language') || '').replace(/[^A-Za-z0-9_+-]/gu, '').slice(0, 32);
    return `\`\`\`${language}\n${nodeText(code || node).replace(/^\n|\n$/gu, '')}\n\`\`\``;
  }
  if (tag === 'TABLE') return tableNodeToMarkdown(node);
  if (tag === 'FIGURE') {
    const image = firstDescendant(node, (item) => tagName(item) === 'IMG');
    if (!image) return '';
    const attachmentId = String(attribute(image, 'data-attachment-id') || '').trim();
    const mapped = attachmentId && attachmentPaths?.get ? attachmentPaths.get(attachmentId) : '';
    const source = mapped || (attachmentId ? `yunspire-draft://${attachmentId}` : String(attribute(image, 'src') || '').trim());
    const alt = cleanText(attribute(image, 'alt') || nodeText(firstDescendant(node, (item) => tagName(item) === 'FIGCAPTION')) || '文章图片');
    return source ? (attachmentId ? `![[${source}]]` : `![${alt}](${source})`) : '';
  }
  if (tag === 'IMG') return inlineNodeToMarkdown(node);
  if (tag === 'HR') return '---';
  if (tag === 'SCRIPT' || tag === 'STYLE' || tag === 'LINK') return '';
  const nestedBlocks = elementChildren(node).map((child) => blockNodeToMarkdown(child, attachmentPaths)).filter(Boolean);
  return nestedBlocks.length ? nestedBlocks.join('\n\n') : cleanText(inlineNodeToMarkdown(node));
}

export function editorElementToMarkdown(editor, { attachmentPaths = new Map() } = {}) {
  if (!editor || typeof editor !== 'object') return '';
  return elementChildren(editor)
    .map((node) => blockNodeToMarkdown(node, attachmentPaths))
    .filter((value) => value.trim())
    .join('\n\n');
}

export function legacyHtmlToMarkdown(html, options = {}) {
  return editorElementToMarkdown(parseHtml(html, options), options);
}

function legacyAssets(metadata) {
  const attachments = Array.isArray(metadata?.attachments) ? metadata.attachments : [];
  return attachments.map((attachment, index) => {
    const id = String(attachment?.id || `legacy-asset-${index + 1}`);
    return {
      ...attachment,
      id,
      kind: attachment?.kind || (String(attachment?.mimeType || '').startsWith('image/') ? 'image' : 'file'),
      source: attachment?.relativePath || `yunspire-draft://${id}`,
      metadata: {
        name: attachment?.name || `asset-${index + 1}`,
        state: attachment?.state || 'local',
        ...(attachment?.relativePath ? { relativePath: attachment.relativePath } : {}),
      },
    };
  });
}

export function migrateLegacyEditorDocument(record = {}, options = {}) {
  const source = isRecord(record) ? record : {};
  const metadata = isRecord(source.metadata) ? { ...source.metadata } : {};
  delete metadata.attachments;
  const canonicalMarkdown = source.canonicalMarkdown || source.markdown || legacyHtmlToMarkdown(source.html || '', options);
  const inferredTitle = String(canonicalMarkdown).match(/^#\s+(.+)$/mu)?.[1]?.trim();
  const title = source.title || inferredTitle || '未命名文档';
  const studioState = isRecord(source.creationStudio) ? source.creationStudio : (isRecord(source.studioState) ? source.studioState : {});
  return normalizeCreationDocument({
    id: source.id,
    title,
    canonicalMarkdown,
    metadata: {
      ...metadata,
      vaultId: metadata.vaultId || '',
      folder: metadata.folder || DEFAULT_CREATION_FOLDER,
    },
    creationStudio: studioState,
    assets: source.assets || legacyAssets(source.metadata),
    publishing: source.publishing,
    revision: source.revision || 1,
    createdAt: source.createdAt || metadata.createdAt || metadata.updatedAt,
    updatedAt: source.updatedAt || metadata.updatedAt,
    provenance: {
      sourceKind: 'legacy-contenteditable',
      sourceRef: source.sourceRef || '',
      generatedBy: 'yunspire-editor-adapter',
      notes: ['Migrated from the v0.2 contenteditable draft format.'],
      ...(isRecord(source.provenance) ? source.provenance : {}),
    },
  }, options);
}

export function creationDocumentFromEditor(editor, record = {}, options = {}) {
  return migrateLegacyEditorDocument({
    ...record,
    html: typeof editor?.innerHTML === 'string' ? editor.innerHTML : '',
    canonicalMarkdown: editorElementToMarkdown(editor, options),
  }, options);
}

function inlineMarkdownToHtml(value) {
  return escapeHtml(value)
    .replace(/!\[([^\]]*)\]\(([^)]+)\)/gu, (_, alt, source) => {
      const attachmentId = source.match(/^yunspire-draft:\/\/(.+)$/u)?.[1];
      return attachmentId
        ? `<img data-attachment-id="${attachmentId}" src="" alt="${alt}">`
        : `<img src="${source}" alt="${alt}">`;
    })
    .replace(/\[([^\]]+)\]\(([^)]+)\)/gu, '<a href="$2">$1</a>')
    .replace(/\[\[([^\]|]+)\|([^\]]+)\]\]/gu, '<span class="wiki-link">[[$1|$2]]</span>')
    .replace(/\[\[([^\]]+)\]\]/gu, '<span class="wiki-link">[[$1]]</span>')
    .replace(/\*\*([^*]+)\*\*/gu, '<strong>$1</strong>')
    .replace(/~~([^~]+)~~/gu, '<s>$1</s>')
    .replace(/(?<!\*)\*([^*]+)\*(?!\*)/gu, '<em>$1</em>')
    .replace(/&lt;u&gt;([\s\S]*?)&lt;\/u&gt;/gu, '<u>$1</u>')
    .replace(/`([^`]+)`/gu, '<code>$1</code>');
}

function splitMarkdownTableRow(value) {
  const source = String(value || '').trim().replace(/^\|/u, '').replace(/\|$/u, '');
  const cells = [];
  let current = '';
  let escaped = false;
  for (const character of source) {
    if (escaped) {
      current += character;
      escaped = false;
    } else if (character === '\\') {
      current += character;
      escaped = true;
    } else if (character === '|') {
      cells.push(current.trim().replace(/\\\|/gu, '|'));
      current = '';
    } else {
      current += character;
    }
  }
  cells.push(current.trim().replace(/\\\|/gu, '|'));
  return cells;
}

function markdownTableDelimiter(value) {
  const cells = splitMarkdownTableRow(value);
  return cells.length > 0 && cells.every((cell) => /^:?-{3,}:?$/u.test(cell.replace(/\s+/gu, '')));
}

function markdownTableStart(lines, index) {
  return String(lines[index] || '').includes('|') && markdownTableDelimiter(lines[index + 1] || '');
}

function markdownTableToHtml(lines, index) {
  const header = splitMarkdownTableRow(lines[index]);
  const body = [];
  let cursor = index + 2;
  while (cursor < lines.length && lines[cursor].trim() && lines[cursor].includes('|')) {
    body.push(splitMarkdownTableRow(lines[cursor]));
    cursor += 1;
  }
  const width = Math.max(header.length, ...body.map((row) => row.length));
  const cells = (row, tag) => Array.from({ length: width }, (_, cellIndex) => `<${tag}>${inlineMarkdownToHtml(row[cellIndex] || '')}</${tag}>`).join('');
  const bodyMarkup = body.length ? `<tbody>${body.map((row) => `<tr>${cells(row, 'td')}</tr>`).join('')}</tbody>` : '';
  return {
    html: `<table><thead><tr>${cells(header, 'th')}</tr></thead>${bodyMarkup}</table>`,
    nextIndex: cursor,
  };
}

export function markdownToEditorHtml(markdown) {
  const lines = String(markdown || '').split(/\r?\n/u);
  const html = [];
  let index = 0;
  while (index < lines.length) {
    const line = lines[index];
    if (!line.trim()) {
      index += 1;
      continue;
    }
    const fence = line.match(/^```([^\s`]*)\s*$/u);
    if (fence) {
      const code = [];
      index += 1;
      while (index < lines.length && !/^```\s*$/u.test(lines[index])) code.push(lines[index++]);
      index += index < lines.length ? 1 : 0;
      html.push(`<pre${fence[1] ? ` data-language="${escapeHtml(fence[1])}"` : ''}><code>${escapeHtml(code.join('\n'))}</code></pre>`);
      continue;
    }
    const heading = line.match(/^(#{1,6})\s+(.+)$/u);
    if (heading) {
      const level = heading[1].length;
      html.push(`<h${level}>${inlineMarkdownToHtml(heading[2])}</h${level}>`);
      index += 1;
      continue;
    }
    if (/^(?:---|\*\*\*|___)\s*$/u.test(line)) {
      html.push('<hr>');
      index += 1;
      continue;
    }
    if (markdownTableStart(lines, index)) {
      const table = markdownTableToHtml(lines, index);
      html.push(table.html);
      index = table.nextIndex;
      continue;
    }
    const image = line.match(/^!\[([^\]]*)\]\(([^)]+)\)$/u) || line.match(/^!\[\[([^\]|]+)(?:\|([^\]]+))?\]\]$/u);
    if (image) {
      const wikiImage = line.startsWith('![[');
      const alt = wikiImage ? (image[2] || image[1]) : image[1] || '文章图片';
      const source = wikiImage ? image[1] : image[2];
      const attachmentId = source.match(/^yunspire-draft:\/\/(.+)$/u)?.[1];
      const imageMarkup = attachmentId
        ? `<img data-attachment-id="${escapeHtml(attachmentId)}" data-attachment-name="${escapeHtml(alt)}" src="" alt="${escapeHtml(alt)}">`
        : `<img src="${escapeHtml(source)}" alt="${escapeHtml(alt)}">`;
      html.push(`<figure>${imageMarkup}<figcaption>${escapeHtml(alt)}</figcaption></figure>`);
      index += 1;
      continue;
    }
    if (/^>\s?/u.test(line)) {
      const quote = [];
      while (index < lines.length && /^>\s?/u.test(lines[index])) quote.push(lines[index++].replace(/^>\s?/u, ''));
      html.push(`<blockquote>${quote.map(inlineMarkdownToHtml).join('<br>')}</blockquote>`);
      continue;
    }
    if (/^(?:[-*+]\s+|\d+[.)]\s+)/u.test(line)) {
      const ordered = /^\d/u.test(line);
      const items = [];
      const pattern = ordered ? /^\d+[.)]\s+/u : /^[-*+]\s+/u;
      while (index < lines.length && pattern.test(lines[index])) items.push(lines[index++].replace(pattern, ''));
      const tag = ordered ? 'ol' : 'ul';
      html.push(`<${tag}>${items.map((item) => `<li>${inlineMarkdownToHtml(item)}</li>`).join('')}</${tag}>`);
      continue;
    }
    const paragraph = [];
    while (index < lines.length && lines[index].trim()
      && !markdownTableStart(lines, index)
      && !/^(?:```|#{1,6}\s+|>\s?|[-*+]\s+|\d+[.)]\s+|---\s*$|\*\*\*\s*$|___\s*$)/u.test(lines[index])) paragraph.push(lines[index++]);
    html.push(`<p>${paragraph.map(inlineMarkdownToHtml).join('<br>')}</p>`);
  }
  return html.join('');
}

export function creationDocumentToEditorHtml(value) {
  const document = normalizeCreationDocument(value, { compatibilityAliases: false });
  if (!document.blocks.length) return markdownToEditorHtml(document.canonicalMarkdown);
  return document.blocks.map((block) => {
    const markdown = document.canonicalMarkdown.slice(block.sourceRange.start, block.sourceRange.end);
    const html = markdownToEditorHtml(markdown);
    if (block.kind !== 'component') return html;
    return `<section data-creation-block="${escapeHtml(block.componentId || 'notice')}">${html}</section>`;
  }).join('');
}

export function applyCreationDocumentToEditor(editor, value, options = {}) {
  if (!editor || typeof editor !== 'object' || !('innerHTML' in editor)) throw new TypeError('A contenteditable editor element is required');
  const document = normalizeCreationDocument(value, options);
  editor.innerHTML = creationDocumentToEditorHtml(document);
  if (options.contentEditable !== undefined && 'contentEditable' in editor) editor.contentEditable = String(Boolean(options.contentEditable));
  return document;
}
