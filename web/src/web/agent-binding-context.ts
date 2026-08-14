export interface PersonalAgentBindingContext {
  scope: 'personal';
  sessionId?: string;
}

export interface WorkspaceAgentBindingContext {
  scope: 'workspace';
  workspaceId: string;
  workspacePath: string;
  sessionId?: string;
}

export type AgentBindingContext =
  | PersonalAgentBindingContext
  | WorkspaceAgentBindingContext;

export type WorkspaceAgentBindingOverride = {
  scope: 'workspace';
  workspaceId: string;
  workspacePath: string;
  sessionId?: string;
};

export type AgentBindingOverride =
  | { scope?: never; workspaceId?: never; workspacePath?: never; sessionId?: string }
  | { scope: 'personal'; workspaceId?: never; workspacePath?: never; sessionId?: string }
  | WorkspaceAgentBindingOverride;

let runtimeBindingInitialized = false;
let runtimeBindingAuthoritative = false;
let runtimeBinding: AgentBindingContext = {
  scope: 'personal',
};

interface AgentBindingContextOptions {
  authoritative?: boolean;
}

function normalizeBindingValue(value: string | null | undefined): string {
  return typeof value === 'string' ? value.trim() : '';
}

function cloneBinding(binding: AgentBindingContext): AgentBindingContext {
  return binding.scope === 'workspace'
    ? { ...binding }
    : {
        scope: 'personal',
        ...(binding.sessionId ? { sessionId: binding.sessionId } : {}),
      };
}

function readWindowBindingContext(): AgentBindingContext {
  if (typeof window === 'undefined') {
    return { scope: 'personal' };
  }

  const currentUrl = new URL(window.location.href);
  const bootstrapWindow = window as unknown as {
    __INITIAL_WORKSPACE_ID__?: string;
    __INITIAL_WORKSPACE_PATH__?: string;
  };
  const sessionId = normalizeBindingValue(currentUrl.searchParams.get('sessionId'));
  if (currentUrl.searchParams.get('scope') !== 'workspace') {
    return {
      scope: 'personal',
      ...(sessionId ? { sessionId } : {}),
    };
  }

  const workspaceId = normalizeBindingValue(currentUrl.searchParams.get('workspaceId'))
    || normalizeBindingValue(bootstrapWindow.__INITIAL_WORKSPACE_ID__);
  const workspacePath = normalizeBindingValue(currentUrl.searchParams.get('workspacePath'))
    || normalizeBindingValue(bootstrapWindow.__INITIAL_WORKSPACE_PATH__);
  if (!workspaceId && !workspacePath) {
    throw new Error('workspace 作用域缺少 workspaceId 或 workspacePath');
  }
  return {
    scope: 'workspace',
    workspaceId,
    workspacePath,
    ...(sessionId ? { sessionId } : {}),
  };
}

function hasExplicitWorkspaceBinding(binding: AgentBindingContext): boolean {
  return binding.scope === 'workspace' && Boolean(binding.workspaceId || binding.workspacePath);
}

export function resolveAgentBindingContext(): AgentBindingContext {
  if (runtimeBindingInitialized && runtimeBindingAuthoritative) {
    return cloneBinding(runtimeBinding);
  }
  const windowBinding = readWindowBindingContext();
  if (hasExplicitWorkspaceBinding(windowBinding)) {
    runtimeBindingInitialized = true;
    runtimeBindingAuthoritative = false;
    runtimeBinding = windowBinding;
    return cloneBinding(runtimeBinding);
  }
  if (runtimeBindingInitialized) {
    return cloneBinding(runtimeBinding);
  }
  return windowBinding;
}

export function agentBindingWorkspaceId(binding: AgentBindingContext): string {
  return binding.scope === 'workspace' ? binding.workspaceId : '';
}

export function agentBindingWorkspacePath(binding: AgentBindingContext): string {
  return binding.scope === 'workspace' ? binding.workspacePath : '';
}

export function seedAgentBindingContextFromWindow(): AgentBindingContext {
  return setAgentBindingContext(readWindowBindingContext());
}

export function setAgentBindingContext(
  binding: AgentBindingContext,
  options: AgentBindingContextOptions = {},
): AgentBindingContext {
  const normalizedBinding: AgentBindingContext = binding.scope === 'workspace'
    ? (() => {
        const workspaceId = normalizeBindingValue(binding.workspaceId);
        const workspacePath = normalizeBindingValue(binding.workspacePath);
        if (!workspaceId && !workspacePath) {
          throw new Error('workspace 作用域缺少 workspaceId 或 workspacePath');
        }
        return {
        scope: 'workspace',
        workspaceId,
        workspacePath,
        ...(normalizeBindingValue(binding.sessionId)
          ? { sessionId: normalizeBindingValue(binding.sessionId) }
          : {}),
        };
      })()
    : {
        scope: 'personal',
        ...(normalizeBindingValue(binding.sessionId)
          ? { sessionId: normalizeBindingValue(binding.sessionId) }
          : {}),
      };
  runtimeBindingInitialized = true;
  runtimeBindingAuthoritative = options.authoritative === true;
  runtimeBinding = normalizedBinding;
  return cloneBinding(runtimeBinding);
}

export function clearAgentBindingContext(options: AgentBindingContextOptions = {}): AgentBindingContext {
  return setAgentBindingContext({ scope: 'personal' }, options);
}
