use std::{collections::HashMap, str::FromStr};

use anyhow::{anyhow, Context as _};
use fallible_iterator::FallibleIterator;
use futures::{future::BoxFuture, FutureExt};
use rusqlite::{params, Connection};
use serenity::{
    async_trait,
    builder::{CreateAutocompleteResponse, CreateInteractionResponse},
    model::application::CommandType,
    model::prelude::{CommandInteraction, Message, Permissions, ReactionType},
    prelude::{Context, RwLock},
};

use crate::{
    command_context::{get_focused_option, get_str_opt_ac},
    db::Db,
    prelude::*,
    RegisterableModule,
};
use serenity_command::{BotCommand, CommandKey, CommandResponse};
use serenity_command_derive::Command;

pub struct AutoReact {
    trigger: String,
    emote: ReactionType,
}

fn parse_emote(s: &str) -> anyhow::Result<ReactionType> {
    Ok(ReactionType::from_str(s)?)
}

impl AutoReact {
    fn new(trigger: &str, emote: &str) -> anyhow::Result<AutoReact> {
        let emote = parse_emote(emote)?;
        Ok(AutoReact {
            trigger: trigger.to_string(),
            emote,
        })
    }
}

impl From<(&str, &str)> for AutoReact {
    fn from((trigger, emote): (&str, &str)) -> Self {
        AutoReact::new(trigger, emote).unwrap()
    }
}

pub type ReactsCache = HashMap<u64, Vec<AutoReact>>;

pub async fn new(db: &Connection) -> anyhow::Result<ReactsCache> {
    let cache = {
        db.prepare("SELECT guild_id, trigger, emote FROM autoreact")?
            .query([])?
            .map(|row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .try_fold::<_, anyhow::Error, _>(
                HashMap::<u64, Vec<AutoReact>>::new(),
                |mut cache, (guild_id, trigger, emote): (u64, String, String)| {
                    cache
                        .entry(guild_id)
                        .or_default()
                        .push(AutoReact::new(&trigger, &emote)?);
                    Ok(cache)
                },
            )?
    };
    Ok(cache)
}

#[derive(Command)]
#[cmd(
    name = "add_autoreact",
    desc = "Automatically add reactions to messages"
)]
pub struct AddAutoreact {
    #[cmd(desc = "The word that will trigger the reaction (case-insensitive)")]
    trigger: String,
    #[cmd(desc = "The emote to react with")]
    emote: String,
}

#[async_trait]
impl BotCommand for AddAutoreact {
    type Data = Handler;
    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        opts: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let trigger = self.trigger.to_lowercase();
        let guild_id = opts
            .guild_id
            .ok_or_else(|| anyhow!("Must be run in a guild"))?
            .get();
        let parsed = AutoReact::new(&trigger, &self.emote)?;
        {
            let db = handler.db.lock().await;
            db.conn().execute(
                "INSERT INTO autoreact (guild_id, trigger, emote) VALUES (?1, ?2, ?3)",
                params![guild_id, &trigger, &self.emote],
            )?;
        }
        handler
            .reacts_cache()?
            .write()
            .await
            .entry(guild_id)
            .or_default()
            .push(parsed);
        CommandResponse::private("Autoreact added")
    }

    const PERMISSIONS: Permissions = Permissions::MANAGE_GUILD_EXPRESSIONS;
}

#[derive(Command)]
#[cmd(name = "remove_autoreact", desc = "Remove automatic reaction")]
pub struct RemoveAutoreact {
    #[cmd(
        desc = "The word that triggers the reaction (case-insensitive)",
        autocomplete
    )]
    trigger: String,
    #[cmd(desc = "The emote to stop reacting with", autocomplete)]
    emote: String,
}

#[async_trait]
impl BotCommand for RemoveAutoreact {
    type Data = Handler;
    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        opts: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let trigger = self.trigger.to_lowercase();
        let guild_id = opts
            .guild_id
            .ok_or_else(|| anyhow!("Must be run in a guild"))?
            .get();
        {
            let db = handler.db.lock().await;
            db.conn().execute(
                "DELETE FROM autoreact WHERE guild_id = ?1 AND trigger = ?2 AND emote = ?3",
                params![guild_id, &trigger, self.emote],
            )?;
        }
        let emote = parse_emote(&self.emote)?;
        if let Some(reacts) = handler.reacts_cache()?.write().await.get_mut(&guild_id) {
            reacts.retain_mut(|ar| ar.trigger != trigger && ar.emote != emote);
        };
        CommandResponse::private("Autoreact removed")
    }

    const PERMISSIONS: Permissions = Permissions::MANAGE_GUILD_EXPRESSIONS;
}

impl Handler {
    pub async fn autocomplete_autoreact(
        &self,
        guild_id: u64,
        trigger: &str,
        emote: &str,
    ) -> anyhow::Result<Vec<(String, String)>> {
        let db = self.db.lock().await;
        let res = db
            .conn()
            .prepare(
                "SELECT trigger, emote FROM autoreact WHERE
                     guild_id = ?1 AND trigger LIKE '%'||?2||'%' AND emote LIKE '%'||?3||'%'
                     LIMIT 25",
            )?
            .query(params![guild_id, trigger, emote])?
            .map(|row| Ok((row.get(0)?, row.get(1)?)))
            .collect()?;
        Ok(res)
    }
}

trait ReactProvider {
    fn reacts_cache(&self) -> anyhow::Result<&RwLock<ReactsCache>>;
}

impl ReactProvider for Handler {
    fn reacts_cache(&self) -> anyhow::Result<&RwLock<ReactsCache>> {
        Ok(&self.module::<ModAutoreacts>()?.cache)
    }
}

#[derive(Default)]
pub struct ModAutoreacts {
    cache: RwLock<ReactsCache>,
}

impl ModAutoreacts {
    pub async fn add_reacts(&self, ctx: &Context, msg: Message) -> anyhow::Result<()> {
        let mut lower = msg.content.to_lowercase();
        lower.push_str(
            &msg.embeds
                .iter()
                .flat_map(|e| e.description.as_deref())
                .collect::<String>()
                .to_lowercase(),
        );
        let mut indices = Vec::new();
        let cache = self.cache.read().await;
        let guild_id = match msg.guild_id {
            Some(id) => id.get(),
            None => return Ok(()),
        };
        let reacts = match cache.get(&guild_id) {
            Some(reacts) => reacts,
            None => return Ok(()),
        };
        for (i, react) in reacts.iter().enumerate() {
            if let Some(ndx) = lower.find(&react.trigger) {
                indices.push((ndx, i));
            }
        }
        // sort by trigger position so reacts get added in order
        indices.sort_by_key(|(ndx, _)| *ndx);
        for (_, i) in indices {
            msg.react(&ctx.http, reacts[i].emote.clone())
                .await
                .context("could not add reaction")?;
        }
        Ok(())
    }

    async fn autocomplete_autoreact(
        handler: &Handler,
        guild_id: u64,
        trigger: &str,
        emote: &str,
    ) -> anyhow::Result<Vec<(String, String)>> {
        let db = handler.db.lock().await;
        let res = db
            .conn()
            .prepare(
                "SELECT trigger, emote FROM autoreact WHERE
                     guild_id = ?1 AND trigger LIKE '%'||?2||'%' AND emote LIKE '%'||?3||'%'
                     LIMIT 25",
            )?
            .query(params![guild_id, trigger, emote])?
            .map(|row| Ok((row.get(0)?, row.get(1)?)))
            .collect()?;
        Ok(res)
    }

    fn complete_reacts<'a>(
        handler: &'a Handler,
        ctx: &'a Context,
        key: CommandKey<'a>,
        ac: &'a CommandInteraction,
    ) -> BoxFuture<'a, anyhow::Result<bool>> {
        async move {
            if key != ("remove_autoreact", CommandType::ChatInput) {
                return Ok(false);
            }
            let guild_id = ac
                .guild_id
                .ok_or_else(|| anyhow!("must be run in a guild"))?
                .get();
            let options = &ac.data.options;
            let trigger = get_str_opt_ac(options, "trigger").unwrap_or("");
            let emote = get_str_opt_ac(options, "emote").unwrap_or("");
            let res = Self::autocomplete_autoreact(handler, guild_id, trigger, emote).await?;
            let focused = match get_focused_option(options) {
                Some(f) => f,
                None => return Ok(true),
            };
            let it = res
                .into_iter()
                .map(|(trigger, emote)| if focused == "trigger" { trigger } else { emote })
                .map(|v| (v.clone(), v));
            let resp = it.fold(CreateAutocompleteResponse::new(), |resp, (name, value)| {
                resp.add_string_choice(name, value)
            });
            ac.create_response(&ctx.http, CreateInteractionResponse::Autocomplete(resp))
                .await?;
            Ok(true)
        }
        .boxed()
    }
}

pub async fn add_reacts(handler: &Handler, ctx: &Context, msg: Message) -> anyhow::Result<()> {
    handler
        .module::<ModAutoreacts>()?
        .add_reacts(ctx, msg)
        .await
}

impl ModAutoreacts {
    pub async fn load_reacts(&self, db: &mut Db) -> anyhow::Result<()> {
        let cache = {
            db.conn()
                .prepare("SELECT guild_id, trigger, emote FROM autoreact")?
                .query([])?
                .map(|row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .try_fold::<_, anyhow::Error, _>(
                    HashMap::<u64, Vec<AutoReact>>::new(),
                    |mut cache, (guild_id, trigger, emote): (u64, String, String)| {
                        cache
                            .entry(guild_id)
                            .or_default()
                            .push(AutoReact::new(&trigger, &emote)?);
                        Ok(cache)
                    },
                )?
        };
        *self.cache.write().await = cache;
        Ok(())
    }
}

#[async_trait]
impl Module for ModAutoreacts {
    async fn setup(&mut self, db: &mut Db) -> anyhow::Result<()> {
        db.conn().execute(
            "CREATE TABLE IF NOT EXISTS autoreact (
                guild_id INTEGER NOT NULL,
                trigger STRING NOT NULL,
                emote STRING NOT NULL
            )",
            [],
        )?;
        Ok(())
    }

    fn register_commands(&self, commands: &mut CommandStore, completions: &mut CompletionStore) {
        commands.register::<AddAutoreact>();
        commands.register::<RemoveAutoreact>();

        completions.push(ModAutoreacts::complete_reacts);
    }
}

impl RegisterableModule for ModAutoreacts {
    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Ok(Default::default())
    }
}
