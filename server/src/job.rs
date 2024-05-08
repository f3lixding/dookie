use super::config::Config;
use crate::{IBundleClient, MediaBundle};
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
///
/// This trait is made (as opposed to a concrete SpawnJob) because I want allow each job to have
/// the freedom to choose how it is to be constructed / equipped.
/// For example, a "scan job" would need to have a reference to the client with which to issue the
/// scan (i.e. this is the plex client). This would mean the concrete type would need to be
/// imported.
pub trait SpawnedJobType<R, I, O>: Future {
    fn give_sender(&mut self) -> Result<Sender<(I, Option<OneShotSender<O>>)>, Box<dyn Error>>;
}

// TODO: merge the following into SpawnedJobType.
// This is because we would need the info here to be implemented by their own Job
// It has the following requirements:
// - give_sender: Returns a Sender that can be used to send the reply
// - poll: This makes the job pollable.
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
    type SpawnedJob: SpawnedJobType<Self::ReturnType, Self::IncomingMessage, Self::OutgoingMessage>;

    fn spawn_<C: IBundleClient>(
        config: &Config,
        media_bundle: Option<MediaBundle<C>>,
        front_desk_handle: &mut Option<
            JoinHandle<Result<(), Box<dyn Error + Send + Sync + 'static>>>,
        >,
    ) -> Result<Self::SpawnedJob, Box<dyn Error>>;

    fn spawn<C: IBundleClient>(
        config: &Config,
        media_bundle: Option<MediaBundle<C>>,
    ) -> Result<Self::SpawnedJob, Box<dyn Error>> {
        static mut NONE: Option<JoinHandle<Result<(), Box<dyn Error + Send + Sync + 'static>>>> =
            None;
        // SAFETY: we'll never do anything with option here under non-test target.
        unsafe { Self::spawn_(config, media_bundle, &mut NONE) }
    }
}
