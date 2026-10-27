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
use serde::{Deserialize, Serialize};
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

/// A movie or show found through radarr / sonarr, trimmed down to what is needed to show it to
/// someone so they can confirm it is the one they want.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaLookup {
    /// The tmdb id for movies and the tvdb id for shows. This is what adding takes.
    pub id: u64,
    pub title: String,
    /// 0 when unknown.
    pub year: u32,
    pub overview: Option<String>,
    pub poster_url: Option<String>,
    /// Whether radarr / sonarr is already tracking it.
    pub tracked: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AddOutcome {
    Added(MediaLookup),
    AlreadyTracked(MediaLookup),
}

/// An image entry in radarr / sonarr resources.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::media_bundle) struct ArrImage {
    cover_type: String,
    remote_url: Option<String>,
    #[serde(flatten)]
    rest: serde_json::Map<String, serde_json::Value>,
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
    radarr_client: Radarr<C>,
    sonarr_client: Sonarr<C>,
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
            radarr_client: {
                let mut client = client.clone();
                client.set_port(config.radarr_port);
                client.set_token(("X-Api-Key", config.radarr_api_key.clone()));
                Radarr::new(client)
            },
            sonarr_client: {
                let mut client = client.clone();
                client.set_port(config.sonarr_port);
                client.set_token(("X-Api-Key", config.sonarr_api_key.clone()));
                Sonarr::new(client)
            },
        }
    }

    pub async fn search_movies(
        &self,
        term: &str,
    ) -> Result<Vec<MediaLookup>, Box<dyn Error + Send + Sync>> {
        match self
            .radarr_client
            .make_call(RadarrInput::Lookup(term.to_string()))
            .await?
        {
            RadarrOutput::Movies(movies) => Ok(movies.into_iter().map(Into::into).collect()),
            _ => Err("Unexpected output".into()),
        }
    }

    /// Adds the movie to radarr (monitored) and kicks off a search for it.
    pub async fn add_movie(
        &self,
        tmdb_id: u64,
    ) -> Result<AddOutcome, Box<dyn Error + Send + Sync>> {
        match self
            .radarr_client
            .make_call(RadarrInput::Add(tmdb_id))
            .await?
        {
            RadarrOutput::Added(outcome) => Ok(outcome),
            _ => Err("Unexpected output".into()),
        }
    }

    pub async fn search_shows(
        &self,
        term: &str,
    ) -> Result<Vec<MediaLookup>, Box<dyn Error + Send + Sync>> {
        match self
            .sonarr_client
            .make_call(SonarrInput::Lookup(term.to_string()))
            .await?
        {
            SonarrOutput::Series(series) => Ok(series.into_iter().map(Into::into).collect()),
            _ => Err("Unexpected output".into()),
        }
    }

    /// Adds the show to sonarr (all seasons monitored) and kicks off a search for it.
    pub async fn add_show(&self, tvdb_id: u64) -> Result<AddOutcome, Box<dyn Error + Send + Sync>> {
        match self
            .sonarr_client
            .make_call(SonarrInput::Add(tvdb_id))
            .await?
        {
            SonarrOutput::Added(outcome) => Ok(outcome),
            _ => Err("Unexpected output".into()),
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

/// Helpers shared by radarr and sonarr, which have near identical v3 APIs.
pub(in crate::media_bundle) mod arr_util {
    use super::{ArrImage, BundleResponse, IBundleClient};
    use serde::{de::DeserializeOwned, Deserialize, Serialize};
    use std::error::Error;

    /// Where newly added media goes and in what quality.
    pub struct AddTarget {
        pub quality_profile_id: u64,
        pub root_folder_path: String,
    }

    #[derive(Deserialize)]
    struct QualityProfile {
        id: u64,
    }

    #[derive(Deserialize)]
    struct RootFolder {
        path: String,
    }

    pub fn is_zero(id: &u64) -> bool {
        *id == 0
    }

    pub fn poster_url(images: &[ArrImage]) -> Option<String> {
        images
            .iter()
            .find(|image| image.cover_type == "poster")
            .and_then(|image| image.remote_url.clone())
    }

    /// Builds `path?key=value` with the value percent encoded.
    pub fn with_query(
        path: &str,
        key: &str,
        value: &str,
    ) -> Result<String, Box<dyn Error + Send + Sync>> {
        let mut url = reqwest::Url::parse("http://localhost")?.join(path)?;
        url.query_pairs_mut().append_pair(key, value);
        Ok(format!(
            "{}?{}",
            url.path(),
            url.query().unwrap_or_default()
        ))
    }

    fn parse_response<T: DeserializeOwned>(
        url: &str,
        status: u16,
        body: Vec<u8>,
    ) -> Result<T, Box<dyn Error + Send + Sync>> {
        if !(200..300).contains(&status) {
            return Err(format!(
                "{url} responded with {status}: {}",
                String::from_utf8_lossy(&body)
            )
            .into());
        }
        Ok(serde_json::from_slice(&body)?)
    }

    // The response type is not known to be Send, so it is scoped to a block that ends before the
    // body is awaited. Otherwise the futures here could not be held by the Send futures of
    // ServerEntity::make_call.
    pub async fn get_json<C: IBundleClient, T: DeserializeOwned>(
        client: &C,
        url: &str,
    ) -> Result<T, Box<dyn Error + Send + Sync>> {
        let (status, body) = {
            let resp = client.get(url).await?;
            (resp.get_statuscode(), resp.as_bytes())
        };
        parse_response(url, status, body.await?)
    }

    pub async fn post_json<C: IBundleClient, T: DeserializeOwned>(
        client: &C,
        url: &str,
        body: &impl Serialize,
    ) -> Result<T, Box<dyn Error + Send + Sync>> {
        let body = serde_json::to_string(body)?;
        let (status, body) = {
            let resp = client.post(url, body).await?;
            (resp.get_statuscode(), resp.as_bytes())
        };
        parse_response(url, status, body.await?)
    }

    /// Uses the first quality profile and the first root folder configured in radarr / sonarr.
    pub async fn default_add_target<C: IBundleClient>(
        client: &C,
    ) -> Result<AddTarget, Box<dyn Error + Send + Sync>> {
        let profiles: Vec<QualityProfile> = get_json(client, "/api/v3/qualityprofile").await?;
        let folders: Vec<RootFolder> = get_json(client, "/api/v3/rootfolder").await?;

        Ok(AddTarget {
            quality_profile_id: profiles.first().ok_or("No quality profile configured")?.id,
            root_folder_path: folders
                .into_iter()
                .next()
                .ok_or("No root folder configured")?
                .path,
        })
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
        #[serial_test::serial(socket_lock_lifetime)]
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

#[cfg(test)]
mod arr_tests {
    use super::*;
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    struct MockResponse(Vec<u8>);

    #[async_trait]
    impl BundleResponse for MockResponse {
        async fn as_bytes(self) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>> {
            Ok(self.0)
        }

        fn get_statuscode(&self) -> u16 {
            200
        }
    }

    /// Serves canned GET responses and echoes POST bodies back, recording them.
    #[derive(Clone, Default)]
    struct MockClient {
        responses: Arc<HashMap<String, Value>>,
        posted: Arc<Mutex<Vec<(String, Value)>>>,
    }

    #[async_trait]
    impl IBundleClient for MockClient {
        fn from_port(_port: u16) -> Self {
            MockClient::default()
        }

        fn set_port(&mut self, _port: u16) {}

        fn set_token(&mut self, _token: (impl AsRef<str>, impl AsRef<str>)) {}

        async fn get(&self, url: &str) -> Result<MockResponse, Box<dyn Error + Send + Sync>> {
            let body = self
                .responses
                .get(url)
                .unwrap_or_else(|| panic!("Unexpected url: {url}"));
            Ok(MockResponse(serde_json::to_vec(body)?))
        }

        async fn post(
            &self,
            url: &str,
            body: impl Into<reqwest::Body> + Send + Sync,
        ) -> Result<MockResponse, Box<dyn Error + Send + Sync>> {
            let body = body.into().as_bytes().unwrap().to_vec();
            self.posted
                .lock()
                .unwrap()
                .push((url.to_string(), serde_json::from_slice(&body)?));
            Ok(MockResponse(body))
        }
    }

    fn bundle_with(responses: HashMap<String, Value>) -> (MediaBundle<MockClient>, MockClient) {
        let client = MockClient {
            responses: Arc::new(responses),
            ..Default::default()
        };
        let bundle = MediaBundle::from_client_with_config(client.clone(), &Config::default());
        (bundle, client)
    }

    fn add_target_responses() -> HashMap<String, Value> {
        HashMap::from([
            (
                "/api/v3/qualityprofile".to_string(),
                json!([{ "id": 4, "name": "HD-1080p" }, { "id": 1, "name": "Any" }]),
            ),
            (
                "/api/v3/rootfolder".to_string(),
                json!([{ "id": 2, "path": "/media/movies" }, { "id": 1, "path": "/other" }]),
            ),
        ])
    }

    #[tokio::test]
    async fn add_movie_posts_the_looked_up_movie_with_add_settings() {
        let mut responses = add_target_responses();
        responses.insert(
            "/api/v3/movie/lookup?term=tmdb:604".to_string(),
            json!([
                { "title": "The Matrix", "tmdbId": 603, "year": 1999 },
                { "title": "The Matrix Reloaded", "tmdbId": 604, "year": 2003, "titleSlug": "the-matrix-reloaded-604" },
            ]),
        );
        let (bundle, client) = bundle_with(responses);

        let outcome = bundle.add_movie(604).await.unwrap();

        assert!(
            matches!(&outcome, AddOutcome::Added(movie) if movie.id == 604 && movie.year == 2003)
        );
        let posted = client.posted.lock().unwrap();
        assert_eq!(posted.len(), 1);
        let (url, body) = &posted[0];
        assert_eq!(url, "/api/v3/movie");
        assert_eq!(body["tmdbId"], 604);
        assert_eq!(body["titleSlug"], "the-matrix-reloaded-604");
        assert_eq!(body["qualityProfileId"], 4);
        assert_eq!(body["rootFolderPath"], "/media/movies");
        assert_eq!(body["monitored"], true);
        assert_eq!(body["addOptions"]["searchForMovie"], true);
        assert!(body.get("id").is_none());
    }

    #[tokio::test]
    async fn add_show_skips_shows_that_are_already_tracked() {
        let mut responses = add_target_responses();
        responses.insert(
            "/api/v3/series/lookup?term=tvdb:81189".to_string(),
            json!([{ "title": "Breaking Bad", "tvdbId": 81189, "year": 2008, "id": 7 }]),
        );
        let (bundle, client) = bundle_with(responses);

        let outcome = bundle.add_show(81189).await.unwrap();

        assert!(
            matches!(outcome, AddOutcome::AlreadyTracked(show) if show.id == 81189 && show.tracked)
        );
        assert!(client.posted.lock().unwrap().is_empty());
    }
}
