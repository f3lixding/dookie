use crate::media_bundle::{
    arr_util, AddOutcome, ArrImage, IBundleClient, MediaLookup, Quality, ServerEntity,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::error::Error;

const APP_NAME: &'static str = "Radarr";

/// A movie as returned by radarr's lookup endpoints. Only the fields we read are typed; the rest
/// are kept so the whole object can be sent back to radarr when adding it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::media_bundle) struct Movie {
    title: String,
    #[serde(default)]
    year: u32,
    overview: Option<String>,
    tmdb_id: u64,
    /// Radarr's own id. It is only non zero when the movie is already in the library.
    #[serde(default, skip_serializing_if = "arr_util::is_zero")]
    id: u64,
    #[serde(default)]
    pub(in crate::media_bundle) path: Option<String>,
    #[serde(default)]
    images: Vec<ArrImage>,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

impl From<Movie> for MediaLookup {
    fn from(movie: Movie) -> Self {
        MediaLookup {
            id: movie.tmdb_id,
            poster_url: arr_util::poster_url(&movie.images),
            title: movie.title,
            year: movie.year,
            overview: movie.overview,
            tracked: movie.id != 0,
        }
    }
}

pub(in crate::media_bundle) enum RadarrInput {
    Lookup(String),
    /// Adds (and monitors) the movie with the given tmdb id and starts searching for it.
    Add(u64, Quality),
    Remove(u64),
}

pub(in crate::media_bundle) enum RadarrOutput {
    Movies(Vec<Movie>),
    Added(AddOutcome),
    Removed(Movie),
    NotTracked(Movie),
}

#[derive(Default, Debug, Clone)]
pub(in crate::media_bundle) struct Radarr<C>
where
    C: IBundleClient,
{
    client: C,
}

impl<C> Radarr<C>
where
    C: IBundleClient,
{
    pub fn new(client: C) -> Self {
        Self { client }
    }
}

#[async_trait]
impl<C> ServerEntity for Radarr<C>
where
    C: IBundleClient,
{
    type Input = RadarrInput;
    type Output = RadarrOutput;
    type Client = C;

    async fn make_call(
        &self,
        input: Self::Input,
    ) -> Result<Self::Output, Box<dyn Error + Send + Sync>> {
        match input {
            RadarrInput::Lookup(term) => {
                let url = arr_util::with_query("/api/v3/movie/lookup", "term", &term)?;
                Ok(RadarrOutput::Movies(
                    arr_util::get_json(&self.client, &url).await?,
                ))
            }
            RadarrInput::Add(tmdb_id, quality) => {
                // Unlike /movie/lookup/tmdb, this marks movies that are already in the library
                let url = format!("/api/v3/movie/lookup?term=tmdb:{tmdb_id}");
                let found: Vec<Movie> = arr_util::get_json(&self.client, &url).await?;
                let mut movie = found
                    .into_iter()
                    .find(|movie| movie.tmdb_id == tmdb_id)
                    .ok_or_else(|| format!("No movie found with tmdb id {tmdb_id}"))?;
                if movie.id != 0 {
                    return Ok(RadarrOutput::Added(AddOutcome::AlreadyTracked(
                        movie.into(),
                    )));
                }

                let target = arr_util::add_target(&self.client, quality).await?;
                movie.rest.extend([
                    (
                        "qualityProfileId".to_string(),
                        json!(target.quality_profile_id),
                    ),
                    ("rootFolderPath".to_string(), json!(target.root_folder_path)),
                    ("monitored".to_string(), json!(true)),
                    ("minimumAvailability".to_string(), json!("released")),
                    ("addOptions".to_string(), json!({ "searchForMovie": true })),
                ]);
                let added: Movie =
                    arr_util::post_json(&self.client, "/api/v3/movie", &movie).await?;

                Ok(RadarrOutput::Added(AddOutcome::Added(added.into())))
            }
            RadarrInput::Remove(tmdb_id) => {
                let url = format!("/api/v3/movie/lookup?term=tmdb:{tmdb_id}");
                let lookup = arr_util::get_json::<C, Vec<Movie>>(&self.client, &url)
                    .await?
                    .into_iter()
                    .find(|movie| movie.tmdb_id == tmdb_id)
                    .ok_or_else(|| format!("No movie found with tmdb id {tmdb_id}"))?;
                if lookup.id == 0 {
                    return Ok(RadarrOutput::NotTracked(lookup));
                }
                let movie: Movie =
                    arr_util::get_json(&self.client, &format!("/api/v3/movie/{}", lookup.id))
                        .await?;
                arr_util::delete(
                    &self.client,
                    &format!("/api/v3/movie/{}?deleteFiles=false", movie.id),
                )
                .await?;
                Ok(RadarrOutput::Removed(movie))
            }
        }
    }

    fn from_bundle_client(client: Self::Client) -> impl ServerEntity {
        Radarr { client }
    }

    fn get_app_name(&self) -> &str {
        APP_NAME
    }
}
