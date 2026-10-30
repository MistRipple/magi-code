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

  const { render } = await server.ssrLoadModule('svelte/server');
  const { default: GeneratedImageBlock } = await server.ssrLoadModule('/src/components/GeneratedImageBlock.svelte');
  for (const sourceFolderId of ['source-a', 'source-b']) {
    const html = render(GeneratedImageBlock, { props: {
      block: { type: 'tool_call', toolCall: {
        name: 'image_generate', status: 'success',
        result: { status: 'succeeded', path: 'generated-images/same.png', media_type: 'image/png' },
      } },
      filePreviewScope: { workspaceId: 'project', workspacePath: '/current-primary', sessionId: 'original-session', sourceFolderId },
    } }).body;
    const url = new URL(html.match(/<img[^>]*src="([^"]+)"/u)?.[1].replaceAll('&amp;', '&'));
    assert.equal(url.searchParams.get('sourceFolderId'), sourceFolderId, '同名生成图片必须保留各自源目录');
    assert.equal(url.searchParams.get('sessionId'), 'original-session', '图片预览必须保留原会话副本');
  }

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
  /preview && imageSrc[\s\S]*?class="message-card generated-image-block"/,
  'image_generate 成功后必须直接展示图片，不再包裹通用工具卡片',
);
assert.match(generatedImageSource, /<ToolCardMeta \{status\}/, '图片生成使用统一状态圆点与右侧列');
assert.match(generatedImageSource, /@keyframes generated-image-pulse/, '生成占位保留轻量动态反馈');
assert.doesNotMatch(generatedImageSource, /prefers-reduced-motion:reduce[\s\S]*?animation:none/, '图片生成等待反馈不能被局部规则冻结，统一使用共享低动态效果');
assert.doesNotMatch(generatedImageSource, /linear-gradient|backdrop-filter|generated-image-progress-dots/, '生成状态不再堆叠渐变、玻璃胶囊和多组状态点');

const toolCallRendererSource = await readFile(
  new URL('../src/components/ToolCallRenderer.svelte', import.meta.url),
  'utf8',
);
assert.match(
  toolCallRendererSource,
  /isGeneratedImageTool && \(hasGeneratedImagePreview \|\| toolStatus === 'pending' \|\| toolStatus === 'running'\)[\s\S]*?<GeneratedImageBlock/,
  '图片生成运行态和成功态必须绕过通用工具卡片',
);
