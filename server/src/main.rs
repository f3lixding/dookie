use dookie_server_lib::{
    auto_torrent_shutoff_job, bind_cli_socket, move_job, scan_library_job, update, BundleClient,
    Config, Job, Logger, MainListener, MediaBundle, SpawnedJobType, Unassigned, Unprimed,
    CLI_SOCKET_PATH,
};
use std::{
    error::Error,
    os::unix::process::CommandExt,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
use structopt::StructOpt;
use tracing::Instrument;

const DAEMONIZED_ENV: &str = "DOOKIE_DAEMONIZED";

#[derive(StructOpt)]
struct Opt {
    #[structopt(short = "c", long = "config_path", default_value = ".")]
    config_path: String,
    /// Keep this process attached to the terminal instead of daemonizing it.
    #[structopt(long)]
    foreground: bool,
    /// Parse the configuration and exit without starting jobs or claiming the socket.
    #[structopt(long)]
    check_config: bool,
    /// Disable automatic release checks and installation for this process.
    #[structopt(long)]
    no_auto_update: bool,
}

fn daemonize() -> Result<(), Box<dyn Error>> {
    // Keep the working directory for relative paths in the configuration, while closing
    // stdin/stdout/stderr and detaching from the controlling terminal.
    if unsafe { libc::daemon(1, 0) } == -1 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let Opt {
        config_path,
        foreground,
        check_config,
        no_auto_update,
    } = Opt::from_args();
    let config_file = std::fs::read(&config_path)?;
    Config::from_buffer(&config_file)?;
    if check_config {
        return Ok(());
    }
    if !foreground
        && std::env::var_os(DAEMONIZED_ENV).as_deref()
            != Some(std::ffi::OsStr::new("1"))
    {
        daemonize()?;
        // The updater replaces this process with exec. Preserve the daemon state so the
        // replacement keeps the same PID instead of forking a second time.
        std::env::set_var(DAEMONIZED_ENV, "1");
    }
    run(config_file, no_auto_update)
}

#[tokio::main]
async fn run(config_file: Vec<u8>, no_auto_update: bool) -> Result<(), Box<dyn Error>> {
    let config = Config::from_buffer(&config_file)?;
    // Claim the singleton before spawning jobs. Both descriptors close automatically on exec.
    let (socket, _socket_lock) = bind_cli_socket(Path::new(CLI_SOCKET_PATH))?;
    let args: Vec<_> = std::env::args_os().skip(1).collect();

    // We need to construct media bundle here for all the jobs that would require it
    let client = BundleClient::default();
    let media_bundle = MediaBundle::<BundleClient>::from_client_with_config(client, &config);

    let (move_job_handle, move_job_sender) = {
        let mut move_job_handle = move_job::JobStruct::spawn(&config)?;
        move_job_handle.assign_bundle(media_bundle.clone());
        let sender = move_job_handle.give_sender()?;
        (move_job_handle, sender)
    };

    let move_job_handle = move_job_handle.instrument(tracing::trace_span!("move_job"));

    // Scan job set up
    let mut scan_job = scan_library_job::JobStruct::<BundleClient>::spawn(&config)?;
    scan_job.assign_media_bundle(media_bundle.clone());
    scan_job.assign_move_job_sender(move_job_sender.clone());

    // Auto shutoff job
    let mut auto_shutoff_job = auto_torrent_shutoff_job::JobStruct::<BundleClient>::spawn(&config)?;
    auto_shutoff_job.assign_media_bundle(media_bundle.clone());

    // Main listener set up
    // TODO: set up listener for scan job
    let listener: MainListener<_, Unassigned> = MainListener::default();
    let listener = listener.assign_sender_bundle(media_bundle);
    let listener = listener.assign_movejob_sender(move_job_sender.clone());
    let listener = listener
        .initiate_listener(socket)
        .instrument(tracing::trace_span!("listener"));

    // Logging set up
    let logger: Logger<Unprimed> = Logger::from_config(&config);
    let (logger, guard, _logger_tx) = logger.prime();

    tokio::select! {
        _ = move_job_handle => {
            println!("Move job exited");
        }
        listener_return = listener => {
            println!("Listener finished {:?}", listener_return);
        }
        _ = auto_shutoff_job => {
            println!("Auto shutoff job exited");
        }
        _ = logger => {
            println!("Logger finished");
        }
        _ = scan_job => {
            println!("Scan job exited");
        }
        prepared = update::wait_for_update(&args), if !no_auto_update => {
            // Stop is handled between move batches, never in the middle of copying a file.
            let (reply, stopped) = tokio::sync::oneshot::channel();
            move_job_sender.send((move_job::IncomingMessage::Stop, Some(reply))).await?;
            if stopped.await? != move_job::OutgoingMessage::Ok {
                return Err("Move job did not stop for update".into());
            }
            tracing::info!("Installing verified update and replacing the server process");
            drop(guard); // Flush logs before exec replaces the entire process.
            let error = prepared.install().and_then(|installed| installed.exec(&args));
            if let Err(error) = &error {
                if error.is::<update::RollbackIncomplete>() {
                    // Never start a potentially mismatched server/client installation.
                    return Err(error.to_string().into());
                }
            }
            // Exec only returns on failure; the install transaction has restored the old pair.
            eprintln!("Update failed, restarting the current version: {error:?}");
            let retry_after = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() + 24 * 60 * 60;
            let error = std::process::Command::new(update::SERVER_PATH)
                .args(&args)
                .env(update::RETRY_AFTER_ENV, retry_after.to_string())
                .exec();
            return Err(error.into());
        }
    }

    Ok::<(), Box<dyn Error>>(())
}
