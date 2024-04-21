use crate::media_bundle::IBundleClient;
use async_trait::async_trait;
use std::error::Error;

/// This is really just a wrapper around reqwest::Client.
/// The existence of it is to also retain information about the port. This way the constructor for
/// ServerEntity is "prettier".
#[derive(Debug, Clone, Default)]
pub struct BundleClient {
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
        self.token_value = token.1.as_ref().to_string();
    }

    #[allow(refining_impl_trait)]
    async fn get(&self, url: &str) -> Result<reqwest::Response, Box<dyn Error + Send + Sync>> {
        let url = format!("http://localhost:{}/{}", self.port, url);
        Ok(self
            .client
            .get(&url)
            .header(&self.token_name, &self.token_value)
            .send()
            .await?)
    }

    #[allow(refining_impl_trait)]
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
