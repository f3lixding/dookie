use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
};

mod dookie_proto {
    include!(concat!(env!("OUT_DIR"), "/dookie.rs"));
}

use dookie_proto::*;
use prost::Message;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
#[structopt(name = "dookiec", about = "Control the local dookie server")]
struct Cli {
    /// Unix socket exposed by the local server.
    #[structopt(long, parse(from_os_str), default_value = "/tmp/dookie.sock")]
    socket: std::path::PathBuf,
    #[structopt(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, StructOpt)]
enum Command {
    /// Show whether the move job is running or when it will run next (the default).
    Status,
    /// Start moving actual files in the server's move_map in the background, ignoring their age.
    /// Existing symlinks are skipped; moved items are replaced with symlinks.
    Move,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::from_args();
    let mut stream = UnixStream::connect(cli.socket)?;

    let inner_request = MoveJobRequest {
        command: match cli.command.unwrap_or(Command::Status) {
            Command::Status => MoveJobCommand::Statusrequest as i32,
            Command::Move => MoveJobCommand::Startjob as i32,
        },
    };
    let request = Envelope {
        data_type: DataType::Movejobrequest as i32,
        data: Some(envelope::Data::MoveJobRequest(inner_request)),
    };
    let request = request.encode_to_vec();

    stream.write_all(&request)?;

    let mut buf = Vec::new();
    let mut temp_buf = [0; 1024];
    loop {
        let size = stream.read(&mut temp_buf)?;
        if size == 0 {
            break;
        }

        buf.extend_from_slice(&temp_buf[..size]);
    }

    let response = Envelope::decode(buf.as_slice())?;
    let Some(envelope::Data::MoveJobResponse(response)) = response.data else {
        return Err("Unexpected response from server".into());
    };
    match response
        .status
        .and_then(|status| MoveJobStatus::try_from(status).ok())
    {
        Some(MoveJobStatus::Idle) => println!(
            "Move job is idle; next run in {} seconds.",
            response.time_to_next_run.ok_or("Missing next run time")?
        ),
        Some(MoveJobStatus::Running) => {
            println!("Move job is already running; no new move started.")
        }
        Some(MoveJobStatus::Ok) => println!("Move job started. Check server logs for the outcome."),
        Some(MoveJobStatus::Error) => {
            return Err("Failed to start move job; check the server logs".into());
        }
        None => return Err("Missing or unknown move job status".into()),
    }

    Ok(())
}
