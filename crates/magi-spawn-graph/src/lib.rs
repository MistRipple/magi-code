//! Task System v2 — L5 SpawnGraph：父代理 spawn 子代理的拓扑图。
//!
//! 设计目标：把父子关系、派发记录与回执路由收敛到一份正式数据结构。
//!
//! ## 概念模型
//!
//! - **节点（node）**：以 `TaskId` 作为唯一标识；一个 Task 在运行期对应一个
//!   Conversation 实例。SpawnGraph 不引入新的 id，避免再起一套
//!   ConversationId/AssignmentId 的映射表。
//! - **边（edge）**：父 spawn 子的有向关系，附带 `TaskKind`、状态（`Open`/`Closed`）、
//!   创建/关闭时间戳。一条边 = 一次 spawn 行为。
//! - **回执路由**：子节点完成（status=Closed）后通过 `mark_closed` 标记；上层
//!   调用 `parent_of(child)` 找回需要投递的父 Mailbox。
//! - **级联停止**：`open_descendants(root)` 一次返回所有未关闭的子孙节点，
//!   交给 caller 逐个取消（SpawnGraph 不直接调度任务，只提供拓扑）。
//! - **限制**：`enforce_limits` 在 `add_edge` 时检查 max_depth / max_fanout，
//!   超限返回错误，由 caller 决定 reject / NeedsApproval。
//!
//! ## 与 `magi-core::Task::parent_task_id` 的关系
//!
//! 后者只在单个 Task 上记录"我的父亲是谁"，无法回答"我的所有未关闭子孙是谁"
//! 也无法承载 spawn 时刻、关闭时刻、TaskKind 等信息。SpawnGraph 是它的超集；
//! S6 后所有"父子关系查询 / 级联停止 / 回执投递"都从 SpawnGraph 走，
//! Task.parent_task_id 仅作为持久化字段供恢复时重建图。
//!
//! ## 不变式
//!
//! 1. 每个 child 至多有一个 parent。
//! 2. 不允许成环（add_edge 时检测 ancestor 链）。
//! 3. 一个 child 不能被 spawn 两次（第二次 add_edge 返回 `EdgeAlreadyExists`）。

use std::collections::{HashMap, HashSet};
use std::time::SystemTime;

use magi_core::ids::TaskId;
use magi_core::task::TaskKind;

// ---------------------------------------------------------------------------
// SpawnEdge / SpawnEdgeStatus
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnEdgeStatus {
    Open,
    Closed,
}

#[derive(Clone, Debug)]
pub struct SpawnEdge {
    pub parent: TaskId,
    pub child: TaskId,
    pub status: SpawnEdgeStatus,
    pub task_kind: TaskKind,
    pub created_at: SystemTime,
    pub closed_at: Option<SystemTime>,
}

// ---------------------------------------------------------------------------
// SpawnGraphLimits
// ---------------------------------------------------------------------------

/// 安全阀：限制深度与扇出，防止 agent 自己 spawn 自己导致无限派生。
#[derive(Clone, Copy, Debug)]
pub struct SpawnGraphLimits {
    pub max_depth: usize,
    pub max_fanout_per_node: usize,
}

impl Default for SpawnGraphLimits {
    fn default() -> Self {
        // 默认参数贴合 codex/claude-code 实践：6 层深度足够 Coordinator → 子代理
        // → 子子代理；单节点 16 个 open 子节点对常规多代理场景已经偏宽。
        Self {
            max_depth: 6,
            max_fanout_per_node: 16,
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum SpawnGraphError {
    #[error("child {child} already has a parent edge (cannot spawn twice)")]
    EdgeAlreadyExists { child: TaskId },
    #[error("adding edge parent={parent} → child={child} would form a cycle")]
    WouldFormCycle { parent: TaskId, child: TaskId },
    #[error("max depth {limit} exceeded at parent={parent}")]
    MaxDepthExceeded { parent: TaskId, limit: usize },
    #[error("parent {parent} already has {open_count} open children (limit {limit})")]
    MaxFanoutExceeded {
        parent: TaskId,
        open_count: usize,
        limit: usize,
    },
    #[error("child {child} not found in graph")]
    UnknownChild { child: TaskId },
}

// ---------------------------------------------------------------------------
// SpawnGraph
// ---------------------------------------------------------------------------

/// 父子拓扑表。结构本身不持有 Conversation/Mailbox 引用，只回答"谁是谁的父亲"
/// 这一纯拓扑问题。Caller 决定怎么把回执投递到对应 Mailbox。
#[derive(Clone, Debug, Default)]
pub struct SpawnGraph {
    /// child → edge，唯一索引。
    edges: HashMap<TaskId, SpawnEdge>,
    /// parent → 子节点列表（保持插入顺序）。
    children: HashMap<TaskId, Vec<TaskId>>,
    limits: SpawnGraphLimits,
}

impl SpawnGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_limits(limits: SpawnGraphLimits) -> Self {
        Self {
            edges: HashMap::new(),
            children: HashMap::new(),
            limits,
        }
    }

    pub fn limits(&self) -> SpawnGraphLimits {
        self.limits
    }

    /// 添加一条 spawn 边。caller 在 dispatch 阶段调用，失败由 caller 决定是否
    /// 转为 Decision Task / 直接拒绝。
    pub fn add_edge(
        &mut self,
        parent: TaskId,
        child: TaskId,
        task_kind: TaskKind,
        now: SystemTime,
    ) -> Result<(), SpawnGraphError> {
        if self.edges.contains_key(&child) {
            return Err(SpawnGraphError::EdgeAlreadyExists { child });
        }
        if parent == child {
            return Err(SpawnGraphError::WouldFormCycle { parent, child });
        }
        // 通过沿 parent 链向上找 child 是否在祖先里来检测成环。
        if self.is_ancestor(&child, &parent) {
            return Err(SpawnGraphError::WouldFormCycle { parent, child });
        }
        // 深度限制：算上即将添加的这条边后的深度不得超过 max_depth。
        let depth_at_parent = self.depth_of(&parent);
        if depth_at_parent + 1 >= self.limits.max_depth {
            return Err(SpawnGraphError::MaxDepthExceeded {
                parent,
                limit: self.limits.max_depth,
            });
        }
        // 扇出限制：parent 当前 open 子节点数量。
        let open_count = self.open_children_count(&parent);
        if open_count >= self.limits.max_fanout_per_node {
            return Err(SpawnGraphError::MaxFanoutExceeded {
                parent,
                open_count,
                limit: self.limits.max_fanout_per_node,
            });
        }
        let edge = SpawnEdge {
            parent: parent.clone(),
            child: child.clone(),
            status: SpawnEdgeStatus::Open,
            task_kind,
            created_at: now,
            closed_at: None,
        };
        self.children.entry(parent).or_default().push(child.clone());
        self.edges.insert(child, edge);
        Ok(())
    }

    /// 子任务完成（成功 / 失败 / 取消）时由 caller 调用。重复关闭不报错，
    /// 便于 caller 在不知道当前状态的情况下幂等调用。
    pub fn mark_closed(&mut self, child: &TaskId, now: SystemTime) -> Result<(), SpawnGraphError> {
        let edge = self
            .edges
            .get_mut(child)
            .ok_or_else(|| SpawnGraphError::UnknownChild {
                child: child.clone(),
            })?;
        if edge.status == SpawnEdgeStatus::Open {
            edge.status = SpawnEdgeStatus::Closed;
            edge.closed_at = Some(now);
        }
        Ok(())
    }

    pub fn parent_of(&self, child: &TaskId) -> Option<&TaskId> {
        self.edges.get(child).map(|edge| &edge.parent)
    }

    pub fn edge_for(&self, child: &TaskId) -> Option<&SpawnEdge> {
        self.edges.get(child)
    }

    pub fn children_of(&self, parent: &TaskId) -> &[TaskId] {
        self.children
            .get(parent)
            .map(|children| children.as_slice())
            .unwrap_or(&[])
    }

    /// 返回所有 `Open` 状态的子孙节点（含直接子）；caller 用于级联取消。
    pub fn open_descendants(&self, root: &TaskId) -> Vec<TaskId> {
        let mut out = Vec::new();
        let mut stack: Vec<TaskId> = self.children_of(root).to_vec();
        let mut seen: HashSet<TaskId> = HashSet::new();
        while let Some(node) = stack.pop() {
            if !seen.insert(node.clone()) {
                continue;
            }
            if let Some(edge) = self.edges.get(&node) {
                if edge.status == SpawnEdgeStatus::Open {
                    out.push(node.clone());
                }
            }
            for child in self.children_of(&node) {
                stack.push(child.clone());
            }
        }
        out
    }

    /// 沿 parent 链向上枚举祖先（不含自身）。
    pub fn ancestors(&self, node: &TaskId) -> Vec<TaskId> {
        let mut out = Vec::new();
        let mut current = self.parent_of(node).cloned();
        while let Some(parent) = current {
            out.push(parent.clone());
            current = self.parent_of(&parent).cloned();
        }
        out
    }

    fn depth_of(&self, node: &TaskId) -> usize {
        let mut depth = 0usize;
        let mut current = self.parent_of(node).cloned();
        while let Some(parent) = current {
            depth += 1;
            current = self.parent_of(&parent).cloned();
        }
        depth
    }

    fn is_ancestor(&self, candidate: &TaskId, of: &TaskId) -> bool {
        let mut current = self.parent_of(of).cloned();
        while let Some(parent) = current {
            if &parent == candidate {
                return true;
            }
            current = self.parent_of(&parent).cloned();
        }
        false
    }

    fn open_children_count(&self, parent: &TaskId) -> usize {
        self.children_of(parent)
            .iter()
            .filter(|child| {
                self.edges
                    .get(*child)
                    .map(|edge| edge.status == SpawnEdgeStatus::Open)
                    .unwrap_or(false)
            })
            .count()
    }

    /// 测试 / 诊断辅助：返回当前所有边的克隆。
    pub fn all_edges(&self) -> Vec<SpawnEdge> {
        self.edges.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH
    }

    fn tid(s: &str) -> TaskId {
        TaskId::new(s)
    }

    #[test]
    fn add_edge_records_parent_and_child() {
        let mut graph = SpawnGraph::new();
        graph
            .add_edge(tid("a"), tid("b"), TaskKind::LocalAgent, now())
            .unwrap();
        assert_eq!(graph.parent_of(&tid("b")), Some(&tid("a")));
        assert_eq!(graph.children_of(&tid("a")), &[tid("b")]);
    }

    #[test]
    fn double_spawn_same_child_rejected() {
        let mut graph = SpawnGraph::new();
        graph
            .add_edge(tid("a"), tid("b"), TaskKind::LocalAgent, now())
            .unwrap();
        let err = graph
            .add_edge(tid("a"), tid("b"), TaskKind::LocalAgent, now())
            .unwrap_err();
        assert!(matches!(err, SpawnGraphError::EdgeAlreadyExists { .. }));
    }

    #[test]
    fn cycle_detected() {
        let mut graph = SpawnGraph::new();
        graph
            .add_edge(tid("a"), tid("b"), TaskKind::LocalAgent, now())
            .unwrap();
        graph
            .add_edge(tid("b"), tid("c"), TaskKind::LocalAgent, now())
            .unwrap();
        let err = graph
            .add_edge(tid("c"), tid("a"), TaskKind::LocalAgent, now())
            .unwrap_err();
        assert!(matches!(err, SpawnGraphError::WouldFormCycle { .. }));
    }

    #[test]
    fn max_depth_enforced() {
        let limits = SpawnGraphLimits {
            max_depth: 3,
            max_fanout_per_node: 4,
        };
        let mut graph = SpawnGraph::with_limits(limits);
        graph
            .add_edge(tid("a"), tid("b"), TaskKind::LocalAgent, now())
            .unwrap();
        graph
            .add_edge(tid("b"), tid("c"), TaskKind::LocalAgent, now())
            .unwrap();
        let err = graph
            .add_edge(tid("c"), tid("d"), TaskKind::LocalAgent, now())
            .unwrap_err();
        assert!(matches!(err, SpawnGraphError::MaxDepthExceeded { .. }));
    }

    #[test]
    fn max_fanout_enforced_only_for_open_children() {
        let limits = SpawnGraphLimits {
            max_depth: 6,
            max_fanout_per_node: 2,
        };
        let mut graph = SpawnGraph::with_limits(limits);
        graph
            .add_edge(tid("p"), tid("c1"), TaskKind::LocalAgent, now())
            .unwrap();
        graph
            .add_edge(tid("p"), tid("c2"), TaskKind::LocalAgent, now())
            .unwrap();
        let err = graph
            .add_edge(tid("p"), tid("c3"), TaskKind::LocalAgent, now())
            .unwrap_err();
        assert!(matches!(err, SpawnGraphError::MaxFanoutExceeded { .. }));
        // 关闭一个 open child 后扇出再次允许新增。
        graph.mark_closed(&tid("c1"), now()).unwrap();
        graph
            .add_edge(tid("p"), tid("c3"), TaskKind::LocalAgent, now())
            .unwrap();
    }

    #[test]
    fn mark_closed_records_timestamp_idempotent() {
        let mut graph = SpawnGraph::new();
        graph
            .add_edge(tid("a"), tid("b"), TaskKind::LocalAgent, now())
            .unwrap();
        graph.mark_closed(&tid("b"), now()).unwrap();
        assert_eq!(
            graph.edge_for(&tid("b")).unwrap().status,
            SpawnEdgeStatus::Closed
        );
        // 重复关闭不报错
        graph.mark_closed(&tid("b"), now()).unwrap();
    }

    #[test]
    fn open_descendants_skips_closed_subtrees() {
        let mut graph = SpawnGraph::new();
        graph
            .add_edge(tid("root"), tid("a"), TaskKind::LocalAgent, now())
            .unwrap();
        graph
            .add_edge(tid("root"), tid("b"), TaskKind::LocalAgent, now())
            .unwrap();
        graph
            .add_edge(tid("a"), tid("a1"), TaskKind::LocalAgent, now())
            .unwrap();
        graph.mark_closed(&tid("a"), now()).unwrap();
        // a 关闭后只剩 b、a1 还是 open（a1 边自身还是 Open）。
        let mut got = graph.open_descendants(&tid("root"));
        got.sort_by_key(|t| t.as_str().to_string());
        assert_eq!(got, vec![tid("a1"), tid("b")]);
    }

    #[test]
    fn ancestors_reports_chain() {
        let mut graph = SpawnGraph::new();
        graph
            .add_edge(tid("a"), tid("b"), TaskKind::LocalAgent, now())
            .unwrap();
        graph
            .add_edge(tid("b"), tid("c"), TaskKind::LocalAgent, now())
            .unwrap();
        assert_eq!(graph.ancestors(&tid("c")), vec![tid("b"), tid("a")]);
        assert!(graph.ancestors(&tid("a")).is_empty());
    }

    #[test]
    fn mark_closed_unknown_child_errors() {
        let mut graph = SpawnGraph::new();
        let err = graph.mark_closed(&tid("missing"), now()).unwrap_err();
        assert!(matches!(err, SpawnGraphError::UnknownChild { .. }));
    }
}
