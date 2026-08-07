/**
 * Rich-text commands for the creation editor.
 *
 * The module deliberately does not touch DOM globals while it is imported.
 * Browser implementations are injected through the adapter object, which also
 * keeps command behavior deterministic and independent of the DOM implementation.
 */

const SAFE_LINK_PROTOCOLS = new Set(['http:', 'https:', 'mailto:', 'tel:']);

const COMMAND_DEFINITIONS = Object.freeze({
  bold: Object.freeze({ command: 'bold', kind: 'toggle' }),
  italic: Object.freeze({ command: 'italic', kind: 'toggle' }),
  underline: Object.freeze({ command: 'underline', kind: 'toggle' }),
  strikeThrough: Object.freeze({ command: 'strikeThrough', kind: 'toggle' }),
  createLink: Object.freeze({ command: 'createLink', kind: 'value', valueRequired: true }),
  blockquote: Object.freeze({ command: 'formatBlock', kind: 'block', value: 'blockquote' }),
  insertUnorderedList: Object.freeze({ command: 'insertUnorderedList', kind: 'action' }),
  insertOrderedList: Object.freeze({ command: 'insertOrderedList', kind: 'action' }),
  undo: Object.freeze({ command: 'undo', kind: 'history' }),
  redo: Object.freeze({ command: 'redo', kind: 'history' }),
});

export const EDITOR_COMMANDS = Object.freeze(Object.keys(COMMAND_DEFINITIONS));
export const EDITOR_COMMAND_MAP = COMMAND_DEFINITIONS;
export const COMMAND_MAP = EDITOR_COMMAND_MAP;

function isRecord(value) {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

function commandName(value) {
  if (typeof value !== 'string' || !value.trim()) {
    throw new TypeError('Editor command must be a non-empty string');
  }
  const name = value.trim();
  if (!Object.prototype.hasOwnProperty.call(COMMAND_DEFINITIONS, name)) {
    throw new RangeError(`Unsupported editor command: ${name}`);
  }
  return name;
}

function isSafeLink(value) {
  if (typeof value !== 'string') return false;
  const candidate = value.trim();
  if (!candidate || /[\u0000-\u001f\u007f]/u.test(candidate)) return false;
  if (/^(?:javascript|vbscript|data):/iu.test(candidate)) return false;
  if (candidate.startsWith('#') || candidate.startsWith('/') || candidate.startsWith('./') || candidate.startsWith('../')) return true;
  try {
    const parsed = new URL(candidate);
    return SAFE_LINK_PROTOCOLS.has(parsed.protocol);
  } catch {
    return false;
  }
}

function normalizeCommandValue(name, value) {
  const definition = COMMAND_DEFINITIONS[name];
  if (name === 'createLink') {
    if (!isSafeLink(value)) {
      throw new TypeError('createLink requires a safe absolute or relative URL');
    }
    return String(value).trim();
  }
  if (definition.kind === 'block') {
    if (value !== undefined && value !== null && value !== '') {
      throw new TypeError('blockquote does not accept a custom format value');
    }
    return definition.value;
  }
  if (value !== undefined && value !== null && value !== '') {
    throw new TypeError(`${name} does not accept a command value`);
  }
  return undefined;
}

export function normalizeEditorCommand(command, value) {
  const name = commandName(command);
  const definition = COMMAND_DEFINITIONS[name];
  return Object.freeze({
    name,
    command: definition.command,
    value: normalizeCommandValue(name, value),
    kind: definition.kind,
  });
}

function defaultExecCommand(command, showUi, value) {
  const documentObject = globalThis.document;
  if (typeof documentObject?.execCommand !== 'function') {
    throw new Error('An execCommand adapter is required outside a browser document');
  }
  return documentObject.execCommand(command, showUi, value);
}

function defaultQueryCommandState(command) {
  const documentObject = globalThis.document;
  if (typeof documentObject?.queryCommandState !== 'function') {
    throw new Error('A queryCommandState adapter is required outside a browser document');
  }
  return documentObject.queryCommandState(command);
}

function defaultGetSelection() {
  if (typeof globalThis.getSelection === 'function') return globalThis.getSelection();
  const documentObject = globalThis.document;
  if (typeof documentObject?.getSelection === 'function') return documentObject.getSelection();
  throw new Error('A selection adapter is required outside a browser document');
}

function cloneRange(range) {
  if (!range || typeof range !== 'object') throw new TypeError('Selection ranges must be objects');
  return typeof range.cloneRange === 'function' ? range.cloneRange() : range;
}

function rangeCount(selection) {
  if (!selection || typeof selection !== 'object') return 0;
  if (Number.isInteger(selection.rangeCount)) return Math.max(0, selection.rangeCount);
  if (Array.isArray(selection.ranges)) return selection.ranges.length;
  return 0;
}

function rangeAt(selection, index) {
  if (typeof selection.getRangeAt === 'function') return selection.getRangeAt(index);
  if (Array.isArray(selection.ranges)) return selection.ranges[index];
  return null;
}

function cloneSnapshot(snapshot, rangeCloner = cloneRange) {
  if (!isRecord(snapshot) || !Array.isArray(snapshot.ranges)) return null;
  return {
    ranges: snapshot.ranges.map(rangeCloner),
    rangeCount: snapshot.ranges.length,
  };
}

function normalizeAdapterOptions(options = {}) {
  if (!isRecord(options)) throw new TypeError('Editor command adapters must be an object');
  const nested = isRecord(options.commandAdapter) ? options.commandAdapter : {};
  const selectionAdapter = isRecord(options.selectionAdapter) ? options.selectionAdapter : {};
  const source = { ...nested, ...options };
  return {
    execCommand: typeof source.execCommand === 'function' ? source.execCommand : defaultExecCommand,
    queryCommandState: typeof source.queryCommandState === 'function' ? source.queryCommandState : defaultQueryCommandState,
    queryCommandValue: typeof source.queryCommandValue === 'function' ? source.queryCommandValue : null,
    getSelection: typeof source.getSelection === 'function'
      ? source.getSelection
      : (typeof selectionAdapter.getSelection === 'function' ? selectionAdapter.getSelection : defaultGetSelection),
    setSelection: typeof source.setSelection === 'function'
      ? source.setSelection
      : (typeof selectionAdapter.setSelection === 'function' ? selectionAdapter.setSelection : null),
    cloneRange: typeof source.cloneRange === 'function' ? source.cloneRange : cloneRange,
    restoreSelection: typeof source.restoreSelection === 'function' ? source.restoreSelection : null,
  };
}

function captureSelection(adapters, suppliedSelection) {
  const selection = suppliedSelection === undefined ? adapters.getSelection() : suppliedSelection;
  const count = rangeCount(selection);
  if (!count) return null;
  const ranges = [];
  for (let index = 0; index < count; index += 1) {
    const range = rangeAt(selection, index);
    if (!range) throw new TypeError(`Selection range ${index} is unavailable`);
    ranges.push(adapters.cloneRange(range));
  }
  return { ranges, rangeCount: ranges.length };
}

function restoreCapturedSelection(adapters, snapshot) {
  const normalized = cloneSnapshot(snapshot, adapters.cloneRange);
  if (!normalized) throw new TypeError('Selection snapshot must contain a ranges array');
  const ranges = normalized.ranges;
  if (adapters.restoreSelection) {
    const result = adapters.restoreSelection({ ranges, rangeCount: ranges.length });
    return result === undefined ? true : Boolean(result);
  }
  if (adapters.setSelection) {
    const result = adapters.setSelection({ ranges, rangeCount: ranges.length });
    return result === undefined ? true : Boolean(result);
  }
  const selection = adapters.getSelection();
  if (!selection || typeof selection.removeAllRanges !== 'function' || typeof selection.addRange !== 'function') {
    throw new Error('Selection adapter must provide setSelection or a DOM Selection object');
  }
  selection.removeAllRanges();
  ranges.forEach((range) => selection.addRange(range));
  return true;
}

export function createEditorCommandController(options = {}) {
  const adapters = normalizeAdapterOptions(options);
  let savedSelection = null;
  const restoreBeforeExecute = options.restoreBeforeExecute !== false;

  const controller = {
    commands: EDITOR_COMMAND_MAP,

    execute(command, value, executeOptions = {}) {
      const normalized = normalizeEditorCommand(command, value);
      const shouldRestore = executeOptions.restoreSelection ?? restoreBeforeExecute;
      if (shouldRestore && savedSelection) restoreCapturedSelection(adapters, savedSelection);
      const result = adapters.execCommand(normalized.command, false, normalized.value);
      return {
        ...normalized,
        applied: result !== false,
        result,
      };
    },

    queryState(command) {
      const name = commandName(command);
      return Boolean(adapters.queryCommandState(COMMAND_DEFINITIONS[name].command));
    },

    queryValue(command) {
      const name = commandName(command);
      if (!adapters.queryCommandValue) return '';
      return String(adapters.queryCommandValue(COMMAND_DEFINITIONS[name].command) ?? '');
    },

    saveSelection(selection) {
      savedSelection = captureSelection(adapters, selection);
      return savedSelection ? cloneSnapshot(savedSelection, adapters.cloneRange) : null;
    },

    restoreSelection(snapshot = savedSelection) {
      if (!snapshot) return false;
      const restored = restoreCapturedSelection(adapters, snapshot);
      if (snapshot === savedSelection) savedSelection = cloneSnapshot(snapshot, adapters.cloneRange);
      return restored;
    },

    clearSelection() {
      savedSelection = null;
    },

    getSavedSelection() {
      return savedSelection ? cloneSnapshot(savedSelection, adapters.cloneRange) : null;
    },

    hasSavedSelection() {
      return Boolean(savedSelection?.ranges?.length);
    },
  };

  return Object.freeze(controller);
}

export function executeEditorCommand(command, value, adapters = {}, options = {}) {
  return createEditorCommandController(adapters).execute(command, value, {
    restoreSelection: options.restoreSelection === true,
  });
}

export function queryEditorCommandState(command, adapters = {}) {
  return createEditorCommandController(adapters).queryState(command);
}

export function saveEditorSelection(adapters = {}, selection) {
  return createEditorCommandController(adapters).saveSelection(selection);
}

export function restoreEditorSelection(snapshot, adapters = {}) {
  const controller = createEditorCommandController(adapters);
  return controller.restoreSelection(snapshot);
}
