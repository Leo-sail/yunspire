import React from 'react';
import { AssistantDock } from './AssistantDock.jsx';

const meta = {
  title: 'Navigation/AssistantDock',
  component: AssistantDock,
  parameters: { layout: 'fullscreen' },
  decorators: [(Story) => (
    <div className="r10-story-assistant-stage">
      <Story />
    </div>
  )],
};

export default meta;

export const OverviewContext = {
  args: {
    contextLabel: '工作台 · 最近积累',
    status: '贴近当前积累，按需出现',
  },
};

export const CreationContext = {
  args: {
    contextLabel: '创作 · 当前草稿',
    status: '贴近当前草稿，按需出现',
    intro: '我可以结合本地知识帮你搭起一段结构、补充引用，或对正文提出可审阅的改写。',
  },
};

export const Closed = {
  args: { open: false },
};
