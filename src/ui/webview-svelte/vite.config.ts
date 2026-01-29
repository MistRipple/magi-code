import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { resolve } from 'path';

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [
    svelte({
      compilerOptions: {
        // 启用 Svelte 5 的 runes 模式
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
    // 输出到 VS Code 扩展可以访问的目录
    outDir: '../../../out/webview',
    emptyOutDir: true,
    rollupOptions: {
      input: {
        main: resolve(__dirname, 'index.html'),
      },
      output: {
        // 单文件输出，方便 VS Code webview 加载
        entryFileNames: 'assets/[name].js',
        chunkFileNames: 'assets/[name].js',
        assetFileNames: 'assets/[name].[ext]',
      },
    },
    // 生产环境不生成 sourcemap（VS Code webview CSP 限制）
    sourcemap: false,
    // 使用 esbuild 压缩（更快，不需要额外安装）
    minify: 'esbuild',
  },
  // 开发服务器配置（用于独立开发调试）
  server: {
    port: 3000,
    open: true,
  },
});

