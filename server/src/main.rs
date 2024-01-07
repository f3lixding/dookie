use dookie_server_lib::{move_job, Config, Job};
use std::{env::temp_dir, error::Error};
use structopt::StructOpt;

mod dookie_proto {
    include!(concat!(env!("OUT_DIR"), "/dookie.rs"));
}

use dookie_proto::{MoveJobRequest, MoveJobResponse, MoveJobStatus};

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
    };
    // let spawned_move_job = move_job::JobStruct::spawn(&config);

    listen().await?;
    Ok::<(), Box<dyn Error>>(())
}

async fn listen() -> Result<(), Box<dyn std::error::Error>> {
    let socket_path = "/tmp/dookie.sock";
    let listener = tokio::net::UnixListener::bind(&socket_path)?;

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let ready = stream.ready(tokio::io::Interest::READABLE).await?;
                let mut data = vec![0; 1024];
                match stream.try_read(&mut data) {
                    Ok(n) => {
                        println!("received data");
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            Err(err) => println!("Error accepting connection: {}", err),
        }
    }

    #[allow(unreachable_code)]
    Ok(())
}
