import { defineConfig } from 'vite';
import path from 'path';

export default defineConfig({
  base: './',
  clearScreen: false,
  server: {
    host: '127.0.0.1',
    port: 1420,
    strictPort: true,
  },
  envPrefix: ['VITE_', 'TAURI_'],
  build: {
    target: ['es2021', 'chrome100', 'safari13'],
    minify: !process.env.TAURI_DEBUG ? 'esbuild' : false,
    sourcemap: !!process.env.TAURI_DEBUG,
    outDir: 'dist',
    emptyOutDir: true,
    rollupOptions: {
      input: {
        desktop: path.resolve(__dirname, 'desktop-ui/index.html'),
      },
    },
  },
});
