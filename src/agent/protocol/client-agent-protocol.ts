import type { StandardMessage, StreamUpdate } from '../../protocol/message-protocol';

export type MagiClientKind = 'vscode' | 'web' | 'idea';

export interface AgentClientIdentity {
  clientId: string;
  clientKind: MagiClientKind;
  clientVersion: string;
  platform: 'macos' | 'windows';
}

export interface AgentWorkspaceBinding {
  workspaceId: string;
  rootPath: string;
}

export interface AgentSessionBinding {
  sessionId: string;
}

export interface AgentHandshakeRequest {
  type: 'agent.handshake.request';
  identity: AgentClientIdentity;
}

export interface AgentHandshakeResponse {
  type: 'agent.handshake.response';
  agentVersion: string;
  capabilities: {
    workspaces: true;
    sessions: true;
    runtimeRelay: true;
    shell: boolean;
    git: boolean;
    worktree: boolean;
    lsp: boolean;
    diagnostics: boolean;
  };
}

export interface AgentBindWorkspaceRequest {
  type: 'agent.workspace.bind';
  workspace: AgentWorkspaceBinding;
}

export interface AgentBindWorkspaceResponse {
  type: 'agent.workspace.bound';
  workspace: AgentWorkspaceBinding;
}

export interface AgentAttachSessionRequest {
  type: 'agent.session.attach';
  session: AgentSessionBinding;
}

export interface AgentAttachSessionResponse {
  type: 'agent.session.attached';
  session: AgentSessionBinding;
}

export interface AgentRuntimeEvent {
  type: 'agent.runtime.event';
  workspaceId: string;
  sessionId: string;
  payload: StandardMessage | StreamUpdate | Record<string, unknown>;
}

export interface AgentDisconnectEvent {
  type: 'agent.disconnect';
  reason: 'client_closed' | 'agent_restarting' | 'agent_stopped' | 'connection_lost';
}

export interface AgentErrorEvent {
  type: 'agent.error';
  code: 'workspace_not_found' | 'session_not_found' | 'permission_denied' | 'unsupported_capability';
  message: string;
}

export type ClientToAgentMessage =
  | AgentHandshakeRequest
  | AgentBindWorkspaceRequest
  | AgentAttachSessionRequest;

export type AgentToClientMessage =
  | AgentHandshakeResponse
  | AgentBindWorkspaceResponse
  | AgentAttachSessionResponse
  | AgentRuntimeEvent
  | AgentDisconnectEvent
  | AgentErrorEvent;
