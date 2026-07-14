import assert from 'node:assert/strict';
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

  console.log('image generation preview golden passed');
}, { configFile: 'vite.web.config.ts' });
