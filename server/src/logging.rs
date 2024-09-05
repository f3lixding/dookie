use std::{
    error::Error,
    future::Future,
    path::PathBuf,
    pin::Pin,
    task::{Context, Poll},
    time::{self, SystemTime, UNIX_EPOCH},
};
use tokio::task;
use tokio::task::JoinError;
use tokio::task::JoinHandle;
use tracing::{level_filters::LevelFilter, Level};
use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{RollingFileAppender, Rotation},
};
use tracing_subscriber::{fmt, prelude::*};

use crate::Config;

#[allow(dead_code)]
#[derive(Default)]
pub struct Unprimed;

#[allow(dead_code)]
pub struct Primed;

pub type RetentionControlHandleReturn = Result<(), Box<dyn Error + Send + Sync>>;
#[derive(Default)]
pub struct Logger<Status = Unprimed> {
    retention_control_handle: Option<JoinHandle<RetentionControlHandleReturn>>,
    log_path: PathBuf,
    _status: std::marker::PhantomData<Status>,
}

impl Logger<Unprimed> {
    pub fn from_config(config: &Config) -> Logger<Unprimed> {
        Logger {
            retention_control_handle: None,
            log_path: config.log_path.clone().into_owned().into(),
            _status: std::marker::PhantomData,
        }
    }
}

impl Logger<Unprimed> {
    pub fn prime(self) -> (Logger<Primed>, WorkerGuard, tokio::sync::mpsc::Sender<()>) {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);

        // Sets up global registry for logging
        let file_appender =
            RollingFileAppender::new(Rotation::DAILY, &self.log_path, "dookied.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        tracing_subscriber::registry()
            .with(tracing_subscriber::filter::LevelFilter::INFO)
            .with(fmt::layer().with_writer(non_blocking))
            .init();
        let path = self.log_path.clone();

        let handle = task::spawn(async move {
            // Retention rate of 7 days
            let threshold = 60 * 60 * 24 * 7;
            loop {
                let mut files = tokio::fs::read_dir(&path).await?;
                let now = SystemTime::now().duration_since(UNIX_EPOCH)?;
                while let Ok(Some(file)) = files.next_entry().await {
                    let metadata = tokio::fs::metadata(file.path()).await?;
                    let age = now.saturating_sub(metadata.modified()?.duration_since(UNIX_EPOCH)?);
                    if age.as_secs() > threshold {
                        tokio::fs::remove_file(file.path()).await?;
                        tracing::info!("Log {} deleted", file.file_name().to_str().unwrap());
                    }
                }

                tokio::select! {
                    _ = tokio::time::sleep(time::Duration::from_secs(60 * 60 * 24)) => {
                        tracing::info!("Log deletion check routine initiated");
                    }
                    _ = rx.recv() => {
                        // TODO: terminate loop and log here
                        tracing::info!("Log deletion routine terminated");
                        break;
                    }
                }
            }

            #[allow(unreachable_code)]
            Ok(())
        });

        let logger = Logger {
            retention_control_handle: Some(handle),
            log_path: self.log_path,
            _status: std::marker::PhantomData,
        };

        (logger, guard, tx)
    }
}

impl Future for Logger<Primed> {
    type Output = Result<RetentionControlHandleReturn, JoinError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // We can unwrap here because we would not want to proceed further if there is nothing in
        // this Option.
        let handle = self.get_mut().retention_control_handle.as_mut().unwrap();
        // Safety: We never move `handle` after it is pinned.
        let handle = Pin::new(handle);

        handle.poll(cx)
    }
}
