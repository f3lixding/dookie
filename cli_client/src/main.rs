use std::{io::Write, os::unix::net::UnixStream};

mod dookie_proto {
    include!(concat!(env!("OUT_DIR"), "/dookie.rs"));
}

use dookie_proto::*;
use prost::Message;

// Note that all of this is just a demo right now and I shall make a proper cli client later
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = UnixStream::connect("/tmp/dookie.sock")?;

    let inner_request = MoveJobRequest {
        command: MoveJobCommand::Statusrequest as i32,
    };
    let request = Envelope {
        data_type: DataType::from_str_name("MOVEJOBREQUEST").unwrap().into(),
        data: Some(envelope::Data::MoveJobRequest(inner_request)),
    };
    let request = request.encode_to_vec();

    stream.write_all(&request)?;

    Ok(())
}
