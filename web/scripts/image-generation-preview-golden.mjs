import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const preview = await server.ssrLoadModule('/src/lib/image-generation-preview.ts');

  const parsed = preview.parseImageGenerationPreview('image_generate', JSON.stringify({
    tool: 'image_generate',
    status: 'succeeded',
    path: 'generated-images/blue-square.png',
    media_type: 'image/png',
    bytes: 8,
    revised_prompt: '一个蓝色方块',
  }));

  assert.deepEqual(parsed, {
    path: 'generated-images/blue-square.png',
    mime: 'image/png',
    bytes: 8,
    revisedPrompt: '一个蓝色方块',
  });
  assert.deepEqual(
    preview.parseImageGenerationPreview('mcp__images__image_generate', JSON.stringify({
      status: 'succeeded',
      path: 'generated-images/namespaced.png',
      media_type: 'image/png',
      bytes: 8,
    })),
    {
      path: 'generated-images/namespaced.png',
      mime: 'image/png',
      bytes: 8,
      revisedPrompt: '',
    },
    'namespaced image_generate results should use the same inline preview path',
  );
  assert.equal(
    preview.parseImageGenerationPreview('view_image', '{}'),
    null,
    'only image_generate results should use generated image preview parsing',
  );
  assert.equal(
    preview.formatImageGenerationToolOutput('image_generate', JSON.stringify({
      tool: 'image_generate',
      status: 'succeeded',
      path: 'generated-images/blue-square.png',
      media_type: 'image/png',
      bytes: 8,
    })).includes('generated-images/blue-square.png'),
    true,
  );
  assert.equal(
    preview.imageGenerationAspectRatio({ size: '1536x1024' }),
    '1536 / 1024',
    '生成中占位区域应遵循请求尺寸比例',
  );
  assert.equal(
    preview.imageGenerationAspectRatio({ size: 'auto' }),
    '1 / 1',
    '无法解析尺寸时使用稳定的正方形占位',
  );

  console.log('image generation preview golden passed');
}, { configFile: 'vite.web.config.ts' });

const generatedImageSource = await readFile(
  new URL('../src/components/GeneratedImageBlock.svelte', import.meta.url),
  'utf8',
);

assert.match(
  generatedImageSource,
  /status === 'pending' \|\| status === 'running'[\s\S]*?class="generated-image-progress"/,
  'image_generate 运行时必须在对话区展示独立生成动画',
);
assert.match(
  generatedImageSource,
  /preview && imageSrc[\s\S]*?class="generated-image-block"/,
  'image_generate 成功后必须直接展示图片，不再包裹通用工具卡片',
);
assert.match(
  generatedImageSource,
  /@keyframes generated-image-shimmer/,
  '图片生成占位必须提供可感知的持续响应动画',
);
assert.match(
  generatedImageSource,
  /@media \(prefers-reduced-motion: reduce\)[\s\S]*?animation: generated-image-reduced-pulse/,
  '减少动态效果时仍需保留非位移式状态脉冲，避免生成过程看起来静止',
);
assert.match(
  generatedImageSource,
  /class="generated-image-progress-dots"[\s\S]*?<span><\/span>[\s\S]*?<span><\/span>[\s\S]*?<span><\/span>/,
  '正在生成图片提示应包含固定宽度的三点动态反馈',
);
assert.match(
  generatedImageSource,
  /@keyframes generated-image-dot/,
  '正在生成图片提示的三个点必须持续变化',
);

const toolCallRendererSource = await readFile(
  new URL('../src/components/ToolCallRenderer.svelte', import.meta.url),
  'utf8',
);
assert.match(
  toolCallRendererSource,
  /isGeneratedImageTool && \(hasGeneratedImagePreview \|\| toolStatus === 'pending' \|\| toolStatus === 'running'\)[\s\S]*?<GeneratedImageBlock/,
  '图片生成运行态和成功态必须绕过通用工具卡片',
);
