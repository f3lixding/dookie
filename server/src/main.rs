use dookie_server_lib::{
    move_job, Config, Job, Logger, MainListener, MediaBundle, Unassigned, Unprimed,
};
use std::{env::temp_dir, error::Error};
use structopt::StructOpt;
use tracing::Instrument;

#[derive(StructOpt)]
struct Opt {
    #[structopt(short = "c", long = "config_path", default_value = ".")]
    config_path: String,
}

const CONFIG_PATH: &'static str = "./var";
const SRC_FOLDER: &'static str = "src_folder";
const DST_FOLDER: &'static str = "dst_folder";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let Opt { config_path } = Opt::from_args();
    let src_dir = format!("{}/{}", CONFIG_PATH, SRC_FOLDER);
    let dst_dir = format!("{}/{}", CONFIG_PATH, DST_FOLDER);
    let config = Config {
        config_path: CONFIG_PATH.into(),
        log_path: CONFIG_PATH.into(),
        radarr_port: 7878,
        sonarr_port: 8989,
        prowlarr_port: 8888,
        qbit_torrent_port: 9090,
        radarr_api_key: String::from(""),
        sonarr_api_key: String::from(""),
        prowlarr_api_key: String::from(""),
        qbit_torrent_api_key: String::from(""),
        move_job_period: 100,
        age_threshold: 100,
        root_path_local: src_dir.into(),
        root_path_ext: dst_dir.into(),
    };

    let (move_job_handle, move_job_sender) = {
        let mut move_job_handle = move_job::JobStruct::spawn(&config)?;
        let sender = move_job_handle.give_sender()?;
        (move_job_handle, sender)
    };
    let bundle = MediaBundle::default();

    // Main listener set up
    let listener: MainListener<Unassigned> = MainListener::default();
    let listener = listener.assign_sender_bundle(bundle);
    let listener = listener.assign_movejob_sender(move_job_sender);
    let listener = listener
        .initiate_listener()
        .instrument(tracing::info_span!("listener"));

    // Logging set up
    let logger: Logger<Unprimed> = Logger::from_config(&config);
    let (logger, _guard, logger_tx) = logger.prime();

    tokio::select! {
        move_job_return = move_job_handle => {
            println!("Move job finished {:?}", move_job_return);
        }
        listener_return = listener => {
            println!("Listener finished {:?}", listener_return);
        }
        _ = logger => {
            println!("Logger finished");
        }
    }

    Ok::<(), Box<dyn Error>>(())
}
