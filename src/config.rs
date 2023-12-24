use serde::{Deserialize, Serialize};
use serde_yaml;
use std::borrow::Cow;
use std::error::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config<'a> {
    pub config_path: Cow<'a, str>,
    pub radarr_port: u16,
    pub sonarr_port: u16,
    pub prowlarr_port: u16,
    pub qbit_torrent_port: u16,
    pub radarr_api_key: String,
    pub sonarr_api_key: String,
    pub prowlarr_api_key: String,
    pub qbit_torrent_api_key: String,
}

impl<'a> Config<'a> {
    pub fn from_buffer(buffer: &'a [u8]) -> Result<Config<'a>, Box<dyn Error>> {
        Ok(serde_yaml::from_slice(buffer)?)
    }
}
