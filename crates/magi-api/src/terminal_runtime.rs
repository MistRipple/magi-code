use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
};
use tokio::sync::broadcast;

const TERMINAL_SCROLLBACK_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalBinding {
    pub terminal_tab_id: String,
    /// 终端继承会话作用域。个人会话在 Magi 私有执行目录中启动，不伪造项目根目录。
    pub workspace_id: Option<String>,
    pub execution_root: PathBuf,
    pub session_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TerminalLifecycle {
    Running,
    Exited {
        exit_code: u32,
        signal: Option<String>,
    },
    Failed {
        message: String,
    },
    Closed,
}

impl TerminalLifecycle {
    pub(crate) fn is_terminal(&self) -> bool {
        !matches!(self, Self::Running)
    }
}

#[derive(Clone, Debug)]
pub(crate) enum TerminalEvent {
    Output {
        sequence: u64,
        bytes: Vec<u8>,
    },
    Lifecycle {
        sequence: u64,
        lifecycle: TerminalLifecycle,
    },
}

impl TerminalEvent {
    pub(crate) fn sequence(&self) -> u64 {
        match self {
            Self::Output { sequence, .. } | Self::Lifecycle { sequence, .. } => *sequence,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TerminalSnapshot {
    pub sequence: u64,
    pub output: Vec<u8>,
    pub lifecycle: TerminalLifecycle,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum TerminalRuntimeError {
    #[error("终端实例已绑定到其他会话或工作区")]
    BindingMismatch,
    #[error("启动本地终端失败: {0}")]
    Start(String),
    #[error("写入本地终端失败: {0}")]
    Write(String),
    #[error("调整本地终端尺寸失败: {0}")]
    Resize(String),
}

struct TerminalSharedState {
    sequence: u64,
    output: VecDeque<u8>,
    lifecycle: TerminalLifecycle,
}

struct TerminalShared {
    state: Mutex<TerminalSharedState>,
    events: broadcast::Sender<TerminalEvent>,
}

impl TerminalShared {
    fn new() -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            state: Mutex::new(TerminalSharedState {
                sequence: 0,
                output: VecDeque::new(),
                lifecycle: TerminalLifecycle::Running,
            }),
            events,
        }
    }

    fn append_output(&self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        let sequence = {
            let mut state = self.state.lock().expect("terminal state lock poisoned");
            state.output.extend(bytes.iter().copied());
            while state.output.len() > TERMINAL_SCROLLBACK_BYTES {
                state.output.pop_front();
            }
            state.sequence += 1;
            state.sequence
        };
        let _ = self.events.send(TerminalEvent::Output { sequence, bytes });
    }

    fn transition(&self, lifecycle: TerminalLifecycle) {
        let sequence = {
            let mut state = self.state.lock().expect("terminal state lock poisoned");
            if state.lifecycle.is_terminal() {
                return;
            }
            state.lifecycle = lifecycle.clone();
            state.sequence += 1;
            state.sequence
        };
        let _ = self.events.send(TerminalEvent::Lifecycle {
            sequence,
            lifecycle,
        });
    }

    fn snapshot(&self) -> TerminalSnapshot {
        let state = self.state.lock().expect("terminal state lock poisoned");
        TerminalSnapshot {
            sequence: state.sequence,
            output: state.output.iter().copied().collect(),
            lifecycle: state.lifecycle.clone(),
        }
    }

    fn is_running(&self) -> bool {
        matches!(
            self.state
                .lock()
                .expect("terminal state lock poisoned")
                .lifecycle,
            TerminalLifecycle::Running
        )
    }
}

pub(crate) struct TerminalSession {
    binding: TerminalBinding,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Option<Box<dyn ChildKiller + Send + Sync>>>,
    shared: Arc<TerminalShared>,
}

impl TerminalSession {
    fn start(binding: TerminalBinding, size: PtySize) -> Result<Arc<Self>, TerminalRuntimeError> {
        let pair = native_pty_system()
            .openpty(size)
            .map_err(|error| TerminalRuntimeError::Start(error.to_string()))?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| TerminalRuntimeError::Start(error.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| TerminalRuntimeError::Start(error.to_string()))?;

        let mut command = CommandBuilder::new_default_prog();
        command.cwd(&binding.execution_root);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.env("TERM_PROGRAM", "Magi");
        command.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| TerminalRuntimeError::Start(error.to_string()))?;
        let killer = child.clone_killer();
        let shared = Arc::new(TerminalShared::new());
        let session = Arc::new(Self {
            binding,
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            killer: Mutex::new(Some(killer)),
            shared: Arc::clone(&shared),
        });
        spawn_terminal_reader(reader, Arc::clone(&shared));
        spawn_terminal_waiter(child, shared);
        Ok(session)
    }

    pub(crate) fn binding(&self) -> &TerminalBinding {
        &self.binding
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<TerminalEvent> {
        self.shared.events.subscribe()
    }

    pub(crate) fn snapshot(&self) -> TerminalSnapshot {
        self.shared.snapshot()
    }

    pub(crate) fn write_input(&self, bytes: &[u8]) -> Result<(), TerminalRuntimeError> {
        if !self.shared.is_running() {
            return Err(TerminalRuntimeError::Write("终端进程已经结束".to_string()));
        }
        let mut writer = self.writer.lock().expect("terminal writer lock poisoned");
        writer
            .write_all(bytes)
            .and_then(|_| writer.flush())
            .map_err(|error| TerminalRuntimeError::Write(error.to_string()))
    }

    pub(crate) fn resize(&self, size: PtySize) -> Result<(), TerminalRuntimeError> {
        self.master
            .lock()
            .expect("terminal master lock poisoned")
            .resize(size)
            .map_err(|error| TerminalRuntimeError::Resize(error.to_string()))
    }

    pub(crate) fn terminate(&self) {
        let killer = self
            .killer
            .lock()
            .expect("terminal killer lock poisoned")
            .take();
        if let Some(mut killer) = killer {
            let _ = killer.kill();
        }
        self.shared.transition(TerminalLifecycle::Closed);
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let killer = self
            .killer
            .get_mut()
            .expect("terminal killer lock poisoned")
            .take();
        if let Some(mut killer) = killer {
            let _ = killer.kill();
        }
        self.shared.transition(TerminalLifecycle::Closed);
    }
}

fn spawn_terminal_reader(mut reader: Box<dyn Read + Send>, shared: Arc<TerminalShared>) {
    thread::Builder::new()
        .name("magi-terminal-reader".to_string())
        .spawn(move || {
            let mut buffer = vec![0_u8; 16 * 1024];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => shared.append_output(buffer[..read].to_vec()),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        shared.transition(TerminalLifecycle::Failed {
                            message: format!("读取终端输出失败: {error}"),
                        });
                        break;
                    }
                }
            }
        })
        .expect("terminal reader thread should start");
}

fn spawn_terminal_waiter(mut child: Box<dyn Child + Send + Sync>, shared: Arc<TerminalShared>) {
    thread::Builder::new()
        .name("magi-terminal-waiter".to_string())
        .spawn(move || match child.wait() {
            Ok(status) => shared.transition(TerminalLifecycle::Exited {
                exit_code: status.exit_code(),
                signal: status.signal().map(str::to_string),
            }),
            Err(error) => shared.transition(TerminalLifecycle::Failed {
                message: format!("等待终端进程退出失败: {error}"),
            }),
        })
        .expect("terminal waiter thread should start");
}

#[derive(Clone, Default)]
pub(crate) struct TerminalSessionManager {
    sessions: Arc<Mutex<HashMap<String, Arc<TerminalSession>>>>,
}

impl TerminalSessionManager {
    pub(crate) fn open_or_create(
        &self,
        binding: TerminalBinding,
        size: PtySize,
    ) -> Result<Arc<TerminalSession>, TerminalRuntimeError> {
        let mut sessions = self
            .sessions
            .lock()
            .expect("terminal sessions lock poisoned");
        if let Some(session) = sessions.get(&binding.terminal_tab_id) {
            if session.binding() != &binding {
                return Err(TerminalRuntimeError::BindingMismatch);
            }
            session.resize(size)?;
            return Ok(Arc::clone(session));
        }
        let terminal_tab_id = binding.terminal_tab_id.clone();
        let session = TerminalSession::start(binding, size)?;
        sessions.insert(terminal_tab_id, Arc::clone(&session));
        Ok(session)
    }

    pub(crate) fn close(&self, binding: &TerminalBinding) -> Result<(), TerminalRuntimeError> {
        let session = {
            let mut sessions = self
                .sessions
                .lock()
                .expect("terminal sessions lock poisoned");
            let Some(session) = sessions.get(&binding.terminal_tab_id) else {
                return Ok(());
            };
            if session.binding() != binding {
                return Err(TerminalRuntimeError::BindingMismatch);
            }
            sessions.remove(&binding.terminal_tab_id)
        };
        if let Some(session) = session {
            session.terminate();
        }
        Ok(())
    }

    pub(crate) fn close_for_session(&self, session_id: &str) -> usize {
        let removed = {
            let mut sessions = self
                .sessions
                .lock()
                .expect("terminal sessions lock poisoned");
            let terminal_ids = sessions
                .iter()
                .filter(|(_, terminal)| terminal.binding().session_id == session_id)
                .map(|(terminal_id, _)| terminal_id.clone())
                .collect::<Vec<_>>();
            terminal_ids
                .into_iter()
                .filter_map(|terminal_id| sessions.remove(&terminal_id))
                .collect::<Vec<_>>()
        };
        let count = removed.len();
        for session in removed {
            session.terminate();
        }
        count
    }

    #[cfg(test)]
    fn active_count(&self) -> usize {
        self.sessions
            .lock()
            .expect("terminal sessions lock poisoned")
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;

    fn binding(root: &std::path::Path) -> TerminalBinding {
        TerminalBinding {
            terminal_tab_id: "terminal-test".to_string(),
            workspace_id: Some("workspace-test".to_string()),
            execution_root: root.to_path_buf(),
            session_id: "session-test".to_string(),
        }
    }

    #[tokio::test]
    async fn pty_session_executes_interactive_shell_input_and_closes() {
        let workspace = TempDir::new().expect("workspace should create");
        let manager = TerminalSessionManager::default();
        let binding = binding(workspace.path());
        let session = manager
            .open_or_create(binding.clone(), PtySize::default())
            .expect("terminal should start");
        let mut events = session.subscribe();
        #[cfg(windows)]
        let command = b"echo magi-pty-ok\r".as_slice();
        #[cfg(not(windows))]
        let command = b"printf 'magi-pty-ok\\n'\r".as_slice();
        session
            .write_input(command)
            .expect("terminal input should write");

        let mut output = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !String::from_utf8_lossy(&output).contains("magi-pty-ok") {
                if let TerminalEvent::Output { bytes, .. } = events
                    .recv()
                    .await
                    .expect("terminal output event should arrive")
                {
                    output.extend(bytes);
                }
            }
        })
        .await
        .expect("interactive command should produce output");

        assert_eq!(manager.active_count(), 1);
        manager.close(&binding).expect("terminal should close");
        assert_eq!(manager.active_count(), 0);
    }

    #[test]
    fn terminal_id_cannot_cross_session_scope() {
        let workspace = TempDir::new().expect("workspace should create");
        let manager = TerminalSessionManager::default();
        let binding = binding(workspace.path());
        manager
            .open_or_create(binding.clone(), PtySize::default())
            .expect("terminal should start");
        let mut other = binding;
        other.session_id = "session-other".to_string();
        assert!(matches!(
            manager.open_or_create(other, PtySize::default()),
            Err(TerminalRuntimeError::BindingMismatch)
        ));
    }

    #[test]
    fn closing_session_terminates_all_owned_terminals() {
        let workspace = TempDir::new().expect("workspace should create");
        let manager = TerminalSessionManager::default();
        let first = binding(workspace.path());
        let mut second = first.clone();
        second.terminal_tab_id = "terminal-test-2".to_string();
        manager
            .open_or_create(first, PtySize::default())
            .expect("first terminal should start");
        manager
            .open_or_create(second, PtySize::default())
            .expect("second terminal should start");

        assert_eq!(manager.close_for_session("session-test"), 2);
        assert_eq!(manager.active_count(), 0);
    }
}
