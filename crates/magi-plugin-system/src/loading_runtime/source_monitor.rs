//! Filesystem notifications invalidate inputs, never directly mutate the runtime.
use super::*;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{path::Path, sync::RwLock};
use tokio::{
    sync::{oneshot, watch},
    task::JoinHandle,
};

/// A current intake observation, not an installation or readiness fact.
#[derive(Clone, Debug)]
pub enum LoadingSourceObservation {
    Inspecting,
    Reconciled {
        revision: u64,
        application: Option<Value>,
    },
    Rejected {
        message: String,
    },
}

/// Watch the accepted closure plus files discovered during the current capture.
/// A missing or invalid file remains watched so its next edit can unblock intake.
pub(super) struct WatchedSources {
    root: PathBuf,
    paths: RwLock<BTreeSet<PathBuf>>,
}

impl WatchedSources {
    pub(super) fn set<'a>(&self, names: impl Iterator<Item = &'a str>) {
        *self.paths.write().expect("source watch paths") =
            names.map(|name| self.root.join(name)).collect();
    }

    pub(super) fn add(&self, name: &str) {
        self.paths
            .write()
            .expect("source watch paths")
            .insert(self.root.join(name));
    }

    fn affected(&self, changed: &Path) -> bool {
        self.paths
            .read()
            .expect("source watch paths")
            .iter()
            .any(|source| {
                source.starts_with(changed)
                    || std::fs::canonicalize(source).is_ok_and(|target| target.starts_with(changed))
            })
    }
}

/// The coordinator owns one monitor per host. Source changes are always
/// compiled against the latest accepted installation and configuration.
pub struct LoadingSourceMonitor {
    watcher: Option<RecommendedWatcher>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
    observations: watch::Receiver<LoadingSourceObservation>,
}

impl LoadingSourceMonitor {
    pub fn observations(&self) -> watch::Receiver<LoadingSourceObservation> {
        self.observations.clone()
    }

    /// Finish the current intake before releasing its owner. Dropping instead
    /// aborts the task; the runtime fences an unconfirmed mutating host call.
    pub async fn stop(mut self) -> Result<(), LoadingRuntimeError> {
        self.watcher.take();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            task.await?;
        }
        Ok(())
    }
}

impl Drop for LoadingSourceMonitor {
    fn drop(&mut self) {
        self.watcher.take();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl LoadingRuntime {
    pub fn monitor_sources(
        &self,
        initial: LoadingPreparation,
    ) -> Result<LoadingSourceMonitor, LoadingRuntimeError> {
        let (source_root, names) = {
            let owner = lock(&self.owner)?;
            let names = owner.loading_configuration().map_or_else(
                || initial.root_sources().cloned().collect::<Vec<_>>(),
                |current| {
                    current
                        .sources
                        .iter()
                        .map(|source| source.name().to_owned())
                        .collect()
                },
            );
            (
                std::fs::canonicalize(owner.loading_source_root()).map_err(PluginError::from)?,
                names,
            )
        };
        let watched = Arc::new(WatchedSources {
            root: source_root,
            paths: RwLock::default(),
        });
        watched.set(names.iter().map(String::as_str));
        let event_sources = watched.clone();
        // Notifications coalesce to one invalidation. A monitoring error is sticky
        // until this monitor is replaced; later file events cannot hide it.
        let (invalidate, mut invalidations) = watch::channel(None::<String>);
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<Event>| match event {
                Ok(event)
                    if !matches!(event.kind, EventKind::Access(_))
                        && (event.need_rescan()
                            || event
                                .paths
                                .iter()
                                .any(|changed| event_sources.affected(changed))) =>
                {
                    invalidate.send_modify(|_| {});
                }
                Ok(_) => {}
                Err(error) => {
                    invalidate.send_replace(Some(error.to_string()));
                }
            })?;
        watcher.watch(&watched.root, RecursiveMode::Recursive)?;
        let (publish, observations) = watch::channel(LoadingSourceObservation::Inspecting);
        let (stop, mut stopped) = oneshot::channel();
        let runtime = self.clone();
        let mut admitted = self.admitted.subscribe();
        let task = tokio::spawn(async move {
            let mut starting = true;
            // Register first, then inspect: edits during preparation remain pending
            // and will be captured again, without a timer or arbitrary debounce.
            loop {
                if !matches!(stopped.try_recv(), Err(oneshot::error::TryRecvError::Empty)) {
                    return;
                }
                let watcher_error = invalidations.borrow_and_update().clone();
                admitted.borrow_and_update();
                if let Some(message) = watcher_error {
                    publish.send_replace(LoadingSourceObservation::Rejected { message });
                    return;
                }
                // Keep the last verdict visible until a new intake settles.
                let observation = match runtime
                    .reconcile_notified_sources(&initial, &watched, starting)
                    .await
                {
                    Ok(result) => LoadingSourceObservation::Reconciled {
                        revision: result.revision,
                        application: result.application,
                    },
                    Err(error) => LoadingSourceObservation::Rejected {
                        message: error.to_string(),
                    },
                };
                starting = false;
                publish.send_replace(observation);
                tokio::select! {
                    biased;
                    _ = &mut stopped => return,
                    changed = invalidations.changed() => if changed.is_err() { return; },
                    changed = admitted.changed() => if changed.is_err() { return; },
                }
            }
        });
        Ok(LoadingSourceMonitor {
            watcher: Some(watcher),
            stop: Some(stop),
            task: Some(task),
            observations,
        })
    }
}
