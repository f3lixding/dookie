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
    async fn message(&self, ctx: serenity::prelude::Context, msg: Message) {}

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
