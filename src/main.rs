use anyhow::{Context, Result};
use log::{debug, error, info};
use notify::{Config, RecommendedWatcher, Watcher};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{BufRead, StdoutLock, stdin, stdout};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use tokio::runtime::Builder;
use tokio::sync::oneshot;
use unison_fsmonitor::{FsEvent, Monitor, MonitorEvent, Watch};
use watchman_client::SubscriptionData;
use watchman_client::pdu::Clock;
use watchman_client::pdu::{SubscribeRequest, SyncTimeout};
use watchman_client::prelude::*;

query_result_type! {
    struct ChangedFile {
        name: NameField,
        exists: ExistsField,
    }
}

#[derive(Clone, Copy, Debug)]
enum Backend {
    Notify,
    Watchman,
}

impl Backend {
    fn detect() -> Self {
        Self::from_watchman_availability(watchman_is_available())
    }

    fn from_watchman_availability(is_available: bool) -> Self {
        if is_available {
            Self::Watchman
        } else {
            Self::Notify
        }
    }
}

fn watchman_is_available() -> bool {
    match Command::new("watchman").arg("--version").output() {
        Ok(output) => output.status.success(),
        Err(_) => false,
    }
}

enum WatchmanCommand {
    Watch(PathBuf),
    Unwatch(PathBuf),
}

struct WatchmanWatcher {
    command_tx: Sender<WatchmanCommand>,
}

impl WatchmanWatcher {
    fn new(fs_event_tx: Sender<Result<FsEvent>>) -> Result<Self> {
        let (command_tx, command_rx) = channel();
        let (ready_tx, ready_rx) = channel();

        thread::spawn(move || {
            if let Err(error) = run_watchman_backend(command_rx, fs_event_tx, ready_tx) {
                error!(
                    "component=file_watcher event=watchman_backend_failure error={} error_chain={:#}",
                    error, error
                );
            }
        });

        ready_rx
            .recv()
            .context("Failed waiting for Watchman backend startup")??;

        Ok(Self { command_tx })
    }
}

impl Watch for WatchmanWatcher {
    fn watch(&mut self, path: &Path) -> Result<()> {
        self.command_tx
            .send(WatchmanCommand::Watch(path.to_path_buf()))
            .with_context(|| format!("Failed sending watch command for {:?}", path))
    }

    fn unwatch(&mut self, path: &Path) -> Result<()> {
        self.command_tx
            .send(WatchmanCommand::Unwatch(path.to_path_buf()))
            .with_context(|| format!("Failed sending unwatch command for {:?}", path))
    }
}

struct WatchmanSubscription {
    cancel_tx: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

struct WatchmanState {
    client: Client,
    fs_event_tx: Sender<Result<FsEvent>>,
    subscriptions: HashMap<PathBuf, WatchmanSubscription>,
}

impl WatchmanState {
    async fn connect(fs_event_tx: Sender<Result<FsEvent>>) -> Result<Self> {
        let client = Connector::new()
            .connect()
            .await
            .context("Failed connecting to Watchman")?;

        Ok(Self {
            client,
            fs_event_tx,
            subscriptions: HashMap::new(),
        })
    }

    async fn handle_command(&mut self, command: WatchmanCommand) -> Result<()> {
        match command {
            WatchmanCommand::Watch(path) => self.watch(path).await,
            WatchmanCommand::Unwatch(path) => self.unwatch(path).await,
        }
    }

    async fn watch(&mut self, path: PathBuf) -> Result<()> {
        let canonical = CanonicalPath::canonicalize(&path)
            .with_context(|| format!("Failed canonicalizing watch path {:?}", path))?;
        let canonical_path = canonical.clone().into_path_buf();

        if self.subscriptions.contains_key(&canonical_path) {
            return Ok(());
        }

        let resolved = self
            .client
            .resolve_root(canonical)
            .await
            .with_context(|| format!("Failed resolving Watchman root for {:?}", path))?;
        let clock = self
            .client
            .clock(&resolved, SyncTimeout::Default)
            .await
            .with_context(|| format!("Failed reading Watchman clock for {:?}", path))?;

        let request = SubscribeRequest {
            since: Some(Clock::Spec(clock)),
            relative_root: resolved.project_relative_path().map(Path::to_path_buf),
            fields: vec!["name"],
            ..Default::default()
        };

        let (mut subscription, response) = self
            .client
            .subscribe::<ChangedFile>(&resolved, request)
            .await
            .with_context(|| format!("Failed subscribing to Watchman for {:?}", path))?;

        debug!(
            "component=file_watcher backend=watchman event=subscription_started path={:?} response={:?}",
            path, response
        );

        let (cancel_tx, mut cancel_rx) = oneshot::channel();
        let fs_event_tx = self.fs_event_tx.clone();
        let watched_path = canonical_path.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut cancel_rx => {
                        if let Err(error) = subscription.cancel().await {
                            error!(
                                "component=file_watcher backend=watchman event=cancel_failed path={:?} error={} error_debug={:?}",
                                watched_path, error, error
                            );
                        }
                        return;
                    }
                    result = subscription.next() => {
                        match result {
                            Ok(SubscriptionData::FilesChanged(result)) => {
                                if let Some(event) = watchman_event(&watched_path, result) {
                                    if fs_event_tx.send(Ok(event)).is_err() {
                                        return;
                                    }
                                }
                            }
                            Ok(SubscriptionData::StateEnter { state_name, .. }) => {
                                debug!(
                                    "component=file_watcher backend=watchman event=state_enter path={:?} state={}",
                                    watched_path, state_name
                                );
                            }
                            Ok(SubscriptionData::StateLeave { state_name, .. }) => {
                                debug!(
                                    "component=file_watcher backend=watchman event=state_leave path={:?} state={}",
                                    watched_path, state_name
                                );
                            }
                            Ok(SubscriptionData::Canceled) => {
                                let _ = fs_event_tx.send(Err(anyhow::anyhow!(
                                    "Watchman subscription canceled for {:?}",
                                    watched_path
                                )));
                                return;
                            }
                            Err(error) => {
                                let _ = fs_event_tx.send(Err(error.into()));
                                return;
                            }
                        }
                    }
                }
            }
        });

        self.subscriptions
            .insert(canonical_path, WatchmanSubscription { cancel_tx, task });

        Ok(())
    }

    async fn unwatch(&mut self, path: PathBuf) -> Result<()> {
        let canonical = CanonicalPath::canonicalize(&path)
            .with_context(|| format!("Failed canonicalizing unwatch path {:?}", path))?;
        let canonical_path = canonical.into_path_buf();

        if let Some(subscription) = self.subscriptions.remove(&canonical_path) {
            let _ = subscription.cancel_tx.send(());
            let _ = subscription.task.await;
        }

        Ok(())
    }

    async fn shutdown(self) {
        for (_, subscription) in self.subscriptions {
            let _ = subscription.cancel_tx.send(());
            let _ = subscription.task.await;
        }
    }
}

fn watchman_event(watched_path: &Path, result: QueryResult<ChangedFile>) -> Option<FsEvent> {
    if result.is_fresh_instance {
        info!(
            "component=file_watcher backend=watchman event=fresh_instance path={:?}",
            watched_path
        );
        return Some(FsEvent {
            paths: vec![watched_path.to_path_buf()],
        });
    }

    let paths = result
        .files?
        .into_iter()
        .map(|file| watched_path.join(file.name.as_path()))
        .collect::<Vec<_>>();

    if paths.is_empty() {
        return None;
    }

    Some(FsEvent { paths })
}

fn run_watchman_backend(
    command_rx: Receiver<WatchmanCommand>,
    fs_event_tx: Sender<Result<FsEvent>>,
    ready_tx: Sender<Result<()>>,
) -> Result<()> {
    let runtime = Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed building Tokio runtime for Watchman backend")?;
    let mut state = match runtime.block_on(WatchmanState::connect(fs_event_tx)) {
        Ok(state) => {
            ready_tx.send(Ok(())).ok();
            state
        }
        Err(error) => {
            ready_tx.send(Err(error)).ok();
            return Ok(());
        }
    };

    for command in command_rx {
        runtime.block_on(state.handle_command(command))?;
    }

    runtime.block_on(state.shutdown());
    Ok(())
}

fn main() -> Result<()> {
    env_logger::init();

    let backend = Backend::detect();
    let (fs_event_tx, fs_event_rx) = channel();

    let stdout = stdout();
    let stdout = stdout.lock();

    info!(
        "component=file_watcher event=backend_selected backend={:?}",
        backend
    );

    match backend {
        Backend::Notify => {
            let watcher = new_notify_watcher(fs_event_tx)?;
            run_monitor(Monitor::new(watcher, stdout), fs_event_rx)
        }
        Backend::Watchman => {
            let watcher = WatchmanWatcher::new(fs_event_tx)?;
            run_monitor(Monitor::new(watcher, stdout), fs_event_rx)
        }
    }
}

fn run_monitor<W: Watch>(
    mut monitor: Monitor<W, StdoutLock<'_>>,
    fs_event_rx: Receiver<Result<FsEvent>>,
) -> Result<()> {
    let (event_tx, event_rx) = channel();

    let stdin_tx = event_tx.clone();
    thread::spawn(move || -> Result<()> {
        let stdin = stdin();
        let mut handle = stdin.lock();

        loop {
            let mut input = String::new();
            if handle.read_line(&mut input)? == 0 {
                return Ok(());
            }
            stdin_tx.send(MonitorEvent::Input(input))?;
        }
    });

    thread::spawn(move || -> Result<()> {
        for result in fs_event_rx {
            match result {
                Ok(event) => event_tx.send(MonitorEvent::FsEvent(event))?,
                Err(error) => error!(
                    "component=file_watcher event=backend_error error={} error_chain={:#}",
                    error, error
                ),
            }
        }

        Ok(())
    });

    for event in event_rx {
        if let Err(error) = monitor.handle_event_ref(&event) {
            let event_kind = match &event {
                MonitorEvent::Input(_) => "input",
                MonitorEvent::FsEvent(_) => "fs_event",
            };
            error!(
                "component=monitor event=handle_event_failure event_kind={} event_payload={:?} error={} error_chain={:#}",
                event_kind, event, error, error
            );
        }
    }

    Ok(())
}

fn new_notify_watcher(fs_event_tx: Sender<Result<FsEvent>>) -> Result<RecommendedWatcher> {
    RecommendedWatcher::new(
        move |result: notify::Result<notify::Event>| match result {
            Ok(event) => {
                let fs_event = FsEvent { paths: event.paths };
                if fs_event_tx.send(Ok(fs_event)).is_err() {
                    error!("File watcher channel closed.");
                }
            }
            Err(error) => {
                if fs_event_tx.send(Err(error.into())).is_err() {
                    error!("File watcher channel closed.");
                }
            }
        },
        Config::default(),
    )
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::{Backend, ChangedFile, watchman_event};
    use std::path::{Path, PathBuf};
    use unison_fsmonitor::FsEvent;
    use watchman_client::fields::{ExistsField, NameField};
    use watchman_client::pdu::{Clock, ClockSpec, QueryResult};

    #[test]
    fn backend_uses_watchman_when_available() {
        assert!(matches!(
            Backend::from_watchman_availability(true),
            Backend::Watchman
        ));
    }

    #[test]
    fn backend_falls_back_to_notify_when_watchman_missing() {
        assert!(matches!(
            Backend::from_watchman_availability(false),
            Backend::Notify
        ));
    }

    #[test]
    fn watchman_event_maps_changed_files_to_absolute_paths() {
        let watched_path = Path::new("/tmp/project");
        let result = query_result(vec![
            PathBuf::from("src/main.rs"),
            PathBuf::from("README.md"),
        ]);

        assert_eq!(
            watchman_event(watched_path, result),
            Some(FsEvent {
                paths: vec![
                    PathBuf::from("/tmp/project/src/main.rs"),
                    PathBuf::from("/tmp/project/README.md"),
                ],
            })
        );
    }

    #[test]
    fn watchman_event_ignores_empty_change_batches() {
        let watched_path = Path::new("/tmp/project");

        assert_eq!(watchman_event(watched_path, query_result(vec![])), None);
    }

    #[test]
    fn watchman_event_marks_fresh_instance_as_root_change() {
        let watched_path = Path::new("/tmp/project");
        let mut result = query_result(vec![PathBuf::from("src/main.rs")]);
        result.is_fresh_instance = true;

        assert_eq!(
            watchman_event(watched_path, result),
            Some(FsEvent {
                paths: vec![PathBuf::from("/tmp/project")],
            })
        );
    }

    fn query_result(files: Vec<PathBuf>) -> QueryResult<ChangedFile> {
        QueryResult {
            version: "test".to_string(),
            is_fresh_instance: false,
            files: Some(
                files
                    .into_iter()
                    .map(|path| ChangedFile {
                        name: NameField::new(path),
                        exists: ExistsField::new(true),
                    })
                    .collect(),
            ),
            clock: Clock::Spec(ClockSpec::default()),
            state_enter: None,
            state_leave: None,
            state_metadata: None,
            saved_state_info: None,
            debug: None,
        }
    }
}
