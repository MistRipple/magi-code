use tokio::sync::{mpsc, oneshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserRuntimeComponentAction {
    CheckForUpdates,
    Install,
    Uninstall,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserRuntimeComponentOperation {
    pub action: BrowserRuntimeComponentAction,
    pub runtime_version: Option<String>,
    pub update_available: bool,
}

pub struct BrowserRuntimeControlRequest {
    pub action: BrowserRuntimeComponentAction,
    pub response: oneshot::Sender<Result<BrowserRuntimeComponentOperation, String>>,
}

pub type BrowserRuntimeControlReceiver = mpsc::Receiver<BrowserRuntimeControlRequest>;

#[derive(Clone)]
pub struct BrowserRuntimeControlClient {
    sender: mpsc::Sender<BrowserRuntimeControlRequest>,
}

impl BrowserRuntimeControlClient {
    pub async fn request(
        &self,
        action: BrowserRuntimeComponentAction,
    ) -> Result<BrowserRuntimeComponentOperation, String> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(BrowserRuntimeControlRequest { action, response })
            .await
            .map_err(|_| "浏览器运行组件控制器已停止".to_string())?;
        receiver
            .await
            .map_err(|_| "浏览器运行组件控制器未返回结果".to_string())?
    }
}

pub fn browser_runtime_control_channel(
    capacity: usize,
) -> (BrowserRuntimeControlClient, BrowserRuntimeControlReceiver) {
    let (sender, receiver) = mpsc::channel(capacity.max(1));
    (BrowserRuntimeControlClient { sender }, receiver)
}
