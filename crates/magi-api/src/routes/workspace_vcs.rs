//! 工作区 Git 版本控制端点。
//!
//! 覆盖主对话输入框左下角 Git 入口所需能力：
//! 读取当前分支 / 本地分支列表 / 工作区状态，以及切换到已有本地分支。
//! 底层直接调用系统 `git` CLI，与用户终端行为一致；不引入 git2/libgit2。

use axum::{Json, Router, extract::State, routing::post};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::{errors::ApiError, state::ApiState};

/// 校验传入路径属于已注册 workspace，返回可直接喂给 git 的规范化绝对路径。
///
/// 信任边界收口：只有注册过的 workspace 才允许执行 git 操作，避免对任意本地路径
/// 探测分支 / 触发 checkout。与 workspaces.rs 的注册校验复用同一套规范化逻辑。
fn resolve_registered_workspace(state: &ApiState, raw_path: &str) -> Result<String, ApiError> {
    let canonical_path = super::workspaces::canonical_workspace_path(raw_path)?;
    if super::workspaces::registered_workspace_for_path(state, &canonical_path).is_none() {
        return Err(ApiError::InvalidInput(
            "工作区未注册，无法执行 Git 操作".to_string(),
        ));
    }
    Ok(canonical_path.to_string_lossy().to_string())
}

pub fn routes() -> Router<ApiState> {
    Router::new()
        .route("/workspace/vcs/branches", post(list_branches))
        .route("/workspace/vcs/checkout", post(checkout_branch))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BranchesRequest {
    workspace_path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BranchesResponse {
    is_repo: bool,
    current_branch: Option<String>,
    branches: Vec<String>,
    status: Option<WorkspaceVcsStatus>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceVcsStatus {
    upstream: Option<String>,
    ahead: u64,
    behind: u64,
    has_uncommitted: bool,
    staged: u64,
    unstaged: u64,
    untracked: u64,
    conflicted: u64,
    renamed: u64,
    deleted: u64,
    additions: u64,
    deletions: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckoutRequest {
    workspace_path: String,
    branch: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CheckoutResponse {
    ok: bool,
    current_branch: Option<String>,
    error: Option<String>,
}

/// 在指定工作目录运行一条 git 命令，返回 (是否成功, stdout, stderr)。
async fn run_git(workspace_path: &str, args: &[&str]) -> Result<(bool, String, String), ApiError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_path)
        .args(args)
        .output()
        .await
        .map_err(|err| ApiError::InternalAssemblyError(format!("无法执行 git 命令：{err}")))?;
    Ok((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    ))
}

/// 读取当前分支名；detached HEAD 返回 None。
async fn current_branch(workspace_path: &str) -> Option<String> {
    match run_git(workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"]).await {
        Ok((true, stdout, _)) if !stdout.is_empty() && stdout != "HEAD" => Some(stdout),
        _ => None,
    }
}

async fn current_upstream(workspace_path: &str) -> Option<String> {
    match run_git(
        workspace_path,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .await
    {
        Ok((true, stdout, _)) if !stdout.is_empty() => Some(stdout),
        _ => None,
    }
}

async fn ahead_behind(workspace_path: &str) -> (u64, u64) {
    let Ok((true, stdout, _)) = run_git(
        workspace_path,
        &["rev-list", "--left-right", "--count", "@{u}...HEAD"],
    )
    .await
    else {
        return (0, 0);
    };
    let mut parts = stdout.split_whitespace();
    let behind = parts
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let ahead = parts
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    (ahead, behind)
}

/// 统计未提交改动的新增/删除行数（git diff HEAD --numstat，含已暂存+未暂存）。
/// 二进制文件 numstat 以 `-` 占位，按 0 行处理。
async fn uncommitted_diff_stat(workspace_path: &str) -> (u64, u64) {
    let Ok((true, stdout, _)) = run_git(workspace_path, &["diff", "HEAD", "--numstat"]).await
    else {
        return (0, 0);
    };
    let mut additions = 0u64;
    let mut deletions = 0u64;
    for line in stdout.lines() {
        let mut cols = line.split('\t');
        let add = cols.next().and_then(|v| v.trim().parse::<u64>().ok());
        let del = cols.next().and_then(|v| v.trim().parse::<u64>().ok());
        additions += add.unwrap_or(0);
        deletions += del.unwrap_or(0);
    }
    (additions, deletions)
}

fn is_conflicted_status(index_status: char, worktree_status: char) -> bool {
    matches!(
        (index_status, worktree_status),
        ('D', 'D') | ('A', 'U') | ('U', 'D') | ('U', 'A') | ('D', 'U') | ('A', 'A') | ('U', 'U')
    )
}

fn apply_porcelain_status_line(status: &mut WorkspaceVcsStatus, line: &str) {
    let mut chars = line.chars();
    let index_status = chars.next().unwrap_or(' ');
    let worktree_status = chars.next().unwrap_or(' ');
    if matches!((index_status, worktree_status), ('?', '?') | ('!', '!')) {
        if (index_status, worktree_status) == ('?', '?') {
            status.untracked += 1;
        }
        return;
    }
    if index_status != ' ' {
        status.staged += 1;
    }
    if worktree_status != ' ' {
        status.unstaged += 1;
    }
    if is_conflicted_status(index_status, worktree_status) {
        status.conflicted += 1;
    }
    if index_status == 'R' || worktree_status == 'R' {
        status.renamed += 1;
    }
    if index_status == 'D' || worktree_status == 'D' {
        status.deleted += 1;
    }
}

async fn workspace_vcs_status(workspace_path: &str) -> WorkspaceVcsStatus {
    let mut status = WorkspaceVcsStatus {
        upstream: current_upstream(workspace_path).await,
        ..WorkspaceVcsStatus::default()
    };
    let (ahead, behind) = ahead_behind(workspace_path).await;
    status.ahead = ahead;
    status.behind = behind;
    if let Ok((true, stdout, _)) = run_git(workspace_path, &["status", "--porcelain"]).await {
        for line in stdout.lines() {
            if line.len() >= 2 {
                apply_porcelain_status_line(&mut status, line);
            }
        }
    }
    let (additions, deletions) = uncommitted_diff_stat(workspace_path).await;
    status.additions = additions;
    status.deletions = deletions;
    status.has_uncommitted =
        status.staged > 0 || status.unstaged > 0 || status.untracked > 0 || status.conflicted > 0;
    status
}

async fn list_branches(
    State(state): State<ApiState>,
    Json(request): Json<BranchesRequest>,
) -> Result<Json<BranchesResponse>, ApiError> {
    let workspace_path = resolve_registered_workspace(&state, &request.workspace_path)?;
    let workspace_path = workspace_path.as_str();

    // 非 git 仓库：返回 isRepo:false，前端据此隐藏分支入口（不视为错误）。
    let inside_work_tree = run_git(workspace_path, &["rev-parse", "--is-inside-work-tree"]).await?;
    if !inside_work_tree.0 || inside_work_tree.1 != "true" {
        return Ok(Json(BranchesResponse {
            is_repo: false,
            current_branch: None,
            branches: Vec::new(),
            status: None,
        }));
    }

    let (ok, stdout, stderr) =
        run_git(workspace_path, &["branch", "--format=%(refname:short)"]).await?;
    if !ok {
        return Err(ApiError::InternalAssemblyError(format!(
            "读取分支列表失败：{stderr}"
        )));
    }
    let branches = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    let current_branch = current_branch(workspace_path).await;
    let status = workspace_vcs_status(workspace_path).await;

    Ok(Json(BranchesResponse {
        is_repo: true,
        current_branch,
        branches,
        status: Some(status),
    }))
}

async fn checkout_branch(
    State(state): State<ApiState>,
    Json(request): Json<CheckoutRequest>,
) -> Result<Json<CheckoutResponse>, ApiError> {
    let workspace_path = resolve_registered_workspace(&state, &request.workspace_path)?;
    let workspace_path = workspace_path.as_str();
    let branch = request.branch.trim();
    if branch.is_empty() {
        return Err(ApiError::InvalidInput("目标分支不能为空".to_string()));
    }

    // git 自身失败（如未提交改动被拒）不转为 ApiError，而是带 error 原文返回 200，
    // 让前端区分「调用失败」与「git 拒绝」，并把拒绝原因如实展示给用户。
    let (ok, _stdout, stderr) = run_git(workspace_path, &["checkout", branch]).await?;
    if !ok {
        return Ok(Json(CheckoutResponse {
            ok: false,
            current_branch: current_branch(workspace_path).await,
            error: Some(if stderr.is_empty() {
                format!("切换到分支 {branch} 失败")
            } else {
                stderr
            }),
        }));
    }

    Ok(Json(CheckoutResponse {
        ok: true,
        current_branch: current_branch(workspace_path).await,
        error: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_status_counts_git_worktree_states() {
        let mut status = WorkspaceVcsStatus::default();
        for line in [
            "M  staged.txt",
            " M unstaged.txt",
            "MM both.txt",
            "?? new.txt",
            "R  old.txt -> new.txt",
            " D removed.txt",
            "UU conflicted.txt",
        ] {
            apply_porcelain_status_line(&mut status, line);
        }

        assert_eq!(status.staged, 4);
        assert_eq!(status.unstaged, 4);
        assert_eq!(status.untracked, 1);
        assert_eq!(status.renamed, 1);
        assert_eq!(status.deleted, 1);
        assert_eq!(status.conflicted, 1);
    }

    #[test]
    fn porcelain_status_ignores_ignored_files() {
        let mut status = WorkspaceVcsStatus::default();
        apply_porcelain_status_line(&mut status, "!! target/debug");

        assert_eq!(status.staged, 0);
        assert_eq!(status.unstaged, 0);
        assert_eq!(status.untracked, 0);
    }
}
