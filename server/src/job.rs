use super::config::Config;
use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::sync::mpsc::Sender;
use tokio::sync::oneshot::Sender as OneShotSender;
use tokio::task::{JoinError, JoinHandle};

/// This is what gets returned from spawninig a job.
/// This struct is made to faciliate manging a job after it is spawned.
/// It can be polled directly since Future is implemented for it.
/// Note that the message type of (I, Option<Sender<O>>) is a tuple of incoming message and sender
/// for a reply.
pub struct SpawnedJob<R, I, O> {
    handle: JoinHandle<R>,
    sender: Option<Sender<(I, Option<OneShotSender<O>>)>>,
}

impl<R, I, O> SpawnedJob<R, I, O> {
    pub fn new(handle: JoinHandle<R>, sender: Sender<(I, Option<OneShotSender<O>>)>) -> Self {
        Self {
            handle,
            sender: Some(sender),
        }
    }

    pub fn give_sender(&mut self) -> Result<Sender<(I, Option<OneShotSender<O>>)>, Box<dyn Error>> {
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

    fn spawn_(
        config: &Config,
        front_desk_handle: &mut Option<
            JoinHandle<Result<(), Box<dyn Error + Send + Sync + 'static>>>,
        >,
    ) -> Result<
        SpawnedJob<Self::ReturnType, Self::IncomingMessage, Self::OutgoingMessage>,
        Box<dyn Error>,
    >;

    #[cfg(not(test))]
    fn spawn(
        config: &Config,
    ) -> Result<
        SpawnedJob<Self::ReturnType, Self::IncomingMessage, Self::OutgoingMessage>,
        Box<dyn Error>,
    > {
        Self::spawn_(config, &mut None)
    }
}
