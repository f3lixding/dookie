use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::error::Error;

use crate::media_bundle::{BundleResponse, IBundleClient, ServerEntity};

const APP_NAME: &'static str = "Plex Media Server";

#[derive(Debug, Serialize, Deserialize)]
pub(in crate::media_bundle) struct OuterMediaContainer {
    #[serde(rename = "MediaContainer")]
    pub media_container: MediaContainer,
}

#[derive(Debug, Serialize, Deserialize)]
pub(in crate::media_bundle) struct MediaContainer {
    pub size: i32,
    #[serde(rename = "Metadata")]
    pub metadata: Vec<Metadata>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(in crate::media_bundle) struct Metadata {
    pub title: String,
    #[serde(rename = "type")]
    pub type_: MediaType,
    // this needs to be optional because deleted items would no longer have an item key
    #[serde(rename = "ratingKey")]
    pub rating_key: Option<String>,
    #[serde(rename = "viewCount")]
    pub view_count: Option<i32>,
    #[serde(rename = "lastViewedAt")]
    pub last_viewed_at: Option<u64>,
    #[serde(rename = "viewedAt")]
    pub viewed_at: Option<u64>,
    #[serde(rename = "leafCount")]
    pub leaf_count: Option<i32>,
    #[serde(rename = "childCount")]
    pub child_count: Option<i32>,
    #[serde(rename = "accountID")]
    pub account_id: Option<i32>,
    #[serde(rename = "grandparentTitle")]
    pub grandparent_title: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub(in crate::media_bundle) enum MediaType {
    #[serde(rename = "movie")]
    Movie,
    #[serde(rename = "show")]
    Show,
    #[serde(rename = "episode")]
    Episode,
}

pub(in crate::media_bundle) enum PlexInput {
    GetSessionHistory(i32),
    GetAllShows,
    GetAllMovies,
}

pub(in crate::media_bundle) enum PlexOutput {
    // TODO: introduce a real struct for various objects
    SessionHistory(Vec<Metadata>),
    ShowList(Vec<Metadata>),
    MovieList(Vec<Metadata>),
}

#[derive(Default, Debug, Clone)]
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

                Ok(PlexOutput::SessionHistory(vec![]))
            }
            PlexInput::GetAllShows => {
                static url: &'static str = "/library/sections/1/all";

                Ok(PlexOutput::ShowList(vec![]))
            }
            PlexInput::GetAllMovies => {
                static url: &'static str = "/library/sections/2/all";

                Ok(PlexOutput::MovieList(vec![]))
            }
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
    use crate::media_bundle::{self, BundleResponse};
    use std::collections::HashMap;
    use std::error::Error;

    const MOVIE_TEST_METADATA: &'static str = "test_data/movies_metadata.json";
    const SHOW_TEST_METADATA: &'static str = "test_data/shows_metadata.json";
    const SESSION_TEST_METADATA: &'static str = "test_data/session_history.json";

    const MOVIE_LIB_METADATA_URL: &'static str = "/library/sections/1/all";
    const SHOW_LIB_METADATA_URL: &'static str = "/library/sections/2/all";
    const SESSION_HISTORY_URL: &'static str = "/status/sessions/history/all";

    #[derive(Default, Clone)]
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

    #[derive(Default, Clone)]
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
        fn from_port(_port: u16) -> Self {
            MockClient::default()
        }

        // Don't really need to implement this as we are mocking the response
        fn set_port(&mut self, _port: u16) {}

        // Don't really need to implement this as we are mocking the response
        fn set_token(&mut self, _token: (impl AsRef<str>, impl AsRef<str>)) {}

        // Here we mock some responses based on the url passed in
        async fn get(&self, url: &str) -> Result<MockResponse, Box<dyn Error + Send + Sync>> {
            let json_as_bytes = match url {
                MOVIE_LIB_METADATA_URL => tokio::fs::read(MOVIE_TEST_METADATA).await?,
                SHOW_LIB_METADATA_URL => tokio::fs::read(SHOW_TEST_METADATA).await?,
                _ => panic!("Unexpected url: {}", url),
            };

            Ok(MockResponse {
                status: 200,
                body: json_as_bytes,
            })
        }

        async fn post(
            &self,
            _url: &str,
            _body: impl Into<reqwest::Body> + Send + Sync,
        ) -> Result<reqwest::Response, Box<dyn Error + Send + Sync>> {
            todo!()
        }
    }

    #[test]
    fn test_deserialize() {
        // checking for movies
        let json_as_bytes = std::fs::read(MOVIE_TEST_METADATA).unwrap();
        let media_container = serde_json::from_slice::<OuterMediaContainer>(&json_as_bytes);
        assert!(media_container.is_ok());

        let media_container = media_container.unwrap().media_container;
        let metadata = media_container.metadata;
        for item in &metadata {
            assert!(
                item.type_ == MediaType::Movie,
                "Expected movie metadata to consist of only movies"
            );
        }

        // checking for shows
        let json_as_bytes = std::fs::read(SHOW_TEST_METADATA).unwrap();
        let media_container = serde_json::from_slice::<OuterMediaContainer>(&json_as_bytes);
        assert!(media_container.is_ok());

        let media_container = media_container.unwrap().media_container;
        let metadata = media_container.metadata;
        for item in &metadata {
            assert!(
                item.type_ == MediaType::Show,
                "Expected show metadata to consist of only shows"
            );
        }

        // checking for session history
        let json_as_bytes = std::fs::read(SESSION_TEST_METADATA).unwrap();
        let media_container = serde_json::from_slice::<OuterMediaContainer>(&json_as_bytes);
        println!("{:?}", media_container);
        assert!(media_container.is_ok());

        let media_container = media_container.unwrap().media_container;
        let metadata = media_container.metadata;
        for item in &metadata {
            assert!(item.account_id.is_some(), "Expected account_id to be set");
            assert!(
                item.viewed_at.is_some(),
                "Expected viewed_at to be set since this a session history"
            );
            match item.type_ {
                MediaType::Movie => assert!(item.grandparent_title.is_none()),
                MediaType::Show => panic!("Metadata should not categorize type as show"),
                MediaType::Episode => assert!(
                    item.grandparent_title.is_some(),
                    "Grand parent title should be set for shows"
                ),
            }
        }
    }

    #[tokio::test]
    async fn test_plex_server_entity() {}
}
