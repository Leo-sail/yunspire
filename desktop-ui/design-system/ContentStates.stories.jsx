import React from 'react';
import { ContentState } from './ContentStates.jsx';

const meta = {
  title: 'Feedback/ContentStates',
  component: ContentState,
  parameters: { layout: 'centered' },
  decorators: [(Story) => <div className="r10-story-state-stage"><Story /></div>],
};

export default meta;

export const KnowledgeEmpty = {
  args: { kind: 'knowledge' },
};

export const ReflectionEmpty = {
  args: { kind: 'reflection' },
};

export const Loading = {
  args: { kind: 'loading' },
};

export const Error = {
  args: { kind: 'error' },
};
