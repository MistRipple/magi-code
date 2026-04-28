import { defineConfig, loadEnv } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { resolve } from 'path';

const DEFAULT_AGENT_BASE_URL = 'http://127.0.0.1:38123';
const DEFAULT_DEV_HOST = '127.0.0.1';
const DEFAULT_DEV_PORT = 3000;
const MAGI_VITE_READY_PATH = '/__magi_vite_ready';

function magiDaemonDevGuardPlugin(agentBaseUrl: string, webRoot: string) {
  return {
    name: 'magi-daemon-dev-guard',
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const pathname = req.url?.split('?', 1)[0] || '';
        if (pathname === MAGI_VITE_READY_PATH) {
          res.statusCode = 200;
          res.setHeader('content-type', 'application/json; charset=utf-8');
          res.end(JSON.stringify({
            app: 'magi-web',
            entry: '/src/main-web.ts',
            agentOrigin: agentBaseUrl,
            webRoot,
          }));
          return;
        }
        if (pathname === '/' || pathname === '/web.html') {
          res.statusCode = 409;
          res.setHeader('content-type', 'text/html; charset=utf-8');
          res.end(`<!doctype html>
<html lang="zh-CN">
  <head><meta charset="utf-8" /><title>Magi Web 开发入口</title></head>
  <body style="font-family: system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; padding: 24px; line-height: 1.6;">
    <h1>请从 daemon 入口访问 Magi Web</h1>
    <p>开发模式请启动 <code>MAGI_WEB_DEV=1 cargo run -p magi-daemon-app</code>，然后访问 <code>${agentBaseUrl}/web.html</code>。</p>
  </body>
</html>`);
          return;
        }
        next();
      });
    },
  };
}

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, __dirname, '');
  const agentBaseUrl =
    env.VITE_AGENT_BASE_URL?.trim() || env.VITE_AGENT_PROXY_TARGET?.trim() || DEFAULT_AGENT_BASE_URL;
  const devHost = env.MAGI_VITE_HOST?.trim() || DEFAULT_DEV_HOST;
  const devPort = Number(env.MAGI_VITE_PORT || DEFAULT_DEV_PORT);
  const webRoot = resolve(__dirname);

  return {
    base: './',
    plugins: [
      magiDaemonDevGuardPlugin(agentBaseUrl, webRoot),
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
          entryFileNames: 'assets/[name]-[hash].js',
          chunkFileNames: 'assets/[name]-[hash].js',
          assetFileNames: 'assets/[name]-[hash].[ext]',
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
      host: devHost,
      port: Number.isFinite(devPort) ? devPort : DEFAULT_DEV_PORT,
      strictPort: true,
      open: env.MAGI_VITE_OPEN === '1' ? '/web.html' : false,
      cors: true,
      hmr: {
        host: devHost,
        port: Number.isFinite(devPort) ? devPort : DEFAULT_DEV_PORT,
      },
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
