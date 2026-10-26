use crate::envelope::Data;
use crate::move_job;
use crate::move_job::IncomingMessage;
use crate::DataType;
use crate::IBundleClient;
use crate::Job;
use crate::MoveJobCommand;
use crate::{media_bundle::MediaBundle, Envelope};
use prost::Message;
use std::fs::{File, OpenOptions};
use std::future::Future;
use std::io;
use std::os::unix::fs::{FileExt, FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::Path;
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

/// Claim the CLI socket before starting jobs. Keep the returned lock file alive for the
/// process lifetime, and never unlink it: all instances must lock the same inode.
/// Like `UnixListener::bind`, this must run inside a Tokio runtime with I/O enabled.
/// On macOS a full live backlog can also refuse connections, so reclamation requires
/// a matching ownership record left by a previous lock-holding instance. Untracked
/// legacy sockets need one-time manual removal after stopping the old daemon.
/// Linux can reclaim untracked sockets after a refused connection.
pub fn bind_cli_socket(path: &Path) -> io::Result<(UnixListener, File)> {
    let mut lock_path = path.as_os_str().to_owned();
    lock_path.push(".lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(lock_path)?;
    if !lock.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "CLI socket lock path is not a regular file",
        ));
    }
    lock.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::AddrInUse,
            "another instance owns the CLI socket lock",
        ),
        std::fs::TryLockError::Error(error) => error,
    })?;

    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_socket() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "CLI socket path exists and is not a socket",
                ));
            }
            match probe_cli_socket(path) {
                Ok(()) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        "another instance is listening on the CLI socket",
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {}
                Err(error) => return Err(error),
            }
            let identity = socket_identity(&metadata);
            if cfg!(target_os = "macos") && !recorded_socket_matches(&lock, &identity)? {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!(
                        "cannot safely reclaim untracked CLI socket {} on macOS: a live \
                         listener with a full backlog can refuse connections; for one-time \
                         migration, stop the old daemon and manually remove the socket \
                         (leave the .lock file in place)",
                        path.display()
                    ),
                ));
            }
            // Fail closed if an older daemon (which does not take our lock) replaced it.
            let current = std::fs::symlink_metadata(path)?;
            if !current.file_type().is_socket() || socket_identity(&current) != identity {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "CLI socket path changed during startup",
                ));
            }
            std::fs::remove_file(path)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let listener = UnixListener::bind(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "CLI socket path changed after binding",
        ));
    }
    // Invalidate any previous record before writing. An interrupted/failed write is
    // not ownership evidence; on macOS the next startup will fail closed.
    let identity = socket_identity(&metadata);
    lock.set_len(0)?;
    lock.write_all_at(&identity, 0)?;
    lock.sync_all()?;
    Ok((listener, lock))
}

// A fixed-size, versioned record prevents unbounded reads of an existing lock file.
// ctime and its nanoseconds distinguish replacement sockets when an inode is reused.
fn socket_identity(metadata: &std::fs::Metadata) -> [u8; 40] {
    let mut identity = [0; 40];
    identity[..8].copy_from_slice(b"dookie01");
    for (chunk, value) in identity[8..].chunks_exact_mut(8).zip([
        metadata.dev().to_le_bytes(),
        metadata.ino().to_le_bytes(),
        metadata.ctime().to_le_bytes(),
        metadata.ctime_nsec().to_le_bytes(),
    ]) {
        chunk.copy_from_slice(&value);
    }
    identity
}

fn recorded_socket_matches(lock: &File, identity: &[u8; 40]) -> io::Result<bool> {
    if lock.metadata()?.len() != identity.len() as u64 {
        return Ok(false);
    }
    let mut recorded = [0; 40];
    lock.read_exact_at(&mut recorded, 0)?;
    Ok(recorded == *identity)
}

/// A full listen backlog must not hang startup. ECONNREFUSED alone proves staleness
/// on Linux, but not macOS; the caller also checks recorded ownership there.
/// In-progress, would-block, and all other errors fail closed on both platforms.
fn probe_cli_socket(path: &Path) -> io::Result<()> {
    let socket = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)?;
    socket.set_nonblocking(true)?;
    socket.connect(&socket2::SockAddr::unix(path)?)
}

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
pub struct MainListener<C: IBundleClient, Status = Unassigned> {
    handle: Option<JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync + 'static>>>>,
    bundle: Option<MediaBundle<C>>,
    move_job_sender: Option<
        Sender<(
            <move_job::JobStruct<C> as Job>::IncomingMessage,
            Option<OneShotSender<<move_job::JobStruct<C> as Job>::OutgoingMessage>>,
        )>,
    >,
    _status: std::marker::PhantomData<Status>,
}

impl<C> MainListener<C, Unassigned>
where
    C: IBundleClient,
{
    pub fn assign_sender_bundle(self, bundle: MediaBundle<C>) -> MainListener<C, NeedsMovejob> {
        MainListener {
            handle: self.handle,
            bundle: Some(bundle),
            move_job_sender: None,
            _status: std::marker::PhantomData,
        }
    }
}

impl<C: IBundleClient> MainListener<C, NeedsMovejob> {
    pub fn assign_movejob_sender(
        self,
        move_job_sender: Sender<(
            <move_job::JobStruct<C> as Job>::IncomingMessage,
            Option<OneShotSender<<move_job::JobStruct<C> as Job>::OutgoingMessage>>,
        )>,
    ) -> MainListener<C, Assigned> {
        MainListener {
            handle: self.handle,
            bundle: self.bundle,
            move_job_sender: Some(move_job_sender),
            _status: std::marker::PhantomData,
        }
    }
}

impl<C: IBundleClient> MainListener<C, Assigned> {
    pub fn initiate_listener(self, listener: UnixListener) -> MainListener<C, Initiated> {
        // This is safe because otherwise we would not be in this state and thus this function
        // would not be callable.
        let handle = task::spawn(async move {
            let bundle = self.bundle.unwrap();
            let move_job_sender = self.move_job_sender.unwrap();

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
                            n
                        } else {
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
                            tracing::error!("No data associated with MoveJobRequest");
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

                // Send the response back via UDS
                stream.ready(Interest::WRITABLE).await?;
                let buf = resp.unwrap_or_default().encode_to_vec();
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

impl<C: IBundleClient> Future for MainListener<C, Initiated> {
    type Output = Result<Result<(), Box<dyn std::error::Error + Send + Sync + 'static>>, JoinError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: the only thing that might not be Unpin here is the client. And we know that is
        // actually Unpin. I just did not want to introduce another trait bound for it.
        let handle = unsafe {
            // We can unwrap here because we would not want to proceed further if there is nothing in
            // this Option.
            self.get_unchecked_mut().handle.as_mut().unwrap()
        };
        // Safety: We never move `handle` after it is pinned.
        let handle = Pin::new(handle);

        handle.poll(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::os::unix::net::UnixListener as StdUnixListener;
    use std::sync::Barrier;
    use std::time::Duration;
    use tokio::net::UnixStream;

    async fn assert_connects(listener: &UnixListener, path: &Path) {
        tokio::time::timeout(Duration::from_secs(2), async {
            let (client, accepted) = tokio::join!(UnixStream::connect(path), listener.accept());
            client.unwrap();
            accepted.unwrap();
        })
        .await
        .expect("socket should accept a connection");
    }

    // Forking subprocess tests briefly inherit flock descriptors until exec. Keep
    // close-and-immediately-reacquire tests separate from those process launches.
    #[tokio::test]
    #[serial_test::serial(socket_lock_lifetime)]
    async fn recorded_stale_socket_can_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cli.sock");
        let (listener, lock) = bind_cli_socket(&path).unwrap();
        assert_connects(&listener, &path).await;
        let lock_path = directory.path().join("cli.sock.lock");
        let lock_inode = std::fs::metadata(&lock_path).unwrap().ino();
        assert_eq!(lock.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        drop(listener);
        // The lifetime lock still protects the socket even when no listener remains.
        assert_eq!(
            bind_cli_socket(&path).unwrap_err().kind(),
            io::ErrorKind::AddrInUse
        );
        drop(lock);

        let (listener, _lock) = bind_cli_socket(&path).unwrap();
        assert_eq!(std::fs::metadata(&lock_path).unwrap().ino(), lock_inode);
        assert_connects(&listener, &path).await;
    }

    #[tokio::test]
    #[serial_test::serial(socket_lock_lifetime)]
    async fn untracked_and_replaced_stale_sockets_follow_platform_policy() {
        for replaced in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("cli.sock");
            let lock_path = directory.path().join("cli.sock.lock");
            if replaced {
                let (listener, lock) = bind_cli_socket(&path).unwrap();
                drop(listener);
                drop(lock);
                // Keep the old inode allocated so replacement cannot reuse it.
                std::fs::rename(&path, directory.path().join("old.sock")).unwrap();
            }
            drop(StdUnixListener::bind(&path).unwrap());
            let identity = socket_identity(&std::fs::symlink_metadata(&path).unwrap());
            let previous_record = std::fs::read(&lock_path).unwrap_or_default();
            let result = bind_cli_socket(&path);
            if cfg!(target_os = "macos") {
                let error = result.unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
                assert!(error.to_string().contains("stop the old daemon"));
                assert!(error.to_string().contains("manually remove the socket"));
                assert_eq!(
                    socket_identity(&std::fs::symlink_metadata(&path).unwrap()),
                    identity
                );
                assert_eq!(std::fs::read(&lock_path).unwrap(), previous_record);
            } else {
                let (listener, _lock) = result.unwrap();
                assert_connects(&listener, &path).await;
            }
        }
    }

    #[tokio::test]
    async fn live_socket_without_lock_is_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cli.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let inode = std::fs::symlink_metadata(&path).unwrap().ino();

        assert_eq!(
            bind_cli_socket(&path).unwrap_err().kind(),
            io::ErrorKind::AddrInUse
        );
        assert_eq!(std::fs::symlink_metadata(&path).unwrap().ino(), inode);
        // Drain the successful liveness probe before testing a fresh connection.
        tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        assert_connects(&listener, &path).await;
    }

    #[tokio::test]
    async fn regular_file_is_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cli.sock");
        std::fs::write(&path, b"keep me").unwrap();

        assert_eq!(
            bind_cli_socket(&path).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"keep me");
    }

    #[tokio::test]
    async fn socket_symlink_and_dangling_symlink_are_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.sock");
        drop(StdUnixListener::bind(&target).unwrap());
        let path = directory.path().join("cli.sock");
        symlink(&target, &path).unwrap();

        for target_exists in [true, false] {
            if !target_exists {
                std::fs::remove_file(&target).unwrap();
            }
            assert_eq!(
                bind_cli_socket(&path).unwrap_err().kind(),
                io::ErrorKind::AlreadyExists
            );
            assert_eq!(std::fs::read_link(&path).unwrap(), target);
        }
    }

    #[tokio::test]
    async fn lock_symlink_is_not_followed() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        std::fs::write(&target, b"keep me").unwrap();
        let path = directory.path().join("cli.sock");
        let lock_path = directory.path().join("cli.sock.lock");
        symlink(&target, &lock_path).unwrap();

        assert!(bind_cli_socket(&path).is_err());
        assert!(!path.exists());
        assert_eq!(std::fs::read_link(&lock_path).unwrap(), target);
        assert_eq!(std::fs::read(&target).unwrap(), b"keep me");
    }

    #[tokio::test]
    async fn concurrent_instances_have_one_winner() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cli.sock");
        let barrier = Barrier::new(2);
        let runtime = tokio::runtime::Handle::current();
        let (first, second) = std::thread::scope(|scope| {
            let claim = || {
                let _entered = runtime.enter();
                barrier.wait();
                bind_cli_socket(&path)
            };
            let first = scope.spawn(claim);
            let second = scope.spawn(claim);
            (first.join().unwrap(), second.join().unwrap())
        });
        let ((listener, _lock), error) = match (first, second) {
            (Ok(winner), Err(error)) | (Err(error), Ok(winner)) => (winner, error),
            results => panic!("expected exactly one winner: {results:?}"),
        };
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        assert_eq!(
            bind_cli_socket(&path).unwrap_err().kind(),
            io::ErrorKind::AddrInUse
        );
        assert_connects(&listener, &path).await;
    }

    #[tokio::test]
    async fn saturated_backlog_fails_closed_without_blocking() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cli.sock");
        let listener =
            socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
        listener
            .bind(&socket2::SockAddr::unix(&path).unwrap())
            .unwrap();
        listener.listen(1).unwrap();
        let mut clients = Vec::new();
        let mut saturated = false;
        for _ in 0..16 {
            let client =
                socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
            client.set_nonblocking(true).unwrap();
            match client.connect(&socket2::SockAddr::unix(&path).unwrap()) {
                Ok(()) => clients.push(client),
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        || error.kind() == io::ErrorKind::ConnectionRefused
                        || error.raw_os_error() == Some(libc::EINPROGRESS) =>
                {
                    saturated = true;
                    break;
                }
                Err(error) => panic!("unexpected probe failure: {error}"),
            }
        }
        assert!(saturated, "test must fill the socket backlog");
        assert!(
            !clients.is_empty(),
            "listener must have accepted connections"
        );
        let identity = socket_identity(&std::fs::symlink_metadata(&path).unwrap());
        let start = std::time::Instant::now();
        assert!(bind_cli_socket(&path).is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_eq!(
            socket_identity(&std::fs::symlink_metadata(&path).unwrap()),
            identity
        );
        // Drain the backlog and prove the original listener still serves this path.
        listener.set_nonblocking(true).unwrap();
        let listener = UnixListener::from_std(listener.into()).unwrap();
        for _ in &clients {
            tokio::time::timeout(Duration::from_secs(2), listener.accept())
                .await
                .unwrap()
                .unwrap();
        }
        assert_connects(&listener, &path).await;
    }
}
