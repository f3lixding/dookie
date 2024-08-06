/// This is the discord bot that we will use for various purposes.
/// So far these include:
/// - Ping / health check
/// - Notifications for remote sessions (this will notification will only be sent to channel admin)
/// - Notifications for new comer on instructions to set everything up
/// - Notifications for leaving soon categories
/// - Accept commands to add media (and return approporiate responses)
use axum::{http::StatusCode, routing::get, Router};
use serde::Deserialize;
use serenity::{
    all::{
        ChannelId, ChannelType, CreateChannel, GuildId, PermissionOverwrite,
        PermissionOverwriteType, Permissions, RoleId,
    },
    model::{channel::Message, gateway::Ready},
    prelude::*,
};
use std::collections::{HashMap, HashSet};

use crate::{IBundleClient, MediaBundle};

use super::commands;

#[derive(Deserialize)]
pub struct GuildConfig {
    guild_id: GuildId,
    channels: Vec<ChannelConfig>,
}

#[derive(Deserialize)]
pub struct ChannelConfig {
    name: String,
    kind: String,
    topic: String,
    nsfw: bool,
    position: u64,
    permissions: Vec<ChannelPermission>,
}

#[derive(Deserialize)]
pub struct ChannelPermission {
    role_id: u64,
    allow: Vec<String>,
    deny: Vec<String>,
}

pub struct DiscordHandler<C: IBundleClient> {
    channel_id: u64,
    permitted_guilds: HashSet<u64>,
    webhook_port: u16,
    media_bundle: MediaBundle<C>,
    guild_config: GuildConfig,
}

impl<C> DiscordHandler<C> where C: IBundleClient {}

pub struct DiscordHandlerBuilder<C: IBundleClient> {
    channel_id: Option<u64>,
    permitted_guilds: Vec<u64>,
    webhook_port: Option<u16>,
    media_bundle: Option<MediaBundle<C>>,
    guild_config: Option<GuildConfig>, // not really sure if there is such a day but if we want to
                                       // add support to multi guilds, we can do so by changing
                                       // this to a Vec<GuildConfig>
}

impl<C> Default for DiscordHandlerBuilder<C>
where
    C: IBundleClient,
{
    fn default() -> Self {
        Self {
            channel_id: None,
            permitted_guilds: vec![],
            webhook_port: None,
            media_bundle: None,
            guild_config: None,
        }
    }
}

impl<C> DiscordHandlerBuilder<C>
where
    C: IBundleClient,
{
    pub fn set_channel_id(&mut self, id: u64) {
        self.channel_id.replace(id);
    }

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
            channel_id: self.channel_id.unwrap(),
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
        let channel_id = ChannelId::new(self.channel_id);
        let webhook_port = self.webhook_port;
        tokio::spawn(async move {
            let app = Router::new().route(
                "/",
                get(move || async move {
                    if let Err(why) = channel_id.say(ctx.http, "Someone started a session").await {
                        println!("Error sending message: {:?}", why);
                    }
                    StatusCode::OK
                }),
            );
            let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", webhook_port))
                .await
                .unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    }
}

mod tests {
    #[allow(unused_imports)]
    use super::GuildConfig;

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
