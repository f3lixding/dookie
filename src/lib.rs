use std::error::Error;
use tokio::sync::mpsc::Receiver;
use tokio::task::{self, JoinHandle};

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
    use super::*;

    pub enum Message {
        Stop(u64),
        Start,
    }

    pub struct JobStruct;

    impl Job for JobStruct {
        type Message = Message;
        type ReturnType = ();

        fn spawn(
            config: &Config,
            receiver: Receiver<Self::Message>,
        ) -> Result<JoinHandle<Self::ReturnType>, Box<dyn Error>> {
            let handle = task::spawn(async {});

            Ok(handle)
        }
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

    pub enum Message {
        StopAll,
        StartAll,
        Start(Subject),
        Stop(Subject),
    }

    pub struct JobStruct;

    impl Job for JobStruct {
        type Message = Message;
        type ReturnType = ();

        fn spawn(
            config: &Config,
            receiver: Receiver<Self::Message>,
        ) -> Result<JoinHandle<Self::ReturnType>, Box<dyn Error>> {
            let handle = task::spawn(async {});

            Ok(handle)
        }
    }
}
