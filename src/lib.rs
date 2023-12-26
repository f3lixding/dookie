use std::error::Error;
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
    use std::path::PathBuf;

    use super::*;

    pub enum IncomingMessage {
        Start,
        StatusRequest,
    }

    pub enum OutgoingMessage {
        TimeUntilNextScan(u64),
    }

    pub struct JobStruct;

    impl Job for JobStruct {
        type IncomingMessage = IncomingMessage;
        type OutgoingMessage = OutgoingMessage;
        type ReturnType = Result<(), Box<dyn Error + Send + Sync + 'static>>;

        fn spawn(
            config: &Config,
        ) -> Result<
            SpawnedJob<Self::ReturnType, Self::IncomingMessage, Self::OutgoingMessage>,
            Box<(dyn Error)>,
        > {
            let move_job_period = config.move_job_period;
            let age_threshold = config.age_threshold;
            let root_path_local = (*config.root_path_local).to_owned();
            let root_path_ext = (*config.root_path_ext).to_owned();

            let (move_sender, mut move_receiver) = tokio::sync::mpsc::channel(100);

            let handle = task::spawn(async move {
                let root_path_local = PathBuf::from(root_path_local);
                let root_path_ext = PathBuf::from(root_path_ext);

                loop {
                    let is_full = false;
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

                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_secs(move_job_period)) => {}
                        msg = move_receiver.recv() => {
                            'routine: {
                                let Some(msg) = msg else {
                                    // TODO: log here
                                    break 'routine;
                                };

                                match msg {
                                    (IncomingMessage::Start, _) => {
                                        match move_file(&root_path_local, &root_path_ext).await {
                                            Ok(_) => {
                                                // TODO: log here
                                            }
                                            Err(_) => {
                                                // TODO: log here
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            });

            Ok(SpawnedJob::new(handle, move_sender))
        }
    }

    async fn move_file(source: &PathBuf, destination: &PathBuf) -> Result<(), Box<dyn Error>> {
        let mut files_in_src = tokio::fs::read_dir(source).await?;

        while let Ok(Some(file)) = files_in_src.next_entry().await {
            if file.path().is_file() {
                tokio::fs::copy(file.path(), destination.join(file.file_name())).await?;
                tokio::fs::remove_file(file.path()).await?;
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

        fn spawn(
            config: &Config,
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
        assert_eq!(files_in_src.count(), 0);

        clean_up();
    }
}
