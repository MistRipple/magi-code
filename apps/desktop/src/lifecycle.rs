use std::sync::Mutex;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesktopState {
    Starting,
    ReadyVisible,
    ReadyHidden,
    ShuttingDown,
    Restarting,
    Stopped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesktopAction {
    ShowWindow,
    HideWindow,
    BeginExit,
    ExitProcess,
    Ignore,
}

#[derive(Debug)]
pub struct DesktopLifecycle {
    state: Mutex<DesktopState>,
}

impl DesktopLifecycle {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(DesktopState::Starting),
        }
    }

    pub fn state(&self) -> DesktopState {
        *self.state.lock().expect("desktop lifecycle lock poisoned")
    }

    pub fn mark_ready(&self) -> DesktopAction {
        self.transition(|state| match state {
            DesktopState::Starting => (DesktopState::ReadyVisible, DesktopAction::ShowWindow),
            _ => (state, DesktopAction::Ignore),
        })
    }

    pub fn request_window_close(&self) -> DesktopAction {
        self.transition(|state| match state {
            DesktopState::ReadyVisible => (DesktopState::ReadyHidden, DesktopAction::HideWindow),
            _ => (state, DesktopAction::Ignore),
        })
    }

    pub fn request_show(&self) -> DesktopAction {
        self.transition(|state| match state {
            DesktopState::ReadyHidden => (DesktopState::ReadyVisible, DesktopAction::ShowWindow),
            DesktopState::ReadyVisible => (state, DesktopAction::ShowWindow),
            _ => (state, DesktopAction::Ignore),
        })
    }

    pub fn request_exit(&self) -> DesktopAction {
        self.transition(|state| match state {
            DesktopState::Starting | DesktopState::ReadyVisible | DesktopState::ReadyHidden => {
                (DesktopState::ShuttingDown, DesktopAction::BeginExit)
            }
            _ => (state, DesktopAction::Ignore),
        })
    }

    pub fn request_update_restart(&self) -> DesktopAction {
        self.transition(|state| match state {
            DesktopState::Starting | DesktopState::ReadyVisible | DesktopState::ReadyHidden => {
                (DesktopState::Restarting, DesktopAction::BeginExit)
            }
            _ => (state, DesktopAction::Ignore),
        })
    }

    pub fn mark_stopped(&self) -> DesktopAction {
        self.transition(|state| match state {
            DesktopState::ShuttingDown | DesktopState::Restarting => {
                (DesktopState::Stopped, DesktopAction::ExitProcess)
            }
            _ => (state, DesktopAction::Ignore),
        })
    }

    fn transition(
        &self,
        reducer: impl FnOnce(DesktopState) -> (DesktopState, DesktopAction),
    ) -> DesktopAction {
        let mut state = self.state.lock().expect("desktop lifecycle lock poisoned");
        let (next, action) = reducer(*state);
        *state = next;
        action
    }
}

impl Default for DesktopLifecycle {
    fn default() -> Self {
        Self::new()
    }
}
