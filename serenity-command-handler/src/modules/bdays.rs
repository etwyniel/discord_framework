use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use chrono::{Datelike, Local, Timelike, Utc};
use fallible_iterator::FallibleIterator;
use rusqlite::params;
use serenity::builder::{CreateCommandOption, CreateEmbed, CreateEmbedAuthor};
use serenity::http::Http;
use serenity::model::prelude::CommandInteraction;
use serenity::model::prelude::GuildId;
use serenity::{async_trait, prelude::Context};
use serenity_command::{BotCommand, CommandResponse};
use serenity_command_derive::Command;
use tokio::sync::Mutex;
use tokio::time::interval;

use crate::db::Db;
use crate::{CommandStore, CompletionStore, Handler, Module, ModuleMap};

pub struct Birthday {
    pub user_id: u64,
    pub day: u8,
    pub month: u8,
    pub year: Option<u16>,
}

async fn add_birthday(
    handler: &Handler,
    guild_id: u64,
    user_id: u64,
    day: u8,
    month: u8,
    year: Option<u16>,
) -> anyhow::Result<()> {
    let db = handler.db.lock().await;
    db.conn.execute(
        "INSERT INTO bdays (guild_id, user_id, day, month, year)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(guild_id, user_id) DO UPDATE
                 SET day = ?3, month = ?4, year = ?5
                 WHERE guild_id = ?1 AND user_id = ?2",
        params![guild_id, user_id, day, month, year],
    )?;
    Ok(())
}

async fn get_bdays(handler: &Handler, guild_id: u64) -> anyhow::Result<Vec<Birthday>> {
    let db = handler.db.lock().await;
    let res = db
        .conn
        .prepare("SELECT user_id, day, month, year FROM bdays WHERE guild_id = ?1")?
        .query([guild_id])?
        .map(|row| {
            Ok(Birthday {
                user_id: row.get(0)?,
                day: row.get(1)?,
                month: row.get(2)?,
                year: row.get(3)?,
            })
        })
        .collect()?;
    Ok(res)
}

#[derive(Command)]
#[cmd(name = "bdays", desc = "List server birthdays")]
pub struct GetBdays;

#[async_trait]
impl BotCommand for GetBdays {
    type Data = Handler;
    async fn run(
        self,
        handler: &Handler,
        ctx: &Context,
        opts: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let guild_id = opts
            .guild_id
            .ok_or_else(|| anyhow!("Must be run in a guild"))?
            .get();
        let mut bdays = get_bdays(handler, guild_id).await?;
        let today = Utc::now().date_naive();
        let current_day = today.day() as u8;
        let current_month = today.month() as u8;
        bdays.sort_unstable_by_key(|Birthday { day, mut month, .. }| {
            if month < current_month || (month == current_month && *day < current_day) {
                month += 12;
            }
            month as u64 * 31 + *day as u64
        });
        let res = bdays
            .into_iter()
            .map(|b| format!("`{:02}/{:02}` • <@{}>", b.day, b.month, b.user_id))
            .collect::<Vec<_>>()
            .join("\n");
        let header = if let Some(server) = opts.guild_id.and_then(|g| g.name(ctx)) {
            format!("Birthdays in {server}")
        } else {
            "Birthdays".to_string()
        };
        let embed = CreateEmbed::default()
            .author(CreateEmbedAuthor::new(header))
            .description(res);
        Ok(CommandResponse::Embed(Box::new(embed)))
    }
}

#[derive(Command)]
#[cmd(name = "bday", desc = "Set your birthday")]
pub struct SetBday {
    #[cmd(desc = "Day")]
    day: i64,
    #[cmd(desc = "Month")]
    month: i64,
    #[cmd(desc = "Year")]
    year: Option<i64>,
}

#[async_trait]
impl BotCommand for SetBday {
    type Data = Handler;
    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        opts: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let user_id = opts.user.id.get();
        let guild_id = opts
            .guild_id
            .ok_or_else(|| anyhow!("Must be run in a guild"))?
            .get();
        add_birthday(
            handler,
            guild_id,
            user_id,
            self.day as u8,
            self.month as u8,
            self.year.map(|y| y as u16),
        )
        .await?;
        Ok(CommandResponse::Private("Birthday set!".to_string()))
    }

    fn setup_options(opt_name: &'static str, mut opt: CreateCommandOption) -> CreateCommandOption {
        match opt_name {
            "day" => {
                opt = opt.min_int_value(1).max_int_value(31);
            }
            "month" => {
                const MONTHS: [&str; 12] = [
                    "January",
                    "February",
                    "March",
                    "April",
                    "May",
                    "June",
                    "July",
                    "August",
                    "September",
                    "October",
                    "November",
                    "December",
                ];
                opt = MONTHS.iter().enumerate().fold(opt, |opt, (n, &month)| {
                    opt.add_int_choice(month, n as i32 + 1)
                });
            }
            _ => {}
        }
        opt
    }
}

async fn wish_bday(http: &Http, user_id: u64, guild_id: GuildId) -> anyhow::Result<()> {
    let member = guild_id.member(http, user_id).await?;
    let channels = guild_id.channels(http).await?;
    let channel = channels
        .values()
        .find(|chan| chan.name() == "general")
        .or_else(|| channels.values().find(|chan| chan.position == 0))
        .ok_or_else(|| anyhow!("Could not find a suitable channel"))?;
    channel
        .say(
            http,
            format!("Happy birthday to <@{}>!", member.user.id.get()),
        )
        .await?;
    Ok(())
}

pub async fn bday_loop(db: Arc<Mutex<Db>>, http: Arc<Http>) {
    let mut interval = interval(Duration::from_secs(3600));
    loop {
        interval.tick().await;
        let now = Local::now();
        if now.hour() != 10 {
            continue;
        }
        let guilds_and_users = {
            let db = db.lock().await;
            let mut stmt = db
                .conn
                .prepare("SELECT guild_id, user_id FROM bdays WHERE day = ?1 AND month = ?2")
                .unwrap();
            stmt.query([now.day(), now.month()])
                .unwrap()
                .map(|row| Ok((row.get(0)?, row.get(1)?)))
                .iterator()
                .filter_map(Result::ok)
                .collect::<Vec<_>>()
        };
        for (guild_id, user_id) in guilds_and_users {
            if let Err(e) = wish_bday(http.as_ref(), user_id, GuildId::new(guild_id)).await {
                eprintln!("Error wishing user birthday: {e:?}");
            }
        }
    }
}

pub struct Bdays;

#[async_trait]
impl Module for Bdays {
    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Ok(Bdays)
    }

    async fn setup(&mut self, db: &mut crate::db::Db) -> anyhow::Result<()> {
        db.conn.execute(
            "CREATE TABLE IF NOT EXISTS bdays (
            guild_id INTEGER NOT NULL,
            user_id INTEGER NOT NULL,
            day INTEGER NOT NULL,
            month INTEGER NOT NULL,
            year INTEGER,
            UNIQUE(guild_id, user_id)
            )",
            [],
        )?;
        Ok(())
    }

    fn register_commands(&self, store: &mut CommandStore, _: &mut CompletionStore) {
        store.register::<GetBdays>();
        store.register::<SetBday>();
    }
}
