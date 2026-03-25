import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { resolve } from 'path';

export default defineConfig({
  base: './',
  plugins: [
    svelte({
      compilerOptions: {
        runes: true,
      },
    }),
  ],
  resolve: {
    alias: {
      $lib: resolve(__dirname, './src/lib'),
      $components: resolve(__dirname, './src/components'),
      $stores: resolve(__dirname, './src/stores'),
    },
  },
  build: {
    outDir: '../../../dist/web',
    emptyOutDir: true,
    chunkSizeWarningLimit: 1500,
    rollupOptions: {
      input: {
        web: resolve(__dirname, 'web.html'),
      },
      output: {
        entryFileNames: 'assets/[name].js',
        chunkFileNames: 'assets/[name].js',
        assetFileNames: 'assets/[name].[ext]',
        manualChunks: {
          mermaid: ['mermaid'],
          highlight: ['highlight.js'],
          markdown: ['marked'],
          cytoscape: ['cytoscape'],
        },
      },
    },
    sourcemap: 'inline',
    minify: false,
  },
  server: {
    port: 3000,
    open: '/web.html',
    proxy: {
      '/api/events': {
        target: 'http://127.0.0.1:46231',
        changeOrigin: true,
        // SSE 长连接：禁止 proxy 超时，避免因心跳间隔（15s）被 proxy 中断
        timeout: 0,
      },
      '/api': {
        target: 'http://127.0.0.1:46231',
        changeOrigin: true,
      },
    },
  },
});
