export type WorkspaceOnboardingOrigin = 'sidebar' | 'composer';

export const workspaceOnboardingState = $state<{
  open: boolean;
  origin: WorkspaceOnboardingOrigin | null;
}>({
  open: false,
  origin: null,
});

export function openWorkspaceFolderPicker(origin: WorkspaceOnboardingOrigin): void {
  workspaceOnboardingState.origin = origin;
  workspaceOnboardingState.open = true;
}

export function closeWorkspaceFolderPicker(): void {
  workspaceOnboardingState.open = false;
  workspaceOnboardingState.origin = null;
}
