//! 工作区 Git 版本控制端点。
//!
//! 仅覆盖主对话输入框左下角分支切换器所需的最小能力：
//! 读取当前分支 / 本地分支列表，以及切换到已有本地分支。
//! 底层直接调用系统 `git` CLI，与用户终端行为一致；不引入 git2/libgit2。

use axum::{Json, Router, extract::State, routing::post};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::{errors::ApiError, state::ApiState};

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
    /// 未提交改动的新增行数（git diff HEAD，含已暂存+未暂存）。
    additions: u64,
    /// 未提交改动的删除行数。
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

/// 统计未提交改动的新增/删除行数（git diff HEAD --numstat，含已暂存+未暂存）。
/// 二进制文件 numstat 以 `-` 占位，按 0 行处理。失败时返回 (0, 0)，不阻断分支查询。
async fn uncommitted_diff_stat(workspace_path: &str) -> (u64, u64) {
    let Ok((true, stdout, _)) = run_git(workspace_path, &["diff", "HEAD", "--numstat"]).await else {
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

async fn list_branches(
    State(_state): State<ApiState>,
    Json(request): Json<BranchesRequest>,
) -> Result<Json<BranchesResponse>, ApiError> {
    let workspace_path = request.workspace_path.trim();
    if workspace_path.is_empty() {
        return Err(ApiError::InvalidInput("工作区路径不能为空".to_string()));
    }

    // 非 git 仓库：返回 isRepo:false，前端据此隐藏分支入口（不视为错误）。
    let inside_work_tree = run_git(workspace_path, &["rev-parse", "--is-inside-work-tree"]).await?;
    if !inside_work_tree.0 || inside_work_tree.1 != "true" {
        return Ok(Json(BranchesResponse {
            is_repo: false,
            current_branch: None,
            branches: Vec::new(),
            additions: 0,
            deletions: 0,
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

    let (additions, deletions) = uncommitted_diff_stat(workspace_path).await;

    Ok(Json(BranchesResponse {
        is_repo: true,
        current_branch: current_branch(workspace_path).await,
        branches,
        additions,
        deletions,
    }))
}

async fn checkout_branch(
    State(_state): State<ApiState>,
    Json(request): Json<CheckoutRequest>,
) -> Result<Json<CheckoutResponse>, ApiError> {
    let workspace_path = request.workspace_path.trim();
    let branch = request.branch.trim();
    if workspace_path.is_empty() {
        return Err(ApiError::InvalidInput("工作区路径不能为空".to_string()));
    }
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
