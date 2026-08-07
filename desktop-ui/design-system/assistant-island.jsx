import React from 'react';
import { createRoot } from 'react-dom/client';
import { AssistantEntry } from './AssistantEntry.jsx';

const mountedRoots = new WeakMap();

export function mountR10AssistantEntry(container, onActivate) {
  if (!container || mountedRoots.has(container)) return;
  const root = createRoot(container);
  mountedRoots.set(container, root);
  root.render(<AssistantEntry onActivate={onActivate} />);
}
