use anyhow::Result;
use log::error;
use notify::{Config, RecommendedWatcher, Watcher};
use std::io::{BufRead, stdin, stdout};
use std::sync::mpsc::channel;
use std::thread;
use unison_fsmonitor::{Monitor, MonitorEvent};

fn main() -> Result<()> {
    env_logger::init();

    let (fs_event_tx, fs_event_rx) = channel();
    let watcher: RecommendedWatcher = RecommendedWatcher::new(
        move |result| {
            if fs_event_tx.send(result).is_err() {
                error!("File watcher channel closed.");
            }
        },
        Config::default(),
    )?;

    let stdout = stdout();
    let stdout = stdout.lock();
    let mut monitor = Monitor::new(watcher, stdout);

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
                Err(error) => error!("File watcher error: {}", error),
            }
        }

        Ok(())
    });

    for event in event_rx {
        if let Err(error) = monitor.handle_event(event) {
            error!("Error handling event: {}", error);
        }
    }

    Ok(())
}
