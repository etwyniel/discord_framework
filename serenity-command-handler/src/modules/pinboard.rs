use anyhow::{anyhow, bail, Context as _};
use fallible_iterator::FallibleIterator;
use itertools::Itertools;
use serenity::builder::{CreateEmbed, CreateEmbedAuthor, CreateEmbedFooter, ExecuteWebhook};
use serenity::model::prelude::Member;
use serenity::model::user::User;
use serenity::{
    async_trait,
    model::{
        prelude::{ChannelId, CommandInteraction, Embed, GuildId, Message},
        Permissions,
    },
    prelude::Context,
};
use serenity_command::{BotCommand, CommandResponse};
use serenity_command_derive::Command;
use std::fmt::Write;

use crate::prelude::*;

const MAX_EMBEDS: usize = 10;

pub fn copy_embed(em: &Embed) -> CreateEmbed {
    let mut out = CreateEmbed::new();
    if let Some(title) = &em.title {
        out = out.title(title);
    }
    if let Some(url) = &em.url {
        out = out.url(url);
    }
    if let Some(author) = &em.author {
        let mut at = CreateEmbedAuthor::new(&author.name);
        if let Some(url) = &author.url {
            at = at.url(url);
        }
        if let Some(icon) = &author.icon_url {
            at = at.icon_url(icon);
        }
        out = out.author(at);
    }
    if let Some(color) = em.colour {
        out = out.color(color);
    }
    if let Some(desc) = &em.description {
        out = out.description(desc);
    }
    out = em
        .fields
        .iter()
        .fold(out, |out, f| out.field(&f.name, &f.value, f.inline));
    if let Some(footer) = &em.footer {
        let mut f = CreateEmbedFooter::new(&footer.text);
        if let Some(icon) = &footer.icon_url {
            f = f.icon_url(icon);
        }
        out = out.footer(f);
    }
    if let Some(img) = &em.image {
        out = out.image(&img.url);
    }
    if let Some(thumbnail) = &em.thumbnail {
        out = out.thumbnail(&thumbnail.url);
    }
    if let Some(ts) = em.timestamp {
        out = out.timestamp(ts);
    }
    out
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
        opts: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let guild_id = opts
            .guild_id
            .ok_or_else(|| anyhow!("Must be run in a guild"))?
            .get();
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

async fn load_allowed_channels(
    handler: &Handler,
    guild_id: GuildId,
) -> anyhow::Result<Vec<ChannelId>> {
    let db = handler.db.lock().await;
    let mut stmt = db
        .conn
        .prepare("SELECT channel_id FROM pinboard_allowed_channels WHERE guild_id = ?1")?;
    let channels: Vec<_> = stmt
        .query([guild_id.get()])?
        .map(|row| Ok(ChannelId::new(row.get(0)?)))
        .collect()?;
    Ok(channels)
}

fn user_avatar(user: &User, member: Option<&Member>) -> Option<String> {
    member
        .and_then(|member| member.avatar_url().clone())
        .filter(|av| av.starts_with("http"))
        .or_else(|| user.avatar_url())
        .filter(|av| av.starts_with("http"))
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
            .get_guild_field(guild_id.get(), "pinboard_webhook")
            .map_err(|_| anyhow!("No webhook configured"))?;
        let allowed_channels = load_allowed_channels(handler, guild_id).await?;
        if !(allowed_channels.is_empty() || allowed_channels.contains(&channel)) {
            return Ok(());
        }
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
            .map(|m| m.display_name())
            .unwrap_or(&author.name);
        let avatar = user_avatar(author, member.as_ref());
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
        let self_name = handler.self_id.get().unwrap().to_user(&ctx).await?.name;
        let mut embeds = Vec::with_capacity(last_pin.embeds.len() + 1);
        let footer_str = format!("Pinned from #{channel_name} using {self_name}");
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
                .map(|m| m.display_name())
                .unwrap_or(&author.name);
            let avatar = user_avatar(author, member.as_ref());
            // Filter attachments to find images
            let image = reply
                .attachments
                .iter()
                .filter(|at| at.height.is_some())
                .map(|at| at.url.as_str())
                .next();
            if !reply.content.is_empty() || image.is_some() {
                embeds.push({
                    let mut em = CreateEmbed::new()
                        .description(&reply.content)
                        .author({
                            let mut at = CreateEmbedAuthor::new(format!("Replying to {name}"));
                            if let Some(icon) = avatar.as_ref() {
                                at = at.icon_url(icon);
                            }
                            at
                        })
                        .url(reply.link());
                    if let Some(img) = image {
                        em = em.image(img);
                    }
                    em
                })
            }
        }
        // put first image with the embed for message text
        let image = images.next();
        if !last_pin.content.is_empty() || image.is_some() {
            embeds.push({
                let mut content = last_pin.content.clone();
                if !content.is_empty() {
                    content.push_str("\n\n");
                }
                _ = write!(&mut content, "[(Source)]({})", last_pin.link());
                let mut em = CreateEmbed::new()
                    .description(content)
                    .footer(CreateEmbedFooter::new(&footer_str))
                    .timestamp(last_pin.timestamp)
                    .author({
                        let mut at = CreateEmbedAuthor::new(name).url(last_pin.link());
                        if let Some(url) = avatar.as_ref() {
                            at = at.icon_url(url);
                        }
                        at
                    });
                if let Some(url) = image {
                    em = em.image(url);
                }
                em
            })
        }
        // create embeds for remaining images
        embeds.extend(images.map(|img| {
            CreateEmbed::new()
                .image(img)
                .footer(CreateEmbedFooter::new(&footer_str))
                .timestamp(last_pin.timestamp)
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
                .execute(&ctx.http, true, {
                    let mut wh = ExecuteWebhook::new().embeds(embeds).username(name);
                    if let Some(url) = avatar.as_ref() {
                        wh = wh.avatar_url(url);
                    }
                    wh
                })
                .await
                .context("error calling pinboard webhook")?;
        }
        last_pin
            .unpin(&ctx.http)
            .await
            .context("error deleting pinned message")?;
        Ok(())
    }
}

#[derive(Command)]
#[cmd(name = "register_channel_to_pinboard")]
struct RegisterChannel;

#[async_trait]
impl BotCommand for RegisterChannel {
    type Data = Handler;
    const PERMISSIONS: Permissions = Permissions::MANAGE_MESSAGES;

    async fn run(
        self,
        data: &Handler,
        _: &Context,
        interaction: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let Some(guild_id) = interaction.guild_id else {
            bail!("Must be run in a guild")
        };
        let db = data.db.lock().await;
        db.conn.execute(
            "INSERT INTO pinboard_allowed_channels (guild_id, channel_id) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
            [guild_id.get(), interaction.channel_id.get()])?;
        Ok(CommandResponse::Private(format!(
            "Registered <#{}> to pinboard",
            interaction.channel_id.get()
        )))
    }
}

#[derive(Command)]
#[cmd(name = "unregister_channel_from_pinboard")]
struct UnregisterChannel;

#[async_trait]
impl BotCommand for UnregisterChannel {
    type Data = Handler;
    const PERMISSIONS: Permissions = Permissions::MANAGE_MESSAGES;

    async fn run(
        self,
        data: &Handler,
        _: &Context,
        interaction: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let Some(guild_id) = interaction.guild_id else {
            bail!("Must be run in a guild")
        };
        let db = data.db.lock().await;
        db.conn.execute(
            "DELETE FROM pinboard_allowed_channels WHERE guild_id = ?1 AND channel_id = ?2",
            [guild_id.get(), interaction.channel_id.get()],
        )?;
        Ok(CommandResponse::Private(format!(
            "Unregistered <#{}> from pinboard",
            interaction.channel_id.get()
        )))
    }
}

#[derive(Command)]
#[cmd(name = "list_pinboard_channels")]
struct ListChannels;

#[async_trait]
impl BotCommand for ListChannels {
    type Data = Handler;
    const PERMISSIONS: Permissions = Permissions::MANAGE_MESSAGES;

    async fn run(
        self,
        handler: &Handler,
        _: &Context,
        interaction: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let Some(guild_id) = interaction.guild_id else {
            bail!("Must be run in a guild")
        };
        let channels = load_allowed_channels(handler, guild_id).await?;
        let resp = match channels.as_slice() {
            [] => "No channels configured, pins from every channel will be sent to pinboard"
                .to_string(),
            _ => format!(
                "Pins from the following channels will be sent to pinboard:\n{}",
                channels
                    .iter()
                    .map(|c| format!("<#{}>", c.get()))
                    .join("\n")
            ),
        };
        Ok(CommandResponse::Public(resp))
    }
}

#[async_trait]
impl Module for Pinboard {
    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Ok(Pinboard)
    }

    async fn setup(&mut self, db: &mut crate::db::Db) -> anyhow::Result<()> {
        db.add_guild_field("pinboard_webhook", "STRING")?;
        db.conn.execute(
            "CREATE TABLE IF NOT EXISTS pinboard_allowed_channels (
                guild_id INTEGER NOT NULL,
                channel_id INTEGER NOT NULL,

                UNIQUE (guild_id, channel_id)
            )",
            [],
        )?;
        Ok(())
    }

    fn register_commands(
        &self,
        store: &mut CommandStore,
        _completion_handlers: &mut CompletionStore,
    ) {
        store.register::<SetPinboardWebhook>();
        store.register::<RegisterChannel>();
        store.register::<UnregisterChannel>();
        store.register::<ListChannels>();
    }
}
