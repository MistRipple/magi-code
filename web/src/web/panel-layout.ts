export const PANEL_LAYOUT = {
  mobileBreakpoint: 900,
  shellPadding: 8,
  shellGap: 8,
  previewHandleWidth: 8,
  // 对话区在紧凑模式下仍需容纳消息列表和响应式输入栏。
  minContentWidth: 448,
  minPreviewWidth: 320,
  previewFocusRatio: 2 / 3,
} as const;

export interface PanelLayoutInput {
  viewportWidth: number;
  sidebarWidth: number;
  previewPanelWidth: number;
  sidebarVisible?: boolean;
  desktopSurface?: boolean;
}

export interface PanelLayoutResolution {
  sidebarDrawer: boolean;
  previewOverlay: boolean;
  panelsCanCoexist: boolean;
}

export interface PreviewPanelWidthBoundsInput {
  viewportWidth: number;
  sidebarWidth: number;
  sidebarVisible: boolean;
  rightPaneOpen: boolean;
  previewOverlay: boolean;
  desktopSurface?: boolean;
}

export interface PreviewPanelWidthBounds {
  minWidth: number;
  maxWidth: number;
}

export function resolvePanelLayout(input: PanelLayoutInput): PanelLayoutResolution {
  const viewportWidth = Math.max(0, input.viewportWidth);
  const sidebarWidth = Math.max(0, input.sidebarWidth);
  const previewPanelWidth = Math.max(PANEL_LAYOUT.minPreviewWidth, input.previewPanelWidth);
  const shellPadding = input.desktopSurface ? 0 : PANEL_LAYOUT.shellPadding;
  const shellGap = input.desktopSurface ? 0 : PANEL_LAYOUT.shellGap;
  const sidebarDrawer = viewportWidth <= PANEL_LAYOUT.mobileBreakpoint;
  const contentFrameWidth = Math.max(
    0,
    viewportWidth - shellPadding * 2,
  );
  const previewSplitWidth =
    PANEL_LAYOUT.minContentWidth
    + PANEL_LAYOUT.previewHandleWidth
    + previewPanelWidth;
  // 右栏位于 sidebar 之后的 workbench-content 内，能否并排必须以该
  // 内容轨道的真实可用宽度判断。此前使用整个窗口宽度，造成 CSS 轨道
  // 实际溢出后再隐藏左栏掩盖问题；右栏变宽不应改变左栏的用户状态。
  const sidebarTakenWidth = input.sidebarVisible === false || sidebarDrawer
    ? 0
    : sidebarWidth + shellGap;
  const workbenchWidth = Math.max(0, contentFrameWidth - sidebarTakenWidth);
  const previewOverlay = sidebarDrawer || workbenchWidth < previewSplitWidth;
  const sideBySideWidth = sidebarTakenWidth + previewSplitWidth;
  const panelsCanCoexist = !previewOverlay && contentFrameWidth >= sideBySideWidth;

  return {
    sidebarDrawer,
    previewOverlay,
    panelsCanCoexist,
  };
}

export function resolvePreviewPanelWidthBounds(
  input: PreviewPanelWidthBoundsInput,
): PreviewPanelWidthBounds {
  const viewportWidth = Math.max(0, input.viewportWidth);
  const sidebarWidth = Math.max(0, input.sidebarWidth);
  const shellPadding = input.desktopSurface ? 0 : PANEL_LAYOUT.shellPadding;
  const shellGap = input.desktopSurface ? 0 : PANEL_LAYOUT.shellGap;
  const shellWidth = Math.max(0, viewportWidth - shellPadding * 2);
  const sidebarTakenWidth = input.sidebarVisible
    ? sidebarWidth + shellGap
    : 0;
  const currentWorkbenchWidth = Math.max(0, shellWidth - sidebarTakenWidth);

  const maxByConversation = Math.max(
    PANEL_LAYOUT.minPreviewWidth,
    currentWorkbenchWidth
      - PANEL_LAYOUT.previewHandleWidth
      - PANEL_LAYOUT.minContentWidth,
  );
  const maxByViewportRatio = Math.floor(
    viewportWidth * PANEL_LAYOUT.previewFocusRatio,
  );

  return {
    minWidth: PANEL_LAYOUT.minPreviewWidth,
    maxWidth: Math.max(
      PANEL_LAYOUT.minPreviewWidth,
      Math.min(maxByConversation, maxByViewportRatio || PANEL_LAYOUT.minPreviewWidth),
    ),
  };
}

export interface PanelVisibilityInput {
  sidebarDrawer: boolean;
  sidebarPreferredOpen: boolean;
  sidebarDrawerOpen: boolean;
  rightPaneOpen: boolean;
}

export function resolvePanelVisibility(
  input: PanelVisibilityInput,
): { sidebarVisible: boolean; rightPaneVisible: boolean } {
  const sidebarVisible = input.sidebarDrawer
    ? input.sidebarDrawerOpen && !input.rightPaneOpen
    : input.sidebarPreferredOpen;

  return {
    sidebarVisible,
    rightPaneVisible: input.rightPaneOpen,
  };
}
