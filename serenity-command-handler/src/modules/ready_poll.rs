use std::collections::VecDeque;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context as _};
use itertools::Itertools;
use serenity::http::Http;
use serenity::model::id::InteractionId;
use serenity::model::prelude::interaction::application_command::ApplicationCommandInteraction;
use serenity::model::prelude::{ChannelId, Message, Reaction, ReactionType, UserId};
use serenity::{async_trait, prelude::Context};
use serenity_command::{BotCommand, CommandBuilder, CommandResponse};
use serenity_command_derive::Command;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::sync::RwLock;
use tokio::time::timeout;

use crate::{CommandStore, CompletionStore, Handler, Module, ModuleMap};

const YES: &str = "<:FeelsGoodCrab:988509541069127780>";
const NO: &str = "<:FeelsBadCrab:988508541499342918>";
const START: &str = "<a:CrabRave:988508208240922635>";
const COUNT: &str = "🦀";
const GO: &str = "<a:CrabRave:988508208240922635>";

const MAX_POLLS: usize = 20;

type PendingPoll = (InteractionId, Option<String>, Option<String>, Vec<String>);

pub type PendingPolls = VecDeque<PendingPoll>;

#[derive(Command, Debug)]
#[cmd(name = "ready_poll", desc = "Poll to start a listening party")]
pub struct ReadyPoll {
    #[cmd(desc = "Count emote")]
    pub count_emote: Option<String>,
    #[cmd(desc = "Emote Go")]
    pub go_emote: Option<String>,
}

impl ReadyPoll {
    async fn create_poll(
        self,
        handler: &Handler,
        ctx: &Context,
        interaction: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let pending_poll = (interaction.id, self.count_emote, self.go_emote, Vec::new());
        let module: &ModPoll = handler.module()?;
        let http = &ctx.http;
        interaction
            .create_interaction_response(http, |msg| {
                msg.interaction_response_data(|data| {
                    data.content("Ready?")
                        .allowed_mentions(|mentions| mentions.empty_users())
                })
            })
            .await
            .context("error creating response")?;
        let resp = interaction.get_interaction_response(http).await?;
        let (sender, receiver) = channel(16);
        {
            let mut polls = module.ready_polls.write().await;
            while polls.len() >= MAX_POLLS {
                polls.pop_back();
            }
            polls.push_front((interaction.id, sender));
        }
        resp.react(http, ReactionType::from_str(&module.yes)?)
            .await
            .context("error adding yes react")?;
        resp.react(http, ReactionType::from_str(&module.no)?)
            .await
            .context("error adding no react")?;
        resp.react(http, ReactionType::from_str(&module.start)?)
            .await
            .context("error adding go react")?;
        let http_arc = Arc::clone(&ctx.http);
        tokio::spawn(poll_task(
            handler.module_arc().unwrap(),
            http_arc,
            resp,
            pending_poll,
            receiver,
        ));
        Ok(CommandResponse::None)
    }
}

fn build_message(users: &[UserId]) -> String {
    let mut msg = "Ready?".to_string();
    if users.is_empty() {
        return msg;
    }
    msg.push_str(" (");
    msg.push_str(&users.iter().map(|UserId(u)| format!("<@{u}>")).join(", "));
    if users.len() == 1 {
        msg.push_str(" is");
    } else {
        msg.push_str(" are");
    }
    msg.push_str(" ready)");
    msg
}

#[async_trait]
impl BotCommand for ReadyPoll {
    type Data = Handler;

    async fn run(
        self,
        handler: &Handler,
        ctx: &Context,
        interaction: &ApplicationCommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let resp = match self.create_poll(handler, ctx, interaction).await {
            Ok(CommandResponse::Public(s) | CommandResponse::Private(s)) => Some(s),
            Err(e) => {
                dbg!(&e);
                Some(e.to_string())
            }
            _ => None,
        };
        if let Some(resp) = resp {
            interaction
                .edit_original_interaction_response(&ctx.http, |msg| {
                    msg.content(resp).allowed_mentions(|m| m.empty_users())
                })
                .await?;
        }
        Ok(CommandResponse::None)
    }
}

enum PollEvent {
    AddReady(UserId),
    RemoveReady(UserId),
    Go,
}

async fn poll_task(
    module: Arc<ModPoll>,
    http: Arc<Http>,
    mut msg: Message,
    poll: PendingPoll,
    mut r: Receiver<PollEvent>,
) {
    let mut users = Vec::new();
    let mut changed = false;
    let mut started = false;
    let mut last_event = Instant::now();
    loop {
        while let Ok(evt) = timeout(Duration::from_millis(500), r.recv()).await {
            let Some(evt) = evt else {
                // channel closed
                return;
            };
            last_event = Instant::now();
            changed = true;
            match evt {
                PollEvent::AddReady(user) => {
                    if !users.contains(&user) {
                        users.push(user)
                    }
                }
                PollEvent::RemoveReady(user) => {
                    if let Some(ndx) = users.iter().position(|&u| u == user) {
                        users.remove(ndx);
                    }
                }
                PollEvent::Go => {
                    if started {
                        continue;
                    }
                    started = true;
                    let res = crabdown(
                        Arc::clone(&module),
                        http.as_ref(),
                        msg.channel_id,
                        poll.1.as_deref(),
                        poll.2.as_deref(),
                    )
                    .await;
                    if let Err(e) = res {
                        eprintln!("error executing crabdown: {e}");
                    }
                }
            }
        }
        if last_event.elapsed() >= Duration::from_secs(900) {
            return;
        }
        if !changed {
            continue;
        }
        let content = build_message(&users);
        let res = msg
            .edit(http.as_ref(), |msg| {
                msg.content(content).allowed_mentions(|m| m.empty_users())
            })
            .await;
        if let Err(e) = res {
            eprintln!("failed to edit ready message: {e}");
        }
        changed = false;
    }
}

pub async fn crabdown(
    module: Arc<ModPoll>,
    http: &Http,
    channel: ChannelId,
    count_emote: Option<&str>,
    go_emote: Option<&str>,
) -> anyhow::Result<()> {
    channel.say(http, "Starting 3s countdown").await?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.tick().await;
    // let module: &ModPoll = handler.module()?;
    let count_emote = count_emote.unwrap_or(&module.count);
    let go_emote = go_emote.unwrap_or(&module.go);
    for i in 0..3 {
        let contents = std::iter::repeat(count_emote).take(3 - i).join(" ");
        channel
            .send_message(http, |msg| msg.content(contents))
            .await?;
        interval.tick().await;
    }
    channel
        .send_message(http, |msg| msg.content(go_emote))
        .await?;
    Ok(())
}

type PollSenders = VecDeque<(InteractionId, Sender<PollEvent>)>;

pub struct ModPoll {
    pub yes: String,
    pub no: String,
    pub start: String,
    pub count: String,
    pub go: String,
    ready_polls: Arc<RwLock<PollSenders>>,
}

impl ModPoll {
    pub fn new<
        'a,
        S1: Into<Option<&'a str>>,
        S2: Into<Option<&'a str>>,
        S3: Into<Option<&'a str>>,
        S4: Into<Option<&'a str>>,
        S5: Into<Option<&'a str>>,
    >(
        yes: S1,
        no: S2,
        start: S3,
        count: S4,
        go: S5,
    ) -> Self {
        ModPoll {
            yes: yes.into().unwrap_or(YES).to_string(),
            no: no.into().unwrap_or(NO).to_string(),
            start: start.into().unwrap_or(START).to_string(),
            count: count.into().unwrap_or(COUNT).to_string(),
            go: go.into().unwrap_or(GO).to_string(),
            ready_polls: Default::default(),
        }
    }

    pub async fn handle_remove_react(
        handler: &Handler,
        ctx: &Context,
        react: &Reaction,
    ) -> anyhow::Result<()> {
        let msg = react.message(&ctx.http).await?;
        if Some(&msg.author.id) != handler.self_id.get() {
            return Ok(());
        }
        let (interaction_id, _) = match msg.interaction.as_ref() {
            Some(interaction) if interaction.name == ReadyPoll::NAME => {
                (interaction.id, interaction.user.id)
            }
            _ => return Ok(()),
        };
        let module: &ModPoll = handler.module()?;
        if react.emoji.to_string() != module.yes {
            return Ok(());
        }
        let user_id = react
            .user_id
            .ok_or_else(|| anyhow!("invalid react: missing userId"))?;
        let polls = module.ready_polls.read().await;
        if let Some((_, sender)) = polls.iter().find(|(id, _)| *id == interaction_id) {
            _ = sender.send(PollEvent::RemoveReady(user_id)).await;
        }
        Ok(())
    }

    pub async fn handle_ready_poll(
        handler: &Handler,
        ctx: &Context,
        react: &Reaction,
    ) -> anyhow::Result<()> {
        let http = &ctx.http;
        let msg = react.message(http).await?;
        let (interaction_id, interaction_user) = match msg.interaction.as_ref() {
            Some(interaction) if interaction.name == ReadyPoll::NAME => {
                (interaction.id, interaction.user.id)
            }
            _ => return Ok(()),
        };
        let module: &ModPoll = handler.module()?;
        let user_id = react
            .user_id
            .ok_or_else(|| anyhow!("invalid react: missing userId"))?;
        if react.emoji.to_string() == module.yes && Some(&user_id) != handler.self_id.get() {
            {
                let polls = module.ready_polls.read().await;
                if let Some((_, sender)) = polls.iter().find(|(id, _)| *id == interaction_id) {
                    let _ = sender.send(PollEvent::AddReady(user_id)).await;
                }
            }
            return Ok(());
        }
        if interaction_user != user_id {
            return Ok(());
        }
        if react.emoji.to_string() != module.start {
            return Ok(());
        }
        {
            let polls = module.ready_polls.read().await;
            if let Some((_, sender)) = polls.iter().find(|(id, _)| *id == interaction_id) {
                let _ = sender.send(PollEvent::Go).await;
            }
        }
        Ok(())
    }
}

impl Default for ModPoll {
    fn default() -> Self {
        Self::new(None, None, None, None, None)
    }
}

#[async_trait]
impl Module for ModPoll {
    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Ok(Default::default())
    }

    fn register_commands(&self, store: &mut CommandStore, _completions: &mut CompletionStore) {
        store.register::<ReadyPoll>();
    }
}
