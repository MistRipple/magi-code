//! magi-session-isolation — 会话级隔离工作副本。
//!
//! 多个会话同时改同一个工作区会互相覆盖，变更归属也无从谈起。隔离会话在自己的工作副本里
//! 读写，互不影响；用户确认后再把改动合并回主工作区。这个机制与 Git 无关：Git 工作区和
//! 普通目录走同一条路径——副本通过写时复制（或普通复制）建立，变更由 `magi-snapshot`
//! 的账本记录，合并按「基线 / 主工作区 / 副本」三方比较，两边都改过的文件作为冲突交给用户。
//!
//! - [`clone`]：建立副本。
//! - [`merge`]：计划与应用合并。
//! - [`registry`]：会话到隔离副本的登记表。

pub mod clone;
pub mod error;
pub mod merge;
pub mod registry;

pub use clone::{CloneOutcome, CloneStrategy, create_isolated_copy};
pub use error::{IsolationError, IsolationResult};
pub use merge::{
    ConflictKind, ConflictResolution, MergeAction, MergeEntry, MergeOutcome, MergePlan,
    MergeSelection, MergeState, apply_merge, plan_merge,
};
pub use registry::{IsolationOrigin, SessionIsolation, SessionIsolationRegistry};
