export default {
  source: ['./desktop-ui/tokens/yunspire-r10.tokens.json'],
  platforms: {
    css: {
      transformGroup: 'css',
      buildPath: './desktop-ui/generated/',
      files: [{ destination: 'yunspire-r10.css', format: 'css/variables' }],
    },
    json: {
      transformGroup: 'js',
      buildPath: './desktop-ui/generated/',
      files: [{ destination: 'yunspire-r10.json', format: 'json/nested' }],
    },
  },
};
