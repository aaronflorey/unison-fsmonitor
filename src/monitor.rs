use crate::protocol::{encode, parse_input};
use anyhow::{Context, Result, bail};
use log::{debug, info};
use notify::{Event as NotifyEvent, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum MonitorEvent {
    Input(String),
    FsEvent(NotifyEvent),
}

pub trait Watch {
    fn watch(&mut self, _path: &Path, _recursive_mode: RecursiveMode) -> Result<()> {
        Ok(())
    }

    fn unwatch(&mut self, _path: &Path) -> Result<()> {
        Ok(())
    }
}

impl Watch for RecommendedWatcher {
    fn watch(&mut self, path: &Path, recursive_mode: RecursiveMode) -> Result<()> {
        Ok(NotifyWatcher::watch(self, path, recursive_mode)?)
    }

    fn unwatch(&mut self, path: &Path) -> Result<()> {
        Ok(NotifyWatcher::unwatch(self, path)?)
    }
}

type ReplicaId = String;

#[derive(Debug)]
struct Replica {
    root: PathBuf,
    paths: HashSet<PathBuf>,
    pending_changes: HashSet<PathBuf>,
    waited_on: bool,
}

impl Replica {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            paths: HashSet::new(),
            pending_changes: HashSet::new(),
            waited_on: false,
        }
    }

    fn is_watching(&self, path: &Path) -> bool {
        self.paths.iter().any(|base| path.starts_with(base))
    }
}

pub struct Monitor<WATCH: Watch, WRITE: Write> {
    current_path: PathBuf,
    replicas: HashMap<ReplicaId, Replica>,
    link_map: HashMap<PathBuf, HashSet<PathBuf>>,
    watcher: WATCH,
    writer: WRITE,
}

impl<WATCH: Watch, WRITE: Write> Monitor<WATCH, WRITE> {
    pub fn new(watcher: WATCH, writer: WRITE) -> Self {
        Self {
            current_path: PathBuf::new(),
            replicas: HashMap::new(),
            link_map: HashMap::new(),
            watcher,
            writer,
        }
    }

    pub fn handle_event(&mut self, event: MonitorEvent) -> Result<()> {
        debug!("event: {:?}", event);

        match event {
            MonitorEvent::Input(input) => self.handle_input(&input),
            MonitorEvent::FsEvent(event) => self.handle_fs_event(event),
        }
    }

    fn handle_input(&mut self, input: &str) -> Result<()> {
        let (command, args) = parse_input(input);

        if command != "WAIT" {
            self.clear_wait_state();
        }

        match command.as_str() {
            "VERSION" => self.handle_version(&args),
            "START" => self.handle_start(&args),
            "DIR" => {
                self.send_ack();
                Ok(())
            }
            "LINK" => self.handle_link(&args),
            "WAIT" => self.handle_wait(&args),
            "CHANGES" => self.handle_changes(&args),
            "RESET" => self.handle_reset(&args),
            "DEBUG" | "DONE" => Ok(()),
            _ => self.send_error(&format!("Unrecognized cmd: {}", command)),
        }
    }

    fn handle_fs_event(&mut self, event: NotifyEvent) -> Result<()> {
        let mut matched_replica_ids = HashSet::new();

        for path in self.paths_for_event(&event) {
            for (id, replica) in &mut self.replicas {
                if let Ok(relative_path) = path.strip_prefix(&replica.root) {
                    matched_replica_ids.insert(id.clone());
                    replica.pending_changes.insert(relative_path.into());
                }
            }
        }

        if matched_replica_ids.is_empty() {
            info!("No replica found for event.");
        }

        for id in &matched_replica_ids {
            if self
                .replicas
                .get(id)
                .is_some_and(|replica| replica.waited_on)
            {
                self.send_changes(id);
            }
        }

        Ok(())
    }

    fn clear_wait_state(&mut self) {
        for replica in self.replicas.values_mut() {
            replica.waited_on = false;
        }
    }

    fn handle_version(&mut self, args: &[String]) -> Result<()> {
        let version = args.first().context("Missing protocol version")?;
        if version != "1" {
            bail!("Unexpected version: {:?}", version);
        }

        self.send_cmd("VERSION", &["1"]);
        Ok(())
    }

    fn handle_start(&mut self, args: &[String]) -> Result<()> {
        let replica_id = args
            .first()
            .context("Missing replica id for START")?
            .clone();
        let root = PathBuf::from(args.get(1).context("Missing root path for START")?);
        let current_path = match args.get(2) {
            Some(dir) => root.join(dir),
            None => root.clone(),
        };
        self.current_path = current_path.clone();

        let should_watch = self
            .replicas
            .get(&replica_id)
            .is_none_or(|replica| !replica.is_watching(&current_path));
        if should_watch {
            self.watcher
                .watch(&current_path, RecursiveMode::Recursive)?;
        }

        let replica = self
            .replicas
            .entry(replica_id)
            .or_insert_with(|| Replica::new(root));
        replica.paths.insert(current_path);

        debug!("replicas: {:?}", self.replicas);
        self.send_ack();
        Ok(())
    }

    fn handle_link(&mut self, args: &[String]) -> Result<()> {
        let path = match args.first() {
            Some(path) => self.current_path.join(path),
            None => self.current_path.clone(),
        };
        let realpath = path
            .canonicalize()
            .with_context(|| format!("Unable to canonicalize path={:?}", path))?;

        self.watcher.watch(&realpath, RecursiveMode::Recursive)?;
        self.link_map.entry(realpath).or_default().insert(path);
        debug!("link_map: {:?}", self.link_map);
        self.send_ack();
        Ok(())
    }

    fn handle_wait(&mut self, args: &[String]) -> Result<()> {
        let replica_id = args.first().context("Missing replica id for WAIT")?;

        if let Some(replica) = self.replicas.get_mut(replica_id) {
            replica.waited_on = true;
            if !replica.pending_changes.is_empty() {
                self.send_changes(replica_id);
            }
            return Ok(());
        }

        self.send_error(&format!("Unknown replica: {}", replica_id))
    }

    fn handle_changes(&mut self, args: &[String]) -> Result<()> {
        let replica_id = args.first().context("Missing replica id for CHANGES")?;
        let mut changed_paths = HashSet::new();

        if let Some(replica) = self.replicas.get_mut(replica_id) {
            changed_paths.extend(replica.pending_changes.drain());
        }

        for path in changed_paths {
            self.send_recursive(&path);
        }
        self.send_done();
        Ok(())
    }

    fn handle_reset(&mut self, args: &[String]) -> Result<()> {
        let replica_id = args.first().context("Missing replica id for RESET")?;

        if let Some(replica) = self.replicas.remove(replica_id) {
            for path in replica.paths {
                if !self.is_watching(&path) {
                    self.watcher.unwatch(&path)?;
                }
            }
        }

        debug!("replicas: {:?}", self.replicas);
        Ok(())
    }

    fn is_watching(&self, path: &Path) -> bool {
        self.replicas
            .values()
            .any(|replica| replica.is_watching(path))
    }

    fn paths_for_event(&self, event: &NotifyEvent) -> Vec<PathBuf> {
        let mut paths = event.paths.clone();

        for path in &event.paths {
            for (realpath, links) in &self.link_map {
                if let Ok(postfix) = path.strip_prefix(realpath) {
                    for link in links {
                        paths.push(link.join(postfix));
                    }
                }
            }
        }

        paths
    }

    fn send_cmd(&mut self, command: &str, args: &[&str]) {
        let mut output = command.to_owned();
        for arg in args {
            output.push(' ');
            output.push_str(&encode(arg));
        }

        debug!(">> {}", output);
        let _ = writeln!(self.writer, "{}", output);
    }

    fn send_ack(&mut self) {
        self.send_cmd("OK", &[]);
    }

    fn send_changes(&mut self, replica: &str) {
        self.send_cmd("CHANGES", &[replica]);
    }

    fn send_recursive(&mut self, path: &Path) {
        self.send_cmd("RECURSIVE", &[&path.to_string_lossy()]);
    }

    fn send_done(&mut self) {
        self.send_cmd("DONE", &[]);
    }

    fn send_error(&mut self, message: &str) -> ! {
        self.send_cmd("ERROR", &[message]);
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{Monitor, MonitorEvent, Watch};
    use notify::{
        Event as NotifyEvent,
        event::{CreateKind, EventKind},
    };
    use std::io::Cursor;
    use std::path::PathBuf;

    #[derive(Default)]
    struct TestWatcher;

    impl Watch for TestWatcher {}

    type TestMonitor = Monitor<TestWatcher, Cursor<Vec<u8>>>;

    fn new_monitor() -> TestMonitor {
        Monitor::new(TestWatcher, Cursor::new(vec![]))
    }

    fn written_lines(monitor: &mut TestMonitor) -> Vec<String> {
        std::str::from_utf8(monitor.writer.get_ref())
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn create_event(path: PathBuf) -> NotifyEvent {
        NotifyEvent::new(EventKind::Create(CreateKind::Any)).add_path(path)
    }

    #[test]
    fn test_version() {
        let mut monitor = new_monitor();

        monitor
            .handle_event(MonitorEvent::Input("VERSION 1\n".into()))
            .unwrap();

        assert_eq!(written_lines(&mut monitor), vec!["VERSION 1".to_owned()]);
    }

    #[test]
    fn test_start() {
        let mut monitor = new_monitor();
        let id = "123";
        let root = PathBuf::from("/tmp/sample");

        monitor
            .handle_event(MonitorEvent::Input(format!(
                "START {} {}\n",
                id,
                root.to_string_lossy()
            )))
            .unwrap();

        assert_eq!(monitor.replicas.len(), 1);
        assert!(monitor.replicas.contains_key(id));
        assert_eq!(monitor.replicas.get(id).unwrap().root, root);
        assert!(monitor.replicas.get(id).unwrap().paths.contains(&root));
        assert_eq!(written_lines(&mut monitor), vec!["OK".to_owned()]);
    }

    #[test]
    fn test_start_with_subdir() {
        let mut monitor = new_monitor();
        let id = "123";
        let root = PathBuf::from("/tmp/sample");
        let subdir = PathBuf::from("subdir");

        monitor
            .handle_event(MonitorEvent::Input(format!(
                "START {} {} {}\n",
                id,
                root.to_string_lossy(),
                subdir.to_string_lossy()
            )))
            .unwrap();

        assert_eq!(monitor.replicas.len(), 1);
        assert!(monitor.replicas.contains_key(id));
        assert_eq!(monitor.replicas.get(id).unwrap().root, root);
        assert!(
            monitor
                .replicas
                .get(id)
                .unwrap()
                .paths
                .contains(&root.join(&subdir))
        );
        assert_eq!(written_lines(&mut monitor), vec!["OK".to_owned()]);
    }

    #[test]
    fn test_dir() {
        let mut monitor = new_monitor();

        monitor
            .handle_event(MonitorEvent::Input("DIR\n".into()))
            .unwrap();

        assert_eq!(written_lines(&mut monitor), vec!["OK".to_owned()]);
    }

    #[test]
    fn test_dir_with_dir() {
        let mut monitor = new_monitor();

        monitor
            .handle_event(MonitorEvent::Input("DIR dir\n".into()))
            .unwrap();

        assert_eq!(written_lines(&mut monitor), vec!["OK".to_owned()]);
    }

    #[test]
    fn test_follow_link() {
        let mut monitor = new_monitor();
        let id = "123";
        let root = PathBuf::from("/usr/bin");
        let file = PathBuf::from("env");

        monitor
            .handle_event(MonitorEvent::Input(format!(
                "START {} {} {}\n",
                id,
                root.to_string_lossy(),
                file.to_string_lossy()
            )))
            .unwrap();
        monitor
            .handle_event(MonitorEvent::Input("LINK\n".into()))
            .unwrap();

        assert_eq!(
            written_lines(&mut monitor),
            vec!["OK".to_owned(), "OK".to_owned()]
        );
    }

    #[test]
    fn test_changes() {
        let mut monitor = new_monitor();
        let id = "123";
        let root = "/tmp/sample";
        let filename = "filename";

        monitor
            .handle_event(MonitorEvent::Input(format!("START {} {}\n", id, root)))
            .unwrap();
        monitor
            .handle_event(MonitorEvent::FsEvent(create_event(
                PathBuf::from(root).join(filename),
            )))
            .unwrap();
        monitor
            .handle_event(MonitorEvent::Input(format!("WAIT {}\n", id)))
            .unwrap();
        monitor
            .handle_event(MonitorEvent::Input(format!("CHANGES {}\n", id)))
            .unwrap();

        assert_eq!(
            written_lines(&mut monitor),
            vec![
                "OK".to_owned(),
                format!("CHANGES {}", id),
                format!("RECURSIVE {}", filename),
                "DONE".to_owned()
            ]
        );
    }

    #[test]
    fn test_changes_after_wait() {
        let mut monitor = new_monitor();
        let id = "123";
        let root = "/tmp/sample";
        let filename = "filename";

        monitor
            .handle_event(MonitorEvent::Input(format!("START {} {}\n", id, root)))
            .unwrap();
        monitor
            .handle_event(MonitorEvent::Input(format!("WAIT {}\n", id)))
            .unwrap();
        monitor
            .handle_event(MonitorEvent::FsEvent(create_event(
                PathBuf::from(root).join(filename),
            )))
            .unwrap();
        monitor
            .handle_event(MonitorEvent::Input(format!("CHANGES {}\n", id)))
            .unwrap();

        assert_eq!(
            written_lines(&mut monitor),
            vec![
                "OK".to_owned(),
                format!("CHANGES {}", id),
                format!("RECURSIVE {}", filename),
                "DONE".to_owned()
            ]
        );
    }

    #[test]
    fn test_changes_with_subdir() {
        let mut monitor = new_monitor();
        let id = "123";
        let root = "/tmp/sample";
        let subdir = "subdir";
        let filename = "filename";

        monitor
            .handle_event(MonitorEvent::Input(format!(
                "START {} {} {}\n",
                id, root, subdir
            )))
            .unwrap();
        monitor
            .handle_event(MonitorEvent::FsEvent(create_event(
                PathBuf::from(root).join(subdir).join(filename),
            )))
            .unwrap();
        monitor
            .handle_event(MonitorEvent::Input(format!("WAIT {}\n", id)))
            .unwrap();
        monitor
            .handle_event(MonitorEvent::Input(format!("CHANGES {}\n", id)))
            .unwrap();

        assert_eq!(
            written_lines(&mut monitor),
            vec![
                "OK".to_owned(),
                format!("CHANGES {}", id),
                format!("RECURSIVE {}%2F{}", subdir, filename),
                "DONE".to_owned(),
            ]
        );
    }

    #[test]
    fn test_changes_no_wait() {
        let mut monitor = new_monitor();
        let id = "123";
        let root = "/tmp/sample";
        let filename = "filename";

        monitor
            .handle_event(MonitorEvent::Input(format!("START {} {}\n", id, root)))
            .unwrap();
        monitor
            .handle_event(MonitorEvent::FsEvent(create_event(
                PathBuf::from(root).join(filename),
            )))
            .unwrap();

        assert_eq!(written_lines(&mut monitor), vec!["OK".to_owned()]);
    }
}
