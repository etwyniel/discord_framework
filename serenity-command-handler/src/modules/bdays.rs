use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use chrono::{Datelike, Local, Timelike, Utc};
use fallible_iterator::FallibleIterator;
use rusqlite::params;
use serenity::all::{ChannelId, UserId};
use serenity::builder::{CreateCommandOption, CreateEmbed, CreateEmbedAuthor};
use serenity::http::Http;
use serenity::model::prelude::CommandInteraction;
use serenity::model::prelude::GuildId;
use serenity::{async_trait, prelude::Context};
use serenity_command::{CommandResponse, args, command};
use tokio::sync::Mutex;
use tokio::task::block_in_place;
use tokio::time::interval;

use crate::db::Db;
use crate::prelude::*;

/// a discord user's birthday
pub struct Birthday {
    pub user_id: u64,
    pub day: u8,
    pub month: u8,
    pub year: Option<u16>,
}

/// store a user's birthday into the database for the specified guild
async fn add_birthday(
    handler: &Handler,
    guild_id: u64,
    Birthday {
        user_id,
        day,
        month,
        year,
    }: Birthday,
) -> anyhow::Result<()> {
    let db = handler.db.lock().await;
    block_in_place(|| {
        db.conn().execute(
            "INSERT INTO bdays (guild_id, user_id, day, month, year)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(guild_id, user_id) DO UPDATE
                 SET day = ?3, month = ?4, year = ?5
                 WHERE guild_id = ?1 AND user_id = ?2",
            params![guild_id, user_id, day, month, year],
        )
    })?;
    Ok(())
}

/// get all registered birthdays for the specified guild
async fn get_guild_bdays(handler: &Handler, guild_id: u64) -> anyhow::Result<Vec<Birthday>> {
    let db = handler.db.lock().await;
    let res = block_in_place(|| {
        db.conn()
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
            .collect()
    })?;
    Ok(res)
}

pub const GET_BDAYS: CommandConst = CommandConst {
    description: "List server birthdays",
    ..command!(/bdays: get_bdays)
};

/// responds to the command with a list of all registered birthdays for the current guild,
/// formatted as an embed
async fn get_bdays(
    handler: &Handler,
    ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let guild_id = command.guild_id()?.get();
    let mut bdays = get_guild_bdays(handler, guild_id).await?;

    // sort birthdays, upcoming first
    let today = Utc::now().date_naive();
    let current_day = today.day() as u8;
    let current_month = today.month() as u8;
    bdays.sort_unstable_by_key(|Birthday { day, month, .. }| {
        let mut month = *month;
        if month < current_month || (month == current_month && *day < current_day) {
            // date is in the past for this year, add 12 to the month
            // to get a total ordering
            month += 12;
        }
        month as u64 * 31 + *day as u64
    });

    // format results
    let res = bdays
        .into_iter()
        .map(|b| format!("`{:02}/{:02}` • <@{}>", b.day, b.month, b.user_id))
        .collect::<Vec<_>>()
        .join("\n");
    let header = if let Some(server) = command.guild_id.and_then(|g| g.name(&ctx.cache)) {
        format!("Birthdays in {server}")
    } else {
        "Birthdays".to_string()
    };
    let embed = CreateEmbed::default()
        .author(CreateEmbedAuthor::new(header))
        .description(res);
    CommandResponse::public(embed)
}

args!(SET_BDAY_ARGS =
    "Day"
    day: i64,
    "Month"
    month: i64,
    "Year"
    year: Option<i64>,
);

/// configure options for the /set_bday command
fn set_bday_options(
    name: &str,
    mut opt: CreateCommandOption<'static>,
) -> CreateCommandOption<'static> {
    if name == "day" {
        // configure day number range
        opt = opt.min_int_value(1).max_int_value(31);
    } else if name == "month" {
        // configure month list
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
            opt.add_int_choice(month, n as i64 + 1)
        });
    }
    opt
}

pub const SET_BDAY: CommandConst = CommandConst {
    description: "Set your birthday",
    ..command!(/bday SET_BDAY_ARGS(set_bday_options): set_bday)
};

/// set the user's birthday for the current guild,
/// respond with a simple ephemeral confirmation message
async fn set_bday(
    (day, month, year): SET_BDAY_ARGS,
    handler: &Handler,
    _ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let user_id = command.user.id.get();
    let guild_id = command.guild_id()?.get();
    let bday = Birthday {
        user_id,
        day: day as u8,
        month: month as u8,
        year: year.map(|y| y as u16),
    };
    add_birthday(handler, guild_id, bday).await?;
    CommandResponse::private("Birthday set!")
}

/// send a message to wish the specified user a happy birthday
async fn wish_bday(
    http: &Http,
    user_id: u64,
    guild_id: GuildId,
    channel: Option<u64>,
) -> anyhow::Result<()> {
    // fetch member data for this user to ensure they are still in the server
    _ = guild_id.member(http, UserId::new(user_id)).await?;

    let channel = if let Some(channel_id) = channel {
        // use supplied channel
        ChannelId::new(channel_id)
    } else {
        // fetch channel list, use #general, or the first channel in the list
        // if there is no #general
        let channels = guild_id.channels(http).await?;
        channels
            .iter()
            .find(|chan| chan.base.name == "general")
            .or_else(|| channels.iter().find(|chan| chan.position == 0))
            .ok_or_else(|| anyhow!("Could not find a suitable channel"))?
            .id
    };

    channel
        .widen()
        .say(http, format!("Happy birthday to <@{user_id}>!"))
        .await?;
    Ok(())
}

/// routine that periodically checks if it is a user's birthday, and sends birthday messages
pub async fn bday_loop(db: Arc<Mutex<Db>>, http: Arc<Http>) {
    // check every hour
    let mut interval = interval(Duration::from_secs(3600));
    loop {
        interval.tick().await;
        let now = Local::now();
        if now.hour() != 10 {
            // wait for 10AM
            continue;
        }
        // fetch users whose birthday it is, and the guilds in which they have set their birthday
        let guilds_and_users = {
            let db = db.lock().await;
            block_in_place(|| {
                let mut stmt = db
                    .conn()
                    .prepare("SELECT guild_id, user_id FROM bdays WHERE day = ?1 AND month = ?2")
                    .unwrap();
                stmt.query([now.day(), now.month()])
                    .unwrap()
                    .map(|row| Ok((row.get(0)?, row.get(1)?)))
                    .iterator()
                    .filter_map(Result::ok)
                    .collect::<Vec<_>>()
            })
        };
        for (guild_id, user_id) in guilds_and_users {
            // fetch configured general channel for this guild
            let general_channel_id = {
                db.lock()
                    .await
                    .get_guild_field(guild_id, "general_channel_id")
                    .ok()
                    .flatten()
            };
            // send birthday message
            if let Err(e) = wish_bday(
                http.as_ref(),
                user_id,
                GuildId::new(guild_id),
                general_channel_id,
            )
            .await
            {
                eprintln!("Error wishing user birthday: {e:?}");
            }
        }
    }
}

pub struct Bdays;

#[async_trait]
impl Module for Bdays {
    async fn setup(&mut self, db: &mut crate::db::Db) -> anyhow::Result<()> {
        block_in_place(|| {
            db.conn().execute(
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
            db.add_guild_field("general_channel_id", "STRING")?;
            Ok(())
        })
    }

    fn register_commands(&self, store: &mut dyn Storer) {
        store.register(GET_BDAYS);
        store.register(SET_BDAY);
    }
}

impl RegisterableModule for Bdays {
    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Ok(Bdays)
    }
}
