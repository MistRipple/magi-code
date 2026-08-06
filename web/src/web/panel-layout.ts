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
}

export interface PreviewPanelWidthBounds {
  minWidth: number;
  maxWidth: number;
}

export function resolvePanelLayout(input: PanelLayoutInput): PanelLayoutResolution {
  const viewportWidth = Math.max(0, input.viewportWidth);
  const sidebarWidth = Math.max(0, input.sidebarWidth);
  const previewPanelWidth = Math.max(PANEL_LAYOUT.minPreviewWidth, input.previewPanelWidth);
  const sidebarDrawer = viewportWidth <= PANEL_LAYOUT.mobileBreakpoint;
  const contentFrameWidth = Math.max(
    0,
    viewportWidth - PANEL_LAYOUT.shellPadding * 2,
  );
  const previewSplitWidth =
    PANEL_LAYOUT.minContentWidth
    + PANEL_LAYOUT.previewHandleWidth
    + previewPanelWidth;
  const previewOverlay = sidebarDrawer || contentFrameWidth < previewSplitWidth;
  const sideBySideWidth =
    sidebarWidth
    + PANEL_LAYOUT.shellGap
    + previewSplitWidth;
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
  const shellWidth = Math.max(0, viewportWidth - PANEL_LAYOUT.shellPadding * 2);
  const sidebarTakenWidth = input.sidebarVisible
    ? sidebarWidth + PANEL_LAYOUT.shellGap
    : 0;
  const currentWorkbenchWidth = Math.max(0, shellWidth - sidebarTakenWidth);

  // 展开右栏时，左栏可以按 panelsCanCoexist 的结果自动让出空间。
  // 这里预留完整工作区，避免拖拽逻辑继续扣除已经隐藏的左栏宽度。
  const focusWorkbenchWidth = input.rightPaneOpen && !input.previewOverlay
    ? shellWidth
    : currentWorkbenchWidth;
  const maxByConversation = Math.max(
    PANEL_LAYOUT.minPreviewWidth,
    focusWorkbenchWidth
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
  panelsCanCoexist: boolean;
  sidebarPreferredOpen: boolean;
  sidebarDrawerOpen: boolean;
  rightPaneOpen: boolean;
}

export function resolvePanelVisibility(
  input: PanelVisibilityInput,
): { sidebarVisible: boolean; rightPaneVisible: boolean } {
  const sidebarVisible = input.sidebarDrawer
    ? input.sidebarDrawerOpen && !input.rightPaneOpen
    : input.sidebarPreferredOpen && (
      input.panelsCanCoexist || !input.rightPaneOpen
    );

  return {
    sidebarVisible,
    rightPaneVisible: input.rightPaneOpen,
  };
}
