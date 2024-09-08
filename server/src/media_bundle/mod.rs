mod plex;
mod prowlarr;
mod qbit;
mod radarr;
mod sonarr;

use plex::*;
use prowlarr::*;
use qbit::*;
use radarr::*;
use sonarr::*;

use async_trait::async_trait;
use std::error::Error;
use std::marker::Unpin;

use crate::Config;

/// Test related. Not sure if there is a better way to do this but this mainly helps with mocking
/// so that tests can be written more easily.
#[async_trait]
pub trait BundleResponse {
    async fn as_bytes(self) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>>;
    fn get_statuscode(&self) -> u16;
}

#[async_trait]
impl BundleResponse for reqwest::Response {
    async fn as_bytes(self) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        let res = self.bytes().await?.into();

        Ok(res)
    }

    fn get_statuscode(&self) -> u16 {
        self.status().as_u16()
    }
}

#[async_trait]
pub trait IBundleClient: Unpin + Send + Sync + Clone + 'static {
    fn from_port(port: u16) -> Self;
    fn set_port(&mut self, port: u16);
    fn set_token(&mut self, token: (impl AsRef<str>, impl AsRef<str>));
    async fn get(&self, url: &str) -> Result<impl BundleResponse, Box<dyn Error + Send + Sync>>;
    async fn post(
        &self,
        url: &str,
        body: impl Into<reqwest::Body> + Send + Sync,
    ) -> Result<impl BundleResponse, Box<dyn Error + Send + Sync>>;
}

#[async_trait]
// TODO: make it so that input takes reference. Currently the trait makes no room for lifetime
pub(in crate::media_bundle) trait ServerEntity {
    type Input;
    type Output;
    type Client: IBundleClient;

    async fn make_call(
        &self,
        input: Self::Input,
    ) -> Result<Self::Output, Box<dyn Error + Send + Sync>>;

    fn from_bundle_client(client: Self::Client) -> impl ServerEntity;
    fn get_app_name(&self) -> &str;

    /// Provided method to start the underlying server.
    async fn start(&self) -> Result<u32, Box<dyn Error + Send + Sync>> {
        Ok(bundle_util::start_process(self.get_app_name())?)
    }

    /// Provided method to stop the underlying server.
    async fn shutdown(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        let process_name = self.get_app_name();
        todo!("Implement proper shutdown logic for {}", process_name);
        // TODO: implement proper shutdown logic
        Ok(())
    }
}

/// This is the construct used to bundle up all the different clients (i.e. radarr, sonarr, plex,
/// etc)
/// MediaBundle is to have a compositional relationship with ServerEntity.
#[derive(Debug, Clone, Default)]
pub struct MediaBundle<C>
where
    C: IBundleClient,
{
    plex_client: Plex<C>,
}

unsafe impl<C> Send for MediaBundle<C> where C: IBundleClient {}
unsafe impl<C> Sync for MediaBundle<C> where C: IBundleClient {}

impl<C> MediaBundle<C>
where
    C: IBundleClient,
{
    pub fn from_client_with_config(client: C, config: &Config) -> Self {
        let Config {
            plex_api_key,
            plex_machine_id,
            plex_client_id,
            ..
        } = config;

        Self {
            plex_client: {
                let mut client = client.clone();
                client.set_port(config.plex_port);
                client.set_token(("X-Plex-Token", plex_api_key.clone()));
                Plex::new(
                    client,
                    plex_machine_id.to_string(),
                    plex_client_id.to_string(),
                    plex_api_key.to_string(),
                )
            },
        }
    }
    /// Refresh the plex library. Be careful when calling this method as a disconnected DAS would
    /// mean the library would be emptied. Only use this when DAS is confirmed to be connected.
    pub async fn refresh_libraries(
        &self,
        lib_id: usize,
    ) -> Result<reqwest::StatusCode, Box<dyn Error + Send + Sync>> {
        let input = PlexInput::RefreshLibrary(lib_id);
        let output = self.plex_client.make_call(input).await?;

        let PlexOutput::StatusCode(code) = output else {
            return Err("Unexpected output".into());
        };

        Ok(reqwest::StatusCode::from_u16(code)?)
    }

    pub async fn grant_library_access(
        &self,
        email: &str,
    ) -> Result<reqwest::StatusCode, Box<dyn Error + Send + Sync>> {
        let machine_id = &self.plex_client.machine_id;
        let input = PlexInput::grant_access(email.to_string(), machine_id.to_string());
        let output = self.plex_client.make_call(input).await;

        match output {
            Ok(PlexOutput::StatusCode(code)) => Ok(reqwest::StatusCode::from_u16(code)?),
            Err(why) => Err(why),
            _ => Err("Unexpected output".into()),
        }
    }
}

/// A collection of utility functions for media bundles
pub(in crate::media_bundle) mod bundle_util {
    use std::error::Error;
    use std::process::Command;
    use std::{thread, time};

    pub fn get_process_id(process_name: &str) -> Result<Option<u32>, Box<dyn Error + Send + Sync>> {
        let processed_process_name = if !process_name.is_empty() {
            let first_letter_wrapped = format!(
                "[{}]",
                process_name.get(..1).ok_or("Process name is empty")?
            );

            format!(
                "{}{}",
                first_letter_wrapped,
                process_name.get(1..).ok_or("Process name is empty")?
            )
        } else {
            return Err("Process name is empty".into());
        };

        let process_search_arg = format!(r#"ps aux | grep -i "{}""#, processed_process_name);
        let output = Command::new("sh")
            .arg("-c")
            .arg(process_search_arg)
            .output()?;

        let binding = String::from_utf8_lossy(&output.stdout);
        let output = binding.split_whitespace().collect::<Vec<&str>>();
        let pid: Option<u32> = output.get(1).and_then(|s| s.parse().ok());

        Ok(pid)
    }

    pub fn start_process(process_name: &str) -> Result<u32, Box<dyn Error + Send + Sync>> {
        let pid = get_process_id(process_name)?;
        if pid.is_some() {
            return Ok(pid.unwrap());
        }

        let _ = Command::new("sh")
            .arg("-c")
            .arg(format!(r#"open -a {}"#, process_name))
            .output()?;

        // Wait for process to start
        thread::sleep(time::Duration::from_millis(500));
        let pid = get_process_id(process_name)?;

        Ok(pid.ok_or("Failed to start process")?)
    }

    #[cfg(test)]
    // TODO: Test start process as well (we might need to manually spawn a process for it)
    mod tests {
        use super::*;

        // I am not sure if this is a good way to test it but I need some process to be searched
        // for in order to test for the functions. And
        const PROCESS_NAME: &'static str = "Application";

        #[test]
        fn test_get_process_id() {
            let pid = get_process_id(PROCESS_NAME);
            assert!(pid.is_ok());

            let pid = pid.unwrap();
            assert!(pid.is_some());

            let pid = pid.unwrap();
            assert!(pid > 0);
        }

        // #[test]
        // fn test_start_process() {
        //     let pid = start_process(RADARR_PROCESS_NAME);
        //     println!("pid is: {:?}", pid);
        //     assert!(pid.is_ok());

        //     let pid = pid.unwrap();
        //     assert!(pid > 0);

        //     println!("pid is: {}", pid);
        // }
    }
}
