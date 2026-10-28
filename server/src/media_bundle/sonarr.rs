use crate::media_bundle::{
    arr_util, AddOutcome, ArrImage, IBundleClient, MediaLookup, Quality, ServerEntity,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::error::Error;

const APP_NAME: &'static str = "Sonarr";

/// A series as returned by sonarr's lookup endpoint. Only the fields we read are typed; the rest
/// are kept so the whole object can be sent back to sonarr when adding it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::media_bundle) struct Series {
    title: String,
    #[serde(default)]
    year: u32,
    overview: Option<String>,
    tvdb_id: u64,
    /// Sonarr's own id. It is only non zero when the series is already in the library.
    #[serde(default, skip_serializing_if = "arr_util::is_zero")]
    id: u64,
    #[serde(default, skip_serializing)]
    statistics: Option<SeriesStatistics>,
    #[serde(default)]
    pub(in crate::media_bundle) path: Option<String>,
    #[serde(default)]
    images: Vec<ArrImage>,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

impl Series {
    pub(in crate::media_bundle) fn tvdb_id(&self) -> u64 {
        self.tvdb_id
    }

    pub(in crate::media_bundle) fn has_files(&self) -> bool {
        self.statistics
            .as_ref()
            .is_some_and(|stats| stats.episode_file_count > 0 || stats.size_on_disk > 0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SeriesStatistics {
    #[serde(default)]
    episode_file_count: u64,
    #[serde(default)]
    size_on_disk: u64,
}

impl From<Series> for MediaLookup {
    fn from(series: Series) -> Self {
        MediaLookup {
            id: series.tvdb_id,
            poster_url: arr_util::poster_url(&series.images),
            title: series.title,
            year: series.year,
            overview: series.overview,
            tracked: series.id != 0,
        }
    }
}

pub(in crate::media_bundle) enum SonarrInput {
    Lookup(String),
    List,
    /// Adds (and monitors) the series with the given tvdb id and starts searching for it.
    Add(u64, Quality),
    Remove(u64),
}

pub(in crate::media_bundle) enum SonarrOutput {
    Series(Vec<Series>),
    Added(AddOutcome),
    Removed(Series),
    NotTracked(Series),
}

#[derive(Default, Debug, Clone)]
pub(in crate::media_bundle) struct Sonarr<C>
where
    C: IBundleClient,
{
    client: C,
}

impl<C> Sonarr<C>
where
    C: IBundleClient,
{
    pub fn new(client: C) -> Self {
        Self { client }
    }
}

#[async_trait]
impl<C> ServerEntity for Sonarr<C>
where
    C: IBundleClient,
{
    type Input = SonarrInput;
    type Output = SonarrOutput;
    type Client = C;

    async fn make_call(
        &self,
        input: Self::Input,
    ) -> Result<Self::Output, Box<dyn Error + Send + Sync>> {
        match input {
            SonarrInput::List => Ok(SonarrOutput::Series(
                arr_util::get_json(&self.client, "/api/v3/series").await?,
            )),
            SonarrInput::Lookup(term) => {
                let url = arr_util::with_query("/api/v3/series/lookup", "term", &term)?;
                Ok(SonarrOutput::Series(
                    arr_util::get_json(&self.client, &url).await?,
                ))
            }
            SonarrInput::Add(tvdb_id, quality) => {
                let url = format!("/api/v3/series/lookup?term=tvdb:{tvdb_id}");
                let found: Vec<Series> = arr_util::get_json(&self.client, &url).await?;
                let mut series = found
                    .into_iter()
                    .find(|series| series.tvdb_id == tvdb_id)
                    .ok_or_else(|| format!("No show found with tvdb id {tvdb_id}"))?;
                if series.id != 0 {
                    return Ok(SonarrOutput::Added(AddOutcome::AlreadyTracked(
                        series.into(),
                    )));
                }

                let target = arr_util::add_target(&self.client, quality).await?;
                series.rest.extend([
                    (
                        "qualityProfileId".to_string(),
                        json!(target.quality_profile_id),
                    ),
                    ("rootFolderPath".to_string(), json!(target.root_folder_path)),
                    ("monitored".to_string(), json!(true)),
                    ("seasonFolder".to_string(), json!(true)),
                    (
                        "addOptions".to_string(),
                        json!({ "monitor": "all", "searchForMissingEpisodes": true }),
                    ),
                ]);
                let added: Series =
                    arr_util::post_json(&self.client, "/api/v3/series", &series).await?;

                Ok(SonarrOutput::Added(AddOutcome::Added(added.into())))
            }
            SonarrInput::Remove(tvdb_id) => {
                let url = format!("/api/v3/series/lookup?term=tvdb:{tvdb_id}");
                let lookup = arr_util::get_json::<C, Vec<Series>>(&self.client, &url)
                    .await?
                    .into_iter()
                    .find(|series| series.tvdb_id == tvdb_id)
                    .ok_or_else(|| format!("No show found with tvdb id {tvdb_id}"))?;
                if lookup.id == 0 {
                    return Ok(SonarrOutput::NotTracked(lookup));
                }
                let series: Series =
                    arr_util::get_json(&self.client, &format!("/api/v3/series/{}", lookup.id))
                        .await?;
                arr_util::delete(
                    &self.client,
                    &format!("/api/v3/series/{}?deleteFiles=false", series.id),
                )
                .await?;
                Ok(SonarrOutput::Removed(series))
            }
        }
    }

    fn from_bundle_client(client: Self::Client) -> impl ServerEntity {
        Sonarr { client }
    }

    fn get_app_name(&self) -> &str {
        APP_NAME
    }
}
