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

pub use qbit::TorrentInfo;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::error::Error;
use std::marker::Unpin;
use std::path::{Path, PathBuf};

use crate::Config;

/// Test related. Not sure if there is a better way to do this but this mainly helps with mocking
/// so that tests can be written more easily.
#[async_trait]
pub trait BundleResponse {
    async fn as_bytes(self) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>>;
    fn get_statuscode(&self) -> u16;
    fn get_cookie(&self) -> Option<String> {
        None
    }
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

    fn get_cookie(&self) -> Option<String> {
        self.headers()
            .get(reqwest::header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::to_owned)
    }
}

#[async_trait]
pub trait IBundleClient: Unpin + Send + Sync + Clone + 'static {
    fn from_port(port: u16) -> Self;
    fn set_port(&mut self, port: u16);
    fn set_token(&mut self, token: (impl AsRef<str>, impl AsRef<str>));
    fn set_cookie(&mut self, _cookie: impl AsRef<str>) {}
    async fn get(&self, url: &str) -> Result<impl BundleResponse, Box<dyn Error + Send + Sync>>;
    async fn post(
        &self,
        url: &str,
        body: impl Into<reqwest::Body> + Send + Sync,
    ) -> Result<impl BundleResponse, Box<dyn Error + Send + Sync>>;

    async fn delete(&self, url: &str) -> Result<impl BundleResponse, Box<dyn Error + Send + Sync>>;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Hd1080p,
    UltraHd,
}

impl Quality {
    pub fn profile_name(self) -> &'static str {
        match self {
            Quality::Hd1080p => "HD-1080p",
            Quality::UltraHd => "Ultra-HD",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AddOutcome {
    Added(MediaLookup),
    AlreadyTracked(MediaLookup),
}

#[derive(Debug, Clone, PartialEq)]
pub enum RemoveOutcome {
    Removed(MediaLookup),
    NotTracked(MediaLookup),
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
    qbit_client: Qbit<C>,
    move_map: Vec<(PathBuf, PathBuf)>,
}

unsafe impl<C> Send for MediaBundle<C> where C: IBundleClient {}
unsafe impl<C> Sync for MediaBundle<C> where C: IBundleClient {}

impl<C> MediaBundle<C>
where
    C: IBundleClient,
{
    const DELETION_CONFIRMATIONS: u8 = 2;

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
            qbit_client: {
                let mut client = client.clone();
                client.set_port(config.qbit_torrent_port);
                Qbit::new(
                    client,
                    config.qbit_torrent_username.to_string(),
                    config.qbit_torrent_password.to_string(),
                )
            },
            move_map: config
                .move_map
                .iter()
                .map(|(src, dst)| (PathBuf::from(src.as_ref()), PathBuf::from(dst.as_ref())))
                .collect(),
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
        quality: Quality,
    ) -> Result<AddOutcome, Box<dyn Error + Send + Sync>> {
        match self
            .radarr_client
            .make_call(RadarrInput::Add(tmdb_id, quality))
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
    pub async fn add_show(
        &self,
        tvdb_id: u64,
        quality: Quality,
    ) -> Result<AddOutcome, Box<dyn Error + Send + Sync>> {
        match self
            .sonarr_client
            .make_call(SonarrInput::Add(tvdb_id, quality))
            .await?
        {
            SonarrOutput::Added(outcome) => Ok(outcome),
            _ => Err("Unexpected output".into()),
        }
    }

    pub async fn remove_movie(
        &self,
        tmdb_id: u64,
    ) -> Result<RemoveOutcome, Box<dyn Error + Send + Sync>> {
        match self
            .radarr_client
            .make_call(RadarrInput::Remove(tmdb_id))
            .await?
        {
            RadarrOutput::Removed(lookup) => {
                self.remove_media_files(lookup.path.as_deref()).await?;
                Ok(RemoveOutcome::Removed(lookup.into()))
            }
            RadarrOutput::NotTracked(lookup) => Ok(RemoveOutcome::NotTracked(lookup.into())),
            _ => Err("Unexpected output".into()),
        }
    }

    pub async fn remove_show(
        &self,
        tvdb_id: u64,
    ) -> Result<RemoveOutcome, Box<dyn Error + Send + Sync>> {
        match self
            .sonarr_client
            .make_call(SonarrInput::Remove(tvdb_id))
            .await?
        {
            SonarrOutput::Removed(lookup) => {
                self.remove_media_files(lookup.path.as_deref()).await?;
                Ok(RemoveOutcome::Removed(lookup.into()))
            }
            SonarrOutput::NotTracked(lookup) => Ok(RemoveOutcome::NotTracked(lookup.into())),
            _ => Err("Unexpected output".into()),
        }
    }

    async fn remove_media_files(
        &self,
        indexed_path: Option<&str>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let Some(indexed_path) = indexed_path.filter(|path| !path.is_empty()) else {
            return Ok(());
        };
        let indexed_path = Path::new(indexed_path);
        let mut paths = vec![indexed_path.to_path_buf()];

        for (local_root, das_root) in &self.move_map {
            if let Ok(relative) = indexed_path.strip_prefix(local_root) {
                if !relative.as_os_str().is_empty() {
                    paths.push(das_root.join(relative));
                }
            } else if let Ok(relative) = indexed_path.strip_prefix(das_root) {
                if !relative.as_os_str().is_empty() {
                    paths.push(local_root.join(relative));
                }
            }
        }

        for path in paths {
            remove_path(&path).await?;
        }
        Ok(())
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

    pub async fn list_active_torrents(
        &self,
    ) -> Result<Vec<TorrentInfo>, Box<dyn Error + Send + Sync>> {
        match self.qbit_client.make_call(QbitInput::ListActive).await? {
            QbitOutput::Torrents(torrents) => Ok(torrents),
            QbitOutput::Deleted => Err("Unexpected qBittorrent response".into()),
        }
    }

    /// Removes the torrent and its downloaded files from disk.
    pub async fn delete_torrent(
        &self,
        hash: &str,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        match self
            .qbit_client
            .make_call(QbitInput::Delete(hash.to_string()))
            .await?
        {
            QbitOutput::Deleted => Ok(()),
            QbitOutput::Torrents(_) => Err("Unexpected qBittorrent response".into()),
        }
    }

    /// Reconciles Plex against the indexers. Plex has no deletion webhook, so a missing item must
    /// be observed twice before the existing indexer and filesystem removal routine is invoked.
    pub async fn run_deletion_poller(self) -> ! {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        let mut missing_movies = std::collections::HashMap::new();
        let mut missing_shows = std::collections::HashMap::new();

        loop {
            interval.tick().await;
            if let Err(error) = self
                .reconcile_deleted_media(&mut missing_movies, &mut missing_shows)
                .await
            {
                tracing::error!("Plex deletion reconciliation failed: {error}");
            }
        }
    }

    async fn reconcile_deleted_media(
        &self,
        missing_movies: &mut std::collections::HashMap<u64, u8>,
        missing_shows: &mut std::collections::HashMap<u64, u8>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        match self.plex_client.make_call(PlexInput::CheckAvailable).await? {
            PlexOutput::StatusCode(status) if (200..300).contains(&status) => {}
            PlexOutput::StatusCode(status) => {
                return Err(format!("Plex availability check returned HTTP {status}").into())
            }
            _ => return Err("Unexpected Plex availability response".into()),
        }

        let plex_movies = match self.plex_client.make_call(PlexInput::GetAllMovies).await? {
            PlexOutput::MovieList(items) => items
                .into_iter()
                .flat_map(|item| {
                    item.external_ids
                        .into_iter()
                        .filter_map(|id| id.id.strip_prefix("tmdb://").and_then(|id| id.parse().ok()))
                        .collect::<Vec<u64>>()
                })
                .collect::<HashSet<_>>(),
            _ => return Err("Unexpected Plex movie response".into()),
        };
        let radarr_movies = match self.radarr_client.make_call(RadarrInput::List).await? {
            RadarrOutput::Movies(items) => items,
            _ => return Err("Unexpected Radarr movie response".into()),
        };

        let mut missing_movie_ids = HashSet::new();
        for movie in &radarr_movies {
            if !plex_movies.contains(&movie.tmdb_id())
                && self.has_media_files(movie.path.as_deref()).await
            {
                missing_movie_ids.insert(movie.tmdb_id());
            }
        }

        let plex_shows = match self.plex_client.make_call(PlexInput::GetAllShows).await? {
            PlexOutput::ShowList(items) => items
                .into_iter()
                .flat_map(|item| {
                    item.external_ids
                        .into_iter()
                        .filter_map(|id| id.id.strip_prefix("tvdb://").and_then(|id| id.parse().ok()))
                        .collect::<Vec<u64>>()
                })
                .collect::<HashSet<_>>(),
            _ => return Err("Unexpected Plex show response".into()),
        };
        let sonarr_shows = match self.sonarr_client.make_call(SonarrInput::List).await? {
            SonarrOutput::Series(items) => items,
            _ => return Err("Unexpected Sonarr show response".into()),
        };

        let mut missing_show_ids = HashSet::new();
        for show in &sonarr_shows {
            if !plex_shows.contains(&show.tvdb_id())
                && self.has_media_files(show.path.as_deref()).await
            {
                missing_show_ids.insert(show.tvdb_id());
            }
        }

        // Only update confirmation state after every source has been read successfully. A
        // transient failure must not turn a partial snapshot into a deletion decision.
        self.reconcile_missing_movies(&missing_movie_ids, missing_movies).await;
        missing_movies.retain(|id, _| missing_movie_ids.contains(id));
        self.reconcile_missing_shows(&missing_show_ids, missing_shows).await;
        missing_shows.retain(|id, _| missing_show_ids.contains(id));
        Ok(())
    }

    async fn has_media_files(&self, indexed_path: Option<&str>) -> bool {
        let Some(indexed_path) = indexed_path.filter(|path| !path.is_empty()) else {
            return false;
        };
        let indexed_path = Path::new(indexed_path);
        let mut paths = vec![indexed_path.to_path_buf()];

        for (local_root, das_root) in &self.move_map {
            if let Ok(relative) = indexed_path.strip_prefix(local_root) {
                if !relative.as_os_str().is_empty() {
                    paths.push(das_root.join(relative));
                }
            } else if let Ok(relative) = indexed_path.strip_prefix(das_root) {
                if !relative.as_os_str().is_empty() {
                    paths.push(local_root.join(relative));
                }
            }
        }

        for path in paths {
            if contains_media_file(&path).await {
                return true;
            }
        }
        false
    }

    async fn reconcile_missing_movies(
        &self,
        missing_ids: &HashSet<u64>,
        confirmations: &mut std::collections::HashMap<u64, u8>,
    ) {
        for id in missing_ids {
            let count = confirmations.entry(*id).or_default();
            *count = count.saturating_add(1);
            if *count >= Self::DELETION_CONFIRMATIONS {
                match self.remove_movie(*id).await {
                    Ok(_) => {
                        tracing::info!("Removed Plex-deleted movie with TMDB id {id}");
                        confirmations.remove(id);
                    }
                    Err(error) => tracing::error!(
                        "Failed to remove Plex-deleted movie with TMDB id {id}: {error}"
                    ),
                }
            }
        }
    }

    async fn reconcile_missing_shows(
        &self,
        missing_ids: &HashSet<u64>,
        confirmations: &mut std::collections::HashMap<u64, u8>,
    ) {
        for id in missing_ids {
            let count = confirmations.entry(*id).or_default();
            *count = count.saturating_add(1);
            if *count >= Self::DELETION_CONFIRMATIONS {
                match self.remove_show(*id).await {
                    Ok(_) => {
                        tracing::info!("Removed Plex-deleted series with TVDB id {id}");
                        confirmations.remove(id);
                    }
                    Err(error) => tracing::error!(
                        "Failed to remove Plex-deleted series with TVDB id {id}: {error}"
                    ),
                }
            }
        }
    }
}

/// Helpers shared by radarr and sonarr, which have near identical v3 APIs.
pub(in crate::media_bundle) mod arr_util {
    use super::{ArrImage, BundleResponse, IBundleClient, Quality};
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
        name: String,
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

    pub async fn delete<C: IBundleClient>(
        client: &C,
        url: &str,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let response = client.delete(url).await?;
        if !(200..300).contains(&response.get_statuscode()) {
            return Err(format!("{url} responded with {}", response.get_statuscode()).into());
        }
        Ok(())
    }

    /// Uses the named quality profile and the first root folder configured in radarr / sonarr.
    pub async fn add_target<C: IBundleClient>(
        client: &C,
        quality: Quality,
    ) -> Result<AddTarget, Box<dyn Error + Send + Sync>> {
        let profiles: Vec<QualityProfile> = get_json(client, "/api/v3/qualityprofile").await?;
        let folders: Vec<RootFolder> = get_json(client, "/api/v3/rootfolder").await?;

        Ok(AddTarget {
            quality_profile_id: profiles
                .into_iter()
                .find(|profile| profile.name == quality.profile_name())
                .ok_or_else(|| format!("No {} quality profile configured", quality.profile_name()))?
                .id,
            root_folder_path: folders
                .into_iter()
                .next()
                .ok_or("No root folder configured")?
                .path,
        })
    }
}

async fn remove_path(path: &Path) -> Result<(), Box<dyn Error + Send + Sync>> {
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        tokio::fs::remove_dir_all(path).await?;
    } else {
        tokio::fs::remove_file(path).await?;
    }
    Ok(())
}

async fn contains_media_file(path: &Path) -> bool {
    let mut paths = vec![path.to_path_buf()];
    while let Some(path) = paths.pop() {
        let Ok(metadata) = tokio::fs::metadata(&path).await else {
            continue;
        };
        if metadata.is_file() {
            return true;
        }
        if !metadata.is_dir() {
            continue;
        }

        let Ok(mut entries) = tokio::fs::read_dir(path).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            paths.push(entry.path());
        }
    }
    false
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

        async fn delete(&self, _url: &str) -> Result<MockResponse, Box<dyn Error + Send + Sync>> {
            Ok(MockResponse(Vec::new()))
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
                json!([
                    { "id": 9, "name": "Ultra-HD" },
                    { "id": 4, "name": "HD-1080p" },
                    { "id": 1, "name": "Any" }
                ]),
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

        let outcome = bundle.add_movie(604, Quality::Hd1080p).await.unwrap();

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

        let outcome = bundle.add_show(81189, Quality::UltraHd).await.unwrap();

        assert!(
            matches!(outcome, AddOutcome::AlreadyTracked(show) if show.id == 81189 && show.tracked)
        );
        assert!(client.posted.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn remove_movie_deletes_indexer_entry_das_copy_and_local_symlink() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let local_root = temporary.path().join("local");
        let das_root = temporary.path().join("das");
        let movie_path = das_root.join("The Matrix");
        std::fs::create_dir_all(&local_root).unwrap();
        std::fs::create_dir_all(&movie_path).unwrap();
        std::fs::write(movie_path.join("movie.mkv"), b"movie").unwrap();
        symlink(&movie_path, local_root.join("The Matrix")).unwrap();

        let responses = HashMap::from([
            (
                "/api/v3/movie/lookup?term=tmdb:603".to_string(),
                json!([{ "title": "The Matrix", "tmdbId": 603, "id": 12 }]),
            ),
            (
                "/api/v3/movie/12".to_string(),
                json!({ "title": "The Matrix", "tmdbId": 603, "id": 12, "path": movie_path }),
            ),
        ]);
        let mut config = Config::default();
        config.move_map.insert(
            local_root.to_string_lossy().into_owned().into(),
            das_root.to_string_lossy().into_owned().into(),
        );
        let client = MockClient {
            responses: Arc::new(responses),
            ..Default::default()
        };
        let bundle = MediaBundle::from_client_with_config(client, &config);

        let outcome = bundle.remove_movie(603).await.unwrap();

        assert!(matches!(outcome, RemoveOutcome::Removed(movie) if movie.id == 603));
        assert!(!movie_path.exists());
        assert!(!local_root.join("The Matrix").exists());
        assert!(std::fs::symlink_metadata(local_root.join("The Matrix")).is_err());
    }

    #[tokio::test]
    async fn remove_movie_deletes_das_copy_when_indexer_reports_local_symlink() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let local_root = temporary.path().join("local");
        let das_root = temporary.path().join("das");
        let movie_path = das_root.join("The Matrix");
        let local_path = local_root.join("The Matrix");
        std::fs::create_dir_all(&movie_path).unwrap();
        std::fs::write(movie_path.join("movie.mkv"), b"movie").unwrap();
        std::fs::create_dir_all(&local_root).unwrap();
        symlink(&movie_path, &local_path).unwrap();

        let responses = HashMap::from([
            (
                "/api/v3/movie/lookup?term=tmdb:603".to_string(),
                json!([{ "title": "The Matrix", "tmdbId": 603, "id": 12 }]),
            ),
            (
                "/api/v3/movie/12".to_string(),
                json!({ "title": "The Matrix", "tmdbId": 603, "id": 12, "path": local_path }),
            ),
        ]);
        let mut config = Config::default();
        config.move_map.insert(
            local_root.to_string_lossy().into_owned().into(),
            das_root.to_string_lossy().into_owned().into(),
        );
        let bundle = MediaBundle::from_client_with_config(
            MockClient {
                responses: Arc::new(responses),
                ..Default::default()
            },
            &config,
        );

        bundle.remove_movie(603).await.unwrap();

        assert!(std::fs::symlink_metadata(&local_path).is_err());
        assert!(!movie_path.exists());
    }

    #[tokio::test]
    async fn empty_indexer_path_is_not_considered_media() {
        let temporary = tempfile::tempdir().unwrap();
        assert!(!contains_media_file(temporary.path()).await);

        std::fs::write(temporary.path().join("movie.mkv"), b"movie").unwrap();
        assert!(contains_media_file(temporary.path()).await);
    }
}
