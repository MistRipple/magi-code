//! 同步读取与 Job 等待共用的作用域取消登记，不持有业务状态。
use super::process::ProcessExecutionScope;
use crate::{ToolExecutionContext, ToolExecutionContextQuery, ToolRuntimeResources};
use std::{
    collections::HashMap,
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

static NEXT_OPERATION: AtomicU64 = AtomicU64::new(1);
type ActiveOperation = (ProcessExecutionScope, Arc<AtomicBool>);
static ACTIVE: LazyLock<Mutex<HashMap<u64, ActiveOperation>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) fn cancel_operations(query: Option<&ToolExecutionContextQuery>) -> usize {
    ACTIVE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values()
        .filter(|(scope, cancelled)| {
            query.is_none_or(|query| scope.matches_query(query))
                && !cancelled.swap(true, Ordering::SeqCst)
        })
        .count()
}

pub(crate) fn has_active_operations(query: &ToolExecutionContextQuery) -> bool {
    ACTIVE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values()
        .any(|(scope, _)| scope.matches_query(query))
}

pub(super) struct OperationCancellation {
    id: u64,
    flag: Arc<AtomicBool>,
}

impl OperationCancellation {
    pub(super) fn new(context: &ToolExecutionContext, resources: &ToolRuntimeResources) -> Self {
        let id = NEXT_OPERATION.fetch_add(1, Ordering::Relaxed);
        let flag = resources
            .cancellation
            .clone()
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
        ACTIVE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                id,
                (ProcessExecutionScope::from_context(context), flag.clone()),
            );
        Self { id, flag }
    }
    pub(super) fn flag(&self) -> &AtomicBool {
        &self.flag
    }
    pub(super) fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

impl Drop for OperationCancellation {
    fn drop(&mut self) {
        ACTIVE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.id);
    }
}
