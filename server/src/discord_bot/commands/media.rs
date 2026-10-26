//! Commands to search for movies (radarr) and shows (sonarr) and to add them for tracking and
//! downloading. Searching shows the tmdb / tvdb id of each result, which is what adding takes, so
//! that what gets added is exactly the result that was looked at.

use serenity::builder::{
    CreateCommand, CreateCommandOption, CreateEmbed, CreateEmbedFooter, EditInteractionResponse,
};
use serenity::model::application::{CommandOptionType, ResolvedOption, ResolvedValue};

use crate::discord_bot::CommandError;
use crate::{AddOutcome, IBundleClient, MediaBundle, MediaLookup};

const MAX_RESULTS: usize = 5;
const MAX_OVERVIEW_CHARS: usize = 300;

#[derive(Debug, Clone, Copy)]
pub enum MediaKind {
    Movie,
    Show,
}

impl MediaKind {
    fn noun(self) -> &'static str {
        match self {
            MediaKind::Movie => "movie",
            MediaKind::Show => "show",
        }
    }

    fn app(self) -> &'static str {
        match self {
            MediaKind::Movie => "Radarr",
            MediaKind::Show => "Sonarr",
        }
    }

    fn id_option(self) -> &'static str {
        match self {
            MediaKind::Movie => "tmdb_id",
            MediaKind::Show => "tvdb_id",
        }
    }

    fn page_url(self, id: u64) -> String {
        match self {
            MediaKind::Movie => format!("https://www.themoviedb.org/movie/{id}"),
            MediaKind::Show => format!("https://www.thetvdb.com/?tab=series&id={id}"),
        }
    }
}

#[derive(Debug)]
pub enum MediaCommandError {
    MissingOption(&'static str),
    RequestFailed(String),
}

impl std::fmt::Display for MediaCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MediaCommandError::MissingOption(name) => write!(f, "Missing option `{name}`"),
            MediaCommandError::RequestFailed(why) => write!(f, "Request failed: {why}"),
        }
    }
}

impl From<MediaCommandError> for CommandError {
    fn from(value: MediaCommandError) -> Self {
        CommandError::MediaCommandError(value)
    }
}

pub async fn search<C: IBundleClient>(
    kind: MediaKind,
    options: &[ResolvedOption<'_>],
    media_bundle: &MediaBundle<C>,
) -> Result<EditInteractionResponse, MediaCommandError> {
    let query = options
        .iter()
        .find_map(|option| match option {
            ResolvedOption {
                name: "query",
                value: ResolvedValue::String(query),
                ..
            } => Some(*query),
            _ => None,
        })
        .ok_or(MediaCommandError::MissingOption("query"))?;

    let results = match kind {
        MediaKind::Movie => media_bundle.search_movies(query).await,
        MediaKind::Show => media_bundle.search_shows(query).await,
    }
    .map_err(|why| MediaCommandError::RequestFailed(why.to_string()))?;

    if results.is_empty() {
        return Ok(EditInteractionResponse::new()
            .content(format!("No {}s found for \"{query}\"", kind.noun())));
    }

    let embeds = results
        .iter()
        .take(MAX_RESULTS)
        .map(|lookup| {
            embed(kind, lookup)
                .field(kind.id_option(), lookup.id.to_string(), true)
                .field(
                    "Status",
                    if lookup.tracked {
                        "Already tracked"
                    } else {
                        "Not tracked"
                    },
                    true,
                )
                .footer(CreateEmbedFooter::new(format!(
                    "/add_{} {}:{}",
                    kind.noun(),
                    kind.id_option(),
                    lookup.id
                )))
        })
        .collect();

    Ok(EditInteractionResponse::new()
        .content(format!(
            "Top {} {} results for \"{query}\". Add one with `/add_{}` and its `{}`.",
            results.len().min(MAX_RESULTS),
            kind.noun(),
            kind.noun(),
            kind.id_option(),
        ))
        .embeds(embeds))
}

pub async fn add<C: IBundleClient>(
    kind: MediaKind,
    options: &[ResolvedOption<'_>],
    media_bundle: &MediaBundle<C>,
) -> Result<EditInteractionResponse, MediaCommandError> {
    let id = options
        .iter()
        .find_map(|option| match option.value {
            ResolvedValue::Integer(id) if option.name == kind.id_option() => u64::try_from(id).ok(),
            _ => None,
        })
        .ok_or(MediaCommandError::MissingOption(kind.id_option()))?;

    let outcome = match kind {
        MediaKind::Movie => media_bundle.add_movie(id).await,
        MediaKind::Show => media_bundle.add_show(id).await,
    }
    .map_err(|why| MediaCommandError::RequestFailed(why.to_string()))?;

    let (content, lookup) = match &outcome {
        AddOutcome::Added(lookup) => (
            format!(
                "Added **{}** to {} and started searching for it.",
                display_title(lookup),
                kind.app()
            ),
            lookup,
        ),
        AddOutcome::AlreadyTracked(lookup) => (
            format!(
                "**{}** is already tracked by {}.",
                display_title(lookup),
                kind.app()
            ),
            lookup,
        ),
    };

    Ok(EditInteractionResponse::new()
        .content(content)
        .embed(embed(kind, lookup)))
}

fn display_title(lookup: &MediaLookup) -> String {
    if lookup.year == 0 {
        lookup.title.clone()
    } else {
        format!("{} ({})", lookup.title, lookup.year)
    }
}

fn embed(kind: MediaKind, lookup: &MediaLookup) -> CreateEmbed {
    let mut embed = CreateEmbed::new()
        .title(display_title(lookup))
        .url(kind.page_url(lookup.id));

    if let Some(overview) = &lookup.overview {
        let mut description = overview
            .chars()
            .take(MAX_OVERVIEW_CHARS)
            .collect::<String>();
        if overview.chars().count() > MAX_OVERVIEW_CHARS {
            description.push('…');
        }
        embed = embed.description(description);
    }
    if let Some(poster_url) = &lookup.poster_url {
        embed = embed.thumbnail(poster_url);
    }

    embed
}

pub fn register() -> Vec<CreateCommand> {
    [MediaKind::Movie, MediaKind::Show]
        .into_iter()
        .flat_map(|kind| {
            let noun = kind.noun();
            [
                CreateCommand::new(format!("search_{noun}"))
                    .description(format!(
                        "Search for a {noun} and show the top {MAX_RESULTS} results"
                    ))
                    .add_option(
                        CreateCommandOption::new(
                            CommandOptionType::String,
                            "query",
                            format!("The {noun} title to search for"),
                        )
                        .required(true),
                    ),
                CreateCommand::new(format!("add_{noun}"))
                    .description(format!(
                        "Track and download a {noun} using the id from /search_{noun}"
                    ))
                    .add_option(
                        CreateCommandOption::new(
                            CommandOptionType::Integer,
                            kind.id_option(),
                            format!(
                                "The {} of the {noun}, as shown by /search_{noun}",
                                kind.id_option()
                            ),
                        )
                        .min_int_value(1)
                        .required(true),
                    ),
            ]
        })
        .collect()
}
