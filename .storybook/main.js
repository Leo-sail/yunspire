/** @type { import('@storybook/react-vite').StorybookConfig } */
const config = {
  stories: ['../desktop-ui/design-system/**/*.stories.@(js|jsx)'],
  addons: [],
  framework: {
    name: '@storybook/react-vite',
    options: {},
  },
  staticDirs: [{ from: '../desktop-ui/assets', to: '/assets' }],
};

export default config;
