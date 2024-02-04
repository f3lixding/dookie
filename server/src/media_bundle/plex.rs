use crate::media_bundle::{IBundleClient, ServerEntity};
use async_trait::async_trait;

const APP_NAME: &'static str = "Plex Media Server";

pub(in crate::media_bundle) enum PlexInput {
    GetSessionHistory,
}

pub(in crate::media_bundle) enum PlexOutput {
    // TODO: introduce a real struct for session object
    SessionHistory(Vec<i32>),
}

pub(in crate::media_bundle) struct Plex<C>
where
    C: IBundleClient,
{
    client: C,
}

#[async_trait]
impl<C> ServerEntity for Plex<C>
where
    C: IBundleClient,
{
    type Input = PlexInput;
    type Output = PlexOutput;
    type Client = C;

    async fn make_call(&self, input: Self::Input) -> Self::Output {
        match input {
            PlexInput::GetSessionHistory => todo!(),
        }
    }

    fn from_bundle_client(client: C) -> Self {
        Plex { client }
    }

    fn get_app_name(&self) -> &str {
        APP_NAME
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_bundle::BundleResponse;
    use std::collections::HashMap;
    use std::error::Error;

    #[derive(Default)]
    struct MockResponse {
        status: u16,
        body: Vec<u8>,
    }

    impl BundleResponse for MockResponse {}

    #[derive(Default)]
    struct MockClient {
        return_map: HashMap<String, MockResponse>,
    }

    // The real constructor we are going to use for testing purposes
    impl MockClient {
        pub fn new_from_return_map(return_map: HashMap<String, MockResponse>) -> Self {
            MockClient { return_map }
        }
    }

    #[async_trait]
    impl IBundleClient for MockClient {
        fn from_scratch(_port: u16) -> Self {
            MockClient::default()
        }

        fn clone_with_port(&self, _port: u16) -> Self {
            MockClient::default()
        }

        // Here we mock some responses based on the url passed in
        async fn get(&self, _url: &str) -> Result<MockResponse, Box<dyn Error + Send + Sync>> {
            Ok(MockResponse::default())
        }

        async fn post(
            &self,
            _url: &str,
            _body: impl Into<reqwest::Body> + Send + Sync,
        ) -> Result<reqwest::Response, Box<dyn Error + Send + Sync>> {
            todo!()
        }
    }

    #[tokio::test]
    async fn test_get_session_history() {}
}
