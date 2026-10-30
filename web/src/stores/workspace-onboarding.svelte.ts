export type WorkspaceOnboardingOrigin = 'sidebar' | 'composer' | 'project';

export const workspaceOnboardingState = $state<{
  open: boolean;
  origin: WorkspaceOnboardingOrigin | null;
  projectCreationRequested: boolean;
  projectCreationTrigger: HTMLElement | null;
}>({
  open: false,
  origin: null,
  projectCreationRequested: false,
  projectCreationTrigger: null,
});

export function requestProjectCreation(trigger: HTMLElement | null): void {
  workspaceOnboardingState.projectCreationTrigger = trigger;
  workspaceOnboardingState.projectCreationRequested = true;
}

export function openWorkspaceFolderPicker(origin: WorkspaceOnboardingOrigin): void {
  workspaceOnboardingState.origin = origin;
  workspaceOnboardingState.open = true;
}

export function closeWorkspaceFolderPicker(): void {
  workspaceOnboardingState.open = false;
  workspaceOnboardingState.origin = null;
}
