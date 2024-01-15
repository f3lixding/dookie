use crate::envelope::Data;
use crate::move_job;
use crate::move_job::IncomingMessage;
use crate::DataType;
use crate::Job;
use crate::MoveJobCommand;
use crate::{media_bundle::MediaBundle, Envelope};
use prost::Message;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::AsyncWriteExt;
use tokio::io::Interest;
use tokio::net::UnixListener;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::oneshot;
use tokio::sync::oneshot::Sender as OneShotSender;
use tokio::task::{self, JoinError, JoinHandle};

pub const CLI_SOCKET_PATH: &str = "/tmp/dookie.sock";

// We are going to enforce a strict order for initialization of the listener.
// I am not sure if this is a good idea scalability wise, but it is good enough for now.
#[allow(dead_code)]
#[derive(Default)]
pub struct Unassigned;

#[allow(dead_code)]
pub struct NeedsMovejob;

#[allow(dead_code)]
pub struct Assigned;

#[allow(dead_code)]
pub struct Initiated;

#[derive(Debug, Default)]
pub struct MainListener<Status = Unassigned> {
    handle: Option<JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync + 'static>>>>,
    bundle: Option<MediaBundle>,
    move_job_sender: Option<
        Sender<(
            <move_job::JobStruct as Job>::IncomingMessage,
            Option<OneShotSender<<move_job::JobStruct as Job>::OutgoingMessage>>,
        )>,
    >,
    _status: std::marker::PhantomData<Status>,
}

impl MainListener<Unassigned> {
    pub fn assign_sender_bundle(self, bundle: MediaBundle) -> MainListener<NeedsMovejob> {
        MainListener {
            handle: self.handle,
            bundle: Some(bundle),
            move_job_sender: None,
            _status: std::marker::PhantomData,
        }
    }
}

impl MainListener<NeedsMovejob> {
    pub fn assign_movejob_sender(
        self,
        move_job_sender: Sender<(
            <move_job::JobStruct as Job>::IncomingMessage,
            Option<OneShotSender<<move_job::JobStruct as Job>::OutgoingMessage>>,
        )>,
    ) -> MainListener<Assigned> {
        MainListener {
            handle: self.handle,
            bundle: self.bundle,
            move_job_sender: Some(move_job_sender),
            _status: std::marker::PhantomData,
        }
    }
}

impl MainListener<Assigned> {
    pub fn initiate_listener(self) -> MainListener<Initiated> {
        // This is safe because otherwise we would not be in this state and thus this function
        // would not be callable.
        let handle = task::spawn(async move {
            let bundle = self.bundle.unwrap();
            let move_job_sender = self.move_job_sender.unwrap();
            let listener = UnixListener::bind(CLI_SOCKET_PATH)?;

            // There is a problem with this set up: the listener is not accepting connections until
            // the previous request has been dealt with. This works well enough for a local set up
            // though. So I am going to leave it as it is for now.
            let mut data = vec![0; 1024];
            loop {
                let bundle = bundle.clone();
                let move_job_sender = move_job_sender.clone();
                let (mut stream, _addr) = listener.accept().await?;
                stream.readable().await?;
                let n = match stream.try_read(&mut data) {
                    Ok(n) => {
                        if n > 0 {
                            println!("read {} bytes", n);
                            n
                        } else {
                            println!("read 0 bytes");
                            continue;
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        continue;
                    }
                    Err(e) => {
                        return Err(e.into());
                    }
                };
                let envelope = Envelope::decode(&data[..n])?;

                let resp: Option<Envelope> = match envelope.data_type() {
                    DataType::Movejobrequest => {
                        let Data::MoveJobRequest(data) = envelope.data.ok_or("no data")? else {
                            // TODO: log this
                            continue;
                        };

                        let (sender, receiver) = oneshot::channel();
                        match data.command() {
                            MoveJobCommand::Statusrequest => {
                                tracing::info!("Received status request");
                                move_job_sender
                                    .send((IncomingMessage::StatusRequest, Some(sender)))
                                    .await?;
                                let resp = receiver.await?;

                                Some(resp.into())
                            }
                            MoveJobCommand::Startjob => {
                                move_job_sender
                                    .send((IncomingMessage::Start, Some(sender)))
                                    .await?;
                                let resp = receiver.await?;

                                Some(resp.into())
                            }
                        }
                    }
                    _ => {
                        // Whatever is here is not yet supported
                        // TODO: log this
                        None
                    }
                };

                println!("Response: {:?}", resp);

                // Send the response back via UDS
                stream.ready(Interest::WRITABLE).await?;
                let buf = resp.unwrap_or_default().encode_to_vec();
                println!("Write {} bytes", buf.len());
                stream.write_all(buf.as_slice()).await?;
            }

            #[allow(unreachable_code)]
            Ok(())
        });

        MainListener {
            handle: Some(handle),
            bundle: None,
            move_job_sender: None,
            _status: std::marker::PhantomData,
        }
    }
}

impl Future for MainListener<Initiated> {
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
