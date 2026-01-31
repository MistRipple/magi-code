/**
 * UI BlockRegistry 单元测试
 */

import { getBlockRenderer } from '../../ui/webview-svelte/src/lib/block-registry';
import TextBlockRenderer from '../../ui/webview-svelte/src/components/TextBlockRenderer.svelte';
import PlanCard from '../../ui/webview-svelte/src/components/blocks/PlanCard.svelte';
import FileChangeCard from '../../ui/webview-svelte/src/components/blocks/FileChangeCard.svelte';
import type { ContentBlock } from '../../ui/webview-svelte/src/types/message';

declare const describe: (name: string, fn: () => void) => void;
declare const test: (name: string, fn: () => void | Promise<void>) => void;
declare const expect: any;

describe('BlockRegistry', () => {
  test('未知 block 类型应降级为 TextBlockRenderer', () => {
    const block = { type: 'unknown', content: 'fallback' } as unknown as ContentBlock;
    expect(getBlockRenderer(block)).toBe(TextBlockRenderer);
  });

  test('plan block 应返回 PlanCard', () => {
    const block = {
      type: 'plan',
      content: '',
      plan: { goal: 'goal' },
    } as ContentBlock;
    expect(getBlockRenderer(block)).toBe(PlanCard);
  });

  test('file_change block 应返回 FileChangeCard', () => {
    const block = {
      type: 'file_change',
      content: '',
      fileChange: { filePath: 'src/a.ts', changeType: 'modify' },
    } as ContentBlock;
    expect(getBlockRenderer(block)).toBe(FileChangeCard);
  });
});
