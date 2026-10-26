use crate::media_bundle::{
    arr_util, AddOutcome, ArrImage, IBundleClient, MediaLookup, ServerEntity,
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
    #[serde(default)]
    images: Vec<ArrImage>,
    #[serde(flatten)]
    rest: Map<String, Value>,
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
    /// Adds (and monitors) the series with the given tvdb id and starts searching for it.
    Add(u64),
}

pub(in crate::media_bundle) enum SonarrOutput {
    Series(Vec<Series>),
    Added(AddOutcome),
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
            SonarrInput::Lookup(term) => {
                let url = arr_util::with_query("/api/v3/series/lookup", "term", &term)?;
                Ok(SonarrOutput::Series(
                    arr_util::get_json(&self.client, &url).await?,
                ))
            }
            SonarrInput::Add(tvdb_id) => {
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

                let target = arr_util::default_add_target(&self.client).await?;
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
        }
    }

    fn from_bundle_client(client: Self::Client) -> impl ServerEntity {
        Sonarr { client }
    }

    fn get_app_name(&self) -> &str {
        APP_NAME
    }
}
