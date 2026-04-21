import { defineConfig, loadEnv } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { resolve } from 'path';

const DEFAULT_AGENT_BASE_URL = 'http://127.0.0.1:38123';

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, __dirname, '');
  const agentBaseUrl =
    env.VITE_AGENT_BASE_URL?.trim() || env.VITE_AGENT_PROXY_TARGET?.trim() || DEFAULT_AGENT_BASE_URL;

  return {
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
      outDir: 'dist',
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
        '/events': {
          target: agentBaseUrl,
          changeOrigin: true,
          timeout: 0,
        },
        '/bootstrap': {
          target: agentBaseUrl,
          changeOrigin: true,
        },
        '^/(health|version|bootstrap|bridges|runtime|ledger|recovery|session)(/.*)?$': {
          target: agentBaseUrl,
          changeOrigin: true,
          timeout: 0,
        },
        '/api': {
          target: agentBaseUrl,
          changeOrigin: true,
        },
      },
    },
  };
});
