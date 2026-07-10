use anyhow::{Context as _, anyhow, bail};
use fallible_iterator::FallibleIterator;
use itertools::Itertools;
use serenity::all::{Channel, GenericChannelId, Http, MessagePin};
use serenity::builder::{CreateEmbed, CreateEmbedAuthor, CreateEmbedFooter, ExecuteWebhook};
use serenity::model::prelude::Member;
use serenity::model::user::User;
use serenity::{
    async_trait,
    model::{
        Permissions,
        prelude::{ChannelId, CommandInteraction, Embed, GuildId, Message},
    },
    prelude::Context,
};
use serenity_command::{CommandResponse, args, command};
use std::fmt::Write;
use std::sync::Arc;
use tokio::task::block_in_place;

use serenity_command_handler::{RegisterableModule, prelude::*};

const MAX_EMBEDS: usize = 10;

/// Copy a message embed to a CreateEmbed
pub fn copy_embed(em: &Embed) -> CreateEmbed<'_> {
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
    for fld in &em.fields {
        out = out.field(&fld.name, &fld.value, fld.inline)
    }
    if let Some(footer) = &em.footer {
        let mut f = CreateEmbedFooter::new(&footer.text);
        if let Some(icon) = &footer.icon_url {
            f = f.icon_url(icon);
        }
        out = out.footer(f);
    }
    if let Some(img) = &em.image {
        out = out.image(&img.url, None);
    }
    if let Some(thumbnail) = &em.thumbnail {
        out = out.thumbnail(&thumbnail.url, None);
    }
    if let Some(ts) = em.timestamp {
        out = out.timestamp(ts);
    }
    out
}

/// Subset of fields of a discord message
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

args!(SETPINBOARDWEBHOOK_ARGS =
    "The webhook URL for the pinboard channel (leave empty to remove)"
    webhook: Option<String>,
);

const SETPINBOARDWEBHOOK: CommandConst = CommandConst {
    description: "Set (or unset) a webhook for the pinboard channel",
    permissions: Permissions::MANAGE_WEBHOOKS,
    ..command!(/setpinboardwebhook SETPINBOARDWEBHOOK_ARGS: set_pinboard_webhook)
};

/// Configure the webhook through which to post pinned messages
async fn set_pinboard_webhook(
    (webhook,): SETPINBOARDWEBHOOK_ARGS,
    handler: &Handler,
    _ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let guild_id = command
        .guild_id
        .ok_or_else(|| anyhow!("Must be run in a guild"))?
        .get();
    handler
        .db
        .lock()
        .await
        .set_guild_field(guild_id, "pinboard_webhook", webhook.as_deref())?;
    CommandResponse::private(if webhook.is_some() {
        "Pinboard webhook set"
    } else {
        "Pinboard webhook removed"
    })
}

/// Load the list of channels for which the bot is configured to
/// move pins to the pinboard
async fn load_allowed_channels(
    handler: &Handler,
    guild_id: GuildId,
) -> anyhow::Result<Vec<ChannelId>> {
    let db = handler.db.lock().await;
    block_in_place(|| {
        let mut stmt = db
            .conn()
            .prepare("SELECT channel_id FROM pinboard_allowed_channels WHERE guild_id = ?1")?;
        let channels: Vec<_> = stmt
            .query([guild_id.get()])?
            .map(|row| Ok(ChannelId::new(row.get(0)?)))
            .collect()?;
        Ok(channels)
    })
}

/// Helper to get a user's avatar, favoring the server avatar if present
fn user_avatar(user: &User, member: Option<&Member>) -> Option<String> {
    member
        .and_then(|member| member.avatar_url().clone())
        .filter(|av| av.starts_with("http"))
        .or_else(|| user.avatar_url())
        .filter(|av| av.starts_with("http"))
}

/// Extract data to create a message pin.
///
/// Returns the message author's name and avatar,
/// as well as any images contained in the message.
async fn prepare_message<'a>(
    msg: &'a Message,
    guild_id: GuildId,
    http: &'_ Http,
) -> anyhow::Result<(String, Option<String>, Vec<&'a str>)> {
    let author = &msg.author;
    let member = match guild_id.member(http, author.id).await {
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
    // filter attachments to find images
    let images = msg
        .attachments
        .iter()
        .filter(|at| at.height.is_some())
        .map(|at| at.url.as_str())
        .collect();
    Ok((name.to_string(), avatar, images))
}

/// Pinboard module.
pub struct Pinboard;

impl Pinboard {
    /// Posts a newly-pinned message to a pinboard channel via webhook and unpins it.
    pub async fn move_pin_to_pinboard(
        handler: &Handler,
        ctx: &Context,
        channel: GenericChannelId,
        guild_id: GuildId,
    ) -> anyhow::Result<()> {
        // load webhook
        let Some(pinboard_webhook) = handler
            .db
            .lock()
            .await
            .get_guild_field(guild_id.get(), "pinboard_webhook")
            .ok()
            .flatten()
            .filter(|s: &String| !s.is_empty())
        else {
            bail!("No webhook configured")
        };

        // check if this pin should be moved to pinboard
        let allowed_channels = load_allowed_channels(handler, guild_id).await?;
        if !(allowed_channels.is_empty() || allowed_channels.contains(&channel.expect_channel())) {
            return Ok(());
        }

        // find pinned message
        let pins = channel
            .pins(&ctx.http, None, None)
            .await
            .context("could not retrieve pins")?;
        let Some(MessagePin {
            message: last_pin, ..
        }) = pins.items.last()
        else {
            // no pinned message, ignore
            return Ok(());
        };

        // retrieve full message in new task
        let http = Arc::clone(&ctx.http);
        let channel = last_pin.channel_id;
        let msg_id = last_pin.id;
        let msg_fut = tokio::spawn(async move { http.get_message(channel, msg_id).await });

        let (name, avatar, images) = prepare_message(last_pin, guild_id, &ctx.http).await?;

        // get channel / thread name to mention in footer
        let ch = channel.to_channel(&ctx.http, Some(guild_id)).await?;
        let channel_name = match &ch {
            Channel::Guild(g) => &g.base.name,
            Channel::GuildThread(t) => &t.base.name,
            _ => "unknown channel",
        };

        // get the bot's name to put in the footer
        let self_name = handler.self_id.get().unwrap().to_user(&ctx).await?.name;
        let footer_str = format!("Pinned from #{channel_name} using {self_name}");

        // allocate list of embeds
        let mut embeds = Vec::with_capacity(last_pin.embeds.len() as usize + 1);

        // retrieve actual message in order to get potential reply
        // and include it in the pinboard message
        let msg = msg_fut.await??;
        if let Some(reply) = &msg.referenced_message {
            let (name, avatar, images) = prepare_message(reply, guild_id, &ctx.http).await?;
            let image = images.first().copied();
            if !reply.content.is_empty() || image.is_some() {
                embeds.push({
                    let mut em = CreateEmbed::new()
                        .description(&reply.content)
                        .author({
                            let mut at = CreateEmbedAuthor::new(format!("Replying to {name}"));
                            if let Some(icon) = avatar.as_ref() {
                                at = at.icon_url(icon.to_owned());
                            }
                            at
                        })
                        .url(reply.link().to_string());
                    if let Some(img) = image {
                        em = em.image(img, None);
                    }
                    em
                })
            }
        }
        // put first image with the embed for message text
        let image = images.first().copied();
        if !last_pin.content.is_empty() || image.is_some() {
            embeds.push({
                let mut content = last_pin.content.to_string();
                if !content.is_empty() {
                    // add space for source link
                    content.push_str("\n\n");
                }
                _ = write!(&mut content, "[(Source)]({})", last_pin.link());
                let mut em = CreateEmbed::new()
                    .description(content)
                    .footer(CreateEmbedFooter::new(&footer_str))
                    .timestamp(last_pin.timestamp)
                    .author({
                        let mut at = CreateEmbedAuthor::new(&name).url(last_pin.link().to_string());
                        if let Some(url) = avatar.as_ref() {
                            at = at.icon_url(url);
                        }
                        at
                    });
                if let Some(url) = image {
                    em = em.image(url, None);
                }
                em
            })
        }
        // create embeds for remaining images
        embeds.extend(images.into_iter().skip(1).map(|img| {
            CreateEmbed::new()
                .image(img, None)
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
                    let mut wh = ExecuteWebhook::new().embeds(embeds).username(&name);
                    if let Some(url) = avatar.as_ref() {
                        wh = wh.avatar_url(url);
                    }
                    wh
                })
                .await
                .context("error calling pinboard webhook")?;
        }
        last_pin
            .unpin(&ctx.http, Some("Moved to pinboard"))
            .await
            .context("error deleting pinned message")?;
        Ok(())
    }
}

const REGISTER_CHANNEL: CommandConst = CommandConst {
    description: "Register_channel_to_pinboard",
    permissions: Permissions::MANAGE_MESSAGES,
    ..command!(/register_channel_to_pinboard: register_channel)
};

async fn register_channel(
    data: &Handler,
    _: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let guild_id = command.guild_id()?;
    let db = data.db.lock().await;
    db.conn().execute(
            "INSERT INTO pinboard_allowed_channels (guild_id, channel_id) VALUES (?1, ?2) ON CONFLICT DO NOTHING",
            [guild_id.get(), command.channel_id.get()])?;
    CommandResponse::private(format!(
        "Registered <#{}> to pinboard",
        command.channel_id.get()
    ))
}

const UNREGISTER_CHANNEL: CommandConst = CommandConst {
    description: "Unregister_channel_from_pinboard",
    permissions: Permissions::MANAGE_MESSAGES,
    ..command!(/unregister_channel_from_pinboard: unregister_channel)
};

/// Disable moving pins to pinboard for the channel this command is called in
async fn unregister_channel(
    data: &Handler,
    _: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let guild_id = command.guild_id()?;
    let db = data.db.lock().await;
    db.conn().execute(
        "DELETE FROM pinboard_allowed_channels WHERE guild_id = ?1 AND channel_id = ?2",
        [guild_id.get(), command.channel_id.get()],
    )?;
    CommandResponse::private(format!(
        "Unregistered <#{}> from pinboard",
        command.channel_id.get()
    ))
}

const LIST_CHANNELS: CommandConst = CommandConst {
    description: "List_pinboard_channels",
    permissions: Permissions::MANAGE_MESSAGES,
    ..command!(/list_pinboard_channels: list_channels)
};

/// List channels for which pins will be moved to pinboard
async fn list_channels(
    handler: &Handler,
    _: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let guild_id = command.guild_id()?;
    let channels = load_allowed_channels(handler, guild_id).await?;
    let resp = match channels.as_slice() {
        [] => {
            "No channels configured, pins from every channel will be sent to pinboard".to_string()
        }
        _ => format!(
            "Pins from the following channels will be sent to pinboard:\n{}",
            channels
                .iter()
                .map(|c| format!("<#{}>", c.get()))
                .join("\n")
        ),
    };
    CommandResponse::public(resp)
}

#[async_trait]
impl Module for Pinboard {
    async fn setup(&mut self, db: &mut serenity_command_handler::db::Db) -> anyhow::Result<()> {
        db.add_guild_field("pinboard_webhook", "STRING")?;
        db.conn().execute(
            "CREATE TABLE IF NOT EXISTS pinboard_allowed_channels (
                guild_id INTEGER NOT NULL,
                channel_id INTEGER NOT NULL,

                UNIQUE (guild_id, channel_id)
            )",
            [],
        )?;
        Ok(())
    }

    fn register_commands(&self, store: &mut dyn Storer) {
        store.register(SETPINBOARDWEBHOOK);
        store.register(REGISTER_CHANNEL);
        store.register(UNREGISTER_CHANNEL);
        store.register(LIST_CHANNELS);
    }
}

impl RegisterableModule for Pinboard {
    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Ok(Pinboard)
    }
}
