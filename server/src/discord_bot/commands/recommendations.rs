//! Weekly trend picks, preserving TMDB's ranking rather than re-sorting by popularity.
use std::{collections::HashSet, sync::Arc, time::Duration};

use chrono::{DateTime, Datelike, Days, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::America::Los_Angeles;
use serde::Deserialize;
use serenity::all::{
    ChannelId, ChannelType, CreateAllowedMentions, CreateCommand, CreateEmbed, CreateEmbedFooter,
    CreateMessage, EditInteractionResponse, GuildId, Http,
};
use tokio::{sync::Mutex, time::Instant};

const CACHE_TTL: Duration = Duration::from_secs(3600);
const MAX_RESULTS: usize = 5;

#[derive(Clone, Debug, serde::Serialize, Deserialize)]
pub struct WeeklySchedule {
    day: Weekday,
    time: NaiveTime,
}

impl Default for WeeklySchedule {
    fn default() -> Self {
        Self {
            day: Weekday::Sun,
            time: NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
        }
    }
}

impl WeeklySchedule {
    fn next_after(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        let today = now.with_timezone(&Los_Angeles).date_naive();
        // A nonexistent spring-forward time skips that week's slot. A repeated fall-back
        // time uses its first occurrence only. Always choose a strictly future slot.
        for offset in 0..=14 {
            let date = today.checked_add_days(Days::new(offset)).unwrap();
            if date.weekday() == self.day {
                if let Some(next) = Los_Angeles
                    .from_local_datetime(&date.and_time(self.time))
                    .earliest()
                {
                    if next > now {
                        return next.with_timezone(&Utc);
                    }
                }
            }
        }
        unreachable!("a weekly Pacific slot exists within two weeks")
    }
}

#[derive(Deserialize)]
struct TrendingPage {
    results: Vec<TrendingItem>,
}

#[derive(Deserialize)]
struct TrendingItem {
    id: u64,
    adult: bool,
    title: Option<String>,
    name: Option<String>,
    overview: Option<String>,
    release_date: Option<String>,
    first_air_date: Option<String>,
    poster_path: Option<String>,
}

#[derive(Deserialize)]
struct ExternalIds {
    tvdb_id: Option<u64>,
}

pub struct Recommendations {
    client: reqwest::Client,
    api_key: String,
    cache: Mutex<Option<(Instant, Vec<CreateEmbed>)>>,
}

impl Recommendations {
    pub fn new(api_key: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            cache: Mutex::new(None),
        }
    }

    pub async fn run(&self) -> Result<EditInteractionResponse, String> {
        self.run_at("https://api.themoviedb.org/3").await
    }

    async fn run_at(&self, base_url: &str) -> Result<EditInteractionResponse, String> {
        let (content, embeds) = self.content_at(base_url).await?;
        Ok(EditInteractionResponse::new()
            .content(content)
            .allowed_mentions(CreateAllowedMentions::new())
            .embeds(embeds))
    }

    async fn content_at(&self, base_url: &str) -> Result<(&'static str, Vec<CreateEmbed>), String> {
        if self.api_key.trim().is_empty() {
            return Err("Recommendations need a configured TMDB API key (tmdb_api_key).".into());
        }
        // Coalesce simultaneous requests as well as caching successes. Never reuse stale picks
        // after an error or cache a partial movie/TV response as a complete recommendation list.
        let mut cache = self.cache.lock().await;
        if cache
            .as_ref()
            .is_none_or(|(at, _)| at.elapsed() >= CACHE_TTL)
        {
            let (movies, shows) = tokio::try_join!(
                self.get::<TrendingPage>(base_url, "/trending/movie/week", "movie trending"),
                self.get::<TrendingPage>(base_url, "/trending/tv/week", "tv trending")
            )?;
            let mut embeds: Vec<_> = picks(movies, "movie")
                .into_iter()
                .map(|item| {
                    let id = format!("TMDB ID: {}", item.id);
                    embed(item, "movie", id)
                })
                .collect();
            // /add_show takes a TVDB id, so show that instead of the TMDB id it cannot use.
            for item in picks(shows, "tv") {
                let path = format!("/tv/{}/external_ids", item.id);
                let ids: ExternalIds = self.get(base_url, &path, "tv external ids").await?;
                let id = match ids.tvdb_id.filter(|id| *id != 0) {
                    Some(id) => format!("TVDB ID: {id}"),
                    None => "No TVDB ID yet".to_string(),
                };
                embeds.push(embed(item, "tv", id));
            }
            *cache = Some((Instant::now(), embeds));
        }
        let embeds = cache.as_ref().unwrap().1.clone();
        let content = if embeds.is_empty() {
            "TMDB returned no eligible weekly movie or show recommendations."
        } else {
            "Weekly trending picks — up to 5 movies and 5 shows, in TMDB trend order. Adult-flagged titles excluded. Not personalized; Plex availability is not checked.\nMovies: use /add_movie with the TMDB ID. Shows: use /add_show with the TVDB ID.\nThis product uses the TMDB API but is not endorsed or certified by TMDB."
        };
        Ok((content, embeds))
    }

    pub async fn post_weekly(
        self: Arc<Self>,
        http: Arc<Http>,
        guild: GuildId,
        channel: Option<ChannelId>,
        schedule: Option<WeeklySchedule>,
    ) {
        let (Some(channel), Some(schedule)) = (channel, schedule) else {
            std::future::pending::<()>().await;
            return;
        };
        loop {
            let next = schedule.next_after(Utc::now());
            // Recheck wall time periodically to tolerate system clock adjustments.
            while Utc::now() < next {
                let wait = (next - Utc::now()).to_std().unwrap_or_default();
                tokio::time::sleep(wait.min(Duration::from_secs(60))).await;
            }
            // Validate the configured destination without creating or modifying channels.
            match channel.to_channel(&http).await {
                Ok(serenity::all::Channel::Guild(destination))
                    if destination.guild_id == guild && destination.kind == ChannelType::Text => {}
                _ => {
                    tracing::error!("Weekly recommendations skipped: destination is not a text channel in the configured guild");
                    continue;
                }
            }
            match self.content_at("https://api.themoviedb.org/3").await {
                Ok((content, embeds)) => {
                    let message = CreateMessage::new()
                        .content(content)
                        .embeds(embeds)
                        .allowed_mentions(CreateAllowedMentions::new());
                    if channel.send_message(&http, message).await.is_err() {
                        // Do not retry an ambiguous send: it may already have reached Discord.
                        tracing::error!("Weekly recommendations could not be delivered");
                    }
                }
                Err(error) => tracing::error!("Weekly recommendations skipped: {error}"),
            }
        }
    }

    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        base_url: &str,
        path: &str,
        kind: &str,
    ) -> Result<T, String> {
        let response = self
            .client
            .get(format!("{base_url}{path}"))
            .query(&[("language", "en-US"), ("api_key", self.api_key.as_str())])
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|_| format!("TMDB {kind} request failed. Please try again later."))?;
        if !response.status().is_success() {
            // Do not surface provider bodies or request URLs (which contain credentials).
            return Err(format!(
                "TMDB {kind} returned HTTP {}. Check the API key or try again later.",
                response.status().as_u16()
            ));
        }
        let body = response
            .bytes()
            .await
            .map_err(|_| format!("TMDB {kind} response could not be read."))?;
        serde_json::from_slice(&body)
            .map_err(|_| format!("TMDB returned an invalid {kind} response."))
    }
}

fn picks(page: TrendingPage, kind: &str) -> Vec<TrendingItem> {
    let mut seen = HashSet::new();
    page.results
        .into_iter()
        .filter(|item| {
            let title = if kind == "movie" { &item.title } else { &item.name };
            !item.adult
                && item.id != 0
                && title.as_ref().is_some_and(|title| !title.trim().is_empty())
                && seen.insert(item.id)
        })
        .take(MAX_RESULTS)
        .collect()
}

/// `id` is the footer's id line: the id the matching add command takes.
fn embed(item: TrendingItem, kind: &str, id: String) -> CreateEmbed {
    let (label, title, date) = if kind == "movie" {
        ("Movie", item.title.unwrap_or_default(), item.release_date)
    } else {
        ("Show", item.name.unwrap_or_default(), item.first_air_date)
    };
    let year = date
        .as_deref()
        .and_then(|s| s.split('-').next())
        .filter(|s| s.len() == 4 && s.bytes().all(|b| b.is_ascii_digit()));
    // Keep all ten embeds below Discord's combined 6,000-character limit, including
    // supplementary Unicode characters counted as two UTF-16 units by Discord.
    let title: String = title.chars().take(80).collect();
    let title = match year {
        Some(year) => format!("{label}: {title} ({year})"),
        None => format!("{label}: {title}"),
    };
    let mut embed = CreateEmbed::new()
        .title(title)
        .url(format!("https://www.themoviedb.org/{kind}/{}", item.id))
        .description(
            item.overview
                .unwrap_or_default()
                .chars()
                .take(150)
                .collect::<String>(),
        )
        .footer(CreateEmbedFooter::new(format!("TMDB weekly trends · {id}")));
    if let Some(path) = item.poster_path.filter(|path| !path.trim().is_empty()) {
        embed = embed.thumbnail(format!("https://image.tmdb.org/t/p/w185{path}"));
    }
    embed
}

pub fn register() -> CreateCommand {
    CreateCommand::new("recommendations")
        .description("Show this week's trending movies and shows in general")
        .dm_permission(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::Request, http::StatusCode, routing::get, Json, Router};
    use serde_json::{json, Value};
    use std::future::IntoFuture;

    #[test]
    fn weekly_schedule_uses_pacific_calendar_without_startup_or_dst_duplicates() {
        let schedule = WeeklySchedule::default();
        for (now, expected) in [
            ("2026-03-01T17:59:59Z", "2026-03-01T18:00:00Z"),
            ("2026-03-01T18:00:00Z", "2026-03-08T17:00:00Z"),
            ("2026-03-01T18:00:01Z", "2026-03-08T17:00:00Z"),
            ("2026-10-25T17:00:00Z", "2026-11-01T18:00:00Z"),
        ] {
            assert_eq!(
                schedule.next_after(now.parse().unwrap()),
                expected.parse::<DateTime<Utc>>().unwrap()
            );
        }
        let sunday: WeeklySchedule = serde_yaml::from_str("day: Sun\ntime: '01:30:00'").unwrap();
        assert_eq!(
            sunday.next_after("2026-11-01T08:30:00Z".parse().unwrap()),
            "2026-11-08T09:30:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        let gap: WeeklySchedule = serde_yaml::from_str("day: Sun\ntime: '02:30:00'").unwrap();
        assert_eq!(
            gap.next_after("2026-03-08T09:00:00Z".parse().unwrap()),
            "2026-03-15T09:30:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert!(serde_yaml::from_str::<WeeklySchedule>("day: Fri\ntime: '25:00:00'").is_err());
    }

    #[tokio::test]
    async fn recommendations_use_weekly_feeds_filter_and_cache_without_mixing_ids() {
        async fn feed(request: Request) -> Json<Value> {
            assert!(!request.headers().contains_key("authorization"));
            assert_eq!(
                request.uri().query(),
                Some("language=en-US&api_key=test-api-key")
            );
            match request.uri().path() {
                "/tv/8/external_ids" => return Json(json!({"id": 8, "tvdb_id": 81189})),
                "/tv/9/external_ids" => return Json(json!({"id": 9, "tvdb_id": null})),
                _ => {}
            }
            let is_movie = request.uri().path() == "/trending/movie/week";
            let rows = if is_movie {
                json!([
                    {"id": 1, "adult": true, "title": "Adult"},
                    {"id": 8, "adult": false, "title": "Weekly first", "popularity": 1, "release_date": "2025-10-01", "poster_path": "/movie-cover.jpg"},
                    {"id": 8, "adult": false, "title": "Duplicate"},
                    {"id": 2, "adult": false, "title": "Weekly second", "popularity": 9999, "poster_path": null},
                    {"id": 0, "adult": false, "title": "Invalid"},
                    {"id": 3, "adult": false, "title": " "},
                    {"id": 4, "adult": false, "title": "Fourth", "poster_path": ""},
                    {"id": 5, "adult": false, "title": "Fifth"},
                    {"id": 6, "adult": false, "title": "Sixth"},
                    {"id": 7, "adult": false, "title": "Over limit"}
                ])
            } else {
                json!([
                    {"id": 8, "adult": false, "name": "A show", "first_air_date": "2024-03-02", "overview": "é".repeat(1000), "poster_path": "/show-cover.jpg"},
                    {"id": 9, "adult": false, "name": "Brand new show"}
                ])
            };
            Json(json!({"results": rows}))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new()
                    .route("/trending/movie/week", get(feed))
                    .route("/trending/tv/week", get(feed))
                    .route("/tv/8/external_ids", get(feed))
                    .route("/tv/9/external_ids", get(feed)),
            )
            .into_future(),
        );
        let recommendations = Recommendations::new("test-api-key".into());
        let first = serde_json::to_value(recommendations.run_at(&url).await.unwrap()).unwrap();
        server.abort();
        let cached =
            serde_json::to_value(recommendations.run_at("http://127.0.0.1:0").await.unwrap())
                .unwrap();
        assert_eq!(first, cached);
        let embeds = first["embeds"].as_array().unwrap();
        assert_eq!(embeds.len(), 7);
        // Movies keep the TMDB id /add_movie takes; shows carry the TVDB id /add_show takes.
        assert_eq!(embeds[0]["footer"]["text"], "TMDB weekly trends · TMDB ID: 8");
        assert_eq!(embeds[5]["footer"]["text"], "TMDB weekly trends · TVDB ID: 81189");
        assert_eq!(embeds[6]["footer"]["text"], "TMDB weekly trends · No TVDB ID yet");
        assert_eq!(embeds[0]["title"], "Movie: Weekly first (2025)");
        assert_eq!(embeds[1]["title"], "Movie: Weekly second");
        assert_eq!(embeds[4]["url"], "https://www.themoviedb.org/movie/6");
        assert_eq!(embeds[5]["title"], "Show: A show (2024)");
        assert_eq!(embeds[5]["url"], "https://www.themoviedb.org/tv/8");
        assert_eq!(
            embeds[0]["thumbnail"]["url"],
            "https://image.tmdb.org/t/p/w185/movie-cover.jpg"
        );
        assert_eq!(
            embeds[5]["thumbnail"]["url"],
            "https://image.tmdb.org/t/p/w185/show-cover.jpg"
        );
        for index in [1, 2, 3] {
            assert!(
                embeds[index].get("thumbnail").is_none(),
                "missing, null, or empty poster paths should omit the image"
            );
        }
        assert_eq!(
            embeds[5]["description"].as_str().unwrap().chars().count(),
            150
        );
        assert!(first["content"].as_str().unwrap().contains("/add_show with the TVDB ID"));
        assert_eq!(first["allowed_mentions"]["parse"], json!([]));

        // Expired results must fail rather than silently claiming stale data is current.
        recommendations.cache.lock().await.as_mut().unwrap().0 = Instant::now() - CACHE_TTL;
        assert!(recommendations.run_at("http://127.0.0.1:0").await.is_err());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(
            axum::serve(
                listener,
                Router::new().fallback(get(|| async {
                    (StatusCode::UNAUTHORIZED, "secret provider body")
                })),
            )
            .into_future(),
        );
        let error = recommendations.run_at(&url).await.unwrap_err();
        assert!(error.contains("401"));
        assert!(!error.contains("secret"));
        assert!(!error.contains("test-api-key"));
        server.abort();
        assert!(Recommendations::new(String::new())
            .run()
            .await
            .unwrap_err()
            .contains("tmdb_api_key"));
    }

    #[tokio::test]
    async fn recommendations_handle_empty_invalid_and_partial_responses_and_discord_limits() {
        for (movie, tv, expected_error) in [
            ("{\"results\": []}", "{\"results\": []}", None),
            ("not json", "{\"results\": []}", Some("invalid movie")),
            ("{\"results\": []}", "{}", Some("invalid tv")),
            // A show whose TVDB id cannot be looked up fails the refresh too.
            (
                "{\"results\": []}",
                "{\"results\": [{\"id\": 1, \"adult\": false, \"name\": \"Show\"}]}",
                Some("tv external ids returned HTTP 404"),
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(
                axum::serve(
                    listener,
                    Router::new()
                        .route("/trending/movie/week", get(move || async move { movie }))
                        .route("/trending/tv/week", get(move || async move { tv })),
                )
                .into_future(),
            );
            let recommendations = Recommendations::new("test-api-key".into());
            let result = recommendations.run_at(&url).await;
            if let Some(expected) = expected_error {
                assert!(result.unwrap_err().contains(expected));
                // A failed refresh must not cache partial results.
                assert!(recommendations.run_at("http://127.0.0.1:0").await.is_err());
            } else {
                let response = serde_json::to_value(result.unwrap()).unwrap();
                assert!(response["content"]
                    .as_str()
                    .unwrap()
                    .contains("no eligible"));
                assert_eq!(response["embeds"], json!([]));
            }
            server.abort();
        }
        let rows: Vec<_> = (1..=5)
            .map(|id| {
                json!({
                    "id": id, "adult": false, "title": "😀".repeat(1000),
                    "name": "😀".repeat(1000), "overview": "😀".repeat(1000)
                })
            })
            .collect();
        let longest_id = format!("TVDB ID: {}", u64::MAX);
        let embeds: Vec<_> = ["movie", "tv"]
            .into_iter()
            .flat_map(|kind| {
                picks(serde_json::from_value(json!({"results": rows})).unwrap(), kind)
                    .into_iter()
                    .map(|item| embed(item, kind, longest_id.clone()))
            })
            .collect();
        assert_eq!(embeds.len(), 10);
        let embeds = serde_json::to_value(embeds).unwrap();
        let total: usize = embeds
            .as_array()
            .unwrap()
            .iter()
            .map(|embed| {
                let title = embed["title"].as_str().unwrap();
                assert!(title.encode_utf16().count() <= 256);
                title.encode_utf16().count()
                    + embed["description"]
                        .as_str()
                        .unwrap()
                        .encode_utf16()
                        .count()
                    + embed["footer"]["text"]
                        .as_str()
                        .unwrap()
                        .encode_utf16()
                        .count()
            })
            .sum();
        assert!(total <= 6000, "combined embed text: {total}");
    }
}
