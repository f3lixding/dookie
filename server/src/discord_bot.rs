/// This is the discord bot that we will use for various purposes.
/// So far these include:
/// - Ping / health check
/// - Notifications for remote sessions (this will notification will only be sent to channel admin)
/// - Notifications for new comer on instructions to set everything up
/// - Notifications for leaving soon categories
/// - Accept commands to add media (and return approporiate responses)
use axum::{http::StatusCode, routing::get, Router};
use serenity::{
    all::ChannelId,
    model::{channel::Message, gateway::Ready},
    prelude::*,
};
use std::collections::HashSet;

use crate::{IBundleClient, MediaBundle};

#[derive(Debug)]
enum CommandParsingError {
    NotACommand,
    Malformation(String),
}

impl std::fmt::Display for CommandParsingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Failed to convert str to a valid command")
    }
}

impl std::error::Error for CommandParsingError {}

enum Command {
    Ping,
    GrantPermission(String),
    QueryMovie(String),
    QueryShow(String),
    DownloadMovie(String),
    DownloadShow(String),
    QueryOngoingRemoteSessions,
}

impl TryFrom<&str> for Command {
    type Error = CommandParsingError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        unimplemented!()
    }
}

pub struct DiscordHandler<C: IBundleClient> {
    channel_id: u64,
    permitted_guilds: HashSet<u64>,
    webhook_port: u16,
    media_bundle: MediaBundle<C>,
}

impl<C> DiscordHandler<C> where C: IBundleClient {}

pub struct DiscordHandlerBuilder<C: IBundleClient> {
    channel_id: Option<u64>,
    permitted_guilds: Vec<u64>,
    webhook_port: Option<u16>,
    media_bundle: Option<MediaBundle<C>>,
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
        }
    }
}

#[async_trait::async_trait]
impl<C> EventHandler for DiscordHandler<C>
where
    C: IBundleClient,
{
    async fn message(&self, ctx: serenity::prelude::Context, msg: Message) {
        let command = Command::try_from(msg.content.as_str());

        if let Err(parsing_error) = command {
            match parsing_error {
                CommandParsingError::Malformation(_) => {
                    // TODO: Depending on how malformed the commands are, provide hints for correct
                    // commands.
                    tracing::error!("Error parsing command");
                }
                _ => { /*noop since text is not a command*/ }
            }
            return;
        }

        let command = command.unwrap();
        match command {
            Command::Ping => {
                if let Err(why) = msg.channel_id.say(&ctx.http, "Pong!").await {
                    tracing::error!("Error sending message for ping check: {:?}", why);
                }
            }
            Command::GrantPermission(email) => {
                // First we need to check permission
                // Right now we are only allowing admins to grant permission
                let member = msg.member(&ctx.http).await;
                if let Err(why) = member {
                    tracing::error!("Error enountered while retrieving member info: {}", why);
                    return;
                }
                let member = member.unwrap();
                let permission = member.permissions;
                if let Some(permissions) = permission {
                    if permissions.administrator() {
                        // TODO: grant access here
                    }
                } else {
                    tracing::error!(
                        "User {} does not have permissions associated during access request",
                        member.display_name()
                    );
                }
            }
            _ => { /* noop on non command messages */ }
        }
    }

    async fn ready(&self, ctx: serenity::prelude::Context, ready: Ready) {
        ready.guilds.iter().for_each(|g| println!("{:?}", g));
        // println!("{} is connected", ready.user.name);
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
