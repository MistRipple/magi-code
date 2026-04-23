use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    Vscode,
    Web,
    Idea,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Macos,
    Windows,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentClientIdentity {
    pub client_id: String,
    pub client_kind: ClientKind,
    pub client_version: String,
    pub platform: Platform,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentWorkspaceBinding {
    pub workspace_id: String,
    pub root_path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionBinding {
    pub session_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    pub workspaces: bool,
    pub sessions: bool,
    pub runtime_relay: bool,
    pub shell: bool,
    pub git: bool,
    pub worktree: bool,
    pub lsp: bool,
    pub diagnostics: bool,
}

impl Default for AgentCapabilities {
    fn default() -> Self {
        Self {
            workspaces: true,
            sessions: true,
            runtime_relay: true,
            shell: false,
            git: false,
            worktree: false,
            lsp: false,
            diagnostics: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisconnectReason {
    ClientClosed,
    AgentRestarting,
    AgentStopped,
    ConnectionLost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentErrorCode {
    WorkspaceNotFound,
    SessionNotFound,
    PermissionDenied,
    UnsupportedCapability,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientToAgentMessage {
    #[serde(rename = "agent.handshake.request")]
    HandshakeRequest { identity: AgentClientIdentity },
    #[serde(rename = "agent.workspace.bind")]
    BindWorkspace { workspace: AgentWorkspaceBinding },
    #[serde(rename = "agent.session.attach")]
    AttachSession { session: AgentSessionBinding },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentToClientMessage {
    #[serde(rename = "agent.handshake.response")]
    HandshakeResponse {
        agent_version: String,
        capabilities: AgentCapabilities,
    },
    #[serde(rename = "agent.workspace.bound")]
    WorkspaceBound { workspace: AgentWorkspaceBinding },
    #[serde(rename = "agent.session.attached")]
    SessionAttached { session: AgentSessionBinding },
    #[serde(rename = "agent.runtime.event")]
    RuntimeEvent {
        workspace_id: String,
        session_id: String,
        payload: serde_json::Value,
    },
    #[serde(rename = "agent.disconnect")]
    Disconnect { reason: DisconnectReason },
    #[serde(rename = "agent.error")]
    Error {
        code: AgentErrorCode,
        message: String,
    },
}
