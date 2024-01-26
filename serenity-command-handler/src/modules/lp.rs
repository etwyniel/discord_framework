use std::borrow::Cow;
use std::fmt::Write;
use std::ops::Add;

use crate::{db::Db, CommandStore, HandlerBuilder, Module};
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context as _;
use chrono::{prelude::*, Duration};
use futures::future::BoxFuture;
use futures::FutureExt;
use regex::Regex;
use serenity::all::AutoArchiveDuration;
use serenity::all::RoleId;
use serenity::async_trait;
use serenity::builder::CreateAllowedMentions;
use serenity::builder::CreateAutocompleteResponse;
use serenity::builder::CreateInteractionResponse;
use serenity::builder::CreateThread;
use serenity::builder::EditMessage;
use serenity::builder::EditThread;
use serenity::builder::ExecuteWebhook;
use serenity::builder::GetMessages;
use serenity::client::Context;
use serenity::model::application::CommandDataOption;
use serenity::model::application::CommandType;
use serenity::model::channel::ChannelType;
use serenity::model::id::GuildId;
use serenity::model::prelude::CommandInteraction;
use serenity::model::Permissions;
use serenity_command_derive::Command;

use crate::album::Album;
use crate::command_context::{get_focused_option, get_str_opt_ac, Responder};
use crate::modules::{Bandcamp, Lastfm, Spotify};
use crate::prelude::*;
use serenity_command::CommandResponse;
use serenity_command::{BotCommand, CommandKey};

use super::AlbumLookup;

#[derive(Command)]
#[cmd(name = "lp", desc = "run a listening party")]
pub struct Lp {
    #[cmd(
        desc = "What you will be listening to (e.g. band - album, spotify/bandcamp link)",
        autocomplete
    )]
    album: String,
    #[cmd(
        desc = "(Optional) Link to the album/playlist (Spotify, Youtube, Bandcamp...)",
        autocomplete
    )]
    link: Option<String>,
    #[cmd(desc = "Time at which the LP will take place (e.g. XX:20, +5)")]
    time: Option<String>,
    #[cmd(desc = "Where to look for album info (defaults to spotify)")]
    provider: Option<String>,
    #[cmd(desc = "Use a specific role instead of the default (admin-only)")]
    role: Option<RoleId>,
}

fn format_end(start: DateTime<Utc>, duration: Option<Duration>) -> String {
    let Some(duration) = duration else {
        return String::new();
    };
    let end = start.add(duration);
    format!(", ends at <t:{}:t>", end.timestamp())
}

fn convert_lp_time(
    time: Option<&str>,
    duration: Option<Duration>,
) -> Result<String, anyhow::Error> {
    let mut lp_time = Utc::now().add(Duration::seconds(10));
    let time = match time {
        Some("now") | None => {
            let end_str = format_end(lp_time, duration);
            return Ok(format!("now (<t:{}:R>{end_str})", lp_time.timestamp()));
        }
        Some(t) => t,
    };
    let xx_re = Regex::new("(?i)^(XX:?)?([0-5][0-9])$")?; // e.g. XX:15, xx15 or 15
    let plus_re = Regex::new(r"\+?(([0-5])?[0-9])m?")?; // e.g. +25
    if let Some(cap) = xx_re.captures(time) {
        let min: i64 = cap.get(2).unwrap().as_str().parse()?;
        if !(0..60).contains(&min) {
            bail!("Invalid time");
        }
        let cur_min = lp_time.minute() as i64;
        let to_add = if cur_min <= min {
            min - cur_min
        } else {
            (60 - cur_min) + min
        };
        lp_time = lp_time.add(Duration::minutes(to_add));
    } else if let Some(cap) = plus_re.captures(time) {
        let extra_mins: i64 = cap.get(1).unwrap().as_str().parse()?;
        lp_time = lp_time.add(Duration::minutes(extra_mins));
    } else {
        return Ok(time.to_string());
    }

    let end_str = format_end(lp_time, duration);
    // timestamp and relative time
    Ok(format!(
        "at <t:{0:}:t> (<t:{0:}:R>{end_str})",
        lp_time.timestamp()
    ))
}

async fn get_lastfm_genres(handler: &Handler, info: &Album) -> Option<Vec<String>> {
    if info.is_playlist || !info.genres.is_empty() {
        return None;
    }
    // No genres, try to get some from last.fm
    match handler
        .module::<Lastfm>()
        .ok()?
        .artist_top_tags(info.artist.as_ref()?)
        .await
    {
        Ok(genres) => Some(genres),
        Err(err) => {
            // Log error but carry on
            eprintln!("Couldn't retrieve genres from lastfm: {err}");
            None
        }
    }
}

async fn build_message_contents(
    lp_name: Option<&str>,
    info: &Album,
    time: Option<&str>,
    role_id: Option<u64>,
) -> anyhow::Result<String> {
    let when = convert_lp_time(time, info.duration)?;
    let lp_name = lp_name
        .map(str::to_string)
        .unwrap_or_else(|| info.format_name());
    let hyperlinked = if let Some(link) = &info.url {
        format!("[**{lp_name}**]({link})")
    } else {
        lp_name
    };
    let mut resp_content = format!(
        "{} {hyperlinked} {}\n",
        role_id // mention role if set
            .map(|id| format!("<@&{id}>"))
            .unwrap_or_else(|| "Listening party: ".to_string()),
        when
    );
    if let Some(duration) = info.duration {
        if duration.num_hours() > 0 {
            _ = write!(&mut resp_content, "{}h", duration.num_hours());
        }
        let minutes = duration.num_minutes() % 60;
        if minutes > 0 {
            _ = write!(&mut resp_content, "{minutes:02}m");
        }
        let seconds = duration.num_seconds();
        if seconds < 60 {
            _ = write!(&mut resp_content, "{seconds}s");
        }
    }
    if let Some(genres) = info.format_genres() {
        if info.duration.is_some() {
            resp_content.push_str(" | ");
        }
        _ = writeln!(&mut resp_content, "{}", &genres);
    }
    Ok(resp_content)
}

#[async_trait]
impl BotCommand for Lp {
    type Data = Handler;
    async fn run(
        self,
        handler: &Handler,
        ctx: &Context,
        command: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let Lp {
            album,
            mut link,
            time,
            provider,
            role,
        } = self;
        if let (Some(_), Some(member)) = (role, &command.member) {
            if !member.permissions.unwrap_or_default().mention_everyone() {
                bail!("Only admins are allowed to specify a role to ping.");
            }
        }
        let mut lp_name = Some(album);
        if lp_name.as_deref().map(|name| name.starts_with("https://")) == Some(true) {
            // As a special case for convenience, if we have a URL in lp_name, use that as link
            if link.is_some() && link != lp_name {
                lp_name = None;
            } else {
                link = lp_name.take();
            }
        }
        let lookup: &AlbumLookup = handler.module()?;
        let http = &ctx.http;
        // Depending on what we have, look up more information
        let mut info = match (&lp_name, &link) {
            (Some(name), None) => lookup.lookup_album(name, provider.as_deref()).await?,
            (name, Some(lnk)) => {
                let mut info = lookup.get_album_info(lnk).await?;
                if let Some((info, name)) = info.as_mut().zip(name.clone()) {
                    info.name = Some(name)
                };
                info
            }
            (None, None) => bail!("Please specify something to LP"),
        }
        .unwrap_or_else(|| Album {
            url: link,
            ..Default::default()
        });
        // get genres if needed
        if let Some(genres) = get_lastfm_genres(handler, &info).await {
            info.genres = genres
        }

        let guild_id = command.guild_id()?.get();
        let mut role_id = handler
            .get_guild_field(guild_id, "role_id")
            .await
            .context("error retrieving LP role")?;
        role_id = role.map(|r| r.get()).or(role_id);
        let resp_content =
            build_message_contents(lp_name.as_deref(), &info, time.as_deref(), role_id).await?;
        let webhook: Option<String> = handler.get_guild_field(guild_id, "webhook").await?;
        let wh = match webhook.as_deref().map(|url| http.get_webhook_from_url(url)) {
            Some(fut) => Some(fut.await?),
            None => None,
        };
        let message = if let Some(wh) = &wh {
            // Send LP message through webhook
            // This lets us impersonate the user who sent the command
            let user = &command.user;
            let avatar_url = GuildId::new(guild_id)
                .member(http, user)
                .await?
                .avatar_url()
                .or_else(|| user.avatar_url());
            let nick = user // try to get the user's nickname
                .nick_in(http, guild_id)
                .await
                .map(Cow::Owned)
                .unwrap_or_else(|| Cow::Borrowed(&user.name));
            wh.execute(http, true, {
                let mut webhook = ExecuteWebhook::new()
                    .content(&resp_content)
                    .allowed_mentions(CreateAllowedMentions::new().roles(role_id))
                    .username(nick.as_str());
                if let Some(url) = avatar_url.as_ref() {
                    webhook = webhook.avatar_url(url);
                }
                webhook
            })
            .await?
            .unwrap() // Message is present because we set wait to true in execute
        } else {
            // prefix response with pinger mention
            let resp = format!("<@{}>: {resp_content}", command.user.id.get());
            // Create interaction response
            command
                .respond(&ctx.http, CommandResponse::Public(resp), role_id)
                .await?
                .unwrap()
        };
        let mut response = format!(
            "LP created: {}",
            message.id.link(message.channel_id, command.guild_id)
        );
        if handler.get_guild_field(guild_id, "create_threads").await? {
            // Create a thread from the response message for the LP to take place in
            let chan = message.channel(http).await?;
            let thread_name = info.name.as_deref().unwrap_or("Listening party");
            let mut guild_chan = chan.guild().map(|c| (c.kind, c));
            if let (None, Some((ChannelType::PublicThread, c))) = (&webhook, &mut guild_chan) {
                // If we're already in a thread, just rename it
                // unless we are using a webhook, in which case we can create a new thread
                c.edit_thread(http, EditThread::new().name(thread_name))
                    .await?;
            } else if let Some((ChannelType::Text, c)) = &guild_chan {
                // Create thread from response message
                let thread = c
                    .create_thread(
                        http,
                        CreateThread::new(thread_name)
                            .kind(ChannelType::PublicThread)
                            .auto_archive_duration(AutoArchiveDuration::OneHour),
                    )
                    .await?;
                response = format!("LP created: <#{}>", thread.id.get());
            }
        }
        if let Some(wh) = wh {
            // If we used a webhook, we still need to create the interaction response
            let response = if wh.channel_id == Some(command.channel_id) {
                CommandResponse::Private(response)
            } else {
                CommandResponse::Public(response)
            };
            command.respond(&ctx.http, response, None).await?;
        }
        Ok(CommandResponse::None)
    }
}

#[derive(Command)]
#[cmd(
    name = "setcreatethreads",
    desc = "set whether to create threads for listening parties"
)]
pub struct SetCreateThreads {
    create_threads: bool,
}

#[async_trait]
impl BotCommand for SetCreateThreads {
    type Data = Handler;
    const PERMISSIONS: Permissions = Permissions::MANAGE_THREADS;
    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        command: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let guild_id = command.guild_id()?.get();
        let mut db = handler.db.lock().await;
        db.set_guild_field(guild_id, "create_threads", self.create_threads)
            .context("updating 'create_threads' guild field")?;
        let resp = if self.create_threads {
            "Will create threads when setting up listening parties"
        } else {
            "Will not create threads when setting up listening parties"
        };
        Ok(CommandResponse::Private(resp.to_string()))
    }
}

#[derive(Command)]
#[cmd(name = "setrole", desc = "set the role to ping for listening parties")]
pub struct SetRole {
    role: Option<RoleId>,
}

#[async_trait]
impl BotCommand for SetRole {
    type Data = Handler;
    const PERMISSIONS: Permissions = Permissions::MANAGE_ROLES;
    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        command: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let guild_id = command.guild_id()?.get();
        let role = self.role.as_ref().map(|r| r.get().to_string());
        let mut db = handler.db.lock().await;
        db.set_guild_field(guild_id, "role_id", &role)
            .context("updating 'role_id' guild field")?;
        let resp = if let Some(role_id) = role {
            format!("Set listening party role to <@&{role_id}>.")
        } else {
            "Unset listening party role.".to_string()
        };
        Ok(CommandResponse::Private(resp))
    }
}

#[derive(Command)]
#[cmd(
    name = "setwebhook",
    desc = "set a webhook to use when creating listening parties"
)]
pub struct SetWebhook {
    webhook: Option<String>,
}

#[async_trait]
impl BotCommand for SetWebhook {
    type Data = Handler;
    const PERMISSIONS: Permissions = Permissions::MANAGE_WEBHOOKS;
    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        command: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let guild_id = command.guild_id()?.get();
        let mut db = handler.db.lock().await;
        db.set_guild_field(guild_id, "webhook", self.webhook.as_ref())
            .context("updating 'webhook' guild field")?;
        let resp = if self.webhook.is_some() {
            "Listening parties will be created using a webhook."
        } else {
            "Listening parties will not be created using a webhook."
        };
        Ok(CommandResponse::Private(resp.to_string()))
    }
}

#[derive(Command)]
#[cmd(name = "edit_lp", desc = "Edit the last LP you created")]
pub struct EditLp {
    cancel: Option<bool>,
    time: Option<String>,
}

#[async_trait]
impl BotCommand for EditLp {
    type Data = Handler;
    async fn run(
        self,
        handler: &Handler,
        ctx: &Context,
        command: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let messages = command
            .channel_id
            .messages(&ctx.http, GetMessages::new().limit(100))
            .await
            .context("couldn't retrieve messages")?;
        let self_id = *handler.self_id.get().unwrap();
        let author_id = command.user.id.get();
        let author_id_str = author_id.to_string();
        let mut msg = messages
            .into_iter()
            .filter(|msg| msg.author.id == self_id)
            .find(|msg| {
                if let Some(interation) = &msg.interaction {
                    interation.user.id == author_id && interation.name == "lp"
                } else {
                    msg.content.contains(&author_id_str)
                }
            })
            .ok_or_else(|| anyhow!("No recent listening party to edit."))?;
        if self.cancel == Some(true) {
            msg.edit(
                &ctx.http,
                EditMessage::new().content(format!("~~{}~~", &msg.content)),
            )
            .await?;
            return Ok(CommandResponse::Public(
                "Canceled listening party".to_string(),
            ));
        }
        if let Some(time) = self.time.as_ref() {
            let formatted = convert_lp_time(Some(time), None)?;
            let re = Regex::new(r"(now|at <t:\d+:t>) \(.*\)").unwrap();
            let replaced = re.replace(&msg.content, &formatted);
            msg.edit(&ctx.http, EditMessage::new().content(replaced))
                .await?;
            return Ok(CommandResponse::Public(format!(
                "Listening party will start {formatted}"
            )));
        }
        bail!("Nothing to change")
    }
}

pub struct ModLp;

impl ModLp {
    async fn autocomplete_lp(
        handler: &Handler,
        options: &[CommandDataOption],
    ) -> anyhow::Result<Vec<(String, String)>> {
        let mut choices = vec![];
        let mut provider = get_str_opt_ac(options, "provider");
        let focused = get_focused_option(options);
        let mut album = get_str_opt_ac(options, "album");
        if let (Some(mut s), Some("album")) = (&mut album, focused) {
            if s.len() >= 7 && !s.starts_with("https://") {
                // if url, don't complete
                if let (None, Some(stripped)) = (&provider, s.strip_prefix("bc:")) {
                    // as a shorthand, search bandcamp for values with the prefix "bc:"
                    s = stripped;
                    provider = Some("bandcamp");
                }
                choices = handler
                    .module::<AlbumLookup>()?
                    .query_albums(s, provider)
                    .await
                    .unwrap_or_default();
            }
            if !s.is_empty() {
                choices.push((s.to_string(), s.to_string()));
            }
        } else if let (Some("link"), Some(album)) = (focused, &album) {
            // If album contains a url, suggest using the same url for link
            if album.starts_with("https://") {
                choices.push((album.to_string(), album.to_string()));
            }
        }
        Ok(choices)
    }

    fn complete_lp<'a>(
        handler: &'a Handler,
        ctx: &'a Context,
        key: CommandKey<'a>,
        ac: &'a CommandInteraction,
    ) -> BoxFuture<'a, anyhow::Result<bool>> {
        async move {
            if key != ("lp", CommandType::ChatInput) {
                return Ok(false);
            }
            let choices = Self::autocomplete_lp(handler, &ac.data.options).await?;
            let resp = choices
                .into_iter()
                .fold(CreateAutocompleteResponse::new(), |resp, (name, value)| {
                    resp.add_string_choice(name, value)
                });
            ac.create_response(&ctx.http, CreateInteractionResponse::Autocomplete(resp))
                .await?;
            Ok(true)
        }
        .boxed()
    }
}

#[async_trait]
impl Module for ModLp {
    async fn add_dependencies(builder: HandlerBuilder) -> anyhow::Result<HandlerBuilder> {
        builder
            .module::<Lastfm>()
            .await?
            .module::<Spotify>()
            .await?
            .module::<Bandcamp>()
            .await?
            .module::<AlbumLookup>()
            .await
    }

    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Ok(ModLp)
    }

    async fn setup(&mut self, db: &mut Db) -> anyhow::Result<()> {
        db.add_guild_field("create_threads", "BOOLEAN NOT NULL DEFAULT(false)")?;
        db.add_guild_field("webhook", "STRING")?;
        db.add_guild_field("role_id", "STRING")?;
        Ok(())
    }

    fn register_commands(&self, store: &mut CommandStore, completions: &mut CompletionStore) {
        store.register::<Lp>();
        store.register::<SetRole>();
        store.register::<SetCreateThreads>();
        store.register::<SetWebhook>();
        store.register::<EditLp>();
        completions.push(ModLp::complete_lp);
    }
}
