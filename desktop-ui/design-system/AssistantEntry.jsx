import React from 'react';
import * as Tooltip from '@radix-ui/react-tooltip';
import { Sparkles } from 'lucide-react';

const noop = () => {};

export function AssistantEntry({ label = '问云枢', onActivate = noop, disabled = false, defaultOpen = false }) {
  return (
    <Tooltip.Provider delayDuration={420} skipDelayDuration={120}>
      <Tooltip.Root defaultOpen={defaultOpen}>
        <Tooltip.Trigger asChild>
          <button
            className="r10-assistant-trigger"
            type="button"
            aria-label={label}
            disabled={disabled}
            onClick={onActivate}
          >
            <Sparkles aria-hidden="true" strokeWidth={1.9} />
          </button>
        </Tooltip.Trigger>
        <Tooltip.Portal>
          <Tooltip.Content className="r10-radix-tooltip" side="bottom" sideOffset={8}>
            {label}
            <Tooltip.Arrow className="r10-radix-tooltip-arrow" />
          </Tooltip.Content>
        </Tooltip.Portal>
      </Tooltip.Root>
    </Tooltip.Provider>
  );
}
