use std::error::Error;
use sysinfo::Disks;
use tokio::sync::oneshot::Sender as OneShotSender;
use tokio::task;

mod config;
mod job;

pub use config::Config;
pub use job::*;

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
        path::PathBuf,
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
            let root_path_local = (*config.root_path_local).to_owned();
            let root_path_ext = (*config.root_path_ext).to_owned();

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
                let root_path_local = PathBuf::from(root_path_local);
                let root_path_ext = PathBuf::from(root_path_ext);
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
                    let old_list: Vec<PathBuf> = {
                        let mut full_list = tokio::fs::read_dir(&root_path_local).await?;
                        let mut filtered_list = vec![];
                        while let Some(file) = full_list.next_entry().await? {
                            let age = file.metadata().await?.created()?.elapsed()?.as_secs();
                            let age: u64 = age / (60 * 60 * 24);
                            if age >= age_threshold {
                                filtered_list.push(file.path());
                            }
                        }
                        filtered_list
                    };

                    if is_full {
                        // TODO: log here
                        match move_file(&root_path_local, &root_path_ext).await {
                            Ok(_) => {
                                // TODO: log here
                            }
                            Err(_) => {
                                // TODO: log here
                            }
                        }
                    } else if !old_list.is_empty() {
                        // TODO: log here
                        for file in old_list {
                            match move_file(&file, &root_path_ext).await {
                                Ok(_) => {
                                    // TODO: log here
                                }
                                Err(_) => {
                                    // TODO: log here
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
                                        match move_file(&root_path_local, &root_path_ext).await {
                                            Ok(_) => {
                                                // TODO: log here
                                                _ = sender.send(OutgoingMessage::Ok);
                                            }
                                            Err(_) => {
                                                // TODO: log here
                                                _ = sender.send(OutgoingMessage::Failed);
                                            }
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

    async fn move_file(source: &PathBuf, destination: &PathBuf) -> Result<(), Box<dyn Error>> {
        let mut files_in_src = tokio::fs::read_dir(source).await?;

        while let Ok(Some(file)) = files_in_src.next_entry().await {
            let metadata = tokio::fs::metadata(file.path()).await?;
            if file.path().is_file() && !metadata.file_type().is_symlink() {
                let dst_path = destination.join(file.file_name());
                let src_path = file.path();

                tokio::fs::copy(&src_path, &dst_path).await?;
                tokio::fs::remove_file(&src_path).await?;

                tokio::fs::symlink(&dst_path, &src_path).await?;
            }
        }

        Ok(())
    }

    #[cfg(test)]
    pub async fn move_file_(source: &PathBuf, destination: &PathBuf) -> Result<(), Box<dyn Error>> {
        Ok(move_file(source, destination).await?)
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
    use std::fs::{create_dir_all, remove_dir_all, File};
    use std::path::PathBuf;

    const CONFIG_PATH: &'static str = "./var";
    const SRC_FOLDER: &'static str = "src_folder";
    const DST_FOLDER: &'static str = "dst_folder";

    fn create_test_config<'a>() -> Config<'a> {
        let src_dir = format!("{}/{}", CONFIG_PATH, SRC_FOLDER);
        let dst_dir = format!("{}/{}", CONFIG_PATH, DST_FOLDER);
        Config {
            config_path: CONFIG_PATH.into(),
            radarr_port: 7878,
            sonarr_port: 8989,
            prowlarr_port: 8888,
            qbit_torrent_port: 9090,
            radarr_api_key: String::from(""),
            sonarr_api_key: String::from(""),
            prowlarr_api_key: String::from(""),
            qbit_torrent_api_key: String::from(""),
            move_job_period: 10,
            age_threshold: 10,
            root_path_local: src_dir.into(),
            root_path_ext: dst_dir.into(),
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

    fn clean_up() {
        remove_dir_all(CONFIG_PATH).unwrap();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn test_move_func_for_move_job() {
        create_test_files();

        let src_dir = PathBuf::from(format!("{}/{}", CONFIG_PATH, "src_folder"));
        let dst_dir = PathBuf::from(format!("{}/{}", CONFIG_PATH, "dst_folder"));

        let files_in_src = src_dir.read_dir().unwrap();
        assert_eq!(files_in_src.count(), 2);

        let move_res = move_job::move_file_(&src_dir, &dst_dir).await;
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
