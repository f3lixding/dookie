use super::config::Config;
use std::error::Error;
use tokio::sync::mpsc::Receiver;
use tokio::task::{self, JoinHandle};

/// This is a trait for all jobs to be polled in the main loop.
/// The purpose of this trait is to provide an uniform interface for spawning jobs.
pub trait Job {
    type Message;
    type ReturnType: Send + Sync;

    fn spawn(
        config: &Config,
        receiver: Receiver<Self::Message>,
    ) -> Result<JoinHandle<Self::ReturnType>, Box<dyn Error>>;
}
