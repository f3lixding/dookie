use super::config::Config;
use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc::Sender;
use tokio::task::{JoinError, JoinHandle};

/// This is what gets returned from spawninig a job.
/// This struct is made to faciliate manging a job after it is spawned.
/// It can be polled directly since Future is implemented for it.
pub struct SpawnedJob<R, I, O> {
    handle: JoinHandle<R>,
    sender: Option<Sender<(I, Option<Sender<O>>)>>,
}

impl<R, I, O> SpawnedJob<R, I, O> {
    pub fn new(handle: JoinHandle<R>, sender: Sender<(I, Option<Sender<O>>)>) -> Self {
        Self {
            handle,
            sender: Some(sender),
        }
    }

    pub fn give_sender(&mut self) -> Result<Sender<(I, Option<Sender<O>>)>, Box<dyn Error>> {
        self.sender
            .take()
            .ok_or("No sender stored in this spawned job".into())
    }
}

impl<R, I, O> Future for SpawnedJob<R, I, O> {
    type Output = Result<R, JoinError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: We never move `handle` after it is pinned.
        let handle = Pin::new(&mut self.get_mut().handle);

        handle.poll(cx)
    }
}

/// This is a trait for all jobs to be polled in the main loop.
/// The purpose of this trait is to provide an uniform interface for spawning jobs.
pub trait Job {
    type OutgoingMessage;
    type IncomingMessage;
    type ReturnType: Send + Sync;

    fn spawn(
        config: &Config,
    ) -> Result<
        SpawnedJob<Self::ReturnType, Self::IncomingMessage, Self::OutgoingMessage>,
        Box<dyn Error>,
    >;
}
