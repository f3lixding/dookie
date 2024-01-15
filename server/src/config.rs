use serde::{Deserialize, Serialize};
use serde_yaml;
use std::borrow::Cow;
use std::error::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config<'a> {
    pub config_path: Cow<'a, str>,
    pub log_path: Cow<'a, str>,
    pub radarr_port: u16,
    pub sonarr_port: u16,
    pub prowlarr_port: u16,
    pub qbit_torrent_port: u16,
    pub radarr_api_key: Cow<'a, str>,
    pub sonarr_api_key: Cow<'a, str>,
    pub prowlarr_api_key: Cow<'a, str>,
    pub qbit_torrent_api_key: Cow<'a, str>,
    pub move_job_period: u64,
    pub age_threshold: u64,
    pub root_path_local: Cow<'a, str>,
    pub root_path_ext: Cow<'a, str>,
}

impl<'a> Config<'a> {
    pub fn from_buffer(buffer: &'a [u8]) -> Result<Config<'a>, Box<dyn Error>> {
        Ok(serde_yaml::from_slice(buffer)?)
    }
}
