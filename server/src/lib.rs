use std::error::Error;
use sysinfo::Disks;
use tokio::sync::oneshot::Sender as OneShotSender;
use tokio::task;

mod config;
mod job;
mod listener;
mod dookie_proto {
    include!(concat!(env!("OUT_DIR"), "/dookie.rs"));
}
mod client;
mod logging;
mod media_bundle;

pub use client::*;
pub use config::Config;
pub use dookie_proto::*;
pub use job::*;
pub use listener::*;
pub use logging::*;
pub use media_bundle::*;

/// This job monitors local drive to check for various factors to determine if content in the
/// specified directory should be moved to the "cold" storage.
///
/// For now, these factors are:
///   - If the remaining space of 95% of the drive is insufficient to accomodate for the files
///   currently being downloaded
///   - If there are any items on the local drive that are older than the specified threshold
///   - If the files satisfying the above 2 conditions are _not_ currently being used / read
///   - If the task is explicitly being told to do so right now
pub mod move_job {
    use std::{
        collections::HashMap,
        future::Future,
        path::PathBuf,
        pin::Pin,
        sync::{atomic::AtomicBool, Arc},
    };

    use tokio::task::{JoinError, JoinHandle};
    use tokio::{
        sync::{mpsc::Sender, RwLock},
        time::Instant,
    };

    use super::*;

    pub type ReturnType = Result<(), Box<dyn Error + Send + Sync + 'static>>;

    #[derive(Debug, PartialEq, Eq)]
    pub enum IncomingMessage {
        Start,
        StatusRequest,
        Stop,
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum OutgoingMessage {
        TimeUntilNextScan(u64),
        InProgress,
        Ok,
        Failed,
    }

    impl Into<Envelope> for OutgoingMessage {
        fn into(self) -> Envelope {
            let mut envelop = Envelope::default();
            envelop.set_data_type(DataType::Movejobresponse);
            let data = envelope::Data::MoveJobResponse({
                let mut response = MoveJobResponse::default();
                match self {
                    OutgoingMessage::TimeUntilNextScan(time) => {
                        response.set_status(MoveJobStatus::Idle);
                        response.time_to_next_run = Some(time);
                    }
                    OutgoingMessage::InProgress => {
                        response.set_status(MoveJobStatus::Running);
                    }
                    OutgoingMessage::Ok => {
                        response.set_status(MoveJobStatus::Ok);
                    }
                    OutgoingMessage::Failed => {
                        response.set_status(MoveJobStatus::Error);
                    }
                }

                response
            });

            envelop.data = Some(data);
            envelop
        }
    }

    pub struct SpawnedJob<C: IBundleClient> {
        handle: JoinHandle<ReturnType>,
        sender: Option<Sender<(IncomingMessage, Option<OneShotSender<OutgoingMessage>>)>>,
        media_bundle: Option<MediaBundle<C>>,
    }

    impl<C> SpawnedJob<C>
    where
        C: IBundleClient,
    {
        pub fn new(
            handle: JoinHandle<Result<(), Box<dyn Error + Send + Sync + 'static>>>,
            sender: Sender<(IncomingMessage, Option<OneShotSender<OutgoingMessage>>)>,
        ) -> Self {
            Self {
                handle,
                sender: Some(sender),
                media_bundle: None,
            }
        }

        pub fn assign_bundle(&mut self, media_bundle: MediaBundle<C>) {
            self.media_bundle = Some(media_bundle);
        }
    }

    impl<M> SpawnedJobType<ReturnType, IncomingMessage, OutgoingMessage> for SpawnedJob<M>
    where
        M: IBundleClient,
    {
        fn give_sender(
            &mut self,
        ) -> Result<Sender<(IncomingMessage, Option<OneShotSender<OutgoingMessage>>)>, Box<dyn Error>>
        {
            self.sender
                .take()
                .ok_or("No sender stored in this spawned job".into())
        }
    }

    impl<M> Future for SpawnedJob<M>
    where
        M: IBundleClient,
    {
        type Output = Result<ReturnType, JoinError>;

        fn poll(
            self: Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            // Safety: We never move `handle` after it is pinned.
            let handle = Pin::new(&mut self.get_mut().handle);

            handle.poll(cx)
        }
    }

    pub struct JobStruct<M> {
        phantom: std::marker::PhantomData<M>,
    }

    impl<M> Job for JobStruct<M>
    where
        M: IBundleClient,
    {
        type IncomingMessage = IncomingMessage;
        type OutgoingMessage = OutgoingMessage;
        type ReturnType = ReturnType;
        type SpawnedJob = SpawnedJob<M>;

        #[allow(refining_impl_trait)]
        fn spawn_(
            config: &Config,
            #[allow(unused)] front_desk_handle: &mut Option<
                JoinHandle<Result<(), Box<dyn Error + Send + Sync + 'static>>>,
            >,
        ) -> Result<Self::SpawnedJob, Box<(dyn Error)>> {
            let move_job_period = config.move_job_period;
            let age_threshold = config.age_threshold;
            let move_map = config.move_map.iter().fold(
                HashMap::<PathBuf, PathBuf>::new(),
                |mut acc, (src_path, dst_path)| {
                    let src_path = src_path.to_owned().to_string();
                    let dst_path = dst_path.to_owned().to_string();
                    acc.insert(PathBuf::from(src_path), PathBuf::from(dst_path));

                    acc
                },
            );

            let (move_sender, mut move_receiver) = tokio::sync::mpsc::channel::<(
                Self::IncomingMessage,
                Option<OneShotSender<Self::OutgoingMessage>>,
            )>(100);
            let (front_desk_sender, mut front_desk_receiver) = tokio::sync::mpsc::channel::<(
                Self::IncomingMessage,
                Option<OneShotSender<Self::OutgoingMessage>>,
            )>(100);
            let is_moving_ = Arc::new(AtomicBool::new(false));
            let timer_: Arc<RwLock<Option<Instant>>> = Arc::new(RwLock::new(Some(Instant::now())));

            let is_moving = is_moving_.clone();
            let timer = timer_.clone();

            // spawning a listening task
            #[allow(unused)]
            let fd_handle = task::spawn(async move {
                while let Some(msg) = front_desk_receiver.recv().await {
                    match msg {
                        // Start command
                        (IncomingMessage::Start, Some(sender)) => {
                            if is_moving.load(std::sync::atomic::Ordering::Relaxed) {
                                _ = sender.send(OutgoingMessage::InProgress);
                            } else {
                                let (oneshot_sender, oneshot_receiver) =
                                    tokio::sync::oneshot::channel::<Self::OutgoingMessage>();
                                if let Ok(_) = move_sender
                                    .send((IncomingMessage::Start, Some(oneshot_sender)))
                                    .await
                                {
                                    let resp = oneshot_receiver.await?;
                                    _ = sender.send(resp);
                                }
                            }
                        }
                        // Status request
                        (IncomingMessage::StatusRequest, Some(sender)) => {
                            if is_moving.load(std::sync::atomic::Ordering::Relaxed) {
                                _ = sender.send(OutgoingMessage::InProgress);
                            } else {
                                if let Some(timer_guard) = &*timer.read().await {
                                    _ = sender.send(OutgoingMessage::TimeUntilNextScan(
                                        move_job_period - timer_guard.elapsed().as_secs(),
                                    ));
                                }
                            }
                        }
                        // Stop command
                        (IncomingMessage::Stop, Some(sender)) => {
                            let (oneshot_sender, oneshot_receiver) =
                                tokio::sync::oneshot::channel::<Self::OutgoingMessage>();
                            if let Ok(_) = move_sender
                                .send((IncomingMessage::Stop, Some(oneshot_sender)))
                                .await
                            {
                                match oneshot_receiver.await {
                                    Ok(OutgoingMessage::Ok) => {
                                        _ = sender.send(OutgoingMessage::Ok);
                                        break;
                                    }
                                    _ => _ = sender.send(OutgoingMessage::Failed),
                                }
                            }
                        }
                        _ => unreachable!("Unexpected message in front_desk_receiver"),
                    }
                }

                Ok::<(), Box<dyn Error + Send + Sync + 'static>>(())
            });

            #[cfg(test)]
            front_desk_handle.replace(fd_handle);

            let is_moving = is_moving_;
            let timer = timer_;
            let handle = task::spawn(async move {
                let mut disks = Disks::new_with_refreshed_list();

                loop {
                    // We are going to make it so that we are moving every loop.
                    is_moving.store(true, std::sync::atomic::Ordering::Relaxed);
                    let is_full = {
                        disks.refresh();
                        // assume there is only one disk
                        let disk = disks.list().get(0).expect("Disk not found");
                        let available_percentage =
                            ((disk.available_space() as f64) / (disk.total_space() as f64)) * 100.0;
                        available_percentage < 5.0
                    };

                    // key: PathBuf to the file
                    // value: Reference to the PathBuf to the destination folder
                    let old_list: Vec<(PathBuf, &PathBuf)> = {
                        let mut filtered_list = vec![];

                        // We do this for each directory we monitor
                        for (src, dst) in &move_map {
                            let mut full_list = tokio::fs::read_dir(&src).await?;
                            while let Some(file) = full_list.next_entry().await? {
                                let metadata = file.metadata().await?;
                                if metadata.is_symlink() {
                                    continue;
                                }
                                let age = metadata.created()?.elapsed()?.as_secs();
                                if age >= age_threshold {
                                    filtered_list.push((file.path(), dst));
                                }
                            }
                        }

                        filtered_list
                    };

                    if is_full {
                        for (root_path_local, root_path_ext) in &move_map {
                            match move_dir(true, true, root_path_local, root_path_ext).await {
                                Ok(_) => {
                                    tracing::info!(
                                        "Moved files from {} to {}, due to disk full",
                                        root_path_local.display(),
                                        root_path_ext.display()
                                    );
                                }
                                Err(e) => {
                                    tracing::error!(
                                    "Failed to move files from {} to {}, initiated due to disk full. Error: {:?}",
                                    root_path_local.display(),
                                    root_path_ext.display(),
                                    e
                                );
                                }
                            }
                        }
                    } else if !old_list.is_empty() {
                        for (file, dst_path) in old_list {
                            match move_dir(true, false, &file, dst_path).await {
                                Ok(_) => {
                                    tracing::info!(
                                        "Moved files from {} to {}, due to old file age",
                                        file.display(),
                                        dst_path.display()
                                    );
                                }
                                Err(e) => {
                                    tracing::error!(
                                        "Failed to move files from {} to {}, initiated due to old file age. Error: {:?}",
                                        file.display(),
                                        dst_path.display(),
                                        e
                                    );
                                }
                            }
                        }
                    }

                    is_moving.store(false, std::sync::atomic::Ordering::Relaxed);
                    timer.write().await.replace(Instant::now());

                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_secs(move_job_period)) => {}
                        msg = move_receiver.recv() => {
                            if let Some(msg) = msg {
                                match msg {
                                    // Start command
                                    (IncomingMessage::Start, Some(sender)) => {
                                        is_moving.store(true, std::sync::atomic::Ordering::Relaxed);

                                        let mut has_failed = false;
                                        for (src_dir, dst_dir) in &move_map {
                                            match move_dir(true, true, src_dir, dst_dir).await {
                                                Ok(_) => {
                                                    tracing::info!(
                                                        "Moved files from {} to {}, due to explicit request",
                                                        src_dir.display(),
                                                        dst_dir.display()
                                                    );
                                                }
                                                Err(e) => {
                                                    tracing::error!(
                                                        "Failed to move files from {} to {}, initiated due to explicit request. Error: {:?}",
                                                        src_dir.display(),
                                                        dst_dir.display(),
                                                        e
                                                    );
                                                    has_failed = true;
                                                }
                                            }
                                        }

                                        if has_failed {
                                            _ = sender.send(OutgoingMessage::Ok);
                                        } else {
                                            _ = sender.send(OutgoingMessage::Failed);
                                        }

                                        is_moving.store(false, std::sync::atomic::Ordering::Relaxed);
                                    }

                                    // Stop command
                                    (IncomingMessage::Stop, Some(sender)) => {
                                        _ = sender.send(OutgoingMessage::Ok);
                                        break Ok(());
                                    }

                                    _ => unreachable!("Unexpected message in move_receiver"),
                                }
                            }
                        }
                    }
                }
            });

            Ok(SpawnedJob::new(handle, front_desk_sender))
        }
    }

    // src_path is pointing to a particular folder to move if is_mass_move is false
    // Otherwise src_path is pointing to a root folder for evertyhing in it to be moved
    // If is_root and is_mass_move are both true, then the function would need to symlink moved
    // items.
    // If is_root is false, and is_mass_move is true, then the function would not symlink
    // If is_root is true, and is_mass_move is false, then the function would symlink src path
    // (which is a folder to a content) to dst_path (which never have the end path attached, which
    // means you would need to attach the end segment to it before you call move).
    fn move_dir(
        is_root: bool,
        is_mass_move: bool,
        src_path: &PathBuf,
        dst_path: &PathBuf,
    ) -> Pin<Box<dyn Future<Output = Result<(), Box<dyn Error>>> + Send>> {
        // This cloning is needed because I am forced to put the function body in the async block.
        let src_path = src_path.clone();
        let mut dst_path = dst_path.clone();

        Box::pin(async move {
            let file_name = src_path.file_name().ok_or("Failed to get file name")?;
            if (is_root && !is_mass_move) || !is_root {
                dst_path.push(file_name);
            }

            if src_path.is_file() {
                tokio::fs::copy(&src_path, &dst_path).await?;

                return Ok(());
            }

            if (is_root && !is_mass_move) || !is_root {
                tokio::fs::create_dir_all(&dst_path).await?;
            }
            let mut files_in_src = tokio::fs::read_dir(&src_path).await?;

            while let Some(file) = files_in_src.next_entry().await? {
                if !file.metadata().await?.is_symlink() {
                    move_dir(false, is_mass_move, &file.path(), &dst_path).await?;
                    if file.metadata().await?.is_dir() {
                        tokio::fs::remove_dir(&file.path()).await?;
                    } else {
                        tokio::fs::remove_file(&file.path()).await?;
                    }

                    if is_root {
                        if is_mass_move {
                            // If this is a mass move, we would need to create another pathbuf to
                            // point to the specific file / folder we had just moved
                            let dst_path = dst_path.join(file.file_name());
                            tokio::fs::symlink(&dst_path, &file.path()).await?;
                            tracing::info!(
                                "Symlinked {} to {}",
                                dst_path.display(),
                                file.path().display()
                            );
                        }
                    }
                }
            }

            // Here we check if this is a targeted move to symlink itself
            if is_root && !is_mass_move {
                if src_path.is_dir() {
                    tokio::fs::remove_dir(&src_path).await?;
                } else {
                    tokio::fs::remove_file(&src_path).await?;
                }
                tokio::fs::symlink(&dst_path, &src_path).await?;
            }

            Ok(())
        })
    }

    #[cfg(test)]
    pub async fn move_dir_(
        is_root: bool,
        is_mass_move: bool,
        src_path: &PathBuf,
        dst_path: &PathBuf,
    ) -> Result<(), Box<dyn Error>> {
        move_dir(is_root, is_mass_move, src_path, dst_path).await
    }
}

/// This job starts the server (that interfaces with all other components). At the time of writing,
/// these are:
///   - Prowlarr
///   - Sonarr
///   - Radarr
///   - QBitTorrent
///   - Plex Server
pub mod spawn_server_job {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::sync::mpsc::Sender;
    use tokio::task::{JoinError, JoinHandle};

    use super::*;

    pub enum Subject {
        Prowlarr,
        Sonarr,
        Radarr,
        QBitTorrent,
        PlexServer,
    }

    pub enum IncomingMessage {
        StopAll,
        StartAll,
        Start(Subject),
        Stop(Subject),
    }

    pub struct SpawnedJob {
        handle: JoinHandle<()>,
        sender: Option<Sender<(IncomingMessage, Option<OneShotSender<()>>)>>,
    }

    impl SpawnedJob {
        pub fn new(
            handle: JoinHandle<()>,
            sender: Sender<(IncomingMessage, Option<OneShotSender<()>>)>,
        ) -> Self {
            Self {
                handle,
                sender: Some(sender),
            }
        }
    }

    impl SpawnedJobType<(), IncomingMessage, ()> for SpawnedJob {
        fn give_sender(
            &mut self,
        ) -> Result<Sender<(IncomingMessage, Option<OneShotSender<()>>)>, Box<dyn Error>> {
            self.sender
                .take()
                .ok_or("No sender stored in this spawned job".into())
        }
    }

    impl Future for SpawnedJob {
        type Output = Result<(), JoinError>;
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            // Safety: We never move `handle` after it is pinned.
            let handle = Pin::new(&mut self.get_mut().handle);

            handle.poll(cx)
        }
    }

    impl std::fmt::Debug for SpawnedJob {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("SpawnedJob").finish()
        }
    }

    pub struct JobStruct;

    impl Job for JobStruct {
        type IncomingMessage = IncomingMessage;
        type OutgoingMessage = ();
        type ReturnType = ();
        type SpawnedJob = SpawnedJob;

        #[allow(refining_impl_trait)]
        fn spawn_(
            config: &Config,
            front_desk_handle: &mut Option<
                JoinHandle<Result<(), Box<dyn Error + Send + Sync + 'static>>>,
            >,
        ) -> Result<SpawnedJob, Box<dyn Error>> {
            let handle = task::spawn(async {});
            let (sender, _receiver) = tokio::sync::mpsc::channel(100);

            Ok(SpawnedJob::new(handle, sender))
        }
    }
}

/// This job monitors the target library directories and initiates scanning to plex if the
/// following conditions are true:
/// - At least 10 symlinks are valid and not broken
/// - There is a change in the directory detected
/// - There are not move job ongoing
///
/// The reason for the existence of this job is so that when the external storage is disconnected,
/// we do not clear out the library by accident.
///
/// For api to refresh the library see https://plexapi.dev/docs/plex/refresh-library
pub mod scan_library_job {
    use super::*;
    use crate::IBundleClient;
    use notify::{recommended_watcher, Watcher};
    use std::error::Error;
    use std::future::Future;
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::task::Poll;
    use tokio::task::JoinHandle;
    use tokio::{sync::mpsc::Sender, task::JoinError};

    #[derive(Debug, PartialEq, Eq)]
    pub enum IncomingMessage {
        Start,
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum OutgoingMessage {
        Ok,
    }

    pub type ReturnType = Result<(), Box<dyn Error + Send + Sync + 'static>>;

    pub struct SpawnedJob<C: IBundleClient> {
        paths_to_watch: Vec<PathBuf>,
        sender: Option<Sender<(IncomingMessage, Option<OneShotSender<OutgoingMessage>>)>>,
        media_bundle: Option<MediaBundle<C>>,
        move_job_sender: Option<
            Sender<(
                move_job::IncomingMessage,
                Option<OneShotSender<move_job::OutgoingMessage>>,
            )>,
        >,
    }

    impl<C> SpawnedJob<C>
    where
        C: IBundleClient,
    {
        pub fn assign_move_job_sender(
            &mut self,
            sender: Sender<(
                move_job::IncomingMessage,
                Option<OneShotSender<move_job::OutgoingMessage>>,
            )>,
        ) {
            self.move_job_sender.replace(sender);
        }

        pub fn assign_media_bundle(&mut self, media_bundle: MediaBundle<C>) {
            self.media_bundle.replace(media_bundle);
        }
    }

    impl<M> SpawnedJobType<ReturnType, IncomingMessage, OutgoingMessage> for SpawnedJob<M>
    where
        M: IBundleClient,
    {
        fn give_sender(
            &mut self,
        ) -> Result<Sender<(IncomingMessage, Option<OneShotSender<OutgoingMessage>>)>, Box<dyn Error>>
        {
            self.sender
                .take()
                .ok_or("No sender stored in this spawned job".into())
        }
    }

    impl<M> Future for SpawnedJob<M>
    where
        M: IBundleClient,
    {
        type Output = Result<ReturnType, JoinError>;

        fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
            // Safety: We never move `handle` after it is pinned.
            let this = self.get_mut();
            let paths_to_watch = this.paths_to_watch.clone();
            let media_bundle = this.media_bundle.take();
            if media_bundle.is_none() {
                return Poll::Ready(Ok(Err("Media bundle not found".into())));
            }
            let media_bundle = media_bundle.unwrap();
            let move_job_sender = this.move_job_sender.take();

            #[inline]
            async fn find_broken_symlinks(
                path: &PathBuf,
            ) -> Result<bool, Box<dyn Error + Send + Sync + 'static>> {
                let mut entries = tokio::fs::read_dir(path).await?;
                let mut count = 0;

                let res = loop {
                    if count >= 10 {
                        break Ok::<_, Box<dyn Error + Send + Sync + 'static>>(false);
                    }

                    if let Some(entry) = entries.next_entry().await? {
                        if entry.file_type().await?.is_symlink() {
                            if let Ok(path) = entry.path().read_link() {
                                let complete_path = path.canonicalize()?;
                                match tokio::fs::metadata(&complete_path).await {
                                    Ok(_) => count += 1,
                                    Err(e) => {
                                        tracing::error!(
                                            "Broken symlink detected: {:?}. Terminating scan job",
                                            e
                                        );
                                        break Ok(true);
                                    }
                                }
                            }
                        }
                    } else {
                        // We had exhausted all the entries in the directory and there are fewer
                        // than 10 symlinks to check and they are all okay. Thus we should return
                        // a go ahead.
                        break Ok(false);
                    }
                };

                res
            }

            let handle = tokio::task::spawn(async move {
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<usize>();

                let tx_for_movies = tx.clone();
                let mut watcher_one =
                    recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                        Ok(event) => {
                            if let notify::EventKind::Modify(_) = event.kind {
                                let _ = tx_for_movies.send(0);
                            } else {
                                tracing::info!("Ignoring event: {:?}", event);
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to watch directory. Error: {:?}", e);
                        }
                    })?;

                let tx_for_shows = tx.clone();
                let mut watcher_two =
                    recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                        Ok(event) => {
                            if let notify::EventKind::Modify(_) = event.kind {
                                let _ = tx_for_shows.send(1);
                            } else {
                                tracing::info!("Ignoring event: {:?}", event);
                            }
                        }
                        Err(e) => {
                            tracing::error!("Failed to watch directory. Error: {:?}", e);
                        }
                    })?;

                // Not sure really if this is too hacky but for the time being this should be okay
                // but in the future make sure that watcher one is watching the first element and
                // watcher two is watching the second element.
                watcher_one.watch(&paths_to_watch[0].clone(), notify::RecursiveMode::Recursive)?;
                watcher_two.watch(&paths_to_watch[1].clone(), notify::RecursiveMode::Recursive)?;

                loop {
                    let res = rx.recv().await;

                    match res {
                        Some(idx) if idx < paths_to_watch.len() => {
                            if let Ok(res) = find_broken_symlinks(&paths_to_watch[idx]).await {
                                let is_moving = if let Some(move_job_sender) = &move_job_sender {
                                    let (tx, rx) = tokio::sync::oneshot::channel::<
                                        move_job::OutgoingMessage,
                                    >();
                                    move_job_sender
                                        .send((move_job::IncomingMessage::StatusRequest, Some(tx)))
                                        .await?;
                                    let move_job_status = rx.await?;
                                    move_job_status == move_job::OutgoingMessage::InProgress
                                } else {
                                    false
                                };

                                match (res, is_moving) {
                                    (true, _) => tracing::error!("Found broken symlinks in library directory. Skipping this scan."),
                                    (false, true) => tracing::info!("Move job is in progress. Skipping this scan."),
                                    (false, false) => {
                                        // here we actually do the scan since everything is okay
                                        let resp = media_bundle.refresh_libraries(idx).await;
                                        if let Err(e) = resp {
                                            tracing::error!("Failed to refresh libraries. Error: {:?}", e);
                                        } else if let Ok(resp_code) = resp {
                                            tracing::error!("Refreshed libraries. Response: {:?}", resp_code);
                                        } else {
                                            tracing::info!("Refreshed library id {}", idx);
                                        }
                                    }
                                }
                            } else {
                                tracing::error!("Failed to determine if there are broken symlinks. Skipping this scan.");
                            }
                        }
                        _ => tracing::info!(
                            "Scan job received a notification that was not a valid event. Noop."
                        ),
                    }
                }

                #[allow(unreachable_code)]
                Ok(())
            });

            let handle = std::pin::pin!(handle);
            handle.poll(cx)
        }
    }

    pub struct JobStruct<M> {
        phantom: std::marker::PhantomData<M>,
    }

    impl<M> Job for JobStruct<M>
    where
        M: IBundleClient,
    {
        type IncomingMessage = IncomingMessage;
        type OutgoingMessage = OutgoingMessage;
        type ReturnType = ReturnType;
        type SpawnedJob = SpawnedJob<M>;

        #[allow(refining_impl_trait)]
        fn spawn_(
            config: &Config,
            #[allow(unused)] front_desk_handle: &mut Option<
                JoinHandle<Result<(), Box<dyn Error + Send + Sync + 'static>>>,
            >,
        ) -> Result<Self::SpawnedJob, Box<dyn Error>> {
            let paths_to_watch =
                config
                    .move_map
                    .iter()
                    .fold(Vec::new(), |mut acc, (src_path, _)| {
                        let src_path = src_path.to_string();
                        acc.push(PathBuf::from(src_path));
                        acc
                    });

            //  There should be no foreseeable use case where we are monitoring more than 2
            //  directories (one for movies and one for shows).
            assert!(paths_to_watch.len() == 2);

            // for testing purposes
            // let fd_handle;

            let (tx, mut rx) = tokio::sync::mpsc::channel::<(Self::IncomingMessage, _)>(100);

            let spawned_job = Self::SpawnedJob {
                paths_to_watch,
                sender: Some(tx),
                media_bundle: None,
                move_job_sender: None,
            };

            Ok(spawned_job)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::borrow::Cow;
    use std::collections::HashMap;
    use std::fs::{create_dir_all, remove_dir_all, File};
    use std::path::PathBuf;

    const CONFIG_PATH: &'static str = "./var_";
    const SRC_FOLDER: &'static str = "src_folder";
    const DST_FOLDER: &'static str = "dst_folder";

    struct CleanUp {}

    impl Drop for CleanUp {
        fn drop(&mut self) {
            remove_dir_all(CONFIG_PATH).unwrap();
        }
    }

    struct MockResponse {
        status: u16,
        body: Vec<u8>,
    }

    impl Default for MockResponse {
        fn default() -> Self {
            Self {
                status: 200,
                body: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl BundleResponse for MockResponse {
        async fn as_bytes(self) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
            Ok(self.body)
        }

        fn get_statuscode(&self) -> u16 {
            self.status
        }
    }

    #[derive(Clone, Default)]
    struct MockClient {
        map: std::sync::Arc<tokio::sync::RwLock<HashMap<String, usize>>>,
    }

    impl MockClient {
        pub fn give_map(&self) -> std::sync::Arc<tokio::sync::RwLock<HashMap<String, usize>>> {
            self.map.clone()
        }
    }

    #[async_trait]
    impl IBundleClient for MockClient {
        async fn get(&self, url: &str) -> Result<MockResponse, Box<dyn Error + Send + Sync>> {
            println!("Get called with url: {}", url);
            let mut call_map = self.map.write().await;
            call_map
                .entry(url.to_string())
                .and_modify(|count| {
                    *count += 1;
                })
                .or_insert(1);

            Ok(MockResponse::default())
        }

        async fn post(
            &self,
            _url: &str,
            _body: impl Into<reqwest::Body> + Send + Sync,
        ) -> Result<reqwest::Response, Box<dyn Error + Send + Sync>> {
            unimplemented!()
        }

        fn from_port(_port: u16) -> Self {
            MockClient::default()
        }

        fn set_port(&mut self, _port: u16) {}

        fn set_token(&mut self, token: (impl AsRef<str>, impl AsRef<str>)) {}
    }

    fn create_test_config<'a>(mappings: &'a Vec<(String, String)>) -> Config<'a> {
        Config {
            config_path: CONFIG_PATH.into(),
            log_path: Cow::from(""),
            radarr_port: 7878,
            sonarr_port: 8989,
            prowlarr_port: 8888,
            qbit_torrent_port: 9090,
            radarr_api_key: Cow::from(""),
            sonarr_api_key: Cow::from(""),
            prowlarr_api_key: Cow::from(""),
            qbit_torrent_api_key: Cow::from(""),
            move_job_period: 10,
            age_threshold: 10,
            move_map: {
                let mut map = HashMap::new();
                for (src_dir, dst_dir) in mappings {
                    map.insert(Cow::from(src_dir), Cow::from(dst_dir));
                }
                map
            },
        }
    }

    fn create_test_files(mappings: &Vec<(String, String)>) -> CleanUp {
        for mapping in mappings {
            let src_dir = PathBuf::from(mapping.0.clone());
            let dst_dir = PathBuf::from(mapping.1.clone());

            create_dir_all(&src_dir).unwrap();
            create_dir_all(&dst_dir).unwrap();
            _ = File::create(format!(
                "{}/{}",
                src_dir.to_str().unwrap(),
                "test_file_1.txt"
            ))
            .unwrap();
            _ = File::create(format!(
                "{}/{}",
                src_dir.to_str().unwrap(),
                "test_file_2.txt"
            ))
            .unwrap();
        }

        CleanUp {}
    }

    // The files should be in src directory in this case.
    // Since in our set up the symlinks should exist in the src directory and the real files should
    // be in the destination directory, we are also going to move the files to destination
    // directory in this function
    fn create_sym_links(mappings: &Vec<(String, String)>) {
        for mapping in mappings {
            let src_dir = PathBuf::from(mapping.0.clone());
            let dst_dir = PathBuf::from(mapping.1.clone());
            let entries = std::fs::read_dir(src_dir).unwrap();

            for entry in entries {
                let entry = entry.unwrap();
                std::fs::copy(entry.path(), dst_dir.join(entry.file_name())).unwrap();
                std::fs::remove_file(entry.path()).unwrap();
                std::os::unix::fs::symlink(dst_dir.join(entry.file_name()), entry.path()).unwrap();
            }
        }
    }

    fn create_test_dirs() -> CleanUp {
        let src_dir = PathBuf::from(format!("{}/{}", CONFIG_PATH, SRC_FOLDER));
        let dst_dir = PathBuf::from(format!("{}/{}", CONFIG_PATH, DST_FOLDER));

        let sub_dir_one = src_dir.join("sub_dir_one");
        let sub_dir_two = src_dir.join("sub_dir_two");

        create_dir_all(&sub_dir_one).unwrap();
        create_dir_all(&sub_dir_two).unwrap();
        create_dir_all(&dst_dir).unwrap();

        _ = File::create(format!(
            "{}/{}",
            sub_dir_one.to_str().unwrap(),
            "test_file_1.txt"
        ))
        .unwrap();
        _ = File::create(format!(
            "{}/{}",
            sub_dir_one.to_str().unwrap(),
            "test_file_2.txt"
        ))
        .unwrap();
        _ = File::create(format!(
            "{}/{}",
            sub_dir_two.to_str().unwrap(),
            "test_file_1.txt"
        ))
        .unwrap();
        _ = File::create(format!(
            "{}/{}",
            sub_dir_two.to_str().unwrap(),
            "test_file_2.txt"
        ))
        .unwrap();

        CleanUp {}
    }

    fn create_nested_test_dirs() -> CleanUp {
        let src_dir = PathBuf::from(format!("{}/{}", CONFIG_PATH, SRC_FOLDER));
        let dst_dir = PathBuf::from(format!("{}/{}", CONFIG_PATH, DST_FOLDER));

        let sub_dir_one = src_dir.join("sub_dir_one").join("sub_sub_dir");
        let sub_dir_two = src_dir.join("sub_dir_two").join("sub_sub_dir");

        create_dir_all(&sub_dir_one).unwrap();
        create_dir_all(&sub_dir_two).unwrap();
        create_dir_all(&dst_dir).unwrap();

        _ = File::create(format!(
            "{}/{}",
            sub_dir_one.to_str().unwrap(),
            "test_file_1.txt"
        ))
        .unwrap();
        _ = File::create(format!(
            "{}/{}",
            sub_dir_one.to_str().unwrap(),
            "test_file_2.txt"
        ))
        .unwrap();
        _ = File::create(format!(
            "{}/{}",
            sub_dir_two.to_str().unwrap(),
            "test_file_1.txt"
        ))
        .unwrap();
        _ = File::create(format!(
            "{}/{}",
            sub_dir_two.to_str().unwrap(),
            "test_file_2.txt"
        ))
        .unwrap();

        CleanUp {}
    }

    #[tokio::test]
    #[serial_test::serial]
    // This tests moving of files directly without having them in the target directory
    async fn test_move_func_for_move_job() {
        let src_dir = format!("{}/{}", CONFIG_PATH, "src_folder");
        let dst_dir = format!("{}/{}", CONFIG_PATH, "dst_folder");
        let mappings = Vec::from([(src_dir.clone(), dst_dir.clone())]);

        let _to_drop = create_test_files(&mappings);
        let src_dir = PathBuf::from(src_dir);
        let dst_dir = PathBuf::from(dst_dir);

        let files_in_src = src_dir.read_dir().unwrap();
        assert_eq!(files_in_src.count(), 2);

        let move_res = move_job::move_dir_(true, true, &src_dir, &dst_dir).await;
        if move_res.is_err() {
            println!("{:?}", move_res);
        }
        assert!(move_res.is_ok());
        let files_in_dst = dst_dir.read_dir().unwrap();
        let files_in_src = src_dir.read_dir().unwrap();
        assert_eq!(files_in_dst.count(), 2);
        assert_eq!(files_in_src.count(), 2);

        for file in src_dir.read_dir().unwrap() {
            let Ok(file) = file else {
                panic!("Files remaining in src dir should be ok to read")
            };

            let metadata = file
                .metadata()
                .expect("Failure to retrieve metadata for test files");

            assert!(metadata.is_symlink(), "remaining files should be symlinks");
        }
    }

    #[tokio::test]
    #[serial_test::serial]
    // This tests moving of files in target folders
    async fn test_move_func_for_move_job_two() {
        let src_dir = PathBuf::from(format!("{}/{}", CONFIG_PATH, "src_folder"));
        let dst_dir = PathBuf::from(format!("{}/{}", CONFIG_PATH, "dst_folder"));

        {
            let _to_drop = create_test_dirs();

            // Testing targeted move first
            let targeted_dir = src_dir.join("sub_dir_one");
            let move_res = move_job::move_dir_(true, false, &targeted_dir, &dst_dir).await;
            if move_res.is_err() {
                println!("move_res error: {:?}", move_res);
            }
            assert!(move_res.is_ok());
            let files_in_dst = dst_dir.read_dir().unwrap();
            let files_in_src = src_dir.read_dir().unwrap();
            assert_eq!(files_in_dst.count(), 1);
            assert_eq!(files_in_src.count(), 2);

            let mut num_normal_files = 0;
            let mut num_symlinks = 0;
            for file in src_dir.read_dir().unwrap() {
                let file = file.expect("Files remaining in src dir should be ok to read");

                if file.metadata().unwrap().is_symlink() {
                    num_symlinks += 1;
                } else {
                    num_normal_files += 1;
                }
            }
            assert_eq!(num_normal_files, 1);
            assert_eq!(num_symlinks, 1);

            // And now we test the mass move
            let move_res = move_job::move_dir_(true, true, &src_dir, &dst_dir).await;
            if move_res.is_err() {
                println!("move_res error: {:?}", move_res);
            }
            assert!(move_res.is_ok());
            let files_in_dst = dst_dir.read_dir().unwrap();
            let files_in_src = src_dir.read_dir().unwrap();
            assert_eq!(files_in_dst.count(), 2);
            assert_eq!(files_in_src.count(), 2);
            for file in src_dir.read_dir().unwrap() {
                let file = file.expect("Files remaining in src dir should be ok to read");

                // Whatever is left should just be symlinks
                assert!(file.metadata().unwrap().is_symlink());
            }
        }

        // Now we test for further nested directories
        {
            let _to_drop = create_nested_test_dirs();

            // Testing targeted move first
            let targeted_dir = src_dir.join("sub_dir_one");
            let move_res = move_job::move_dir_(true, false, &targeted_dir, &dst_dir).await;
            if move_res.is_err() {
                println!("move_res error: {:?}", move_res);
            }
            assert!(move_res.is_ok());
            let files_in_dst = dst_dir.read_dir().unwrap();
            let files_in_src = src_dir.read_dir().unwrap();
            assert_eq!(files_in_dst.count(), 1);
            assert_eq!(files_in_src.count(), 2);

            let mut num_normal_files = 0;
            let mut num_symlinks = 0;
            for file in src_dir.read_dir().unwrap() {
                let file = file.expect("Files remaining in src dir should be ok to read");

                if file.metadata().unwrap().is_symlink() {
                    num_symlinks += 1;
                } else {
                    num_normal_files += 1;
                }
            }
            assert_eq!(num_normal_files, 1);
            assert_eq!(num_symlinks, 1);

            // And now we test the mass move
            let move_res = move_job::move_dir_(true, true, &src_dir, &dst_dir).await;
            if move_res.is_err() {
                println!("move_res error: {:?}", move_res);
            }
            assert!(move_res.is_ok());
            let files_in_dst = dst_dir.read_dir().unwrap();
            let files_in_src = src_dir.read_dir().unwrap();
            assert_eq!(files_in_dst.count(), 2);
            assert_eq!(files_in_src.count(), 2);
            for file in src_dir.read_dir().unwrap() {
                let file = file.expect("Files remaining in src dir should be ok to read");

                // Whatever is left should just be symlinks
                assert!(file.metadata().unwrap().is_symlink());
            }
        }
    }

    // Perhaps this is one for the readme:
    // tasks spawned in tests need to be _explicitly_ polled / awaited in order to be executed
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn test_channel_for_move_job() {
        let src_dir = format!("{}/{}", CONFIG_PATH, SRC_FOLDER);
        let dst_dir = format!("{}/{}", CONFIG_PATH, DST_FOLDER);
        let mappings = Vec::from([(src_dir, dst_dir)]);

        let _to_drop = create_test_files(&mappings);

        let config = create_test_config(&mappings);
        let mut fd_handle = None;
        let mut job_move: move_job::SpawnedJob<MockClient> =
            move_job::JobStruct::spawn_(&config, &mut fd_handle).unwrap();
        let front_desk_sender = job_move.give_sender().unwrap();

        let job_move_handle = task::spawn(async move {
            tokio::select! {
                join_result = job_move => {
                    let join_result = join_result?;
                    assert!(join_result.is_ok());
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => {}
            }

            Ok::<(), Box<dyn Error + Send + Sync>>(())
        });

        let test_send_msg_handle = task::spawn(async move {
            let (oneshot_sender, oneshot_receiver) = tokio::sync::oneshot::channel();
            front_desk_sender
                .send((
                    move_job::IncomingMessage::StatusRequest,
                    Some(oneshot_sender),
                ))
                .await
                .unwrap();
            let resp = oneshot_receiver.await.unwrap();
            if let move_job::OutgoingMessage::TimeUntilNextScan(time_left) = resp {
                assert!(time_left <= 10, "Wrong time left on status request");
            } else {
                panic!("Wrong response received from move job upon status request");
            }

            let (oneshot_sender, oneshot_receiver) = tokio::sync::oneshot::channel();
            front_desk_sender
                .send((move_job::IncomingMessage::Stop, Some(oneshot_sender)))
                .await
                .unwrap();
            let resp = oneshot_receiver.await.unwrap();
            assert_eq!(
                resp,
                move_job::OutgoingMessage::Ok,
                "Shut down of worker task should be successful"
            );
        });

        let fd_handle = fd_handle.expect("Failed to get front desk handle");
        let (job_move_handle_res, test_send_msg_handle_res, _fd_handle_res) =
            tokio::join!(job_move_handle, test_send_msg_handle, fd_handle);
        assert!(job_move_handle_res.is_ok());
        assert!(test_send_msg_handle_res.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn test_scan_job() {
        let shows_src = format!("{}/{}", CONFIG_PATH, "shows_src");
        let shows_dst = format!("{}/{}", CONFIG_PATH, "shows_dst");
        let movies_src = format!("{}/{}", CONFIG_PATH, "movies_src");
        let movies_dst = format!("{}/{}", CONFIG_PATH, "movies_dst");
        let mappings = Vec::from([
            (shows_src.clone(), shows_dst.clone()),
            (movies_src.clone(), movies_dst.clone()),
        ]);

        let _to_drop = create_test_files(&mappings);

        create_sym_links(&mappings);

        for (src, _) in &mappings {
            let src_dir = PathBuf::from(&src);
            let entries = src_dir.read_dir().unwrap();
            let mut file_count = 0;

            for entry in entries {
                let entry = entry.unwrap();

                let md = entry.metadata().unwrap();
                assert!(md.is_symlink());
                file_count += 1;
            }

            assert_eq!(file_count, 2);
        }

        let config = create_test_config(&mappings);
        let mut scan_job = scan_library_job::JobStruct::<MockClient>::spawn(&config).unwrap();
        let (move_tx, mut move_rx) = tokio::sync::mpsc::channel::<(
            move_job::IncomingMessage,
            Option<OneShotSender<move_job::OutgoingMessage>>,
        )>(10);
        let mock_client = MockClient::default();
        let call_map = mock_client.give_map();
        let media_bundle = MediaBundle::from_client(mock_client);
        scan_job.assign_media_bundle(media_bundle);
        scan_job.assign_move_job_sender(move_tx.clone());

        let move_job_ans_task = task::spawn(async move {
            while let Some((msg, oneshot_sender)) = move_rx.recv().await {
                if let move_job::IncomingMessage::Stop = msg {
                    break;
                }
                if let Some(oneshot_sender) = oneshot_sender {
                    oneshot_sender
                        .send(move_job::OutgoingMessage::TimeUntilNextScan(100))
                        .unwrap();
                }
            }
        });

        let assert_task = tokio::spawn(async move {
            // First we add a file to shows_src to see if it triggers a scan.
            _ = File::create(format!("{}/file1.mkv", shows_src)).unwrap();
            {
                // Needed to wait for the file to be added. If we run the lines after directly
                // without waiting the test is going to fail.
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                let call_map = call_map.read().await;
                let expected_url = format!("/library/sections/{}/refresh", 0);
                assert!(call_map.get(&expected_url).is_some());
            }

            // And then we are going to break a symlink in the movies_src.
            // This should not result in a scan.
            std::fs::remove_file(format!("{}/test_file_1.txt", movies_dst)).unwrap();
            _ = File::create(format!("{}/file1.mkv", movies_src)).unwrap();
            {
                // Needed to wait for the file to be added. If we run the lines after directly
                // without waiting the test is going to fail.
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                let call_map = call_map.read().await;
                let expected_url = format!("/library/sections/{}/refresh", 1);
                assert!(call_map.get(&expected_url).is_none());
            }

            // If we restore the symlink we should be a go again. (And this time we should not have
            // to create anything since the act of restoration is a change event)
            _ = File::create(format!("{}/test_file_1.txt", movies_dst)).unwrap();
            {
                // Needed to wait for the file to be added. If we run the lines after directly
                // without waiting the test is going to fail.
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                let call_map = call_map.read().await;
                let expected_url = format!("/library/sections/{}/refresh", 1);
                let res = call_map.get(&expected_url);
                assert!(res.is_some());
                assert_eq!(*res.unwrap(), 1);
            }

            // Don't forget to end the move_job_ans_task otherwise the test will hang.
            move_tx
                .send((move_job::IncomingMessage::Stop, None))
                .await
                .unwrap();
        });

        tokio::select! {
            _ = move_job_ans_task => {},
            _ = assert_task => {},
            _ = scan_job => {},
        }
    }
}
