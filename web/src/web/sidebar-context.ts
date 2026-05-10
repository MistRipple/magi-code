import { getContext, setContext } from 'svelte';

export interface WebSidebarContext {
  readonly collapsed: boolean;
  readonly hidden: boolean;
  readonly isDrawer: boolean;
  readonly drawerOpen: boolean;
  toggle(): void;
}

const WEB_SIDEBAR_CONTEXT_KEY = Symbol('webSidebar');

export function setWebSidebarContext(value: WebSidebarContext): void {
  setContext(WEB_SIDEBAR_CONTEXT_KEY, value);
}

export function getWebSidebarContext(): WebSidebarContext | undefined {
  return getContext<WebSidebarContext | undefined>(WEB_SIDEBAR_CONTEXT_KEY);
}
