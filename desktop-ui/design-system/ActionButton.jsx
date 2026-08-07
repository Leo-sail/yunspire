import React from 'react';
import { ArrowRight, LoaderCircle } from 'lucide-react';

const noop = () => {};

export function ActionButton({ children = '继续阅读', variant = 'primary', loading = false, disabled = false, onClick = noop }) {
  return (
    <button
      className={`r10-button r10-button-${variant}`}
      type="button"
      aria-busy={loading || undefined}
      disabled={disabled || loading}
      onClick={onClick}
    >
      {loading ? <LoaderCircle className="r10-button-spinner" aria-hidden="true" /> : <ArrowRight aria-hidden="true" />}
      <span>{loading ? '正在打开' : children}</span>
    </button>
  );
}
