use std::{
    borrow::{Borrow, Cow},
    cmp::{Eq, PartialEq},
    collections::HashSet,
    fmt::Write,
    hash::Hash,
    sync::Arc,
    time::Duration,
};

use anyhow::{anyhow, bail, Context as _};
use chrono::{DateTime, Local, Timelike, Utc};
use fallible_iterator::FallibleIterator;
use futures::{future::BoxFuture, FutureExt};
use itertools::Itertools;
use rand::random;
use regex::Regex;
use rusqlite::{params, Error::SqliteFailure, ErrorCode};
use serenity::{
    all::{AutoArchiveDuration, CreateMessage, CreateThread, Http},
    async_trait,
    builder::{
        CreateAutocompleteResponse, CreateCommandOption, CreateEmbed, CreateEmbedAuthor,
        CreateEmbedFooter, CreateInteractionResponse, GetMessages,
    },
    model::{
        self,
        application::{CommandInteraction, CommandType},
        channel::Message,
        id::MessageId,
        prelude::{ChannelId, GuildId, ReactionType, UserId},
    },
    prelude::{Context, Mutex},
};

use serenity_command::{BotCommand, CommandKey, CommandResponse};
use serenity_command_derive::Command;
use tokio::time::interval;

use crate::{command_context::get_str_opt_ac, db::Db, prelude::*};

pub async fn message_to_quote_contents(
    _handler: &Handler,
    ctx: &Context,
    message: &Message,
) -> anyhow::Result<String> {
    let quote_ndx = message
        .reactions
        .iter()
        .find_position(|r| r.reaction_type == ReactionType::Unicode("🗨️".to_string()))
        .map(|(ndx, _)| ndx)
        .unwrap_or(message.reactions.len());
    let prev_react = message
        .reactions
        .get(quote_ndx.wrapping_sub(1))
        .map(|r| &r.reaction_type);
    let mut messages: Vec<(String, u64)> = Default::default();
    if let Some(ReactionType::Unicode(emoji)) = prev_react {
        let first_byte = emoji.as_bytes()[0];
        if (b'1'..=b'9').contains(&first_byte) {
            let num = first_byte as u64 - (b'0' as u64) - 1;
            let http = &ctx.http;
            let before = message
                .channel(http)
                .await?
                .guild()
                .unwrap()
                .messages(http, GetMessages::new().before(message.id).limit(num as u8))
                .await?;
            messages.extend(
                before
                    .iter()
                    .rev()
                    .map(|msg| (msg.content.clone(), msg.author.id.get())),
            );
        }
    }
    if messages.is_empty() {
        messages.extend(
            message
                .referenced_message
                .as_ref()
                .map(|msg| (msg.content.clone(), msg.author.id.get())),
        );
    }
    messages.push((message.content.clone(), message.author.id.get()));
    let mut contents = String::new();
    let mut prev_author = messages.first().unwrap().1;
    for (msg, author) in messages {
        if prev_author != author {
            _ = writeln!(&mut contents, "- <@{prev_author}>");
        }
        contents.push_str(&msg);
        contents.push('\n');
        prev_author = author;
    }
    Ok(contents)
}

pub struct Quote {
    pub quote_number: u64,
    pub guild_id: u64,
    pub channel_id: u64,
    pub message_id: MessageId,
    pub ts: DateTime<Utc>,
    pub author_id: u64,
    pub author_name: String,
    pub contents: String,
    pub image: Option<String>,
}

pub fn fetch_quote(db: &Db, guild_id: u64, quote_number: u64) -> anyhow::Result<Option<Quote>> {
    let res = db.conn.query_row(
            "SELECT guild_id, channel_id, message_id, ts, author_id, author_name, contents, image FROM quote
     WHERE guild_id = ?1 AND quote_number = ?2",
            [guild_id, quote_number],
            |row| {
                let ts = DateTime::from_timestamp(row.get(3)?, 0)
                    .unwrap_or_default(); // yes this was quoted in 1970, what of it?
                Ok(Quote {
                    quote_number,
                    guild_id: row.get(0)?,
                    channel_id: row.get(1)?,
                    message_id: MessageId::new(row.get(2)?),
                    ts,
                    author_id: row.get(4)?,
                    author_name: row.get(5)?,
                    contents: crate::db::column_as_string(row.get_ref(6)?)?,
                    image: row.get(7)?,
                })
            },
        );
    match res {
        Ok(q) => Ok(Some(q)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e).context("Error fetching quote"),
    }
}

pub async fn add_quote(
    handler: &Handler,
    ctx: &Context,
    guild_id: u64,
    message: &Message,
) -> anyhow::Result<Option<u64>> {
    let contents = message_to_quote_contents(handler, ctx, message).await?;
    let mut db = handler.db.lock().await;
    let tx = db.conn.transaction()?;
    let last_quote: u64 = tx
        .query_row(
            "SELECT quote_number FROM quote WHERE guild_id = ?1 ORDER BY quote_number DESC",
            [guild_id],
            |row| row.get(0),
        )
        .unwrap_or(0);
    let channel_id = message.channel_id.get();
    let ts = message.timestamp;
    let author_id = message.author.id.get();
    let author_name = &message.author.name;
    let image = message
        .attachments
        .iter()
        .find(|att| att.height.is_some())
        .map(|att| att.url.clone());
    match tx.execute(
        r"INSERT INTO quote (
    guild_id, channel_id, message_id, ts, quote_number,
    author_id, author_name, contents, image
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            guild_id,
            channel_id,
            message.id.get(),
            ts.unix_timestamp(),
            last_quote + 1,
            author_id,
            author_name,
            contents.trim(),
            image
        ],
    ) {
        Err(SqliteFailure(e, _)) if e.code == ErrorCode::ConstraintViolation => {
            return Ok(None); // Quote already exists
        }
        Ok(n) => Ok(Some(n)),
        Err(e) => Err(e),
    }?;
    tx.commit()?;
    Ok(Some(last_quote + 1))
}

pub fn get_random_quote(
    db: &Db,
    guild_id: u64,
    user: Option<u64>,
) -> anyhow::Result<Option<Quote>> {
    let number = {
        let mut stmt = db.conn.prepare(
            "SELECT quote_number FROM quote WHERE guild_id = ?1 AND (?2 IS NULL OR author_id = ?2)",
        )?;
        let numbers: Vec<_> = stmt
            .query(params![guild_id, user])?
            .map(|row| row.get(0))
            .collect()?;
        if numbers.is_empty() {
            bail!("No quotes saved");
        }
        numbers[rand::random::<usize>() % numbers.len()]
    };
    fetch_quote(db, guild_id, number)
}

#[derive(Clone)]
pub struct CaseInsensitiveString<'a>(Cow<'a, str>);

impl CaseInsensitiveString<'_> {
    fn simplify_bytes(&self) -> impl Iterator<Item = u8> + '_ {
        self.0
            .bytes()
            .filter(|b| !"\".,?-!&:*$%#(){}<>'; \t\n|".as_bytes().contains(b))
            .map(|b| b.to_ascii_lowercase())
    }
}

impl Hash for CaseInsensitiveString<'_> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.simplify_bytes().for_each(|b| state.write_u8(b));
    }
}

impl PartialEq for CaseInsensitiveString<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.simplify_bytes().eq(other.simplify_bytes())
    }
}

impl Eq for CaseInsensitiveString<'_> {}

pub async fn quotes_markov_chain(
    handler: &Handler,
    guild_id: u64,
    user: Option<u64>,
    order: Option<usize>,
) -> anyhow::Result<(
    markov::Chain<CaseInsensitiveString>,
    HashSet<CaseInsensitiveString>,
)> {
    let db = handler.db.lock().await;
    let mut stmt = db.conn.prepare(
        "SELECT contents FROM quote WHERE guild_id = ?1 AND (?2 IS NULL or author_id = ?2)",
    )?;
    let mut chain = markov::Chain::of_order(order.unwrap_or(1));
    let mut quotes = HashSet::new();
    stmt.query(params![guild_id, user])?
        .map(|row| crate::db::column_as_string(row.get_ref(0)?))
        .for_each(|quote: String| {
            let parts = quote.split("- <@").collect_vec();
            parts.iter().copied().enumerate().for_each(|(i, mut msg)| {
                if i > 0 {
                    msg = match msg.split_once('\n') {
                        None => return,
                        Some((_, s)) => s,
                    };
                    // msg = msg.split_once('').map(|(_, msg)| msg).unwrap_or(msg);
                }
                if let Some(user_id) = user {
                    let author_id = parts
                        .get(i + 1)
                        .and_then(|next| next.split_once('>'))
                        .and_then(|(id, _)| id.parse::<u64>().ok());
                    if author_id.is_some_and(|id| id != user_id) {
                        return;
                    }
                }
                quotes.insert(CaseInsensitiveString(Cow::Owned(msg.to_string())));
                chain.feed(
                    msg.split_whitespace()
                        .map(|s| CaseInsensitiveString(Cow::Owned(s.to_string())))
                        .collect::<Vec<_>>(),
                );
            });
            Ok(())
        })?;
    Ok((chain, quotes))
}

pub async fn list_quotes(
    handler: &Handler,
    guild_id: u64,
    like: &str,
) -> anyhow::Result<Vec<(u64, String)>> {
    let db = handler.db.lock().await;
    let res = db.conn.prepare(
            "SELECT quote_number, contents FROM quote WHERE guild_id = ?1 AND contents LIKE '%'||?2||'%' LIMIT 15",
        )?
            .query(params![guild_id, like])?
            .map(|row| Ok((row.get(0)?, row.get(1)?)))
            .collect()?;
    Ok(res)
}

#[derive(Command)]
#[cmd(name = "quote", desc = "Retrieve a quote")]
pub struct GetQuote {
    #[cmd(desc = "Number the quote was saved as (optional)", autocomplete)]
    pub number: Option<i64>,
    #[cmd(desc = "Get a random quote from a specific user")]
    pub user: Option<UserId>,
    #[cmd(desc = "Hide the username for even more confusion")]
    pub hide_author: Option<bool>,
}

#[async_trait]
impl BotCommand for GetQuote {
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
        self.get_quote(handler, ctx, guild_id).await
    }

    fn setup_options(opt_name: &'static str, opt: CreateCommandOption) -> CreateCommandOption {
        if opt_name == "number" {
            opt.min_int_value(1)
        } else {
            opt
        }
    }
}

struct QuoteEmbedElements {
    contents: String,
    author_avatar: Option<String>,
    channel_name: String,
    message_url: String,
}

async fn create_quote_embed(http: &Http, quote: &Quote) -> anyhow::Result<QuoteEmbedElements> {
    let message_url = format!(
        "https://discord.com/channels/{}/{}/{}",
        quote.guild_id, quote.channel_id, quote.message_id
    );
    let channel = ChannelId::new(quote.channel_id)
        .to_channel(http)
        .await?
        .guild();
    let channel_name = channel
        .as_ref()
        .map(|c| c.name())
        .unwrap_or("unknown-channel")
        .to_owned();
    let contents = format!(
        "{}\n- <@{}> [(Source)]({})",
        &quote.contents, quote.author_id, message_url
    );
    let author_avatar = UserId::new(quote.author_id)
        .to_user(http)
        .await?
        .avatar_url()
        .filter(|av| av.starts_with("http"));
    Ok(QuoteEmbedElements {
        contents,
        author_avatar,
        channel_name,
        message_url,
    })
}

impl GetQuote {
    pub async fn get_quote(
        self,
        handler: &Handler,
        ctx: &Context,
        guild_id: u64,
    ) -> anyhow::Result<CommandResponse> {
        let quote = {
            let db = handler.db.lock().await;
            if let Some(quote_number) = self.number {
                fetch_quote(db.borrow(), guild_id, quote_number as u64)?
            } else {
                get_random_quote(db.borrow(), guild_id, self.user.map(|u| u.get()))?
            }
        }
        .ok_or_else(|| anyhow!("No such quote"))?;
        let QuoteEmbedElements {
            mut contents,
            author_avatar,
            channel_name,
            message_url,
        } = create_quote_embed(&ctx.http, &quote).await?;
        let hide_author = self.hide_author == Some(true);
        let quote_header = match (self.user, self.number, hide_author) {
            (_, Some(_), _) => "".to_string(), // Set quote number, not random
            (Some(_), _, false) => format!(" - Random quote from {}", &quote.author_name),
            (Some(_), _, true) => " - Random quote from REDACTED".to_string(),
            (None, None, _) => " - Random quote".to_string(),
        };
        if hide_author {
            let hide_author_re = Regex::new("(<@\\d+>)").unwrap();
            let padding = random::<usize>() % 10;
            let mut patt = "||$1`".to_string();
            patt.push_str(&" ".repeat(padding));
            patt.push_str("`||");
            contents = hide_author_re.replace_all(&contents, &patt).to_string();
        }
        let mut create = CreateEmbed::default()
            .author(
                CreateEmbedAuthor::new(format!("#{}{}", quote.quote_number, quote_header))
                    .icon_url(author_avatar.unwrap_or_default()),
            )
            .description(&contents)
            .url(message_url)
            .footer(CreateEmbedFooter::new(format!("in #{channel_name}")))
            .timestamp(model::Timestamp::parse(&quote.ts.format("%+").to_string()).unwrap());

        if let Some(image) = quote.image {
            create = create.image(image);
        }
        CommandResponse::public(create)
    }
}

#[derive(Command)]
#[cmd(name = "quote", message)]
pub struct SaveQuote(Message);

#[async_trait]
impl BotCommand for SaveQuote {
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
        // messages received through command interactions are partial
        // retrieve full message to have referenced_message
        let message = ctx.http.get_message(self.0.channel_id, self.0.id).await?;
        let quote_number = add_quote(handler, ctx, guild_id, &message).await?;
        let link = self
            .0
            .id
            .link(self.0.channel_id, Some(GuildId::new(guild_id)));
        let resp_text = match quote_number {
            Some(n) => format!("Quote saved as #{n}: {link}"),
            None => "Quote already added".to_string(),
        };
        CommandResponse::public(resp_text)
    }
}

#[derive(Command)]
#[cmd(name = "fake_quote", desc = "Get a procedurally generated quote")]
pub struct FakeQuote {
    user: Option<UserId>,
    start: Option<String>,
    order: Option<usize>,
}

#[async_trait]
impl BotCommand for FakeQuote {
    type Data = Handler;
    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        opts: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let (chain, quotes) = quotes_markov_chain(
            handler,
            opts.guild_id
                .ok_or_else(|| anyhow!("must be run in a guild"))?
                .get(),
            self.user.map(|u| u.get()),
            self.order,
        )
        .await?;
        let mut resp = String::new();
        for _ in 0..100 {
            resp = if let Some(start) = &self.start {
                chain.generate_from_token(CaseInsensitiveString(start.into()))
                // chain.generate_str_from_token(&start)
            } else {
                chain.generate()
            }
            .into_iter()
            .map(|CaseInsensitiveString(s)| s)
            .join(" ");
            if !quotes.contains(&CaseInsensitiveString(resp.as_str().into())) {
                break;
            }
            eprintln!("generated a real quote, trying again");
        }
        if resp.is_empty() {
            resp = "Failed to generate quote".to_string();
        } else if let Some(id) = self.user.map(UserId::get) {
            write!(&mut resp, "\n - <@{id}>").unwrap();
        }
        CommandResponse::public(resp)
    }

    fn setup_options(opt_name: &'static str, opt: CreateCommandOption) -> CreateCommandOption {
        if opt_name == "order" {
            opt.min_int_value(1)
                .max_int_value(4)
                .description("Markov chain order. Higher = closer to real quotes but more coherent")
        } else {
            opt
        }
    }
}

pub async fn send_qotd(
    db: &Mutex<Db>,
    http: &Http,
    guild_id: u64,
    channel_id: u64,
) -> anyhow::Result<()> {
    // let Some(channel_id): Option<u64> = db.get_guild_field(guild_id, "qotd_channel_id")? else {
    //     return Ok(());
    // };
    let today = Local::now().date_naive();
    let Some(qotd) = ({
        // access db in inner scope to avoid holding lock across awaits
        get_random_quote(db.lock().await.borrow(), guild_id, None)?
    }) else {
        return Ok(());
    };
    let QuoteEmbedElements {
        contents,
        author_avatar,
        channel_name,
        message_url,
    } = create_quote_embed(http, &qotd).await?;
    let mut embed = CreateEmbed::default()
        .author(
            CreateEmbedAuthor::new(format!("#{} - Quote of the day", qotd.quote_number))
                .icon_url(author_avatar.unwrap_or_default()),
        )
        .description(&contents)
        .url(message_url)
        .footer(CreateEmbedFooter::new(format!("in #{channel_name}")))
        .timestamp(model::Timestamp::parse(&qotd.ts.format("%+").to_string()).unwrap());

    if let Some(image) = qotd.image {
        embed = embed.image(image);
    }
    let channel = ChannelId::new(channel_id);
    let msg = channel
        .send_message(http, CreateMessage::new().embed(embed))
        .await?;
    let thread_name = if qotd.contents.is_empty() {
        format!("Quote #{}", qotd.quote_number)
    } else {
        qotd.contents.chars().take(100).collect::<String>()
    };
    channel
        .create_thread_from_message(
            http,
            msg.id,
            CreateThread::new(thread_name).auto_archive_duration(AutoArchiveDuration::OneDay),
        )
        .await?;
    db.lock()
        .await
        .set_guild_field(guild_id, "qotd_last_sent", today)?;
    Ok(())
}

pub async fn qotd_loop(db: Arc<Mutex<Db>>, http: Arc<Http>) {
    let mut interval = interval(Duration::from_secs(3600));
    loop {
        interval.tick().await;
        let now = Local::now();
        if now.hour() != 11 {
            continue;
        }
        let guilds_and_channels = {
            let db = db.lock().await;
            let mut stmt = db
                .conn
                .prepare(
                    r"SELECT id, qotd_channel_id FROM guild
                         WHERE qotd_channel_id IS NOT NULL AND
                         (qotd_last_sent iS NULL OR qotd_last_sent < ?1)",
                )
                .unwrap();
            stmt.query([now.date_naive().to_string()])
                .unwrap()
                .map(|row| Ok((row.get(0)?, row.get(1)?)))
                .iterator()
                .filter_map(Result::ok)
                .collect::<Vec<_>>()
        };
        for (guild_id, channel_id) in guilds_and_channels {
            if let Err(e) = send_qotd(&db, http.as_ref(), guild_id, channel_id).await {
                eprintln!("Error sending quote of the day for guild {guild_id}: {e:?}");
            }
        }
    }
}

pub struct Quotes;

impl Quotes {
    fn complete_quotes<'a>(
        handler: &'a Handler,
        ctx: &'a Context,
        key: CommandKey<'a>,
        ac: &'a CommandInteraction,
    ) -> BoxFuture<'a, anyhow::Result<bool>> {
        async move {
            if key != ("quote", CommandType::ChatInput) {
                return Ok(false);
            }
            let guild_id = ac
                .guild_id
                .ok_or_else(|| anyhow!("must be run in a guild"))?
                .get();
            let options = &ac.data.options;
            let val = get_str_opt_ac(options, "number");
            let Some(v) = val else {
                return Ok(true);
            };
            let quotes = list_quotes(handler, guild_id, v).await?;
            let resp = quotes
                .into_iter()
                .filter(|(_, quote)| !quote.is_empty())
                .map(|(num, quote)| (num, quote.chars().take(100).collect::<String>()))
                .fold(CreateAutocompleteResponse::new(), |resp, (num, q)| {
                    resp.add_int_choice(q, num as i64)
                });
            ac.create_response(&ctx.http, CreateInteractionResponse::Autocomplete(resp))
                .await?;
            Ok(true)
        }
        .boxed()
    }
}

#[async_trait]
impl Module for Quotes {
    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Ok(Quotes)
    }

    async fn setup(&mut self, db: &mut crate::db::Db) -> anyhow::Result<()> {
        db.conn.execute(
            "CREATE TABLE IF NOT EXISTS quote (
                guild_id INTEGER,
                channel_id INTEGER,
                message_id INTEGER,
                ts INTEGER,
                quote_number INTEGER,
                author_id INTEGER,
                author_name STRING,
                contents STRING,
                image STRING,
                UNIQUE(guild_id, quote_number),
                UNIQUE(guild_id, message_id)
            )",
            [],
        )?;
        db.add_guild_field("qotd_channel_id", "INTEGER")?;
        db.add_guild_field("qotd_last_sent", "STRING")?;
        Ok(())
    }

    fn register_commands(&self, store: &mut CommandStore, completions: &mut CompletionStore) {
        store.register::<GetQuote>();
        store.register::<SaveQuote>();
        store.register::<FakeQuote>();
        completions.push(Quotes::complete_quotes);
    }
}
