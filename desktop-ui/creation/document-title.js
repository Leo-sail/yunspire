const DEFAULT_TITLE = '未命名笔记';
const MAX_TITLE_LENGTH = 100;
const INVALID_FILENAME_CHARACTERS = /[\\/:*?"<>|#%{}[\]\u0000-\u001F\u007F-\u009F]+/gu;
const WINDOWS_RESERVED_NAME = /^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/iu;
const NUMBERED_SUFFIX = /^(.*) \((\d+)\)$/u;

function truncateTitle(value, maximum = MAX_TITLE_LENGTH) {
  return [...value].slice(0, maximum).join('').replace(/[ .-]+$/gu, '');
}

function filenameSafeTitle(value) {
  const normalized = String(value ?? '')
    .normalize('NFC')
    .replace(INVALID_FILENAME_CHARACTERS, '-')
    .replace(/\s+/gu, ' ')
    .replace(/^[ .-]+|[ .-]+$/gu, '');
  const title = truncateTitle(normalized) || DEFAULT_TITLE;
  return WINDOWS_RESERVED_NAME.test(title) ? truncateTitle(`_${title}`) : title;
}

function titleKey(value) {
  return filenameSafeTitle(value).normalize('NFC').toLowerCase();
}

function titleList(value) {
  if (Array.isArray(value)) return value;
  if (value instanceof Set) return [...value];
  if (value && typeof value === 'object') return Object.keys(value);
  return [];
}

function numberedTitle(base, number) {
  const suffix = ` (${number})`;
  const availableLength = Math.max(1, MAX_TITLE_LENGTH - [...suffix].length);
  const safeBase = truncateTitle(base, availableLength) || DEFAULT_TITLE;
  return `${safeBase}${suffix}`;
}

/**
 * Returns a stable, cross-platform filename-safe title that does not collide
 * with another creation document. The current document's own title is ignored.
 */
export function resolveCreationDocumentTitle(requestedTitle, { currentTitle = '', titles = [] } = {}) {
  const requested = filenameSafeTitle(requestedTitle);
  const currentKey = currentTitle ? titleKey(currentTitle) : '';
  const occupied = new Set(
    titleList(titles)
      .map(titleKey)
      .filter((key) => key !== currentKey),
  );

  if (!occupied.has(titleKey(requested))) return requested;

  const suffixMatch = requested.match(NUMBERED_SUFFIX);
  const base = suffixMatch?.[1] || requested;
  let number = suffixMatch ? Math.max(2, Number(suffixMatch[2]) + 1) : 2;
  let candidate = numberedTitle(base, number);
  while (occupied.has(titleKey(candidate))) {
    number += 1;
    candidate = numberedTitle(base, number);
  }
  return candidate;
}
