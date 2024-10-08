use serde::{Deserialize, Serialize};
use serde_yaml;
use std::borrow::Cow;
use std::collections::HashMap;
use std::error::Error;

use crate::discord_bot::GuildConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config<'a> {
    pub config_path: Cow<'a, str>,
    pub log_path: Cow<'a, str>,
    pub radarr_port: u16,
    pub sonarr_port: u16,
    pub prowlarr_port: u16,
    pub plex_port: u16,
    pub webhook_port: Option<u16>,
    pub qbit_torrent_port: u16,
    pub radarr_api_key: Cow<'a, str>,
    pub sonarr_api_key: Cow<'a, str>,
    pub prowlarr_api_key: Cow<'a, str>,
    pub qbit_torrent_api_key: Cow<'a, str>,
    pub plex_api_key: Cow<'a, str>,
    pub plex_client_id: Cow<'a, str>,
    pub plex_machine_id: Cow<'a, str>,
    pub move_job_period: u64,
    pub age_threshold: u64,
    pub discord_token: Option<Cow<'a, str>>,
    pub permitted_guilds: Vec<u64>,
    pub move_map: HashMap<Cow<'a, str>, Cow<'a, str>>,
    pub guild_config: Option<GuildConfig>,
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
            plex_port: 32400,
            webhook_port: Some(1025),
            radarr_api_key: "".into(),
            sonarr_api_key: "".into(),
            prowlarr_api_key: "".into(),
            qbit_torrent_api_key: "".into(),
            plex_api_key: "".into(),
            plex_client_id: "".into(),
            plex_machine_id: "".into(),
            move_job_period: 100,
            age_threshold: 100,
            discord_token: Some("".into()),
            permitted_guilds: Vec::new(),
            move_map: HashMap::new(),
            guild_config: None,
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
            plex_port: 32400
            radarr_api_key: "some_key"
            sonarr_api_key: "some_key"
            prowlarr_api_key: "some_key"
            qbit_torrent_api_key: "some_key"
            plex_api_key: "some_key"
            plex_client_id: "some_key"
            plex_machine_id: "some_key"
            move_job_period: 100
            age_threshold: 100
            discord_token: "some_key"
            permitted_guilds: [1, 2, 3]
            move_map:
                "/root/path/local/one": "/root/path/ext/one"
                "/root/path/local/two": "/root/path/ext/two"
            guild_id: 123
            channels:
                - name: "channel_one"
                  kind: "text"
                  topic: "test"
                  nsfw: false
                  position: 0
                  permissions:
                    - role_id: 123
                      allow: ["SEND_MESSAGES", "VIEW_CHANNEL"]
                      deny: []
                - name: "channel_two"
                  kind: "test"
                  topic: "test"
                  nsfw: false
                  position: 1
                  permissions: 
                    - role_id: 123
                      allow: []
                      deny: ["SEND_MESSAGES", "VIEW_CHANNEL"] 
                    - role_id: 456
                      allow: ["SEND_MESSAGES", "VIEW_CHANNEL"]
                      deny: []
        "#;

    #[test]
    fn test_read_config() {
        let config = Config::from_buffer(TEST_YML.as_bytes());
        assert!(config.is_ok());
    }
}
