use std::{io::Write, os::unix::net::UnixStream};

mod dookie_proto {
    include!(concat!(env!("OUT_DIR"), "/dookie.rs"));
}

use dookie_proto::{MoveJobCommand, MoveJobRequest, MoveJobResponse, MoveJobStatus};
use prost::Message;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = UnixStream::connect("/tmp/dookie.sock")?;

    let request = MoveJobRequest { command: 0 };
    let request = request.encode_to_vec();

    stream.write_all(&request)?;

    Ok(())
}
