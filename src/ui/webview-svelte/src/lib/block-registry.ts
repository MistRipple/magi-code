import type { Component } from 'svelte';
import type { ContentBlock } from '../types/message';
import TextBlockRenderer from '../components/TextBlockRenderer.svelte';
import CodeBlockRenderer from '../components/CodeBlockRenderer.svelte';
import ThinkingBlockRenderer from '../components/ThinkingBlockRenderer.svelte';
import ToolCallRenderer from '../components/ToolCallRenderer.svelte';
import FileChangeCard from '../components/blocks/FileChangeCard.svelte';
import PlanCard from '../components/blocks/PlanCard.svelte';

// eslint-disable-next-line @typescript-eslint/no-explicit-any
type BlockComponent = Component<any, any, any>;

class BlockRegistry {
  private renderers = new Map<string, BlockComponent>();

  register(type: string, component: BlockComponent): void {
    this.renderers.set(type, component);
  }

  get(type: string): BlockComponent {
    return this.renderers.get(type) || TextBlockRenderer;
  }
}

export const blockRegistry = new BlockRegistry();
blockRegistry.register('text', TextBlockRenderer);
blockRegistry.register('code', CodeBlockRenderer);
blockRegistry.register('thinking', ThinkingBlockRenderer);
blockRegistry.register('tool_call', ToolCallRenderer);
blockRegistry.register('file_change', FileChangeCard);
blockRegistry.register('plan', PlanCard);

export function getBlockRenderer(block: ContentBlock): BlockComponent {
  return blockRegistry.get(block.type);
}
