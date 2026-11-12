use super::*;

#[derive(Clone, Debug)]
pub struct BranchCreateOptions {
    pub branch: String,
    pub start_point: Option<String>,
    pub switch: bool,
    pub precondition: GitPrecondition,
}

#[derive(Clone, Debug)]
pub struct BranchSwitchOptions {
    pub branch: String,
    pub precondition: GitPrecondition,
}

#[derive(Clone, Debug)]
pub struct MergeOptions {
    pub target: String,
    pub ff_only: bool,
    pub precondition: GitPrecondition,
}

#[derive(Clone, Debug)]
pub struct PullOptions {
    pub remote: Option<String>,
    pub branch: Option<String>,
    pub ff_only: bool,
    pub precondition: GitPrecondition,
}

#[derive(Clone, Debug)]
pub struct PushOptions {
    pub remote: Option<String>,
    pub branch: Option<String>,
    pub set_upstream: bool,
    pub force_with_lease: bool,
    pub confirm_force: bool,
    pub precondition: GitPrecondition,
}

#[derive(Clone, Debug)]
pub struct BranchDeleteOptions {
    pub branch: String,
    pub remote: Option<String>,
    pub force: bool,
    pub confirm_force: bool,
    pub confirm_remote: bool,
    pub precondition: GitPrecondition,
}

#[derive(Clone, Debug)]
pub struct WorktreeCreateOptions {
    pub path: PathBuf,
    pub base: String,
    pub branch: Option<String>,
    pub create_branch: bool,
    pub detached: bool,
    pub precondition: GitPrecondition,
}

#[derive(Clone, Debug)]
pub struct WorktreeRemoveOptions {
    pub path: PathBuf,
    pub force: bool,
    pub confirm_force: bool,
    pub precondition: GitPrecondition,
}
