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
mod logging;
mod media_bundle;

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

    use tokio::task::JoinHandle;
    use tokio::{sync::RwLock, time::Instant};

    use super::*;

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

    pub struct JobStruct;

    impl Job for JobStruct {
        type IncomingMessage = IncomingMessage;
        type OutgoingMessage = OutgoingMessage;
        type ReturnType = Result<(), Box<dyn Error + Send + Sync + 'static>>;

        fn spawn_(
            config: &Config,
            #[allow(unused)] front_desk_handle: &mut Option<
                JoinHandle<Result<(), Box<dyn Error + Send + Sync + 'static>>>,
            >,
        ) -> Result<
            SpawnedJob<Self::ReturnType, Self::IncomingMessage, Self::OutgoingMessage>,
            Box<(dyn Error)>,
        > {
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
    use tokio::task::JoinHandle;

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

    pub struct JobStruct;

    impl Job for JobStruct {
        type IncomingMessage = IncomingMessage;
        type OutgoingMessage = ();
        type ReturnType = ();

        fn spawn_(
            config: &Config,
            front_desk_handle: &mut Option<
                JoinHandle<Result<(), Box<dyn Error + Send + Sync + 'static>>>,
            >,
        ) -> Result<
            SpawnedJob<Self::ReturnType, Self::IncomingMessage, Self::OutgoingMessage>,
            Box<dyn Error>,
        > {
            let handle = task::spawn(async {});
            let (sender, _receiver) = tokio::sync::mpsc::channel(100);

            Ok(SpawnedJob::new(handle, sender))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;
    use std::collections::HashMap;
    use std::fs::{create_dir_all, remove_dir_all, File};
    use std::path::PathBuf;

    const CONFIG_PATH: &'static str = "./var_";
    const SRC_FOLDER: &'static str = "src_folder";
    const DST_FOLDER: &'static str = "dst_folder";

    fn create_test_config<'a>() -> Config<'a> {
        let src_dir = format!("{}/{}", CONFIG_PATH, SRC_FOLDER);
        let dst_dir = format!("{}/{}", CONFIG_PATH, DST_FOLDER);
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
                map.insert(Cow::from(src_dir), Cow::from(dst_dir));
                map
            },
        }
    }

    fn create_test_files() {
        let src_dir = PathBuf::from(format!("{}/{}", CONFIG_PATH, SRC_FOLDER));
        let dst_dir = PathBuf::from(format!("{}/{}", CONFIG_PATH, DST_FOLDER));

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

    fn create_test_dirs() {
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
    }

    fn create_nested_test_dirs() {
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
    }

    fn clean_up() {
        remove_dir_all(CONFIG_PATH).unwrap();
    }

    #[tokio::test]
    #[serial_test::serial]
    // This tests moving of files directly without having them in the target directory
    async fn test_move_func_for_move_job() {
        create_test_files();

        let src_dir = PathBuf::from(format!("{}/{}", CONFIG_PATH, "src_folder"));
        let dst_dir = PathBuf::from(format!("{}/{}", CONFIG_PATH, "dst_folder"));

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

        clean_up();
    }

    #[tokio::test]
    #[serial_test::serial]
    // This tests moving of files in target folders
    async fn test_move_func_for_move_job_two() {
        let src_dir = PathBuf::from(format!("{}/{}", CONFIG_PATH, "src_folder"));
        let dst_dir = PathBuf::from(format!("{}/{}", CONFIG_PATH, "dst_folder"));

        create_test_dirs();

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
        clean_up();

        // Now we test for further nested directories
        create_nested_test_dirs();

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

        clean_up();
    }

    // Perhaps this is one for the readme:
    // tasks spawned in tests need to be _explicitly_ polled / awaited in order to be executed
    #[tokio::test(flavor = "multi_thread")]
    #[serial_test::serial]
    async fn test_channel_for_move_job() {
        create_test_files();
        let config = create_test_config();
        let mut fd_handle = None;
        let mut job_move = move_job::JobStruct::spawn_(&config, &mut fd_handle).unwrap();
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

        clean_up();
    }
}
