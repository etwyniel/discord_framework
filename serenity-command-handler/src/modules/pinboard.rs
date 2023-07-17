use anyhow::{anyhow, Context as _};
use serenity::model::prelude::interaction::application_command::ApplicationCommandInteraction;
use serenity::{
    async_trait,
    model::{
        prelude::{ChannelId, Embed, GuildId, Message},
        Permissions,
    },
    prelude::Context,
};
use serenity_command::{BotCommand, CommandResponse};
use serenity_command_derive::Command;

use crate::prelude::*;

const MAX_EMBEDS: usize = 10;

pub fn copy_embed(em: &Embed) -> serenity::json::Value {
    Embed::fake(|out| {
        em.title.as_ref().map(|title| out.title(title));
        em.url.as_ref().map(|url| out.url(url));
        em.author.as_ref().map(|author| {
            out.author(|at| {
                author.url.as_ref().map(|url| at.url(url));
                author.icon_url.as_ref().map(|url| at.icon_url(url));
                at.name(&author.name)
            })
        });
        em.colour.as_ref().map(|colour| out.colour(*colour));
        em.description
            .as_ref()
            .map(|description| out.description(description));
        em.fields.iter().for_each(|f| {
            out.field(&f.name, &f.value, f.inline);
        });
        em.footer.as_ref().map(|footer| {
            out.footer(|f| {
                footer.icon_url.as_ref().map(|url| f.icon_url(url));
                f.text(&footer.text)
            })
        });
        em.image.as_ref().map(|image| out.image(&image.url));
        em.thumbnail
            .as_ref()
            .map(|thumbnail| out.thumbnail(&thumbnail.url));
        em.timestamp
            .as_deref()
            .map(|timestamp| out.timestamp(timestamp));
        out
    })
}

#[derive(Debug)]
#[allow(unused)]
struct SimpleMessage<'a> {
    author: &'a str,
    content: &'a str,
    attachments: Vec<&'a str>,
    embeds: &'a [Embed],
}

impl<'a> From<&'a Message> for SimpleMessage<'a> {
    fn from(msg: &'a Message) -> Self {
        let author = &msg.author.name;
        let content = &msg.content;
        let attachments = msg.attachments.iter().map(|a| a.url.as_str()).collect();
        let embeds = &msg.embeds;
        SimpleMessage {
            author,
            content,
            attachments,
            embeds,
        }
    }
}

#[derive(Command)]
#[cmd(
    name = "setpinboardwebhook",
    desc = "Set (or unset) a webhook for the pinboard channel"
)]
pub struct SetPinboardWebhook {
    #[cmd(desc = "The webhook URL for the pinboard channel (leave empty to remove)")]
    webhook: Option<String>,
}

#[async_trait]
impl BotCommand for SetPinboardWebhook {
    type Data = Handler;
    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        opts: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let guild_id = opts
            .guild_id
            .ok_or_else(|| anyhow!("Must be run in a guild"))?
            .0;
        handler.db.lock().await.set_guild_field(
            guild_id,
            "pinboard_webhook",
            self.webhook.as_deref(),
        )?;
        Ok(CommandResponse::Private(
            if self.webhook.is_some() {
                "Pinboard webhook set"
            } else {
                "Pinboard webhook removed"
            }
            .to_string(),
        ))
    }

    const PERMISSIONS: Permissions = Permissions::MANAGE_WEBHOOKS;
}

pub struct Pinboard;

impl Pinboard {
    // Posts a newly-pinned message to a pinboard channel via webhook and unpins it.
    pub async fn move_pin_to_pinboard(
        handler: &Handler,
        ctx: &Context,
        channel: ChannelId,
        guild_id: GuildId,
    ) -> anyhow::Result<()> {
        let pinboard_webhook: String = handler
            .db
            .lock()
            .await
            .get_guild_field(guild_id.0, "pinboard_webhook")
            .map_err(|_| anyhow!("No webhook configured"))?;
        let pins = channel
            .pins(&ctx.http)
            .await
            .context("could not retrieve pins")?;
        let last_pin = match pins.last() {
            Some(m) => m,
            _ => return Ok(()),
        };
        let message: SimpleMessage = last_pin.into();
        dbg!(message);
        let author = &last_pin.author;
        // retrieve user as guild member in order to get nickname and guild avatar
        let member = match guild_id.member(&ctx.http, author).await {
            Ok(m) => Some(m),
            Err(e) => {
                // log error but carry on
                eprintln!("Error getting member: {e:#}");
                None
            }
        };
        let name = member
            .as_ref()
            .and_then(|m| m.nick.as_deref())
            .unwrap_or(&author.name);
        let avatar = member
            .as_ref()
            .and_then(|member| member.avatar.clone())
            .filter(|av| av.starts_with("http"))
            .or_else(|| author.avatar_url())
            .filter(|av| av.starts_with("http"));
        let channel_name = channel
            .to_channel(&ctx)
            .await?
            .guild()
            .map(|ch| ch.name().to_string())
            .unwrap_or_else(|| "unknown-channel".to_string());
        // Filter attachments to find images
        let mut images = last_pin
            .attachments
            .iter()
            .filter(|at| at.height.is_some())
            .map(|at| at.url.as_str());
        let mut embeds = Vec::with_capacity(last_pin.embeds.len() + 1);
        let footer_str = format!("Message pinned from #{channel_name} using LPBot");
        // retrieve actual message in order to get potential reply
        let msg = last_pin.channel_id.message(&ctx.http, last_pin.id).await?;
        if let Some(reply) = &msg.referenced_message {
            let author = &reply.author;
            // retrieve user as guild member in order to get nickname and guild avatar
            let member = match guild_id.member(&ctx.http, author).await {
                Ok(m) => Some(m),
                Err(e) => {
                    // log error but carry on
                    eprintln!("Error getting member: {e:#}");
                    None
                }
            };
            let name = member
                .as_ref()
                .and_then(|m| m.nick.as_deref())
                .unwrap_or(&author.name);
            let avatar = member
                .as_ref()
                .and_then(|member| member.avatar.clone())
                .filter(|av| av.starts_with("http"))
                .or_else(|| author.avatar_url())
                .filter(|av| av.starts_with("http"));
            // Filter attachments to find images
            let image = reply
                .attachments
                .iter()
                .filter(|at| at.height.is_some())
                .map(|at| at.url.as_str())
                .next();
            if !reply.content.is_empty() || image.is_some() {
                embeds.push(Embed::fake(|val| {
                    image.map(|url| val.image(url));
                    val.description(&reply.content)
                        .timestamp(reply.timestamp)
                        .author(|author| {
                            avatar.map(|av| author.icon_url(av));
                            author.name(format!("Replying to {name}"))
                        })
                }))
            }
        }
        // put first image with the embed for message text
        let image = images.next();
        if !last_pin.content.is_empty() || image.is_some() {
            embeds.push(Embed::fake(|val| {
                image.map(|url| val.image(url));
                val.description(format!(
                    "{}\n\n[(Source)]({})",
                    last_pin.content,
                    last_pin.link()
                ))
                .footer(|footer| footer.text(&footer_str))
                .timestamp(last_pin.timestamp)
                .author(|author| {
                    avatar.as_ref().map(|av| author.icon_url(av));
                    author.name(name)
                })
            }))
        }
        // create embeds for remaining images
        embeds.extend(images.map(|img| {
            Embed::fake(|out| {
                out.image(img)
                    .footer(|f| f.text(&footer_str))
                    .timestamp(last_pin.timestamp)
            })
        }));
        embeds.extend(
            last_pin
                .embeds
                .iter()
                .filter(|em| em.kind.as_deref() == Some("rich"))
                .map(copy_embed),
        );
        for embeds in embeds.chunks(MAX_EMBEDS).map(Vec::from) {
            ctx.http
                .get_webhook_from_url(&pinboard_webhook)
                .await
                .context("error getting webhook")?
                .execute(&ctx.http, true, |message| {
                    message.embeds(embeds);
                    avatar.as_ref().map(|av| message.avatar_url(av));
                    message.username(name)
                })
                .await
                .context("error calling pinboard webhook")?;
            last_pin
                .unpin(&ctx.http)
                .await
                .context("error deleting pinned message")?;
        }
        Ok(())
    }
}

#[async_trait]
impl Module for Pinboard {
    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Ok(Pinboard)
    }

    async fn setup(&mut self, db: &mut crate::db::Db) -> anyhow::Result<()> {
        db.add_guild_field("pinboard_webhook", "STRING")?;
        Ok(())
    }

    fn register_commands(
        &self,
        store: &mut CommandStore,
        _completion_handlers: &mut CompletionStore,
    ) {
        store.register::<SetPinboardWebhook>();
    }
}
