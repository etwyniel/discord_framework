use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write;
use std::ops::Add;
use std::sync::Arc;

use crate::{ComponentCommandConst, ModalCommandConst, RegisterableModule};
use crate::{HandlerBuilder, Module, db::Db};
use anyhow::{Context as _, bail};
use chrono::{Duration, prelude::*};
use futures::FutureExt;
use futures::future::BoxFuture;
use itertools::Itertools;
use regex::Regex;
use reqwest::Url;
use serde::Deserialize;
use serde::Serialize;
use serenity::async_trait;
use serenity::prelude::Context;
use tokio::sync::RwLock;
use tokio::sync::mpsc::Receiver;

use serenity::all::{
    AutoArchiveDuration, AutocompleteChoice, Channel, ChannelType, CommandDataOption,
    CommandInteraction, CommandType, ComponentInteraction, CreateActionRow, CreateAllowedMentions,
    CreateAttachment, CreateAutocompleteResponse, CreateButton, CreateCommandOption,
    CreateComponent, CreateInputText, CreateInteractionResponse, CreateInteractionResponseFollowup,
    CreateInteractionResponseMessage, CreateLabel, CreateModal, CreateModalComponent,
    CreateSelectMenu, CreateSelectMenuKind, CreateThread, EditInteractionResponse, EditMessage,
    EditThread, ExecuteWebhook, GenericChannelId, GuildId, Http, InputTextStyle, InteractionId,
    Member, Message, MessageCommandInteractionMetadata, MessageId, MessageInteractionMetadata,
    ModalInteraction, Permissions, RoleId, UserId,
};

use crate::album::Album;
use crate::command_context::{
    InteractionExt, InteractionHandle, Responder, create_response_with_token, get_focused_option,
    get_str_opt_ac,
};
use crate::modules::{Bandcamp, Lastfm, Spotify};
use crate::prelude::*;
use serenity_command::{CommandKey, ContentAndFlags, component_command, modal_command};
use serenity_command::{CommandResponse, args, command};

use super::{AlbumLookup, Tidal};

const SEPARATOR: char = '\u{200B}';
const LP_URI: &str = "http://lp";

#[derive(Serialize, Deserialize, Debug)]
pub struct ResolvedLp {
    #[serde(rename = "rtitle")]
    pub resolved_title: Option<String>,
    #[serde(rename = "rlink")]
    pub resolved_link: Option<String>,
    #[serde(rename = "rstart")]
    pub resolved_start: Option<DateTime<Utc>>,
    #[serde(rename = "rdur")]
    pub resolved_duration_s: Option<i64>,
    #[serde(flatten)]
    pub params: Lp,
}

fn format_time(
    time_param: Option<&str>,
    resolved_start: Option<DateTime<Utc>>,
    duration: Option<Duration>,
) -> String {
    let Some(start) = resolved_start else {
        return time_param.unwrap_or("").to_string();
    };
    let end_str = format_end(start, duration);
    if Some("now") == time_param {
        return format!("now (<t:{}:t>{end_str})", start.timestamp());
    }
    // timestamp and relative time
    format!("<t:{0:}:R> (<t:{0:}:t>{end_str})", start.timestamp())
}

impl ResolvedLp {
    fn format_time(&self, duration: Option<Duration>) -> String {
        let time_param = self.params.time.as_deref();
        format_time(time_param, self.resolved_start, duration)
    }

    fn build_message_contents(
        &self,
        info: &Album,
        role_id: Option<u64>,
        desc: Option<&str>,
    ) -> anyhow::Result<String> {
        let when = self.format_time(info.duration);
        let hyperlinked = info.as_linked_header(self.resolved_title.as_deref());
        let mut resp_content = format!(
            "{}{SEPARATOR}\n{hyperlinked}\n{SEPARATOR}\n{when}\n",
            role_id // mention role if set
                .map(|id| format!("<@&{id}>"))
                .unwrap_or_else(|| "Listening party: ".to_string()),
        );
        let mut add_sep = false;
        if let Some(duration) = info.duration {
            add_sep = true;
            resp_content.push('*');
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
            resp_content.push('*');
        }
        if let Some(release_date) = &info.release_date {
            if add_sep {
                resp_content.push_str(" | ");
            }
            add_sep = true;
            _ = write!(&mut resp_content, "__*{release_date}*__");
        }
        if let Some(genres) = info.format_genres() {
            if add_sep {
                resp_content.push_str(" | ");
            }
            _ = write!(&mut resp_content, "{}", &genres);
        }
        if let Some(desc) = desc {
            resp_content.push_str("\n\n");
            resp_content.push(SEPARATOR);
            resp_content.push_str(desc);
            resp_content.push(SEPARATOR);
        }
        if !info.has_rich_embed {
            let track_info = info.format_tracks(Some(10));
            if !track_info.is_empty() {
                resp_content.push_str("\n\n");
                resp_content.push_str(track_info.trim());
            }
        }
        let encoded_data = serde_urlencoded::ser::to_string(self).unwrap();
        let mut encoded_data_url = Url::parse(LP_URI).unwrap();
        encoded_data_url.set_query(Some(&encoded_data));
        let data: String = encoded_data_url.into();
        _ = write!(&mut resp_content, "[̣]({data})");
        Ok(resp_content)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Lp {
    pub album: String,
    pub link: Option<String>,
    pub time: Option<String>,
    pub provider: Option<String>,
    pub role: Option<RoleId>,
}

fn format_end(start: DateTime<Utc>, duration: Option<Duration>) -> String {
    let Some(duration) = duration else {
        return String::new();
    };
    let end = start.add(duration);
    format!(" -> <t:{}:t>", end.timestamp())
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

fn build_message_contents(
    lp: Lp,
    lp_name: Option<&str>,
    info: &Album,
    role_id: Option<u64>,
    resolved_start: Option<DateTime<Utc>>,
    desc: Option<&str>,
) -> anyhow::Result<String> {
    let resolved = ResolvedLp {
        resolved_start,
        resolved_title: lp_name.map(|s| s.to_string()),
        resolved_link: info.url.clone(),
        resolved_duration_s: info.duration.map(|d| d.num_seconds()),
        params: lp,
    };
    resolved.build_message_contents(info, role_id, desc)
}

async fn find_album<'a>(
    handler: &Handler,
    album: &'a str,
    mut link: Option<&str>,
    provider: Option<&str>,
) -> anyhow::Result<(Option<&'a str>, Album)> {
    let mut lp_name = Some(album);
    if lp_name.map(|name| name.starts_with("https://")) == Some(true) {
        // As a special case for convenience, if we have a URL in lp_name, use that as link
        if link.is_some() && link != lp_name {
            lp_name = None;
        } else {
            link = lp_name.take();
        }
    }
    let lookup: &AlbumLookup = handler.module()?;
    // Depending on what we have, look up more information
    let info = match (lp_name, &link) {
        (Some(name), None) => lookup.lookup_album(name, provider).await?,
        (name, Some(lnk)) => {
            let mut info = lookup.get_album_info(lnk).await?;
            if let Some((info, name)) = info.as_mut().zip(name) {
                info.name = Some(name.to_string())
            };
            info
        }
        (None, None) => bail!("Please specify something to LP"),
    }
    .unwrap_or_else(|| Album {
        url: link.map(|s| s.to_string()),
        ..Default::default()
    });
    Ok((lp_name, info))
}

fn resolve_time(time: Option<&str>) -> Option<DateTime<Utc>> {
    let mut lp_time = Utc::now().add(Duration::seconds(10));
    let time = match time {
        Some("now") | None => {
            return Some(lp_time);
        }
        Some(t) => t,
    };
    let xx_re = Regex::new("(?i)^(XX:?)?([0-5][0-9])$").unwrap(); // e.g. XX:15, xx15 or 15
    let plus_re = Regex::new(r"\+?(([0-5])?[0-9])m?").unwrap(); // e.g. +25
    if let Some(cap) = xx_re.captures(time) {
        let min: i64 = cap
            .get(2)
            .unwrap()
            .as_str()
            .parse()
            .expect("regex match should be a valid integer");
        if !(0..60).contains(&min) {
            return None;
        }
        let cur_min = lp_time.minute() as i64;
        let to_add = if cur_min <= min {
            min - cur_min
        } else {
            (60 - cur_min) + min
        };
        lp_time = lp_time.add(Duration::minutes(to_add));
    } else if let Some(cap) = plus_re.captures(time) {
        let extra_mins: i64 = cap
            .get(1)
            .unwrap()
            .as_str()
            .parse()
            .expect("regex match should be a valid integer");
        lp_time = lp_time.add(Duration::minutes(extra_mins));
    } else {
        return None;
    }

    // timestamp and relative time
    Some(lp_time)
}

impl Lp {
    fn resolve_time(&self) -> Option<DateTime<Utc>> {
        resolve_time(self.time.as_deref())
    }

    async fn resolve(
        mut self,
        handler: &Handler,
        guild_id: GuildId,
    ) -> anyhow::Result<(ResolvedLp, Album)> {
        let Lp {
            album,
            link,
            provider,
            role,
            ..
        } = &self;
        let album = album.trim();
        let link = link.as_deref().map(str::trim);
        let (lp_name, mut info) = find_album(handler, album, link, provider.as_deref()).await?;
        let lp_name = if link.is_none() {
            info.name.clone()
        } else {
            lp_name.map(|s| s.to_string())
        };
        // get genres if needed
        if let Some(genres) = get_lastfm_genres(handler, &info).await {
            info.genres = genres
        }
        let role_id = if let Some(r) = role {
            *r
        } else {
            RoleId::new(
                handler
                    .get_guild_field(guild_id.get(), "role_id")
                    .await
                    .context("error retrieving LP role")?,
            )
        };
        self.role = Some(role_id);
        let resolved_start = self.resolve_time();
        let resolved = ResolvedLp {
            resolved_start,
            resolved_title: lp_name.map(|s| s.to_string()),
            resolved_link: info.url.clone(),
            resolved_duration_s: info.duration.map(|d| d.num_seconds()),
            params: self,
        };
        Ok((resolved, info))
    }

    async fn build_contents(
        self,
        handler: &Handler,
        command: impl InteractionExt,
        resolved_start: Option<DateTime<Utc>>,
        desc: Option<&str>,
    ) -> anyhow::Result<(String, Option<u64>, Album)> {
        let Lp {
            album,
            link,
            provider,
            role,
            ..
        } = &self;
        let album = album.trim();
        let link = link.as_deref().map(str::trim);
        let (lp_name, mut info) = find_album(handler, album, link, provider.as_deref()).await?;
        let lp_name = lp_name.map(|s| s.to_string());
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
        let resp_content = build_message_contents(
            self,
            lp_name.as_deref(),
            &info,
            role_id,
            resolved_start,
            desc,
        )?;
        Ok((resp_content, role_id, info))
    }

    pub async fn send(
        handler: &Handler,
        member: &Member,
        lp: &ResolvedLp,
        info: &Album,
        resp_content: &str,
        (interaction_id, interaction_token): InteractionHandle<'_>,
        is_followup: bool,
        channel_id: GenericChannelId,
    ) -> anyhow::Result<CommandResponse> {
        let http = handler.http.get().unwrap();
        let guild_id = member.guild_id;
        let webhook: Option<String> = handler.get_guild_field(guild_id.get(), "webhook").await?;
        let wh = match webhook.as_deref().map(|url| http.get_webhook_from_url(url)) {
            Some(fut) => Some(fut.await?),
            None => None,
        };
        let roles: Vec<_> = lp.params.role.clone().into_iter().collect();
        let message = if let Some(wh) = &wh {
            // Send LP message through webhook
            // This lets us impersonate the user who sent the command
            let avatar_url = member.avatar_url().or_else(|| member.user.avatar_url());
            let nick = member
                .nick
                .as_deref()
                .unwrap_or_else(|| member.user.display_name());
            wh.execute(http, true, {
                let mut webhook = ExecuteWebhook::new()
                    .content(resp_content)
                    .allowed_mentions(CreateAllowedMentions::new().roles(roles))
                    .username(nick);
                if let Some(url) = avatar_url.as_ref() {
                    webhook = webhook.avatar_url(url);
                }
                webhook
            })
            .await?
            .unwrap() // Message is present because we set wait to true in execute
        } else {
            // prefix response with pinger mention
            let resp = format!("<@{}>: {resp_content}", member.user.id.get());
            // Create interaction response
            let allowed_mentions = CreateAllowedMentions::new().roles(roles);
            if is_followup {
                let mut create_followup = CreateInteractionResponseFollowup::new()
                    .content(resp)
                    .allowed_mentions(allowed_mentions);
                if let Some(cover) = &info.cover
                    && !info.has_rich_embed
                {
                    let file = CreateAttachment::url(http, cover, "cover.jpg").await?;
                    create_followup = create_followup.add_file(file);
                }
                create_followup
                    .execute(http, None, interaction_token)
                    .await?
            } else {
                let mut create_msg = CreateInteractionResponseMessage::new()
                    .content(resp)
                    .allowed_mentions(allowed_mentions);
                if let Some(cover) = &info.cover
                    && !info.has_rich_embed
                {
                    let file = CreateAttachment::url(http, cover, "cover.jpg").await?;
                    create_msg = create_msg.add_file(file);
                }
                CreateInteractionResponse::Message(create_msg)
                    .execute(http, interaction_id, interaction_token)
                    .await?;
                http.get_original_interaction_response(interaction_token)
                    .await?
            }
        };
        let mod_lp: &ModLp = handler.module().unwrap();
        mod_lp
            .lp_messages
            .write()
            .await
            .insert(channel_id, (message.id, member.user.id));
        let mut response = format!(
            "LP created: {}",
            message.id.link(message.channel_id, Some(guild_id))
        );
        if handler
            .get_guild_field(guild_id.get(), "create_threads")
            .await?
        {
            // Create a thread from the response message for the LP to take place in
            let mut chan = message.channel(http).await?;
            let mut thread_name = info.name.as_deref().unwrap_or("Listening party");
            if thread_name.len() > 100 {
                thread_name = &thread_name[..100];
            }
            // let mut guild_chan = chan.guild().map(|c| (c.kind, c));
            if let (None, Channel::GuildThread(thread)) = (&webhook, &mut chan) {
                // If we're already in a thread, just rename it
                // unless we are using a webhook, in which case we can create a new thread
                thread
                    .edit(http, EditThread::new().name(thread_name))
                    .await?;
            } else if let Channel::Guild(channel) = &chan {
                // Create thread from response message
                let thread = channel
                    .id
                    .create_thread_from_message(
                        http,
                        message.id,
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
            let response = if wh.channel_id.map(|id| id.get()) == Some(channel_id.get()) {
                CommandResponse::Private(response.into())
            } else {
                CommandResponse::Public(response.into())
            };
            if is_followup {
                if let Some(ContentAndFlags(contents, embeds, _, flags)) =
                    response.to_contents_and_flags()
                {
                    CreateInteractionResponseFollowup::new()
                        .content(contents)
                        .add_embeds(embeds.into_iter().flatten())
                        .flags(flags)
                        .execute(http, None, interaction_token)
                        .await?;
                } else {
                    // FIXME
                }
            } else {
                create_response_with_token(http, response, None, interaction_id, interaction_token)
                    .await?;
            }
        }
        Ok(CommandResponse::None)
    }

    pub async fn run_lp(
        self,
        handler: &Handler,
        ctx: &Context,
        command: impl InteractionExt + Responder + Copy,
        desc: Option<&str>,
    ) -> anyhow::Result<CommandResponse> {
        let guild_id = command.guild_id()?;
        if let Some(role_id) = self.role {
            let role = ctx.http.get_guild_role(guild_id, role_id).await?;
            if !role.mentionable()
                && let Some(member) = command.member()
                && !member.permissions.unwrap_or_default().mention_everyone()
            {
                bail!("Only admins are allowed to ping <@&{role_id}>.");
            }
        }
        let (resolved, info) = self.resolve(handler, guild_id).await?;
        let resp_content =
            resolved.build_message_contents(&info, resolved.params.role.map(RoleId::get), desc)?;
        let member = guild_id.member(ctx, command.user().id).await?;
        Self::send(
            handler,
            &member,
            &resolved,
            &info,
            &resp_content,
            command.handle(),
            false,
            command.channel_id(),
        )
        .await?;
        Ok(CommandResponse::None)
    }
}

fn set_lp_options(name: &str, opt: CreateCommandOption<'static>) -> CreateCommandOption<'static> {
    if name == "provider" {
        opt.add_string_choice("spotify", "spotify")
            .add_string_choice("bandcamp", "bandcamp")
            .add_string_choice("tidal", "tidal")
    } else {
        opt
    }
}

args! {LP_ARGS =
    "What you will be listening to (e.g. band - album, spotify/bandcamp link)"
    album[autocomplete]: String,
     "(Optional) Link to the album/playlist (Spotify, Youtube, Bandcamp...)"
    link[autocomplete]: Option<String>,
     "Time at which the LP will take place (e.g. XX:20, +5)"
    time: Option<String>,
     "Where to look for album info (defaults to spotify)"
    provider: Option<String>,
     "Use a specific role instead of the default (admin-only)"
    role: Option<RoleId>,
}

const LP: CommandConst = CommandConst {
    description: "run a listening party",
    ..command!(/lp LP_ARGS(set_lp_options): lp_func)
};

async fn lp_func(
    (album, link, time, provider, role): LP_ARGS,
    handler: &Handler,
    ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let params = Lp {
        album,
        link,
        time,
        provider,
        role,
    };
    params.run_lp(handler, ctx, command, None).await
}

args!(SETCREATETHREADS_ARGS = create_threads: bool);

const SETCREATETHREADS: CommandConst = CommandConst {
    description: "Configure thread creation for listening parties",
    permissions: Permissions::MANAGE_THREADS,
    ..command!(/setcreatethreads SETCREATETHREADS_ARGS: set_create_threads)
};

async fn set_create_threads(
    (create_threads,): SETCREATETHREADS_ARGS,
    handler: &Handler,
    _ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let guild_id = command.guild_id()?.get();
    let db = handler.db.lock().await;
    db.set_guild_field(guild_id, "create_threads", create_threads)
        .context("updating 'create_threads' guild field")?;
    let resp = if create_threads {
        "Will create threads when setting up listening parties"
    } else {
        "Will not create threads when setting up listening parties"
    };
    CommandResponse::private(resp)
}

args!(SETROLE_ARGS = role: Option<RoleId>);

const SETROLE: CommandConst = CommandConst {
    description: "Set the role to ping for Listening Parties",
    permissions: Permissions::MANAGE_ROLES,
    ..command!(/setrole SETROLE_ARGS: setrole)
};

async fn setrole(
    (role,): SETROLE_ARGS,
    handler: &Handler,
    _ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let guild_id = command.guild_id()?.get();
    let role = role.as_ref().map(|r| r.get().to_string());
    let db = handler.db.lock().await;
    db.set_guild_field(guild_id, "role_id", &role)
        .context("updating 'role_id' guild field")?;
    let resp = if let Some(role_id) = role {
        format!("Set listening party role to <@&{role_id}>.")
    } else {
        "Unset listening party role.".to_string()
    };
    CommandResponse::private(resp)
}

args!(SETWEBHOOK_ARGS =
    webhook: Option<String>,
);

const SETWEBHOOK: CommandConst = CommandConst {
    description: "set a webhook to use when creating listening parties",
    permissions: Permissions::MANAGE_WEBHOOKS,
    ..command!(/setwebhook SETWEBHOOK_ARGS: setwebhook)
};

async fn setwebhook(
    (webhook,): SETWEBHOOK_ARGS,
    handler: &Handler,
    _ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let guild_id = command.guild_id()?.get();
    let db = handler.db.lock().await;
    db.set_guild_field(guild_id, "webhook", webhook.as_ref())
        .context("updating 'webhook' guild field")?;
    let resp = if webhook.is_some() {
        "Listening parties will be created using a webhook."
    } else {
        "Listening parties will not be created using a webhook."
    };
    CommandResponse::private(resp)
}

pub struct EditLp {
    album: Option<String>,
    time: Option<String>,
}

impl EditLp {
    async fn edit_from_embedded_data(
        &self,
        handler: &Handler,
        ctx: &Context,
        msg: &mut Message,
        command: &CommandInteraction,
        desc: Option<&str>,
    ) -> anyhow::Result<CommandResponse> {
        let guild_id = command.guild_id()?;
        let Some(pos) = msg.content.find(LP_URI) else {
            bail!("no embedded data");
        };
        let url: Url = msg.content[pos..]
            .trim_end_matches(')')
            .parse()
            .context("invalid embedded URL")?;
        let mut lp: ResolvedLp = serde_urlencoded::de::from_str(url.query().unwrap_or_default())
            .context("failed to deserialize embedded data")?;
        let mut changed = false;
        if let Some(album) = &self.album {
            lp.params.album = album.clone();
            lp.params.link = None;
            changed = true;
            // save resolved_start, may not have changed
            let resolved_start = lp.resolved_start;
            (lp, _) = lp.params.resolve(handler, guild_id).await?;
            lp.resolved_start = resolved_start;
        }
        let mut new_start_formatted = None;
        if let Some(time) = &self.time {
            lp.params.time = Some(time.clone());
            lp.resolved_start = lp.params.resolve_time();
            new_start_formatted =
                Some(lp.format_time(lp.resolved_duration_s.map(|d| Duration::seconds(d))));
            changed = true;
        }
        if !changed {
            return CommandResponse::private("Nothing to change");
        }
        let (contents, role_id, info) = lp
            .params
            .build_contents(handler, command, lp.resolved_start, desc)
            .await?;
        // prefix response with pinger mention
        let contents = format!("<@{}>: {contents}", command.user.id.get());
        msg.edit(
            &ctx.http,
            EditMessage::new().content(contents).allowed_mentions(
                CreateAllowedMentions::new()
                    .roles(role_id.map(|id| vec![RoleId::new(id)]).unwrap_or_default()),
            ),
        )
        .await?;
        // build response to indicate what was updated
        let mut resp = String::new();
        if self.album.is_some() {
            _ = writeln!(&mut resp, "Updated album to {}", info.as_link(None));
        }
        if let Some(when) = new_start_formatted {
            _ = writeln!(&mut resp, "Listening party will start {when}");
        }
        CommandResponse::public(resp)
    }
}

args!(EDIT_LP_ARGS =
    album[autocomplete]: Option<String>,
    time: Option<String>,
    cancel: Option<bool>,
);

const EDIT_LP: CommandConst = CommandConst {
    description: "Edit the last LP you created",
    ..command!(/edit_lp EDIT_LP_ARGS: edit_lp)
};

async fn edit_lp(
    (album, time, cancel): EDIT_LP_ARGS,
    handler: &Handler,
    ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let author_id = command.user.id;
    let mod_lp: &ModLp = handler.module().unwrap();
    let Some((message_id, user_id)) = mod_lp
        .lp_messages
        .read()
        .await
        .get(&command.channel_id)
        .copied()
    else {
        return CommandResponse::private("No recent listening party to edit.");
    };
    let mut msg = ctx.http.get_message(command.channel_id, message_id).await?;
    if user_id != author_id
        && let Some(member) = &command.member
        && !member
            .permissions
            .map(|p| p.manage_events())
            .unwrap_or_default()
    {
        return CommandResponse::private("Cannot edit listening party");
    }
    if cancel == Some(true) {
        msg.edit(
            &ctx.http,
            EditMessage::new().content(format!("~~{}~~", &msg.content)),
        )
        .await?;
        return CommandResponse::public("Canceled listening party");
    }
    let desc = msg.content.split(SEPARATOR).nth(3).map(str::to_string);
    let edit_lp = EditLp { album, time };
    match edit_lp
        .edit_from_embedded_data(handler, ctx, &mut msg, command, desc.as_deref())
        .await
    {
        Ok(resp) => return Ok(resp),
        Err(e) => eprintln!("Could not edit LP from embedded data: {e:?}"),
    }
    let mut new_content = Cow::<'_, str>::Borrowed(&msg.content);
    let mut resp = String::new();
    if let Some(album) = &edit_lp.album {
        let (lp_name, info) = find_album(handler, album, None, None).await?;
        let hyperlinked = info.as_link(lp_name);
        new_content = Cow::Owned(
            new_content
                .splitn(3, SEPARATOR)
                .enumerate()
                .map(|(i, s)| if i != 1 { s } else { &hyperlinked })
                .join(&SEPARATOR.to_string()),
        );
        _ = writeln!(&mut resp, "Listening party album updated to {hyperlinked}");
    }
    if let time @ Some(_) = edit_lp.time.as_deref() {
        let resolved = resolve_time(time);
        let formatted = format_time(time, resolved, None);
        let re = Regex::new(r"(now|at <t:\d+:t>) \(.*\)").unwrap();
        new_content = Cow::Owned(re.replace(&new_content, &formatted).to_string());
        _ = writeln!(&mut resp, "Listening party will start {formatted}");
    }
    if resp.is_empty() {
        return CommandResponse::private("Nothing to change");
    }
    msg.edit(
        &ctx.http,
        EditMessage::new().content(new_content.into_owned()),
    )
    .await?;
    CommandResponse::public(resp)
}

const CREATE_LP: CommandConst = CommandConst {
    description: "Create a Listening Party",
    ..command!(/create_listening_party: create_lp)
};

async fn wait_for_create_lp_submit(
    handler: Arc<Handler>,
    http: Arc<Http>,
    mut receiver: Receiver<LpCreationEvent>,
    (interaction_id, interaction_token): (InteractionId, String),
    member: Member,
    guild_id: GuildId,
    channel_id: GenericChannelId,
) {
    // await initial configuration from the modal submission
    let (modal_id, modal_token, album, link, description, time) = loop {
        match receiver.recv().await {
            None => return,
            Some(LpCreationEvent::Initial {
                modal_id,
                modal_token,
                album,
                link,
                description,
                time,
            }) => break (modal_id, modal_token, album, link, description, time),
            _ => continue, // should be unreachable but handle to avoid breakage
        }
    };

    // build params, resolve starting time, album info, role, genres...
    let lp = Lp {
        album,
        link,
        time,
        provider: None,
        role: None,
    };
    let Ok((resolved, info)) = lp.resolve(&handler, guild_id).await else {
        return;
    };

    // build initial message contents
    let contents = match resolved.build_message_contents(
        &info,
        resolved.params.role.map(RoleId::get),
        description.as_deref(),
    ) {
        Ok(contents) => contents,
        Err(e) => {
            let err_msg = format!("Failed to create listening party: {e:?}");
            eprintln!("{err_msg}");
            // attempt to return error to user, ignore failure
            _ = CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(err_msg)
                    .ephemeral(true),
            )
            .execute(&http, modal_id, &modal_token)
            .await;
            return;
        }
    };

    // build components to add to the LP creation message
    let message_components =
        listening_party_creator_components(Some(&member), resolved.params.role.clone());
    // create ephemeral editable message
    let res = CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .ephemeral(true)
            .content(format!("# PREVIEW\n{contents}"))
            .components(message_components),
    )
    .execute(&http, modal_id, &modal_token)
    .await;
    if let Err(e) = res {
        eprintln!("failed to create ephemeral LP creation message: {e:?}");
        return;
    }

    // start event routine
    let mut creator = LpCreator {
        id: interaction_id,
        token: interaction_token,
        channel_id: channel_id,
        guild_id,
        member,
        modal_token,
        lp: resolved,
        info,
        desc: description,
    };
    creator.run(&handler, &http, receiver).await;
}

async fn create_lp(
    handler: &Handler,
    ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    // require guild ID
    let guild_id = command.guild_id()?;

    // create modal fields
    let album_field = CreateLabel::input_text(
        "Album",
        CreateInputText::new(InputTextStyle::Short, "album").required(true),
    )
    .description("Album link, listening party title, or album search query");
    let link_field = CreateLabel::input_text(
        "Link",
        CreateInputText::new(InputTextStyle::Short, "link").required(false),
    )
    .description("Optional");
    let time_field = CreateLabel::input_text(
        "Time",
        CreateInputText::new(InputTextStyle::Short, "time")
            .required(false)
            .placeholder("+5"),
    )
    .description("Listening Party time (e.g. +5, XX:20)");
    let description_field = CreateLabel::input_text(
        "Description",
        CreateInputText::new(InputTextStyle::Paragraph, "description").required(false),
    );
    let fields = vec![
        CreateModalComponent::Label(album_field),
        CreateModalComponent::Label(link_field),
        CreateModalComponent::Label(description_field),
        CreateModalComponent::Label(time_field),
    ];

    // create channel for events, store sender in module
    let mod_lp: Arc<ModLp> = handler.module_arc()?;
    let receiver = {
        let (sender, receiver) = tokio::sync::mpsc::channel::<LpCreationEvent>(5);
        mod_lp
            .lp_creation_events
            .write()
            .await
            .insert(command.id, sender);
        receiver
    };

    // create modal
    command
        .create_response(
            &ctx.http,
            CreateInteractionResponse::Modal(
                CreateModal::new(
                    format!("create_lp.{}", command.id),
                    "Start a Listening Party",
                )
                .components(fields),
            ),
        )
        .await?;

    // get handles to values required for the event listening routine
    let handler = handler.me.upgrade().unwrap();
    let http = Arc::clone(&ctx.http);
    let interaction_id = command.id;
    let handle = (command.id, command.token.to_string());
    let member = command
        .member
        .as_deref()
        .map(|m| m.clone())
        .context("expected member")?;
    let channel_id = command.channel_id;

    // spawn event handling routine, with a timeout
    tokio::spawn(async move {
        _ = tokio::time::timeout(
            std::time::Duration::from_mins(10),
            wait_for_create_lp_submit(
                Arc::clone(&handler),
                http,
                receiver,
                handle,
                member,
                guild_id,
                channel_id,
            ),
        )
        .await;
        // remove sender from module
        mod_lp
            .lp_creation_events
            .write()
            .await
            .remove(&interaction_id);
    });
    Ok(CommandResponse::None)
}

fn listening_party_creator_components(
    member: Option<&Member>,
    role_id: Option<RoleId>,
) -> Vec<CreateComponent<'static>> {
    let mut message_components = vec![];

    if let Some(member) = member
        && member
            .permissions
            .unwrap_or_default()
            .contains(Permissions::MENTION_EVERYONE)
    {
        let role_field = CreateSelectMenu::new(
            "select_listening_party_role",
            CreateSelectMenuKind::Role {
                default_roles: role_id.map(|role| Cow::Owned(vec![role])),
            },
        )
        .max_values(1);
        message_components.push(CreateComponent::ActionRow(CreateActionRow::SelectMenu(
            role_field,
        )));
    }
    message_components.push(CreateComponent::ActionRow(CreateActionRow::buttons(vec![
        CreateButton::new("button_edit_lp").emoji('✏').label("Edit"),
        CreateButton::new("button_send_lp").emoji('▶').label("Send"),
    ])));
    message_components
}

enum LpCreationEvent {
    Initial {
        modal_id: InteractionId,
        modal_token: String,
        album: String,
        link: Option<String>,
        description: Option<String>,
        time: Option<String>,
    },
    ChangeRole(RoleId),
    Edit {
        album: String,
        link: Option<String>,
        description: Option<String>,
        time: Option<String>,
    },
    Send,
}

struct LpCreator {
    id: InteractionId,
    token: String,
    channel_id: GenericChannelId,
    modal_token: String,
    guild_id: GuildId,
    lp: ResolvedLp,
    info: Album,
    desc: Option<String>,
    member: Member,
}

impl LpCreator {
    fn build_contents(&mut self) -> anyhow::Result<String> {
        self.lp.build_message_contents(
            &self.info,
            self.lp.params.role.map(RoleId::get),
            self.desc.as_deref(),
        )
    }

    async fn edit_preview(&mut self, http: &Http) {
        let contents = self.build_contents().unwrap();
        let resp = EditInteractionResponse::new()
            .content(format!("# PREVIEW\n{contents}"))
            .execute(http, &self.modal_token)
            .await;
        if let Err(e) = resp {
            eprintln!("failed to edit ephemeral LP creation message: {e:?}");
        }
    }

    async fn send(&mut self, handler: &Handler, http: &Http) -> bool {
        let contents = self.build_contents().unwrap();
        // send listening party as specified by configuration in DB
        let res = Lp::send(
            handler,
            &self.member,
            &self.lp,
            &self.info,
            &contents,
            (self.id, &self.token),
            true,
            self.channel_id,
        )
        .await;
        if let Err(e) = res {
            eprintln!("failed to send LP: {e:?}");
            // attempt to send a new ephemeral response to notify user
            _ = CreateInteractionResponseFollowup::new()
                .content(format!("failed to send listening party: {e:?}"))
                .ephemeral(true)
                .execute(http, None, &self.modal_token)
                .await;
            _ = EditInteractionResponse::new()
                .content(format!("failed to send listening party: {e:?}"))
                .execute(http, &self.modal_token)
                .await;
            return false;
        }
        _ = http
            .delete_original_interaction_response(&self.modal_token)
            .await;
        return true;
    }

    async fn run(
        &mut self,
        handler: &Handler,
        http: &Http,
        mut receiver: Receiver<LpCreationEvent>,
    ) {
        use LpCreationEvent::*;
        while let Some(evt) = receiver.recv().await {
            match evt {
                Initial { .. } => continue, // should not happen, ignore and carry on
                Send => {
                    // done editing, send final version
                    if self.send(handler, http).await {
                        // send succeeded, done
                        return;
                    }
                    // send failed, stay in loop to allow retrying
                }
                ChangeRole(new_role) => {
                    self.lp.params.role = Some(new_role);
                    self.edit_preview(http).await;
                }
                Edit {
                    album: title,
                    link,
                    description,
                    time,
                } => {
                    let params = &mut self.lp.params;
                    if params.album != title || params.link != link {
                        // changed which album is being listened to, resolve album info again
                        let lp = Lp {
                            album: title,
                            link,
                            time,
                            role: params.role,
                            provider: params.provider.clone(),
                        };
                        let Ok((resolved, info)) = lp.resolve(handler, self.guild_id).await else {
                            continue;
                        };
                        self.lp = resolved;
                        self.info = info;
                    } else {
                        if params.time != time {
                            self.lp.resolved_start = None;
                        }
                        params.time = time;
                    }
                    self.desc = description;
                    self.edit_preview(http).await;
                }
            }
        }
    }
}

args!(SUBMIT_CREATE_LP_ARGS =
    album: String,
    link: Option<String>,
    description: Option<String>,
    time: Option<String>,
);

const SUBMIT_CREATE_LP: ModalCommandConst =
    modal_command!(create_lp SUBMIT_CREATE_LP_ARGS: submit_create_lp);

fn get_interaction_id(custom_id: &str) -> anyhow::Result<InteractionId> {
    custom_id
        .split('.')
        .nth(1)
        .and_then(|s| s.parse::<u64>().ok())
        .map(InteractionId::new)
        .context("Cannot find interaction ID")
}

// handle submit of LP creation modal
async fn submit_create_lp(
    (album, link, description, time): SUBMIT_CREATE_LP_ARGS,
    handler: &Handler,
    _ctx: &Context,
    modal: &ModalInteraction,
) -> anyhow::Result<CommandResponse> {
    // retrieve ID of the original command interaction
    let original_interaction_id = get_interaction_id(&modal.data.custom_id)?;
    let mod_lp: &ModLp = handler.module()?;

    let evt = LpCreationEvent::Initial {
        modal_id: modal.id,
        modal_token: modal.token.to_string(),
        album,
        link,
        description,
        time,
    };
    mod_lp.dispatch_event(original_interaction_id, evt).await?;
    Ok(CommandResponse::None)
}

const BUTTON_SEND_LP: ComponentCommandConst = component_command!(button_send_lp: button_send_lp);

// handle start button for LP creator
async fn button_send_lp(
    handler: &Handler,
    _ctx: &Context,
    component: &ComponentInteraction,
) -> anyhow::Result<CommandResponse> {
    // retrieve ID of the original command interaction
    let msg = &component.message;
    let Some(MessageInteractionMetadata::ModalSubmit(modal_data)) =
        msg.interaction_metadata.as_deref()
    else {
        return CommandResponse::ACK;
    };
    let MessageInteractionMetadata::Command(MessageCommandInteractionMetadata {
        id: interaction_id,
        ..
    }) = modal_data.triggering_interaction_metadata.as_ref()
    else {
        return CommandResponse::ACK;
    };

    // use interaction ID to find sender to LP creator routine
    let mod_lp: &ModLp = handler.module()?;
    mod_lp
        .dispatch_event(*interaction_id, LpCreationEvent::Send)
        .await?;
    CommandResponse::ACK
}

args!(SELECT_LP_ROLE_ARGS = role: RoleId);

const CHANGE_LP_ROLE: ComponentCommandConst =
    component_command!(select_listening_party_role SELECT_LP_ROLE_ARGS: select_lp_role);

// handle role selection message component for LP creation
async fn select_lp_role(
    (role,): SELECT_LP_ROLE_ARGS,
    handler: &Handler,
    _ctx: &Context,
    component: &ComponentInteraction,
) -> anyhow::Result<CommandResponse> {
    // retrieve ID of the original command interaction
    let msg = &component.message;
    let Some(MessageInteractionMetadata::ModalSubmit(modal_data)) =
        msg.interaction_metadata.as_deref()
    else {
        return CommandResponse::ACK;
    };
    let MessageInteractionMetadata::Command(MessageCommandInteractionMetadata {
        id: interaction_id,
        ..
    }) = modal_data.triggering_interaction_metadata.as_ref()
    else {
        return CommandResponse::ACK;
    };

    // use interaction ID to find sender to LP creator routine
    let mod_lp: &ModLp = handler.module()?;
    if let Some(sender) = mod_lp.lp_creation_events.read().await.get(interaction_id) {
        sender.send(LpCreationEvent::ChangeRole(role)).await?;
    } else {
        bail!("Interaction not found");
    }
    CommandResponse::ACK
}

const BUTTON_EDIT_LP: ComponentCommandConst = component_command!(button_edit_lp: button_edit_lp);

async fn button_edit_lp(
    _handler: &Handler,
    ctx: &Context,
    component: &ComponentInteraction,
) -> anyhow::Result<CommandResponse> {
    // retrieve ID of the original command interaction
    let msg = &component.message;
    let Some(MessageInteractionMetadata::ModalSubmit(modal_data)) =
        msg.interaction_metadata.as_deref()
    else {
        return CommandResponse::ACK;
    };
    let MessageInteractionMetadata::Command(MessageCommandInteractionMetadata {
        id: interaction_id,
        ..
    }) = modal_data.triggering_interaction_metadata.as_ref()
    else {
        return CommandResponse::ACK;
    };

    // extract information about current LP configuration from message
    let Some(pos) = msg.content.find(LP_URI) else {
        return CommandResponse::private("no embedded data");
    };
    let url: Url = msg.content[pos..]
        .trim_end_matches(')')
        .parse()
        .context("invalid embedded URL")?;
    let lp: ResolvedLp = serde_urlencoded::de::from_str(url.query().unwrap_or_default())
        .context("failed to deserialize embedded data")?;

    // create modal fields, pre-filled with current values
    let mut album_input = CreateInputText::new(InputTextStyle::Short, "album").required(true);
    if let Some(title) = &lp.resolved_title {
        album_input = album_input.value(title);
    }
    let album_field = CreateLabel::input_text("Album", album_input)
        .description("Album link, listening party title, or album search query");

    let mut link_input = CreateInputText::new(InputTextStyle::Short, "link").required(false);
    if let Some(link) = &lp.resolved_link {
        link_input = link_input.value(link);
    }
    let link_field = CreateLabel::input_text("Link", link_input).description("Optional");

    let mut time_input = CreateInputText::new(InputTextStyle::Short, "time")
        .required(false)
        .placeholder("+5");
    if let Some(time) = &lp.resolved_start {
        let minute = time.minute();
        time_input = time_input.value(format!("XX:{minute:02}"));
    }
    let time_field = CreateLabel::input_text("Time", time_input)
        .description("Listening Party time (e.g. +5, XX:20)");

    // extract description if any
    let desc = msg.content.split(SEPARATOR).nth(3);
    let mut description_input =
        CreateInputText::new(InputTextStyle::Paragraph, "description").required(false);
    if let Some(desc) = desc {
        description_input = description_input.value(desc);
    }
    let description_field = CreateLabel::input_text("Description", description_input);
    let fields = vec![
        CreateModalComponent::Label(album_field),
        CreateModalComponent::Label(link_field),
        CreateModalComponent::Label(description_field),
        CreateModalComponent::Label(time_field),
    ];
    component
        .create_response(
            &ctx.http,
            CreateInteractionResponse::Modal(
                CreateModal::new(
                    format!("edit_lp_creator.{interaction_id}"),
                    "Edit Listening Party",
                )
                .components(fields),
            ),
        )
        .await?;
    Ok(CommandResponse::None)
}

// use the same arguments as the create LP modal
const SUBMIT_EDIT_LP: ModalCommandConst =
    modal_command!(edit_lp_creator SUBMIT_CREATE_LP_ARGS: submit_edit_lp);

// handle submit from edition modal for LP creator
async fn submit_edit_lp(
    (album, link, description, time): SUBMIT_CREATE_LP_ARGS,
    handler: &Handler,
    _ctx: &Context,
    modal: &ModalInteraction,
) -> anyhow::Result<CommandResponse> {
    // extract original interaction ID, stored in modal custom_id
    let original_interaction_id = get_interaction_id(&modal.data.custom_id)?;

    // use interaction ID to find sender to LP creator routine
    let evt = LpCreationEvent::Edit {
        album,
        link,
        description,
        time,
    };
    let mod_lp: &ModLp = handler.module()?;
    mod_lp.dispatch_event(original_interaction_id, evt).await?;
    CommandResponse::ACK
}

const EDIT_LISTENING_PARTY: CommandConst = CommandConst {
    description: "Edit a Listening Party",
    ..command!(/edit_listening_party: edit_listening_party)
};

async fn edit_listening_party(
    handler: &Handler,
    ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let author_id = command.user.id;
    // find last LP pinged in that channel
    let mod_lp: &ModLp = handler.module().unwrap();
    let Some((message_id, user_id)) = mod_lp
        .lp_messages
        .read()
        .await
        .get(&command.channel_id)
        .copied()
    else {
        return CommandResponse::private("No recent listening party to edit.");
    };

    // check that user can edit this LP
    if user_id != author_id
        && let Some(member) = &command.member
        && !member
            .permissions
            .map(|p| p.manage_events())
            .unwrap_or_default()
    {
        return CommandResponse::private("Cannot edit listening party");
    }

    // load full LP message
    let msg = ctx.http.get_message(command.channel_id, message_id).await?;
    // find embedded parameters
    let Some(pos) = msg.content.find(LP_URI) else {
        return CommandResponse::private("no embedded data");
    };
    let url: Url = msg.content[pos..]
        .trim_end_matches(')')
        .parse()
        .context("invalid embedded URL")?;
    let lp: ResolvedLp = serde_urlencoded::de::from_str(url.query().unwrap_or_default())
        .context("failed to deserialize embedded data")?;

    // create edition modal
    let album_input_id = format!("album");
    let mut album_input =
        CreateInputText::new(InputTextStyle::Short, album_input_id).required(true);
    if let Some(title) = &lp.resolved_title {
        album_input = album_input.value(title);
    }
    let album_field = CreateLabel::input_text("Album", album_input)
        .description("Album link, listening party title, or album search query");

    let link_input_id = format!("link");
    let mut link_input = CreateInputText::new(InputTextStyle::Short, link_input_id).required(false);
    if let Some(link) = &lp.resolved_link {
        link_input = link_input.value(link);
    }
    let link_field = CreateLabel::input_text("Link", link_input).description("Optional");

    let time_input_id = format!("time");
    let mut time_input = CreateInputText::new(InputTextStyle::Short, time_input_id)
        .required(false)
        .placeholder("+5");
    if let Some(time) = &lp.resolved_start {
        let minute = time.minute();
        time_input = time_input.value(format!("XX:{minute:02}"));
    }
    let time_field = CreateLabel::input_text("Time", time_input)
        .description("Listening Party time (e.g. +5, XX:20)");

    let desc = msg.content.split(SEPARATOR).nth(3);
    let desc_input_id = format!("desc");
    let mut description_input =
        CreateInputText::new(InputTextStyle::Paragraph, desc_input_id).required(false);
    if let Some(desc) = desc {
        description_input = description_input.value(desc);
    }
    let description_field = CreateLabel::input_text("Description", description_input);

    let fields = vec![
        CreateModalComponent::Label(album_field),
        CreateModalComponent::Label(link_field),
        CreateModalComponent::Label(description_field),
        CreateModalComponent::Label(time_field),
    ];

    command
        .create_response(
            &ctx.http,
            CreateInteractionResponse::Modal(
                CreateModal::new(format!("create_lp.{}", command.id), "Edit Listening Party")
                    .components(fields),
            ),
        )
        .await?;
    Ok(CommandResponse::None)
}

#[derive(Default)]
pub struct ModLp {
    lp_messages: RwLock<HashMap<GenericChannelId, (MessageId, UserId)>>,
    lp_creation_events: RwLock<HashMap<InteractionId, tokio::sync::mpsc::Sender<LpCreationEvent>>>,
}

impl ModLp {
    // dispatch LP creation event using the initial interaction ID
    async fn dispatch_event(
        &self,
        interaction: InteractionId,
        evt: LpCreationEvent,
    ) -> anyhow::Result<()> {
        if let Some(sender) = self.lp_creation_events.read().await.get(&interaction) {
            sender.send(evt).await?;
        } else {
            bail!("Interaction not found");
        }
        Ok(())
    }

    async fn autocomplete_lp(
        handler: &Handler,
        options: &[CommandDataOption],
    ) -> anyhow::Result<Vec<(String, String)>> {
        let mut choices = vec![];
        let mut provider = get_str_opt_ac(options, "provider");
        let focused = get_focused_option(options);
        let mut album = get_str_opt_ac(options, "album");
        if let (Some(s), Some("album")) = (&mut album, focused) {
            if s.len() >= 7 && !s.starts_with("https://") {
                // if url, don't complete
                if let (None, Some(stripped)) = (&provider, s.strip_prefix("bc:")) {
                    // as a shorthand, search bandcamp for values with the prefix "bc:"
                    *s = stripped;
                    provider = Some("bandcamp");
                }
                choices = match handler
                    .module::<AlbumLookup>()?
                    .query_albums(s, provider)
                    .await
                {
                    Ok(c) => c,
                    Err(e) => {
                        dbg!(e);
                        vec![]
                    }
                };
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
            let ("lp" | "edit_lp", CommandType::ChatInput) = key else {
                return Ok(false);
            };
            let choices = Self::autocomplete_lp(handler, &ac.data.options).await?;
            let resp = choices
                .into_iter()
                .filter(|(_, value)| value.len() < 100)
                .fold(CreateAutocompleteResponse::new(), |resp, (name, value)| {
                    resp.add_choice(AutocompleteChoice::new(name, value))
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
    async fn setup(&mut self, db: &mut Db) -> anyhow::Result<()> {
        db.add_guild_field("create_threads", "BOOLEAN NOT NULL DEFAULT(false)")?;
        db.add_guild_field("webhook", "STRING")?;
        db.add_guild_field("role_id", "STRING")?;
        Ok(())
    }

    fn register_commands(&self, store: &mut dyn Storer) {
        store.register(LP);
        store.register(SETROLE);
        store.register(SETCREATETHREADS);
        store.register(SETWEBHOOK);
        store.register(EDIT_LP);
        store.register(CREATE_LP);
        store.register(BUTTON_EDIT_LP);
        store.register(SUBMIT_EDIT_LP);
        store.register(EDIT_LISTENING_PARTY);
        store.register(SUBMIT_CREATE_LP);
        store.register(CHANGE_LP_ROLE);
        store.register(BUTTON_SEND_LP);
        store.register(ModLp::complete_lp as CompletionHandler);
    }
}

impl RegisterableModule for ModLp {
    async fn add_dependencies(builder: HandlerBuilder) -> anyhow::Result<HandlerBuilder> {
        builder
            .module::<Lastfm>()
            .await?
            .module::<Spotify>()
            .await?
            .module::<Bandcamp>()
            .await?
            .module::<Tidal>()
            .await?
            .module::<AlbumLookup>()
            .await
    }

    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Ok(ModLp::default())
    }
}
