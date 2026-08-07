import React from 'react';
import { AssistantEntry } from './AssistantEntry.jsx';

const meta = {
  title: 'Actions/AssistantEntry',
  component: AssistantEntry,
  args: { label: '问云枢' },
};

export default meta;

export const Default = {};

export const TooltipOpen = {
  args: { defaultOpen: true },
};

export const Disabled = {
  args: { disabled: true },
};
