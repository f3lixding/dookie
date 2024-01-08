use crate::Envelope;
use prost::Message;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::net::UnixListener;
use tokio::task::{self, JoinError, JoinHandle};

pub const CLI_SOCKET_PATH: &str = "/tmp/dookie.sock";

#[allow(dead_code)]
pub struct Unassigned;

#[allow(dead_code)]
pub struct Assigned;

#[derive(Debug, Clone)]
pub struct SenderBundle {}

unsafe impl Send for SenderBundle {}
unsafe impl Sync for SenderBundle {}

#[derive(Debug, Default)]
pub struct MainListener<Status = Unassigned> {
    handle: Option<JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync + 'static>>>>,
    bundle: Option<SenderBundle>,
    _status: std::marker::PhantomData<Status>,
}

impl MainListener<Unassigned> {
    pub fn assign_sender_bundle(self, bundle: SenderBundle) -> MainListener<Assigned> {
        MainListener {
            handle: self.handle,
            bundle: Some(bundle),
            _status: std::marker::PhantomData,
        }
    }
}

impl MainListener<Assigned> {
    pub fn initiate_listener(self) -> MainListener<Assigned> {
        // This is safe because otherwise we would not be in this state and thus this function
        // would not be callable.
        let handle = task::spawn(async move {
            let bundle = self.bundle.unwrap();
            let listener = UnixListener::bind(CLI_SOCKET_PATH)?;

            loop {
                let (stream, _addr) = listener.accept().await?;
                let bundle = bundle.clone();
                let mut data = vec![0; 1024];
                let n = stream.try_read(&mut data)?;
                let envelope = Envelope::decode(&data[..n])?;
            }

            #[allow(unreachable_code)]
            Ok(())
        });

        MainListener {
            handle: Some(handle),
            bundle: None,
            _status: std::marker::PhantomData,
        }
    }
}

impl Future for MainListener<Assigned> {
    type Output = Result<Result<(), Box<dyn std::error::Error + Send + Sync + 'static>>, JoinError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // We can unwrap here because we would not want to proceed further if there is nothing in
        // this Option.
        let handle = self.get_mut().handle.as_mut().unwrap();
        // Safety: We never move `handle` after it is pinned.
        let handle = Pin::new(handle);

        handle.poll(cx)
    }
}
