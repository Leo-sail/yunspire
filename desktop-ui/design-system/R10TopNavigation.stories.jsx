import React from 'react';
import { R10TopNavigation } from './R10TopNavigation.jsx';

const meta = {
  title: 'Navigation/TopNavigation',
  component: R10TopNavigation,
  parameters: { layout: 'fullscreen' },
  args: {
    activeRoute: 'dashboard',
    vaultName: '我的知识库',
  },
};

export default meta;

export const Overview = {};

export const KnowledgeActive = {
  args: { activeRoute: 'search' },
};

export const LongVaultName = {
  args: { vaultName: '个人长期知识积累与研究资料库' },
};

export const DisabledDestination = {
  args: { disabledRoutes: ['reports'] },
};
