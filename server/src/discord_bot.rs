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

#[derive(Default)]
pub struct DiscordHandler {
    channel_id: u64,
    permitted_guilds: HashSet<u64>,
    webhook_port: u16,
}

#[async_trait::async_trait]
impl EventHandler for DiscordHandler {
    async fn message(&self, ctx: serenity::prelude::Context, msg: Message) {
        match msg.content.as_str() {
            "!ping" => {
                if let Err(why) = msg.channel_id.say(&ctx.http, "Pong!").await {
                    tracing::error!("Error sending message for ping check: {:?}", why);
                }
            }
            _ => { /* noop on non command messages */ }
        }
        if msg.content == "!ping" {
            if let Err(why) = msg.channel_id.say(&ctx.http, "Pong!").await {
                println!("Error sending message: {:?}", why);
            }
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
