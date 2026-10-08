//! daemon 持有的浏览器网络策略如何到达 Electron Main。
//!
//! 设置的唯一所有者是 daemon（`BrowserCapabilitySettings`）；Main 只在 guest 网络层执行，
//! 因此 Host 连接建立时与设置变化时都由 daemon 推送，界面不参与中转。
//! Main 重启后在收到推送之前默认拒绝局域网，所以推送失败只会让浏览器更严格，不会放宽。

use magi_browser_authority::{BrowserHostClient, BrowserHostCommand, BrowserHostCommandOutcome};

pub async fn push_network_policy(
    client: &BrowserHostClient,
    lan_access_enabled: bool,
) -> Result<(), String> {
    let reply = client
        .request(BrowserHostCommand::ConfigureNetworkPolicy { lan_access_enabled })
        .await
        .map_err(|error| error.to_string())?;
    match reply.response.outcome {
        BrowserHostCommandOutcome::Succeeded(_) => Ok(()),
        BrowserHostCommandOutcome::Failed(error)
        | BrowserHostCommandOutcome::Indeterminate(error) => Err(error.message),
        BrowserHostCommandOutcome::Cancelled => Err("browser network policy push cancelled".into()),
    }
}
