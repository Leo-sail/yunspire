function normalizeSnapshot(value) {
  if (!value || typeof value !== 'object') throw new TypeError('History snapshot must be an object');
  return {
    html: String(value.html ?? ''),
    title: String(value.title ?? ''),
  };
}

function sameSnapshot(left, right) {
  return left?.html === right?.html && left?.title === right?.title;
}

export function createEditorHistory(options = {}) {
  const limit = Math.max(2, Math.min(500, Number(options.limit || 100)));
  let entries = [];
  let index = -1;

  return Object.freeze({
    reset(snapshot) {
      const normalized = normalizeSnapshot(snapshot);
      entries = [normalized];
      index = 0;
      return { ...normalized };
    },
    push(snapshot) {
      const normalized = normalizeSnapshot(snapshot);
      if (sameSnapshot(entries[index], normalized)) return false;
      entries = entries.slice(0, index + 1);
      entries.push(normalized);
      if (entries.length > limit) entries = entries.slice(entries.length - limit);
      index = entries.length - 1;
      return true;
    },
    undo() {
      if (index <= 0) return null;
      index -= 1;
      return { ...entries[index] };
    },
    redo() {
      if (index < 0 || index >= entries.length - 1) return null;
      index += 1;
      return { ...entries[index] };
    },
    current() {
      return index >= 0 ? { ...entries[index] } : null;
    },
    canUndo() {
      return index > 0;
    },
    canRedo() {
      return index >= 0 && index < entries.length - 1;
    },
    size() {
      return entries.length;
    },
  });
}
