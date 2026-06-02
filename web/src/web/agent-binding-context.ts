export interface AgentBindingContext {
  workspaceId: string;
  workspacePath: string;
  sessionId: string;
}

let runtimeBindingInitialized = false;
let runtimeBinding: AgentBindingContext = {
  workspaceId: '',
  workspacePath: '',
  sessionId: '',
};

function normalizeBindingValue(value: string | null | undefined): string {
  return typeof value === 'string' ? value.trim() : '';
}

function readWindowBindingContext(): AgentBindingContext {
  if (typeof window === 'undefined') {
    return { workspaceId: '', workspacePath: '', sessionId: '' };
  }

  const currentUrl = new URL(window.location.href);
  const bootstrapWindow = window as unknown as {
    __INITIAL_WORKSPACE_ID__?: string;
    __INITIAL_WORKSPACE_PATH__?: string;
  };
  const queryWorkspaceId = normalizeBindingValue(currentUrl.searchParams.get('workspaceId'));
  const queryWorkspacePath = normalizeBindingValue(currentUrl.searchParams.get('workspacePath'));
  const querySessionId = normalizeBindingValue(currentUrl.searchParams.get('sessionId'));

  return {
    workspaceId: queryWorkspaceId
      || normalizeBindingValue(bootstrapWindow.__INITIAL_WORKSPACE_ID__)
      || '',
    workspacePath: queryWorkspacePath
      || normalizeBindingValue(bootstrapWindow.__INITIAL_WORKSPACE_PATH__)
      || '',
    sessionId: querySessionId,
  };
}

function hasExplicitWorkspaceBinding(binding: AgentBindingContext): boolean {
  return Boolean(binding.workspaceId || binding.workspacePath);
}

export function resolveAgentBindingContext(): AgentBindingContext {
  const windowBinding = readWindowBindingContext();
  if (hasExplicitWorkspaceBinding(windowBinding)) {
    runtimeBindingInitialized = true;
    runtimeBinding = windowBinding;
    return { ...runtimeBinding };
  }
  if (runtimeBindingInitialized) {
    return { ...runtimeBinding };
  }
  return windowBinding;
}

export function seedAgentBindingContextFromWindow(): AgentBindingContext {
  return setAgentBindingContext(readWindowBindingContext());
}

export function setAgentBindingContext(binding: Partial<AgentBindingContext>): AgentBindingContext {
  runtimeBindingInitialized = true;
  runtimeBinding = {
    workspaceId: normalizeBindingValue(binding.workspaceId),
    workspacePath: normalizeBindingValue(binding.workspacePath),
    sessionId: normalizeBindingValue(binding.sessionId),
  };
  return { ...runtimeBinding };
}

export function clearAgentBindingContext(): AgentBindingContext {
  return setAgentBindingContext({
    workspaceId: '',
    workspacePath: '',
    sessionId: '',
  });
}
