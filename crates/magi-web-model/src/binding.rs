//! 应用级唯一 GPT Web 槽位。
//!
//! 槽位是 daemon 进程内的运行态事实：它只记录拥有者、页面和远端保存对话
//! 引用，不复制网页上下文，也不把 active turn 写入 provider_context 或磁盘。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::errors::{WebModelError, WebModelErrorCode};

static NEXT_TURN: AtomicU64 = AtomicU64::new(1);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebConversationMode {
    Temporary,
    Saved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebConversationSyncState {
    Unbound,
    Pending,
    Active,
    Stale,
    Deleted,
    Conflict,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WebConversationBinding {
    pub mode: WebConversationMode,
    pub engine_id: String,
    pub remote_conversation_id: Option<String>,
    pub remote_title: Option<String>,
    pub last_synced_remote_message_id: Option<String>,
    pub remote_updated_at: Option<String>,
    pub sync_state: WebConversationSyncState,
}

impl WebConversationBinding {
    pub fn temporary() -> Self {
        Self {
            mode: WebConversationMode::Temporary,
            engine_id: crate::site::WEB_MODEL_ENGINE_ID.to_string(),
            remote_conversation_id: None,
            remote_title: None,
            last_synced_remote_message_id: None,
            remote_updated_at: None,
            sync_state: WebConversationSyncState::Unbound,
        }
    }

    pub fn saved(conversation_id: impl Into<String>) -> Self {
        Self {
            mode: WebConversationMode::Saved,
            engine_id: crate::site::WEB_MODEL_ENGINE_ID.to_string(),
            remote_conversation_id: Some(conversation_id.into()),
            remote_title: None,
            last_synced_remote_message_id: None,
            remote_updated_at: None,
            sync_state: WebConversationSyncState::Active,
        }
    }

    pub fn pending_saved() -> Self {
        Self {
            mode: WebConversationMode::Saved,
            engine_id: crate::site::WEB_MODEL_ENGINE_ID.to_string(),
            remote_conversation_id: None,
            remote_title: None,
            last_synced_remote_message_id: None,
            remote_updated_at: None,
            sync_state: WebConversationSyncState::Pending,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct WebSlotOwner {
    pub session_id: String,
    pub project_id: String,
}

impl WebSlotOwner {
    pub fn new(session_id: impl Into<String>, project_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            project_id: project_id.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WebSlotSnapshot {
    pub owner_session_id: String,
    pub owner_project_id: String,
    pub mode: WebConversationMode,
    pub page_id: String,
    pub remote_conversation_id: Option<String>,
    pub active_turn: bool,
    pub claimed_at: u64,
}

#[derive(Clone, Debug)]
struct WebSlot {
    owner: WebSlotOwner,
    binding: WebConversationBinding,
    page_id: String,
    active_turn_id: Option<u64>,
    claimed_at: u64,
    /// 本槽位最近一次从网页读到的最后一条消息：临时对话存活判定的唯一比对对象。
    /// 只在内存里，随槽位一起消失（临时上下文本来就不可恢复）。
    last_message: Option<crate::driver::WebMessage>,
}

impl WebSlot {
    fn snapshot(&self) -> WebSlotSnapshot {
        WebSlotSnapshot {
            owner_session_id: self.owner.session_id.clone(),
            owner_project_id: self.owner.project_id.clone(),
            mode: self.binding.mode,
            page_id: self.page_id.clone(),
            remote_conversation_id: self.binding.remote_conversation_id.clone(),
            active_turn: self.active_turn_id.is_some(),
            claimed_at: self.claimed_at,
        }
    }
}

/// 槽位拥有者的 turn 结束或槽位被释放时调用（不持有槽位锁）。
/// MCP 网关用它取消该拥有者遗留的待审批；回调必须快速返回。
pub type SlotEndHook = Arc<dyn Fn(&WebSlotOwner) + Send + Sync>;

#[derive(Default)]
struct SlotState {
    slot: Option<WebSlot>,
    end_hook: Option<SlotEndHook>,
}

/// 由 client 与 MCP Gateway 共享的唯一槽位表。
#[derive(Clone, Default)]
pub struct WebSlotTable {
    state: Arc<Mutex<SlotState>>,
}

impl WebSlotTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// 占用槽位；同一拥有者重复 claim 是幂等的，其他拥有者一律 busy。
    pub fn claim(
        &self,
        owner: WebSlotOwner,
        binding: WebConversationBinding,
        page_id: impl Into<String>,
    ) -> Result<WebSlotClaim, WebModelError> {
        let page_id = page_id.into();
        let mut state = self.state.lock().expect("web slot lock poisoned");
        match state.slot.as_mut() {
            None => {
                let slot = WebSlot {
                    owner,
                    binding,
                    page_id,
                    active_turn_id: None,
                    claimed_at: now_ms(),
                    last_message: None,
                };
                let snapshot = slot.snapshot();
                state.slot = Some(slot);
                Ok(WebSlotClaim {
                    snapshot,
                    newly_claimed: true,
                })
            }
            Some(slot) if slot.owner == owner => {
                if slot.binding.mode != binding.mode
                    || slot.binding.remote_conversation_id != binding.remote_conversation_id
                {
                    return Err(WebModelError::new(
                        WebModelErrorCode::WebSessionBusy,
                        "当前会话已经绑定了另一种 GPT Web 对话模式",
                    ));
                }
                Ok(WebSlotClaim {
                    snapshot: slot.snapshot(),
                    newly_claimed: false,
                })
            }
            Some(slot) => Err(WebModelError::new(
                WebModelErrorCode::WebSessionBusy,
                format!("GPT Web 正被会话 {} 占用", slot.owner.session_id),
            )),
        }
    }

    pub fn begin_turn(&self, owner: &WebSlotOwner) -> Result<WebTurnLease, WebModelError> {
        let mut state = self.state.lock().expect("web slot lock poisoned");
        let slot = state.slot.as_mut().ok_or_else(|| {
            WebModelError::new(WebModelErrorCode::WebSessionBusy, "GPT Web 槽位尚未占用")
        })?;
        if &slot.owner != owner {
            return Err(WebModelError::new(
                WebModelErrorCode::WebSessionBusy,
                format!("GPT Web 正被会话 {} 占用", slot.owner.session_id),
            ));
        }
        if slot.active_turn_id.is_some() {
            return Err(WebModelError::new(
                WebModelErrorCode::WebSessionBusy,
                "当前 GPT Web 会话已有进行中的 turn",
            ));
        }
        let id = NEXT_TURN.fetch_add(1, Ordering::Relaxed);
        slot.active_turn_id = Some(id);
        Ok(WebTurnLease {
            table: self.clone(),
            owner: owner.clone(),
            turn_id: id,
        })
    }

    /// 注册 turn 结束 / 槽位释放的回调（唯一一个；后注册的替换先前的）。
    pub fn set_end_hook(&self, hook: Option<SlotEndHook>) {
        self.state.lock().expect("web slot lock poisoned").end_hook = hook;
    }

    fn notify_end(&self, hook: Option<SlotEndHook>, owner: &WebSlotOwner) {
        if let Some(hook) = hook {
            hook(owner);
        }
    }

    pub(crate) fn finish_turn(&self, owner: &WebSlotOwner, turn_id: u64) {
        let hook = {
            let mut state = self.state.lock().expect("web slot lock poisoned");
            let hook = state.end_hook.clone();
            match state.slot.as_mut() {
                Some(slot) if &slot.owner == owner && slot.active_turn_id == Some(turn_id) => {
                    slot.active_turn_id = None;
                    hook
                }
                _ => None,
            }
        };
        self.notify_end(hook, owner);
    }

    pub fn release(&self, owner: &WebSlotOwner) -> bool {
        let (released, hook) = {
            let mut state = self.state.lock().expect("web slot lock poisoned");
            let hook = state.end_hook.clone();
            if state.slot.as_ref().is_some_and(|slot| &slot.owner == owner) {
                state.slot = None;
                (true, hook)
            } else {
                (false, None)
            }
        };
        self.notify_end(hook, owner);
        released
    }

    /// 释放指定会话持有的槽位。项目 id 由当前槽位权威状态决定，调用方只需
    /// 提供会话 id，避免 UI 为了停止应用级 Web 宿主复制一份项目绑定事实。
    pub fn release_session(&self, session_id: &str) -> bool {
        let (owner, hook) = {
            let mut state = self.state.lock().expect("web slot lock poisoned");
            let hook = state.end_hook.clone();
            if state
                .slot
                .as_ref()
                .is_some_and(|slot| slot.owner.session_id == session_id)
            {
                (state.slot.take().map(|slot| slot.owner), hook)
            } else {
                (None, None)
            }
        };
        match owner {
            Some(owner) => {
                self.notify_end(hook, &owner);
                true
            }
            None => false,
        }
    }

    /// 新建的已保存对话在首条消息被接受后才有远端 id：把它记进槽位绑定，
    /// 否则同一会话的下一轮 claim 会因为 id 对不上而被当成「另一种对话模式」。
    pub fn bind_remote_conversation(&self, owner: &WebSlotOwner, conversation_id: &str) {
        let mut state = self.state.lock().expect("web slot lock poisoned");
        if let Some(slot) = state.slot.as_mut().filter(|slot| &slot.owner == owner) {
            slot.binding.remote_conversation_id = Some(conversation_id.to_string());
        }
    }

    /// 记录槽位最近一次读到的网页最后一条消息（turn 成功完成后调用）。
    pub fn set_last_message(&self, owner: &WebSlotOwner, message: crate::driver::WebMessage) {
        let mut state = self.state.lock().expect("web slot lock poisoned");
        if let Some(slot) = state.slot.as_mut().filter(|slot| &slot.owner == owner) {
            slot.last_message = Some(message);
        }
    }

    pub fn last_message(&self, owner: &WebSlotOwner) -> Option<crate::driver::WebMessage> {
        let state = self.state.lock().expect("web slot lock poisoned");
        state
            .slot
            .as_ref()
            .filter(|slot| &slot.owner == owner)
            .and_then(|slot| slot.last_message.clone())
    }

    pub fn snapshot(&self) -> Option<WebSlotSnapshot> {
        self.state
            .lock()
            .expect("web slot lock poisoned")
            .slot
            .as_ref()
            .map(WebSlot::snapshot)
    }

    pub fn owner(&self) -> Option<WebSlotOwner> {
        self.state
            .lock()
            .expect("web slot lock poisoned")
            .slot
            .as_ref()
            .map(|slot| slot.owner.clone())
    }

    pub fn binding(&self) -> Option<WebConversationBinding> {
        self.state
            .lock()
            .expect("web slot lock poisoned")
            .slot
            .as_ref()
            .map(|slot| slot.binding.clone())
    }

    /// 只用于 MCP Gateway：返回当前拥有者与内部 turn 代次，不返回任何令牌。
    /// 当前唯一 Web turn 的归属，用于 Magi MCP 的 `follow_web_slot` 令牌。
    /// 不返回页面或上下文内容；槽位释放后立即返回 None。
    pub fn active_context(&self) -> Option<(WebSlotOwner, u64)> {
        self.state
            .lock()
            .expect("web slot lock poisoned")
            .slot
            .as_ref()
            .and_then(|slot| slot.active_turn_id.map(|id| (slot.owner.clone(), id)))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebSlotClaim {
    pub snapshot: WebSlotSnapshot,
    pub newly_claimed: bool,
}

pub struct WebTurnLease {
    table: WebSlotTable,
    owner: WebSlotOwner,
    turn_id: u64,
}

impl WebTurnLease {
    pub fn turn_id(&self) -> u64 {
        self.turn_id
    }

    pub fn owner(&self) -> &WebSlotOwner {
        &self.owner
    }
}

impl Drop for WebTurnLease {
    fn drop(&mut self) {
        self.table.finish_turn(&self.owner, self.turn_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_owner_can_claim_the_slot() {
        let slots = WebSlotTable::new();
        let first = WebSlotOwner::new("session-a", "project-a");
        let second = WebSlotOwner::new("session-b", "project-b");
        let first_claim = slots
            .claim(
                first.clone(),
                WebConversationBinding::temporary(),
                "web-slot",
            )
            .expect("first claim");
        assert!(first_claim.newly_claimed);
        assert!(
            slots
                .claim(second, WebConversationBinding::temporary(), "web-slot")
                .is_err()
        );
        assert!(
            !slots
                .claim(
                    first.clone(),
                    WebConversationBinding::temporary(),
                    "web-slot"
                )
                .expect("idempotent claim")
                .newly_claimed
        );
        let lease = slots.begin_turn(&first).expect("turn");
        assert!(slots.begin_turn(&first).is_err());
        drop(lease);
        assert!(slots.begin_turn(&first).is_ok());
    }

    #[test]
    fn saved_binding_does_not_accept_a_temporary_rebind() {
        let slots = WebSlotTable::new();
        let owner = WebSlotOwner::new("s", "p");
        slots
            .claim(
                owner.clone(),
                WebConversationBinding::saved("conversation-1"),
                "web-slot",
            )
            .expect("claim");
        let error = slots
            .claim(owner, WebConversationBinding::temporary(), "web-slot")
            .expect_err("mode change must fail");
        assert_eq!(error.code, WebModelErrorCode::WebSessionBusy);
    }

    #[test]
    fn end_hook_fires_when_a_turn_finishes_or_the_slot_is_released() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let slots = WebSlotTable::new();
        let owner = WebSlotOwner::new("s", "p");
        let fired = Arc::new(AtomicUsize::new(0));
        let counter = fired.clone();
        slots.set_end_hook(Some(Arc::new(move |owner: &WebSlotOwner| {
            assert_eq!(owner.session_id, "s");
            counter.fetch_add(1, Ordering::SeqCst);
        })));
        slots
            .claim(owner.clone(), WebConversationBinding::temporary(), "page")
            .unwrap();
        let lease = slots.begin_turn(&owner).unwrap();
        assert_eq!(fired.load(Ordering::SeqCst), 0);
        drop(lease);
        assert_eq!(fired.load(Ordering::SeqCst), 1, "turn 结束必须通知");
        assert!(slots.release(&owner));
        assert_eq!(fired.load(Ordering::SeqCst), 2, "槽位释放必须通知");
        assert!(!slots.release(&owner), "重复释放不通知");
        assert_eq!(fired.load(Ordering::SeqCst), 2);
    }
}
