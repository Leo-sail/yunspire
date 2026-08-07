import React from 'react';
import { ActionButton } from './ActionButton.jsx';

const meta = {
  title: 'Actions/ActionButton',
  component: ActionButton,
  args: { children: '继续阅读', variant: 'primary' },
};

export default meta;

export const Default = {};

export const Secondary = {
  args: { variant: 'secondary', children: '查看批注' },
};

export const Loading = {
  args: { loading: true },
};

export const Disabled = {
  args: { disabled: true },
};

export const FocusVisible = {
  decorators: [(Story) => <div className="r10-story-force-focus"><Story /></div>],
};

export const Pressed = {
  decorators: [(Story) => <div className="r10-story-force-pressed"><Story /></div>],
};
