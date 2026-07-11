export const PANEL_LAYOUT = {
  mobileBreakpoint: 900,
  shellPadding: 8,
  shellGap: 8,
  previewHandleWidth: 8,
  minContentWidth: 620,
  minPreviewWidth: 320,
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
