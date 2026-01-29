import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

export default {
  // 启用 TypeScript 和 SCSS 等预处理
  preprocess: vitePreprocess(),
  
  compilerOptions: {
    // 启用 Svelte 5 runes 模式
    runes: true,
  },
};

