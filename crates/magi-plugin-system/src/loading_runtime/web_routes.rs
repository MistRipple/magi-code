//! Admit a plugin document against the same activation owner as configuration.
use super::*;
use crate::LoadingActivation;
use magi_extension_host::shared::PrivateWebRouteEndpoint;
use std::net::SocketAddrV4;
use tokio::sync::watch;

pub struct LoadingWebRoutes {
    owner: Arc<Mutex<PluginManager>>,
    host_generation: u64,
    endpoint: Arc<PrivateWebRouteEndpoint>,
}

/// Server-side binding created by the daemon after page admission. A later
/// configuration of the same plugin cannot retarget this document.
pub struct PluginRoutePage {
    scope: PageScope,
}

enum PageScope {
    Plugin {
        activation: LoadingActivation,
        call_epoch: u64,
    },
    Host {
        host_generation: u64,
    },
}

/// Revocable private transport access for one admitted route call.
pub struct PluginRouteLease {
    endpoint: Arc<PrivateWebRouteEndpoint>,
    scope: LeaseScope,
}

enum LeaseScope {
    Plugin {
        activation: LoadingActivation,
        call_epoch: u64,
        running: watch::Receiver<()>,
    },
    Host {
        host_generation: u64,
    },
}

impl LoadingRuntime {
    /// Called by the owner of a bound HTTP origin. Retains only the live
    /// endpoint for this host generation and the existing PluginManager.
    pub async fn open_web_routes(
        &self,
        origin: &str,
    ) -> Result<LoadingWebRoutes, LoadingRuntimeError> {
        let endpoint = self.host.open_web_routes(origin).await?;
        Ok(LoadingWebRoutes {
            owner: self.owner.clone(),
            host_generation: self.host.generation(),
            endpoint: Arc::new(endpoint),
        })
    }
}

fn stale_page() -> PluginError {
    PluginError::Conflict("插件页面的运行实例已失效，请重新打开页面".into())
}

impl LoadingWebRoutes {
    pub fn bind_page(
        &self,
        activation: &LoadingActivation,
    ) -> Result<PluginRoutePage, PluginError> {
        if activation.host_generation != self.host_generation || self.endpoint.is_closed() {
            return Err(stale_page());
        }
        let owner = lock(&self.owner)?;
        let (call_epoch, _) = owner
            .loading_call_scope(activation)
            .ok_or_else(stale_page)?;
        Ok(PluginRoutePage {
            scope: PageScope::Plugin {
                activation: activation.clone(),
                call_epoch,
            },
        })
    }

    pub fn bind_host_page(&self) -> Result<PluginRoutePage, PluginError> {
        if self.endpoint.is_closed() {
            return Err(stale_page());
        }
        Ok(PluginRoutePage {
            scope: PageScope::Host {
                host_generation: self.host_generation,
            },
        })
    }

    pub fn admit(&self, page: &PluginRoutePage) -> Result<PluginRouteLease, PluginError> {
        if self.endpoint.is_closed() {
            return Err(stale_page());
        }
        match &page.scope {
            PageScope::Plugin {
                activation,
                call_epoch,
            } => {
                if activation.host_generation != self.host_generation {
                    return Err(stale_page());
                }
                let owner = lock(&self.owner)?;
                let (epoch, running) = owner
                    .loading_call_scope(activation)
                    .ok_or_else(stale_page)?;
                if epoch != *call_epoch {
                    return Err(stale_page());
                }
                Ok(PluginRouteLease {
                    endpoint: self.endpoint.clone(),
                    scope: LeaseScope::Plugin {
                        activation: activation.clone(),
                        call_epoch: epoch,
                        running,
                    },
                })
            }
            PageScope::Host { host_generation } => {
                if *host_generation != self.host_generation {
                    return Err(stale_page());
                }
                Ok(PluginRouteLease {
                    endpoint: self.endpoint.clone(),
                    scope: LeaseScope::Host {
                        host_generation: *host_generation,
                    },
                })
            }
        }
    }
}

type PrivateRouteHeaders = [(&'static str, String); 4];

impl PluginRouteLease {
    pub fn assert_current(&self) -> Result<(), PluginError> {
        if self.endpoint.is_closed() {
            return Err(stale_page());
        }
        match &self.scope {
            LeaseScope::Plugin { running, .. } => {
                running.has_changed().map(|_| ()).map_err(|_| stale_page())
            }
            LeaseScope::Host { .. } => Ok(()),
        }
    }

    /// Only the daemon sends these headers over its private transport.
    pub fn target(&self) -> Result<(SocketAddrV4, PrivateRouteHeaders), LoadingRuntimeError> {
        self.assert_current()?;
        let headers = match &self.scope {
            LeaseScope::Plugin {
                activation,
                call_epoch,
                ..
            } => self.endpoint.headers(
                activation.host_generation,
                &activation.executable.instance_id,
                activation.activation_generation,
                *call_epoch,
            )?,
            LeaseScope::Host { host_generation } => {
                self.endpoint
                    .headers(*host_generation, "$host", *host_generation, 1)?
            }
        };
        Ok((self.endpoint.address(), headers))
    }

    /// Gateway selects this against connect, headers and response body polling.
    pub async fn revoked(&self) {
        match &self.scope {
            LeaseScope::Plugin { running, .. } => {
                let mut running = running.clone();
                tokio::select! {
                    _ = running.changed() => {},
                    _ = self.endpoint.closed() => {},
                }
            }
            LeaseScope::Host { .. } => {
                self.endpoint.closed().await;
            }
        }
    }
}
