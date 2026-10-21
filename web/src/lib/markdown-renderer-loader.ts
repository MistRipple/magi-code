import type { Component } from 'svelte';

export type MarkdownRendererProps = {
  source: string;
  isStreaming?: boolean;
};

type MarkdownRendererModule = {
  default: Component<MarkdownRendererProps>;
};

let markdownRendererPromise: Promise<MarkdownRendererModule> | null = null;

/**
 * Markdown rendering pulls in the parser, KaTeX and their styles. Keep that
 * payload out of the application entry and share one request across messages.
 */
export function loadMarkdownRenderer(): Promise<MarkdownRendererModule> {
  markdownRendererPromise ??= import('../components/MarkdownRenderer.svelte');
  return markdownRendererPromise;
}

export function preloadMarkdownRenderer(): void {
  void loadMarkdownRenderer();
}
