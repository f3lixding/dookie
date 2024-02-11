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

/// Test related. Not sure if there is a better way to do this but this mainly helps with mocking
/// so that tests can be written more easily.
#[async_trait]
pub(in crate::media_bundle) trait BundleResponse {
    async fn as_bytes(self) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>>;
}

#[async_trait]
impl BundleResponse for reqwest::Response {
    async fn as_bytes(self) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
        let res = self.bytes().await?.into();

        Ok(res)
    }
}

#[async_trait]
pub(in crate::media_bundle) trait IBundleClient: Send + Sync {
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

/// This is really just a wrapper around reqwest::Client.
/// The existence of it is to also retain information about the port. This way the constructor for
/// ServerEntity is "prettier".
#[derive(Debug, Clone)]
pub(in crate::media_bundle) struct BundleClient {
    client: reqwest::Client,
    token_name: String,
    token_value: String,
    port: u16,
}

#[async_trait]
impl IBundleClient for BundleClient {
    fn from_port(port: u16) -> Self {
        BundleClient {
            client: reqwest::Client::new(),
            token_name: String::default(),
            token_value: String::default(),
            port,
        }
    }

    fn set_port(&mut self, port: u16) {
        self.port = port;
    }

    fn set_token(&mut self, token: (impl AsRef<str>, impl AsRef<str>)) {
        self.token_name = token.0.as_ref().to_string();
        self.token_name = token.1.as_ref().to_string();
    }

    async fn get(&self, url: &str) -> Result<reqwest::Response, Box<dyn Error + Send + Sync>> {
        let url = format!("http://localhost:{}/{}", self.port, url);
        Ok(self
            .client
            .get(&url)
            .header(&self.token_name, &self.token_value)
            .send()
            .await?)
    }

    async fn post(
        &self,
        url: &str,
        body: impl Into<reqwest::Body> + Send + Sync,
    ) -> Result<reqwest::Response, Box<dyn Error + Send + Sync>> {
        let url = format!("http://localhost:{}/{}", self.port, url);
        let msg_body: reqwest::Body = body.into();

        Ok(self
            .client
            .post(&url)
            .header(&self.token_name, &self.token_value)
            .body::<reqwest::Body>(msg_body)
            .send()
            .await?)
    }
}

#[async_trait]
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
pub struct MediaBundle {}

unsafe impl Send for MediaBundle {}
unsafe impl Sync for MediaBundle {}

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
