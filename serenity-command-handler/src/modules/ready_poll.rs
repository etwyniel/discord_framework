use std::collections::VecDeque;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context as _};
use itertools::Itertools;
use serenity::http::Http;
use serenity::model::application::interaction::MessageInteraction;
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
    ) -> anyhow::Result<()> {
        let pending_poll = (interaction.id, self.count_emote, self.go_emote, Vec::new());
        let module: &ModPoll = handler.module()?;
        let http = &ctx.http;
        // create initial response to the interaction
        interaction
            .create_interaction_response(http, |msg| {
                msg.interaction_response_data(|data| {
                    data.content("Ready?")
                        .allowed_mentions(|mentions| mentions.empty_users())
                })
            })
            .await
            .context("error creating response")?;

        // retrieve handle to interaction response so we can edit it later
        let resp = interaction.get_interaction_response(http).await?;
        // create async channel in order to process reactions asynchronously
        let (sender, receiver) = channel(16);

        {
            // add this poll to the list
            // using a sub-scope to ensure write lock gets dropped ASAP
            let mut polls = module.ready_polls.write().await;
            while polls.len() >= MAX_POLLS {
                polls.pop_back();
            }
            polls.push_front((interaction.id, sender));
        }

        // add reacts to interaction response
        resp.react(http, ReactionType::from_str(&module.yes)?)
            .await
            .context("error adding yes react")?;
        resp.react(http, ReactionType::from_str(&module.no)?)
            .await
            .context("error adding no react")?;
        resp.react(http, ReactionType::from_str(&module.start)?)
            .await
            .context("error adding go react")?;

        // spawn task to handle reactions
        let http_arc = Arc::clone(&ctx.http);
        tokio::spawn(poll_task(
            handler.module_arc().unwrap(),
            http_arc,
            resp,
            pending_poll,
            receiver,
        ));
        // already created a response
        Ok(())
    }
}

// build ready poll message.
// lists users that have clicked the YES react as being ready.
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
        // create ready poll message
        let resp = match self.create_poll(handler, ctx, interaction).await {
            Err(e) => {
                dbg!(&e);
                Some(e.to_string())
            }
            _ => None,
        };
        // in case creating the poll failed, try to edit the interaction response with an error message
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
    Start,
}

// task responsible for handling reactions to a poll
async fn poll_task(
    module: Arc<ModPoll>,
    http: Arc<Http>,
    msg: Message,
    poll: PendingPoll,
    mut r: Receiver<PollEvent>,
) {
    // poll state
    let mut users = Vec::new(); // list of users who have clicked the READY react
    let mut changed = false; // whether the message needs to be edited
    let mut started = false; // whether the poll's author has clicked the GO react
    let mut last_event = Instant::now();

    loop {
        // poll for new events
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
                    users.retain(|&u| u != user);
                }
                PollEvent::Start => {
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
            // too long since last event, stop this task
            return;
        }
        if !changed {
            // no change, no need to edit the message
            continue;
        }
        let content = build_message(&users);
        // edit message in a separate task to avoid blocking this one
        tokio::spawn({
            let http = Arc::clone(&http);
            let mut msg = msg.clone();
            async move {
                let res = msg
                    .edit(http.as_ref(), |msg| {
                        msg.content(content).allowed_mentions(|m| m.empty_users())
                    })
                    .await;
                if let Err(e) = res {
                    eprintln!("failed to edit ready message: {e}");
                }
            }
        });
        changed = false;
    }
}

// performs the actual countdown
pub async fn crabdown(
    module: Arc<ModPoll>,
    http: &Http,
    channel: ChannelId,
    count_emote: Option<&str>,
    go_emote: Option<&str>,
) -> anyhow::Result<()> {
    // announce countdown is starting, wait briefly
    channel.say(http, "Starting 3s countdown").await?;
    tokio::time::sleep(Duration::from_secs(2)).await;

    // use interval instead of sleep to minimize drift due to the time it takes to send a message
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    // first tick happens with no delay, skip it
    interval.tick().await;

    let count_emote = count_emote.unwrap_or(&module.count);
    let go_emote = go_emote.unwrap_or(&module.go);
    for i in 0..3 {
        // repeat count emote 3 - i times
        let contents = std::iter::repeat(count_emote).take(3 - i).join(" ");
        channel.say(http, contents).await?;
        interval.tick().await;
    }
    channel.say(http, go_emote).await?;
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
    // instantiate the module with any number of emotes specified.
    // unspecified emotes will use the default.
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

    // returns None if msg is not a /ready_poll interaction response
    fn get_poll_interaction<'a>(
        handler: &Handler,
        msg: &'a Message,
    ) -> Option<&'a MessageInteraction> {
        // check if we sent this message
        if Some(&msg.author.id) != handler.self_id.get() {
            // not ours
            return None;
        }
        // check if this message is a response to a /ready_poll command
        msg.interaction
            .as_ref()
            .filter(|interaction| interaction.name == ReadyPoll::NAME)
    }

    // callback for react removal
    pub async fn handle_remove_react(
        handler: &Handler,
        ctx: &Context,
        react: &Reaction,
    ) -> anyhow::Result<()> {
        let msg = react.message(&ctx.http).await?;
        let Some(interaction) = Self::get_poll_interaction(handler, &msg) else {
            return Ok(());
        };

        // we only care about YES reacts being removed
        let module: &ModPoll = handler.module()?;
        if react.emoji.to_string() != module.yes {
            return Ok(());
        }

        // get the ID of the user who removed the react
        let user_id = react
            .user_id
            .ok_or_else(|| anyhow!("invalid react: missing userId"))?;

        // find the sender for that poll's handler and send a RemoveReady event
        let polls = module.ready_polls.read().await;
        if let Some((_, sender)) = polls.iter().find(|(id, _)| *id == interaction.id) {
            _ = sender.send(PollEvent::RemoveReady(user_id)).await;
        }
        Ok(())
    }

    // callback for adding a react
    pub async fn handle_ready_poll(
        handler: &Handler,
        ctx: &Context,
        react: &Reaction,
    ) -> anyhow::Result<()> {
        let http = &ctx.http;
        let msg = react.message(http).await?;
        let Some(interaction) = Self::get_poll_interaction(handler, &msg) else {
            return Ok(());
        };

        // get the ID of the user who added the react
        let user_id = react
            .user_id
            .ok_or_else(|| anyhow!("invalid react: missing userId"))?;

        let module: &ModPoll = handler.module()?;
        let event =
            if react.emoji.to_string() == module.yes && Some(&user_id) != handler.self_id.get() {
                // user added a YES react (and is not the bot)
                // send AddReady event
                PollEvent::AddReady(user_id)
            } else if interaction.user.id == user_id && react.emoji.to_string() == module.start {
                // poll author clicked the START react
                // send Start event
                PollEvent::Start
            } else {
                // not a react we care about
                return Ok(());
            };

        // send event to the poll's handler task
        let polls = module.ready_polls.read().await;
        if let Some((_, sender)) = polls.iter().find(|(id, _)| *id == interaction.id) {
            let _ = sender.send(event).await;
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
