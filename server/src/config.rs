use serde::{Deserialize, Serialize};
use serde_yaml;
use std::borrow::Cow;
use std::collections::HashMap;
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
    pub move_map: HashMap<Cow<'a, str>, Cow<'a, str>>,
}

impl<'a> Config<'a> {
    pub fn from_buffer(buffer: &'a [u8]) -> Result<Config<'a>, Box<dyn Error>> {
        Ok(serde_yaml::from_slice(buffer)?)
    }
}

impl<'a> Default for Config<'a> {
    fn default() -> Self {
        Config {
            config_path: ".".into(),
            log_path: "/~/Library/Logs/dookie".into(),
            radarr_port: 7878,
            sonarr_port: 8989,
            prowlarr_port: 8888,
            qbit_torrent_port: 9090,
            radarr_api_key: "".into(),
            sonarr_api_key: "".into(),
            prowlarr_api_key: "".into(),
            qbit_torrent_api_key: "".into(),
            move_job_period: 100,
            age_threshold: 100,
            move_map: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_YML: &'static str = r#"
            config_path: "."
            log_path: "/~/Library/Logs/dookie"
            radarr_port: 7878
            sonarr_port: 8989
            prowlarr_port: 8888
            qbit_torrent_port: 9090
            radarr_api_key: "some_key"
            sonarr_api_key: "some_key"
            prowlarr_api_key: "some_key"
            qbit_torrent_api_key: "some_key"
            move_job_period: 100
            age_threshold: 100
            move_map:
                "/root/path/local/one": "/root/path/ext/one"
                "/root/path/local/two": "/root/path/ext/two"
        "#;

    #[test]
    fn test_read_config() {
        let config = Config::from_buffer(TEST_YML.as_bytes());
        println!("{:?}", config);
        assert!(config.is_ok());
    }
}
