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
    outDir: '../../../dist/webview',
    emptyOutDir: true,
    // 提高 chunk 大小警告阈值（VS Code webview 场景下大文件可接受）
    chunkSizeWarningLimit: 1500,
    rollupOptions: {
      input: {
        main: resolve(__dirname, 'index.html'),
      },
      output: {
        // 单文件输出，方便 VS Code webview 加载
        entryFileNames: 'assets/[name].js',
        chunkFileNames: 'assets/[name].js',
        assetFileNames: 'assets/[name].[ext]',
        // 手动分割大型依赖
        manualChunks: {
          // Mermaid 图表库（约 1.5MB）
          mermaid: ['mermaid'],
          // 代码高亮库
          highlight: ['highlight.js'],
          // Markdown 解析
          markdown: ['marked'],
          // Cytoscape 图形库
          cytoscape: ['cytoscape'],
        },
      },
    },
    // 生成 inline sourcemap 以确保 VS Code webview 可定位 Svelte 组件行号
    sourcemap: 'inline',
    // 使用 esbuild 压缩（更快，不需要额外安装）
    //minify: 'esbuild',
    minify: false,
  },
  // 开发服务器配置（用于独立开发调试）
  server: {
    port: 3000,
    open: true,
  },
});
