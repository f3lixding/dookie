use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::error::Error;

use crate::media_bundle::{BundleResponse, IBundleClient, ServerEntity};

const APP_NAME: &'static str = "Plex Media Server";

#[derive(Debug, Serialize, Deserialize)]
pub(in crate::media_bundle) struct OuterMediaContainer {
    #[serde(rename = "MediaContainer")]
    media_container: MediaContainer,
}

#[derive(Debug, Serialize, Deserialize)]
pub(in crate::media_bundle) struct MediaContainer {
    size: i32,
    #[serde(rename = "Metadata")]
    metadata: Vec<Metadata>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(in crate::media_bundle) struct Metadata {
    title: String,
    #[serde(rename = "type")]
    type_: MediaType,
    #[serde(rename = "ratingKey")]
    rating_key: String,
    #[serde(rename = "viewCount")]
    view_count: Option<i32>,
    #[serde(rename = "lastViewedAt")]
    last_viewed_at: Option<u64>,
    #[serde(rename = "leafCount")]
    leaf_count: Option<i32>,
    #[serde(rename = "childCount")]
    child_count: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(in crate::media_bundle) enum MediaType {
    #[serde(rename = "movie")]
    Movie,
    #[serde(rename = "show")]
    Show,
}

pub(in crate::media_bundle) enum PlexInput {
    GetSessionHistory(i32),
    GetAllShows,
    GetAllMovies,
}

pub(in crate::media_bundle) enum PlexOutput {
    // TODO: introduce a real struct for various objects
    SessionHistory(Metadata),
    ShowList(Vec<Metadata>),
    MovieList(Vec<Metadata>),
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

    async fn make_call(
        &self,
        input: Self::Input,
    ) -> Result<Self::Output, Box<dyn Error + Send + Sync>> {
        match input {
            PlexInput::GetSessionHistory(id) => {
                static url: &'static str = "/status/sessions/history/all";
                let resp = self.client.get(url).await?;
                let body_as_bytes = resp.as_bytes().await?;

                let container: MediaContainer = serde_json::from_slice(&body_as_bytes)?;
                Ok(PlexOutput::ShowList(vec![]))
            }
            PlexInput::GetAllShows => todo!(),
            PlexInput::GetAllMovies => todo!(),
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

    #[async_trait]
    impl BundleResponse for MockResponse {
        async fn as_bytes(self) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
            Ok(self.body)
        }
    }

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

    #[test]
    fn test_deserialize() {
        let json_as_bytes = std::fs::read("test_data/movies_metadata.txt").unwrap();
        let media_container =
            serde_json::from_slice::<OuterMediaContainer>(&json_as_bytes).unwrap();
        println!("{:#?}", media_container);
        println!(
            "length is: {}",
            media_container.media_container.metadata.len()
        );
    }
}
