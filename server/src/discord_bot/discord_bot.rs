use super::{commands, CommandError};
use crate::{IBundleClient, MediaBundle};
/// This is the discord bot that we will use for various purposes.
/// So far these include:
/// - Ping / health check
/// - Notifications for remote sessions (this will notification will only be sent to channel admin)
/// - Notifications for new comer on instructions to set everything up
/// - Notifications for leaving soon categories
/// - Accept commands to add media (and return approporiate responses)
use axum::{http::StatusCode, response::IntoResponse as _, routing::post, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serenity::{
    all::{
        ChannelId, ChannelType, CreateChannel, CreateInteractionResponse,
        CreateInteractionResponseMessage, GuildId, Interaction, PermissionOverwrite,
        PermissionOverwriteType, Permissions, RoleId,
    },
    model::{channel::Message, gateway::Ready},
    prelude::*,
};
use std::{
    collections::{HashMap, HashSet},
    sync::atomic::AtomicBool,
};

static ADMIN_CHANNEL_NAME: &'static str = "admin-notifications";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GuildConfig {
    guild_id: GuildId,
    channels: Vec<ChannelConfig>,
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

#[derive(Debug, Deserialize, Clone)]
struct Account {
    title: String,
    id: u64,
}

#[derive(Debug, Deserialize, Clone)]
enum MediaType {
    #[serde(rename = "movie")]
    Movie,
    #[serde(rename = "show")]
    Show,
}

impl std::fmt::Display for MediaType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MediaType::Movie => write!(f, "movie"),
            MediaType::Show => write!(f, "show"),
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
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventType::Resume => write!(f, "resume"),
            EventType::Play => write!(f, "play"),
            EventType::Stop => write!(f, "stop"),
            EventType::Pause => write!(f, "pause"),
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
                .get("librarySectionType")
                .ok_or_else(|| serde::de::Error::missing_field("librarySectionType"))?
                .clone(),
        )
        .map_err(serde::de::Error::custom)?;

        let session_info = match media_type {
            MediaType::Show => {
                let title = metadata
                    .get("grandparentSlug")
                    .ok_or_else(|| serde::de::Error::missing_field("grandparentSlug"))?
                    .to_string();

                SessionInfo { title, media_type }
            }
            MediaType::Movie => {
                let title = metadata
                    .get("title")
                    .ok_or_else(|| serde::de::Error::missing_field("grandparentSlug"))?
                    .to_string();

                SessionInfo { title, media_type }
            }
        };

        Ok(WebhookEnvelope {
            account,
            event,
            session_info,
        })
    }
}

pub struct DiscordHandler<C: IBundleClient> {
    permitted_guilds: HashSet<u64>,
    webhook_port: u16,
    media_bundle: MediaBundle<C>,
    guild_config: GuildConfig,
    has_ready_been_called: AtomicBool,
}

impl<C> DiscordHandler<C> where C: IBundleClient {}

pub struct DiscordHandlerBuilder<C: IBundleClient> {
    permitted_guilds: Vec<u64>,
    webhook_port: Option<u16>,
    media_bundle: Option<MediaBundle<C>>,
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
            webhook_port: None,
            media_bundle: None,
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

    pub fn set_webhook_port(&mut self, port: u16) {
        self.webhook_port.replace(port);
    }

    pub fn set_media_bundle(&mut self, bundle: MediaBundle<C>) {
        self.media_bundle.replace(bundle);
    }

    pub fn set_guild_config(&mut self, config: GuildConfig) {
        self.guild_config.replace(config);
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
            webhook_port: self.webhook_port.unwrap(),
            media_bundle: self.media_bundle.unwrap(),
            guild_config: self.guild_config.unwrap(),
            has_ready_been_called: self.has_ready_been_called,
        }
    }
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
    // - Spawn a task to monitor the webhook port for remote session events.
    async fn ready(&self, ctx: serenity::prelude::Context, ready: Ready) {
        // Programmatic channel management
        let channels = self.guild_config.guild_id.channels(&ctx.http).await;
        let channels = channels.unwrap_or_else(|_| {
            tracing::error!("Error retrieving channel info from guild.");
            HashMap::default()
        });

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

        // Spawn a task to monitor the webhook port for remote session events
        // TODO: maybe we should have a dedicated channel for this so we don't bombard the general
        // chat with status updates that no one care about
        let admin_channel_id = channels
            .iter()
            .find(|(_, channel)| channel.name == ADMIN_CHANNEL_NAME)
            .map(|(channel_id, _)| *channel_id);
        let webhook_port = self.webhook_port;
        let cache_http = ctx.http.clone();

        let has_ready_been_called = self
            .has_ready_been_called
            .load(std::sync::atomic::Ordering::Relaxed);
        if has_ready_been_called {
            return;
        }

        if let Some(admin_channel_id) = admin_channel_id {
            // TODO: give a handle to the spawned task so we can kill it later
            tokio::spawn(async move {
                let app = Router::new().route(
                    "/",
                    post(move |mut multipart: axum::extract::Multipart| async move {
                        while let Ok(Some(field)) = multipart.next_field().await {
                            if field.name() == Some("payload") {
                                if let Ok(json_str) = field.text().await {
                                    match serde_json::from_str::<WebhookEnvelope>(&json_str) {
                                        Ok(payload) => {
                                            if let Err(why) = admin_channel_id
                                                .say(
                                                    &cache_http,
                                                    format!(
                                                        "{} {}ed session with {}",
                                                        payload.account.title,
                                                        payload.event,
                                                        payload.session_info.title
                                                    ),
                                                )
                                                .await
                                            {
                                                tracing::error!("Failed to parse JSON: {}", why);
                                                return axum::http::StatusCode::BAD_REQUEST;
                                            }
                                            return axum::http::StatusCode::OK;
                                        }
                                        Err(err) => {
                                            tracing::error!("Failed to parse JSON: {}", err);
                                            return axum::http::StatusCode::BAD_REQUEST;
                                        }
                                    }
                                }
                            }
                        }
                        #[warn(clippy::needless_return)]
                        return axum::http::StatusCode::BAD_REQUEST;
                    }),
                );
                let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", webhook_port))
                    .await
                    .expect("Failed to bind to webhook port");
                axum::serve(listener, app).await.unwrap();
            });
            self.has_ready_been_called
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Command(command) = interaction {
            let content: Option<Result<String, CommandError>> = match command.data.name.as_str() {
                "grant_access" => Some(
                    commands::grant_access::run(&command.data.options(), &self.media_bundle)
                        .await
                        .map_err(CommandError::from),
                ),
                _ => None,
            };

            if let Some(content) = content {
                let response = match content {
                    Ok(content) => content,
                    Err(why) => {
                        tracing::error!("Cannot grant access due to error: {:#?}", why);
                        format!("Cannot grant access due to error: {why}")
                    }
                };

                let data = CreateInteractionResponseMessage::new().content(response);
                let builder = CreateInteractionResponse::Message(data);
                if let Err(why) = command.create_response(&ctx.http, builder).await {
                    tracing::error!("Cannot respond to slash command: {why}");
                }
            }
        }
    }
}

mod tests {
    #[allow(unused_imports)]
    use super::*;

    static BODY: &str = r#"
        {"event":"media.resume","user":true,"owner":true,"Account":{"id":304665517,"thumb":"redacted","title":"user_redacted"},"Server":{"title":"felix_macbook_pro_13","uuid":"uuidredacted"},"Player":{"local":true,"publicAddress":"publicaddressredacted","title":"Felixs-MacBook-Pro.local","uuid":"uuidredacted"},"Metadata":{"librarySectionType":"show","ratingKey":"17946","key":"/library/metadata/17946","parentRatingKey":"17943","grandparentRatingKey":"17933","guid":"plex://episode/5e8339c4ec007c00412fee31","parentGuid":"plex://season/602e75c7fdd281002ce0b667","grandparentGuid":"plex://show/5d9c09152192ba001f31b6c8","grandparentSlug":"solar-opposites","type":"episode","title":"The Quantum Ring","titleSort":"Quantum Ring","grandparentKey":"/library/metadata/17933","parentKey":"/library/metadata/17943","librarySectionTitle":"TV Shows","librarySectionID":2,"librarySectionKey":"/library/sections/2","grandparentTitle":"Solar Opposites","parentTitle":"Season 1","contentRating":"TV-MA","summary":"TaDAH! Korvo becomes a magician!","index":3,"parentIndex":1,"audienceRating":8.1,"viewOffset":372000,"lastViewedAt":1724018723,"year":2020,"thumb":"/library/metadata/17946/thumb/1711856426","art":"/library/metadata/17933/art/1711856419","parentThumb":"/library/metadata/17943/thumb/1711856426","grandparentThumb":"/library/metadata/17933/thumb/1711856419","grandparentArt":"/library/metadata/17933/art/1711856419","grandparentTheme":"/library/metadata/17933/theme/1711856419","duration":1260000,"originallyAvailableAt":"2020-05-08","addedAt":1711856407,"updatedAt":1711856426,"audienceRatingImage":"themoviedb://image.rating","UltraBlurColors":{"topLeft":"16236c","topRight":"201439","bottomRight":"443adc","bottomLeft":"1b2075"},"Guid":[{"id":"imdb://tt8910944"},{"id":"tmdb://2061894"},{"id":"tvdb://7547572"}],"Rating":[{"image":"themoviedb://image.rating","value":8.1,"type":"audience"}],"Director":[{"id":2688,"filter":"director=2688","tag":"Lucas Gray","tagKey":"5f3ff40fbf3e560040b4151b"}],"Writer":[{"id":9763,"filter":"writer=9763","tag":"Matt McKenna","tagKey":"5d9f3513d74e670020020a6b"}],"Role":[{"id":2517,"filter":"actor=2517","tag":"Justin Roiland","tagKey":"5d776a3447dd6e001f6cfd4d","role":"Korvo (voice)","thumb":"https://metadata-static.plex.tv/a/people/af4c83335d57bcf3266a1e585833a3d4.jpg"},{"id":7538,"filter":"actor=7538","tag":"Sean Giambrone","tagKey":"5d776959fb0d55001f523663","role":"Yumyulack (voice)","thumb":"https://metadata-static.plex.tv/3/people/39bf19592edf0c6d2f0a955238470b12.jpg"},{"id":2588,"filter":"actor=2588","tag":"Thomas Middleditch","tagKey":"5d77684f61141d001fb184c6","role":"Terry (voice)","thumb":"https://metadata-static.plex.tv/e/people/e00806e61932c2b0129cc04542807b9d.jpg"},{"id":8151,"filter":"actor=8151","tag":"Mary Mack","tagKey":"5d7770706afb3d002061a406","role":"Jesse (voice)","thumb":"https://metadata-static.plex.tv/6/people/66b7a88d7779d53a1f45c86833a3e37c.jpg"}]}}
    "#;

    #[test]
    fn test_de_for_webhook_body() {
        let envelope = serde_json::from_str::<WebhookEnvelope>(BODY);
        assert!(envelope.is_ok());
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
