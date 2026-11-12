//! 主对话与代理共享的结构化 Git 服务。
//!
//! 本 crate 只描述 Git branch / repository / worktree，不承载对话历史分叉或任务执行树。
//! 所有会改变 refs、HEAD、index 或 worktree 的操作都在 repository mutex 内重新观测状态，
//! 并校验调用方携带的预期 branch/HEAD/worktree，防止外部终端和其他会话造成竞态。

use magi_process::tokio_command;
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};
use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

mod git_types;
pub use git_types::{
    AgentWorktreeContext, AgentWorktreeMode, GitBranch, GitBranchList, GitDirtySummary,
    GitMergePreview, GitObservation, GitPrecondition, GitRefObservation, GitTreeBaselineEntry,
    GitWorktree, SessionCodeContext, SessionCodeContextRegistry, SessionGitContext,
};
mod session_context;
pub use session_context::SessionContextError;
use session_context::*;
mod coordination;
#[cfg(test)]
use coordination::*;
pub use coordination::{GitCoordinationError, GitMutationLease, WorkspaceGitOperationCoordinator};
mod options;
pub use options::{
    BranchCreateOptions, BranchDeleteOptions, BranchSwitchOptions, MergeOptions, PullOptions,
    PushOptions, WorktreeCreateOptions, WorktreeRemoveOptions,
};
mod errors;
pub use errors::GitError;
mod service;
pub use service::GitService;
use service::*;
mod observation;
use observation::*;
mod process;
pub use process::same_path;
use process::*;

#[cfg(test)]
mod tests;
