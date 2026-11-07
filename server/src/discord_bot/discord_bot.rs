use super::commands::{
    self, media,
    media::MediaKind,
    qbit,
    recommendations::{Recommendations, WeeklySchedule},
    system,
};
use super::CommandError;
use crate::{IBundleClient, MediaBundle};
/// This is the discord bot that we will use for various purposes.
/// So far these include:
/// - Ping / health check
/// - Notifications for remote sessions (this will notification will only be sent to channel admin)
/// - Notifications for new comer on instructions to set everything up
/// - Notifications for leaving soon categories
/// - Accept commands to add media (and return approporiate responses)
use axum::{routing::post, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serenity::{
    all::{
        ChannelId, ChannelType, CreateChannel, CreateInteractionResponse,
        CreateInteractionResponseMessage, EditInteractionResponse, GuildId, Http, Interaction,
        PermissionOverwrite, PermissionOverwriteType, Permissions, RoleId,
    },
    model::{channel::Message, gateway::Ready},
    prelude::*,
};
use std::{
    collections::{HashMap, HashSet},
    error::Error,
    sync::{atomic::AtomicBool, Arc},
};
use tokio::task::JoinHandle;

static ADMIN_CHANNEL_NAME: &str = "admin-notifications";

fn default_recommendation_schedule() -> Option<WeeklySchedule> {
    Some(WeeklySchedule::default())
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GuildConfig {
    guild_id: GuildId,
    channels: Vec<ChannelConfig>,
    /// Set to the existing general text channel's ID; never create or guess a destination.
    #[serde(default)]
    recommendation_channel_id: Option<ChannelId>,
    #[serde(default = "default_recommendation_schedule")]
    recommendation_schedule: Option<WeeklySchedule>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ChannelConfig {
    name: String,
    kind: String,
    topic: String,
    nsfw: bool,
    position: u64,
    permissions: Vec<ChannelPermission>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ChannelPermission {
    role_id: u64,
    allow: Vec<String>,
    deny: Vec<String>,
}

impl GuildConfig {
    pub(crate) fn guild_id(&self) -> GuildId {
        self.guild_id
    }

    fn recommendation_channel_error(
        &self,
        guild: Option<GuildId>,
        channel: ChannelId,
    ) -> Option<String> {
        if guild != Some(self.guild_id) {
            return Some("Recommendations are only available in the configured server.".into());
        }
        match self.recommendation_channel_id {
            Some(id) if id == channel => None,
            Some(id) => Some(format!("Use /recommendations in <#{id}> so everyone can see the picks.")),
            None => Some("Recommendations need guild_config.recommendation_channel_id set to the general text channel's ID.".into()),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
struct Account {
    title: String,
}

#[derive(Debug, Deserialize, Clone)]
enum MediaType {
    #[serde(rename = "movie")]
    Movie,
    #[serde(rename = "show")]
    Show,
    #[serde(rename = "episode")]
    Episode,
}

impl std::fmt::Display for MediaType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MediaType::Movie => write!(f, "movie"),
            MediaType::Show => write!(f, "show"),
            MediaType::Episode => write!(f, "episode"),
        }
    }
}

#[derive(Debug, Deserialize)]
enum EventType {
    #[serde(rename = "media.resume")]
    Resume,
    #[serde(rename = "media.play")]
    Play,
    #[serde(rename = "media.stop")]
    Stop,
    #[serde(rename = "media.pause")]
    Pause,
    #[serde(rename = "library.new")]
    New,
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventType::Resume => write!(f, "resume"),
            EventType::Play => write!(f, "play"),
            EventType::Stop => write!(f, "stop"),
            EventType::Pause => write!(f, "pause"),
            EventType::New => write!(f, "new"),
        }
    }
}

#[derive(Debug)]
struct SessionInfo {
    media_type: MediaType,
    title: String,
}

#[derive(Debug)]
struct WebhookEnvelope {
    account: Account,
    event: EventType,
    session_info: SessionInfo,
}

impl<'de> Deserialize<'de> for WebhookEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;

        let account: Account = serde_json::from_value(
            value
                .get("Account")
                .ok_or_else(|| serde::de::Error::missing_field("account"))?
                .clone(),
        )
        .map_err(serde::de::Error::custom)?;

        let event: EventType = serde_json::from_value(
            value
                .get("event")
                .ok_or_else(|| serde::de::Error::missing_field("event"))?
                .clone(),
        )
        .map_err(serde::de::Error::custom)?;

        let metadata = value
            .get("Metadata")
            .ok_or_else(|| serde::de::Error::missing_field("Metadata"))?
            .clone();

        let media_type: MediaType = serde_json::from_value(
            metadata
                .get("type")
                .or_else(|| metadata.get("librarySectionType"))
                .ok_or_else(|| serde::de::Error::missing_field("type"))?
                .clone(),
        )
        .map_err(serde::de::Error::custom)?;

        let title = match media_type {
            MediaType::Movie | MediaType::Show => metadata.get("title"),
            MediaType::Episode => metadata
                .get("grandparentTitle")
                .or_else(|| metadata.get("grandparentSlug"))
                .or_else(|| metadata.get("title")),
        }
        .and_then(Value::as_str)
        .ok_or_else(|| serde::de::Error::missing_field("title"))?
        .to_owned();
        let session_info = SessionInfo { title, media_type };

        Ok(WebhookEnvelope {
            account,
            event,
            session_info,
        })
    }
}

pub struct DiscordHandler<C: IBundleClient> {
    permitted_guilds: HashSet<u64>,
    media_bundle: MediaBundle<C>,
    guild_config: GuildConfig,
    recommendations: Arc<Recommendations>,
    has_ready_been_called: AtomicBool,
}

impl<C: IBundleClient> DiscordHandler<C> {
    pub fn weekly_recommendations(
        &self,
        http: Arc<Http>,
    ) -> impl std::future::Future<Output = ()> + Send + 'static {
        let recommendations = Arc::clone(&self.recommendations);
        let config = self.guild_config.clone();
        async move {
            recommendations
                .post_weekly(
                    http,
                    config.guild_id,
                    config.recommendation_channel_id,
                    config.recommendation_schedule,
                )
                .await;
        }
    }
}

pub struct DiscordHandlerBuilder<C: IBundleClient> {
    permitted_guilds: Vec<u64>,
    media_bundle: Option<MediaBundle<C>>,
    tmdb_api_key: String,
    guild_config: Option<GuildConfig>, // not really sure if there is such a day but if we want to
    // add support to multi guilds, we can do so by changing
    // this to a Vec<GuildConfig>
    has_ready_been_called: AtomicBool,
}

impl<C> Default for DiscordHandlerBuilder<C>
where
    C: IBundleClient,
{
    fn default() -> Self {
        Self {
            permitted_guilds: vec![],
            media_bundle: None,
            tmdb_api_key: String::new(),
            guild_config: None,
            has_ready_been_called: AtomicBool::new(false),
        }
    }
}

impl<C> DiscordHandlerBuilder<C>
where
    C: IBundleClient,
{
    pub fn set_permitted_guilds(&mut self, guilds: Vec<u64>) {
        self.permitted_guilds = guilds;
    }

    pub fn set_media_bundle(&mut self, bundle: MediaBundle<C>) {
        self.media_bundle.replace(bundle);
    }

    pub fn set_guild_config(&mut self, config: GuildConfig) {
        self.guild_config.replace(config);
    }

    pub fn set_tmdb_api_key(&mut self, api_key: String) {
        self.tmdb_api_key = api_key;
    }

    pub fn build(self) -> DiscordHandler<C> {
        DiscordHandler {
            permitted_guilds: {
                let mut set = HashSet::new();
                for guild in self.permitted_guilds {
                    set.insert(guild);
                }
                set
            },
            media_bundle: self.media_bundle.unwrap(),
            guild_config: self.guild_config.unwrap(),
            recommendations: Arc::new(Recommendations::new(self.tmdb_api_key)),
            has_ready_been_called: self.has_ready_been_called,
        }
    }
}

fn webhook_message(payload: &WebhookEnvelope) -> String {
    match payload.event {
        EventType::Play => format!(
            "{} started playing {}",
            payload.account.title, payload.session_info.title
        ),
        EventType::Pause => format!(
            "{} paused {}",
            payload.account.title, payload.session_info.title
        ),
        EventType::Resume => format!(
            "{} resumed {}",
            payload.account.title, payload.session_info.title
        ),
        EventType::Stop => format!(
            "{} stopped {}",
            payload.account.title, payload.session_info.title
        ),
        EventType::New => format!(
            "New {} added to Plex: {}",
            payload.session_info.media_type, payload.session_info.title
        ),
    }
}

pub(crate) async fn start_webhook_listener(
    webhook_port: u16,
    guild_id: GuildId,
    cache_http: Arc<Http>,
) -> Result<JoinHandle<()>, Box<dyn Error + Send + Sync>> {
    let channels = guild_id.channels(&cache_http).await?;
    let admin_channel_id = channels
        .iter()
        .find(|(_, channel)| channel.name == ADMIN_CHANNEL_NAME)
        .map(|(channel_id, _)| *channel_id)
        .ok_or("Discord admin-notifications channel not found")?;

    let app = Router::new().route(
        "/",
        post(move |mut multipart: axum::extract::Multipart| async move {
            while let Ok(Some(field)) = multipart.next_field().await {
                if field.name() == Some("payload") {
                    if let Ok(json_str) = field.text().await {
                        match serde_json::from_str::<WebhookEnvelope>(&json_str) {
                            Ok(payload) => {
                                if let Err(why) = admin_channel_id
                                    .say(&cache_http, webhook_message(&payload))
                                    .await
                                {
                                    tracing::error!("Failed to send webhook notification: {}", why);
                                    return axum::http::StatusCode::BAD_GATEWAY;
                                }
                                return axum::http::StatusCode::OK;
                            }
                            Err(err) => {
                                tracing::error!("Failed to parse webhook JSON: {}", err);
                                return axum::http::StatusCode::BAD_REQUEST;
                            }
                        }
                    }
                }
            }
            axum::http::StatusCode::BAD_REQUEST
        }),
    );

    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", webhook_port)).await?;
    tracing::info!(
        "Plex webhook listener started on 127.0.0.1:{}",
        webhook_port
    );
    Ok(tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            tracing::error!("Plex webhook listener stopped: {}", err);
        }
    }))
}

#[async_trait::async_trait]
impl<C> EventHandler for DiscordHandler<C>
where
    C: IBundleClient,
{
    async fn message(&self, ctx: serenity::prelude::Context, msg: Message) {}

    // The ready routine is in charge of quite of a few things.
    // To make it easier to keep track of them, we shall list them out here (this list might grow
    // in the future):
    // - Programmatic channel management (i.e. channel creations, permissions, etc).
    // - Register commands for bastion (this is the bot's name).
    async fn ready(&self, ctx: serenity::prelude::Context, ready: Ready) {
        let has_ready_been_called = self
            .has_ready_been_called
            .load(std::sync::atomic::Ordering::Relaxed);
        if has_ready_been_called {
            tracing::info!("ready called more than once. skipping");
            return;
        }

        // Programmatic channel management
        let channels = self.guild_config.guild_id.channels(&ctx.http).await;
        if channels.is_err() {
            tracing::error!("Error retrieving channel info from guild. {:?}", channels);
        }
        let channels = channels.unwrap_or_else(|_| HashMap::default());

        let existing_channel_names = channels.iter().map(|e| &e.1.name).collect::<Vec<_>>();
        for channel_config in &self.guild_config.channels {
            let mut is_one_of_existing_channels = false;
            for name in &existing_channel_names {
                if *name == &channel_config.name {
                    is_one_of_existing_channels = true;
                    break;
                }
            }

            // We don't want to keep creating the same channels
            // note that here we are also choosing to preserve whatever changes that has been made
            // through the discord client
            // We don't have a more robust way other than names right now
            if is_one_of_existing_channels {
                continue;
            }

            tracing::info!("Creating channel: {}", channel_config.name);
            for name in &existing_channel_names {
                tracing::info!("Existing channel: {}", name);
            }

            // Now we do the actual creation
            let permissions = channel_config
                .permissions
                .iter()
                .map(|perm| {
                    let mut allow = Permissions::empty();
                    let mut deny = Permissions::empty();

                    for p in &perm.allow {
                        if let Some(perm) = Permissions::from_name(p) {
                            allow |= perm;
                        }
                    }

                    for p in &perm.deny {
                        if let Some(perm) = Permissions::from_name(p) {
                            deny |= perm;
                        }
                    }

                    PermissionOverwrite {
                        allow,
                        deny,
                        kind: PermissionOverwriteType::Role(RoleId::new(perm.role_id)),
                    }
                })
                .collect::<Vec<_>>();

            let kind = match channel_config.kind.as_str() {
                "text" => ChannelType::Text,
                "voice" => ChannelType::Voice,
                _ => ChannelType::Text, // Default to text if invalid kind is provided
            };

            let builder = CreateChannel::new(&channel_config.name)
                .kind(kind)
                .topic(&channel_config.topic)
                .nsfw(channel_config.nsfw)
                .position(channel_config.position as u16)
                .permissions(permissions);

            let channel_creation = self
                .guild_config
                .guild_id
                .create_channel(&ctx.http, builder)
                .await;

            match channel_creation {
                Ok(channel) => println!("Created channel: {}", channel.name),
                Err(e) => println!("Failed to create channel: {:?}", e),
            }
        }

        // Here we clear the command list. This is to clear any old commands.
        if let Err(e) = self
            .guild_config
            .guild_id
            .set_commands(&ctx.http, Vec::new())
            .await
        {
            tracing::error!("Error clearing commands for bastion: {:?}", e);
        }

        // Here we register commands for bastion
        if let Err(e) = self
            .guild_config
            .guild_id
            .set_commands(&ctx.http, commands::get_create_commands())
            .await
        {
            tracing::error!("Error setting commands for bastion: {:?}", e);
        }

        self.has_ready_been_called
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let Interaction::Command(command) = interaction else {
            return;
        };

        if command.data.name == "recommendations" {
            if let Some(error) = self
                .guild_config
                .recommendation_channel_error(command.guild_id, command.channel_id)
            {
                let response = CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(error)
                        .ephemeral(true),
                );
                if let Err(why) = command.create_response(&ctx.http, response).await {
                    tracing::error!("Cannot respond to slash command: {why}");
                }
                return;
            }
        }

        let is_admin = command.guild_id == Some(self.guild_config.guild_id)
            && command
                .member
                .as_ref()
                .and_then(|member| member.permissions)
                .is_some_and(|permissions| permissions.administrator());
        let admin_command = system::is_admin_command(&command.data.name);
        // Enforce permissions at execution time as well as in Discord's command defaults.
        if admin_command && !is_admin {
            let response = CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(
                        "This command requires Administrator permission in the configured guild.",
                    )
                    .ephemeral(true),
            );
            if let Err(why) = command.create_response(&ctx.http, response).await {
                tracing::error!("Cannot respond to slash command: {why}");
            }
            return;
        }

        // Discord only waits 3 seconds for a response and looking media up can take longer, so we
        // acknowledge the command first and fill in the response once it is done.
        let defer = CreateInteractionResponse::Defer(
            CreateInteractionResponseMessage::new()
                .ephemeral(admin_command || command.data.name == "help"),
        );
        if let Err(why) = command.create_response(&ctx.http, defer).await {
            tracing::error!("Cannot respond to slash command: {why}");
            return;
        }

        let options = command.data.options();
        let media_bundle = &self.media_bundle;
        let response: Result<EditInteractionResponse, CommandError> =
            match command.data.name.as_str() {
                "docker_ps" | "disk_space" | "restart_container" | "restart_services" => {
                    system::run(&command.data.name, &options)
                        .await
                        .map_err(CommandError::SystemCommandError)
                }
                "torrent_list" => qbit::list(media_bundle).await.map_err(CommandError::from),
                "torrent_delete" => qbit::delete(&options, media_bundle)
                    .await
                    .map_err(CommandError::from),
                "help" => Ok(EditInteractionResponse::new().content(commands::help(is_admin))),
                "recommendations" => self
                    .recommendations
                    .run()
                    .await
                    .map_err(CommandError::RecommendationError),
                "version" => Ok(EditInteractionResponse::new()
                    .content(format!("Dookie version {}", env!("CARGO_PKG_VERSION")))),
                "grant_access" => commands::grant_access::run(&options, media_bundle)
                    .await
                    .map(|content| EditInteractionResponse::new().content(content))
                    .map_err(CommandError::from),
                "search_movie" => media::search(MediaKind::Movie, &options, media_bundle)
                    .await
                    .map_err(CommandError::from),
                "add_movie" => media::add(MediaKind::Movie, &options, media_bundle)
                    .await
                    .map_err(CommandError::from),
                "remove_movie" => media::remove(MediaKind::Movie, &options, media_bundle)
                    .await
                    .map_err(CommandError::from),
                "search_show" => media::search(MediaKind::Show, &options, media_bundle)
                    .await
                    .map_err(CommandError::from),
                "add_show" => media::add(MediaKind::Show, &options, media_bundle)
                    .await
                    .map_err(CommandError::from),
                "remove_show" => media::remove(MediaKind::Show, &options, media_bundle)
                    .await
                    .map_err(CommandError::from),
                name => {
                    Ok(EditInteractionResponse::new().content(format!("Unknown command {name}")))
                }
            };

        let response = response.unwrap_or_else(|why| {
            tracing::error!("/{} failed: {:#?}", command.data.name, why);
            EditInteractionResponse::new().content(
                format!("/{} failed: {why}", command.data.name)
                    .chars()
                    .take(1900)
                    .collect::<String>(),
            )
        });
        if let Err(why) = command.edit_response(&ctx.http, response).await {
            tracing::error!("Cannot respond to slash command: {why}");
        }
    }

    async fn resume(&self, ctx: Context, event: serenity::all::ResumedEvent) {
        // we don't technically need this here
        // I am putting it here because I want to keep track of the occurrence of this event
        // This is because serenity takes care of the reconnection for us
        tracing::info!("resume called with event {:?}", event);
    }
}

mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn recommendations_require_the_explicit_general_channel_and_guild() {
        let mut config: GuildConfig = serde_json::from_value(serde_json::json!({
            "guild_id": 1, "channels": []
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(&config).unwrap()["recommendation_schedule"],
            serde_json::json!({"day": "Sun", "time": "10:00:00"})
        );
        let disabled: GuildConfig = serde_json::from_value(serde_json::json!({
            "guild_id": 1, "channels": [], "recommendation_schedule": null
        }))
        .unwrap();
        assert!(disabled.recommendation_schedule.is_none());
        assert!(config
            .recommendation_channel_error(Some(GuildId::new(1)), ChannelId::new(2))
            .unwrap()
            .contains("recommendation_channel_id"));
        config.recommendation_channel_id = Some(ChannelId::new(2));
        assert!(config
            .recommendation_channel_error(Some(GuildId::new(1)), ChannelId::new(2))
            .is_none());
        assert!(config
            .recommendation_channel_error(Some(GuildId::new(1)), ChannelId::new(3))
            .unwrap()
            .contains("<#2>"));
        assert!(config
            .recommendation_channel_error(None, ChannelId::new(2))
            .is_some());
        assert!(config
            .recommendation_channel_error(Some(GuildId::new(9)), ChannelId::new(2))
            .is_some());
    }

    static BODY: &str = r#"
        {"event":"media.resume","user":true,"owner":true,"Account":{"id":304665517,"thumb":"redacted","title":"user_redacted"},"Server":{"title":"felix_macbook_pro_13","uuid":"uuidredacted"},"Player":{"local":true,"publicAddress":"publicaddressredacted","title":"Felixs-MacBook-Pro.local","uuid":"uuidredacted"},"Metadata":{"librarySectionType":"show","ratingKey":"17946","key":"/library/metadata/17946","parentRatingKey":"17943","grandparentRatingKey":"17933","guid":"plex://episode/5e8339c4ec007c00412fee31","parentGuid":"plex://season/602e75c7fdd281002ce0b667","grandparentGuid":"plex://show/5d9c09152192ba001f31b6c8","grandparentSlug":"solar-opposites","type":"episode","title":"The Quantum Ring","titleSort":"Quantum Ring","grandparentKey":"/library/metadata/17933","parentKey":"/library/metadata/17943","librarySectionTitle":"TV Shows","librarySectionID":2,"librarySectionKey":"/library/sections/2","grandparentTitle":"Solar Opposites","parentTitle":"Season 1","contentRating":"TV-MA","summary":"TaDAH! Korvo becomes a magician!","index":3,"parentIndex":1,"audienceRating":8.1,"viewOffset":372000,"lastViewedAt":1724018723,"year":2020,"thumb":"/library/metadata/17946/thumb/1711856426","art":"/library/metadata/17933/art/1711856419","parentThumb":"/library/metadata/17943/thumb/1711856426","grandparentThumb":"/library/metadata/17933/thumb/1711856419","grandparentArt":"/library/metadata/17933/art/1711856419","grandparentTheme":"/library/metadata/17933/theme/1711856419","duration":1260000,"originallyAvailableAt":"2020-05-08","addedAt":1711856407,"updatedAt":1711856426,"audienceRatingImage":"themoviedb://image.rating","UltraBlurColors":{"topLeft":"16236c","topRight":"201439","bottomRight":"443adc","bottomLeft":"1b2075"},"Guid":[{"id":"imdb://tt8910944"},{"id":"tmdb://2061894"},{"id":"tvdb://7547572"}],"Rating":[{"image":"themoviedb://image.rating","value":8.1,"type":"audience"}],"Director":[{"id":2688,"filter":"director=2688","tag":"Lucas Gray","tagKey":"5f3ff40fbf3e560040b4151b"}],"Writer":[{"id":9763,"filter":"writer=9763","tag":"Matt McKenna","tagKey":"5d9f3513d74e670020020a6b"}],"Role":[{"id":2517,"filter":"actor=2517","tag":"Justin Roiland","tagKey":"5d776a3447dd6e001f6cfd4d","role":"Korvo (voice)","thumb":"https://metadata-static.plex.tv/a/people/af4c83335d57bcf3266a1e585833a3d4.jpg"},{"id":7538,"filter":"actor=7538","tag":"Sean Giambrone","tagKey":"5d776959fb0d55001f523663","role":"Yumyulack (voice)","thumb":"https://metadata-static.plex.tv/3/people/39bf19592edf0c6d2f0a955238470b12.jpg"},{"id":2588,"filter":"actor=2588","tag":"Thomas Middleditch","tagKey":"5d77684f61141d001fb184c6","role":"Terry (voice)","thumb":"https://metadata-static.plex.tv/e/people/e00806e61932c2b0129cc04542807b9d.jpg"},{"id":8151,"filter":"actor=8151","tag":"Mary Mack","tagKey":"5d7770706afb3d002061a406","role":"Jesse (voice)","thumb":"https://metadata-static.plex.tv/6/people/66b7a88d7779d53a1f45c86833a3e37c.jpg"}]}}
    "#;

    #[test]
    fn parses_and_formats_plex_webhook_body() {
        let envelope = serde_json::from_str::<WebhookEnvelope>(BODY).unwrap();
        assert!(matches!(envelope.event, EventType::Resume));
        assert!(matches!(
            envelope.session_info.media_type,
            MediaType::Episode
        ));
        assert_eq!(envelope.session_info.title, "Solar Opposites");
        assert_eq!(
            webhook_message(&envelope),
            "user_redacted resumed Solar Opposites"
        );
    }

    #[cfg(any())]
    #[test]
    fn parses_plex_delete_ids_and_distinguishes_series_from_episode() {
        let delete_movie = serde_json::json!({
            "event": "library.delete",
            "Account": {"id": 1, "title": "owner"},
            "Metadata": {
                "type": "movie",
                "title": "The Matrix",
                "Guid": [{"id": "tmdb://603"}]
            }
        });
        let movie = serde_json::from_value::<WebhookEnvelope>(delete_movie).unwrap();
        assert!(matches!(movie.event, EventType::Delete));
        assert!(matches!(movie.session_info.media_type, MediaType::Movie));
        assert_eq!(
            external_id(&movie.session_info.external_ids, "tmdb"),
            Some(603)
        );

        let delete_episode = serde_json::json!({
            "event": "library.delete",
            "Account": {"id": 1, "title": "owner"},
            "Metadata": {
                "type": "episode",
                "grandparentTitle": "Example Show",
                "Guid": [{"id": "tvdb://81189"}]
            }
        });
        let episode = serde_json::from_value::<WebhookEnvelope>(delete_episode).unwrap();
        assert!(matches!(
            episode.session_info.media_type,
            MediaType::Episode
        ));

        let delete_series = serde_json::json!({
            "event": "library.delete",
            "Account": {"id": 1, "title": "owner"},
            "Metadata": {
                "type": "show",
                "title": "Example Show",
                "Guid": [{"id": "tvdb://81189"}]
            }
        });
        let series = serde_json::from_value::<WebhookEnvelope>(delete_series).unwrap();
        assert!(matches!(series.session_info.media_type, MediaType::Show));
        assert_eq!(
            external_id(&series.session_info.external_ids, "tvdb"),
            Some(81189)
        );
    }

    #[test]
    fn test_config_format() {
        let config_yml = r#"
            guild_id: 123
            channels:
                - name: "channel_one"
                  kind: "text"
                  topic: "test"
                  nsfw: false
                  position: 0
                  permissions:
                    - role_id: 123
                      allow: ["SEND_MESSAGES", "VIEW_CHANNEL"]
                      deny: []
                - name: "channel_two"
                  kind: "test"
                  topic: "test"
                  nsfw: false
                  position: 1
                  permissions: 
                    - role_id: 123
                      allow: []
                      deny: ["SEND_MESSAGES", "VIEW_CHANNEL"] 
                    - role_id: 456
                      allow: ["SEND_MESSAGES", "VIEW_CHANNEL"]
                      deny: []
            "#;

        let config = serde_yaml::from_str::<GuildConfig>(config_yml);
        assert!(config.is_ok());
    }
}
