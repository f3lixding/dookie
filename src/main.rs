use dookie::{move_job, Job};
use std::error::Error;
use structopt::StructOpt;

#[derive(StructOpt)]
struct Opt {
    #[structopt(short = "c", long = "config_path", default_value = ".")]
    config_path: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let Opt { config_path } = Opt::from_args();

    Ok::<(), Box<dyn Error>>(())
}
