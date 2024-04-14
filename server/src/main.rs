use dookie_server_lib::{
    move_job, BundleClient, Config, Job, Logger, MainListener, MediaBundle, SpawnedJobType,
    Unassigned, Unprimed,
};
use std::error::Error;
use structopt::StructOpt;
use tracing::Instrument;

#[derive(StructOpt)]
struct Opt {
    #[structopt(short = "c", long = "config_path", default_value = ".")]
    config_path: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let Opt { config_path } = Opt::from_args();
    let config_file = std::fs::read(config_path)?;
    let config = Config::from_buffer(&config_file)?;

    // We need to construct media bundle here for all the jobs that would require it
    let media_bundle = MediaBundle::<BundleClient>::default();

    let (move_job_handle, move_job_sender) = {
        let mut move_job_handle = move_job::JobStruct::spawn::<BundleClient>(&config, None)?;
        let sender = move_job_handle.give_sender()?;
        (move_job_handle, sender)
    };
    let move_job_handle = move_job_handle.instrument(tracing::trace_span!("move_job"));
    let bundle = MediaBundle::<BundleClient>::default();

    // Main listener set up
    let listener: MainListener<_, Unassigned> = MainListener::default();
    let listener = listener.assign_sender_bundle(bundle);
    let listener = listener.assign_movejob_sender(move_job_sender);
    let listener = listener
        .initiate_listener()
        .instrument(tracing::trace_span!("listener"));

    // Logging set up
    let logger: Logger<Unprimed> = Logger::from_config(&config);
    let (logger, _guard, _logger_tx) = logger.prime();

    tokio::select! {
        _ = move_job_handle => {
            println!("Move job exited");
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
