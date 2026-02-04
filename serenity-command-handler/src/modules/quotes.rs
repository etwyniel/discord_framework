use std::{
    borrow::{Borrow, Cow},
    cmp::{Eq, PartialEq},
    collections::HashSet,
    fmt::Write,
    hash::{Hash, Hasher},
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context as _, anyhow, bail};
use chrono::{DateTime, Local, Timelike, Utc};
use fallible_iterator::FallibleIterator;
use futures::{FutureExt, future::BoxFuture};
use itertools::Itertools;
use rand::random;
use regex::Regex;
use rusqlite::{Error::SqliteFailure, ErrorCode, params};
use serenity::{
    all::{
        AutoArchiveDuration, AutocompleteChoice, CreateAttachment, CreateMessage, CreateThread,
        Http,
    },
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

use serenity_command::{CommandKey, CommandResponse, args, command};
use tokio::time::interval;

use crate::{RegisterableModule, command_context::get_str_opt_ac, db::Db, prelude::*};

const SEPARATORS: &str = "\".,?-!&:*$%#(){}<>'; \t\n|";

pub async fn message_to_quote_contents(
    _handler: &Handler,
    ctx: &Context,
    message: &Message,
) -> anyhow::Result<String> {
    let quote_ndx = message
        .reactions
        .iter()
        .find_position(|r| r.reaction_type == ReactionType::from_str("🗨️").unwrap())
        .map(|(ndx, _)| ndx)
        .unwrap_or(message.reactions.len() as usize);
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
                .id
                .widen()
                .messages(http, GetMessages::new().before(message.id).limit(num as u8))
                .await?;
            messages.extend(
                before
                    .iter()
                    .rev()
                    .map(|msg| (msg.content.to_string(), msg.author.id.get())),
            );
        }
    }
    if messages.is_empty() {
        messages.extend(
            message
                .referenced_message
                .as_ref()
                .map(|msg| (msg.content.to_string(), msg.author.id.get())),
        );
    }
    messages.push((message.content.to_string(), message.author.id.get()));
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

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum AttachmentType {
    Image,
    Video,
    Audio,
    Other,
}

pub struct Attachment {
    pub ty: AttachmentType,
    pub url: String,
    pub name: Option<String>,
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
    pub attachments: Vec<Attachment>,
}

pub fn fetch_quote(db: &Db, guild_id: u64, quote_number: u64) -> anyhow::Result<Option<Quote>> {
    let res = db.conn().query_row(
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
                    attachments: vec![],
                })
            },
        );
    let mut q = match res {
        Ok(q) => q,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(e).context("Error fetching quote"),
    };
    let mut qry = db.conn().prepare(
        "SELECT type, url, name FROM quote_attachments WHERE guild_id = ?1 AND quote_number = ?2 ORDER BY ndx",
    )?;
    let attachments = qry.query_map([guild_id, quote_number], |row| {
        let ty = match row
            .get::<_, String>(0)?
            .as_str()
            .split_once("/")
            .map(|(ty, _)| ty)
            .unwrap_or_default()
        {
            "image" => AttachmentType::Image,
            "video" => AttachmentType::Video,
            "audio" => AttachmentType::Audio,
            _ => AttachmentType::Other,
        };
        Ok(Attachment {
            ty,
            url: row.get(1)?,
            name: row.get(2)?,
        })
    })?;
    for attachment in attachments {
        q.attachments.push(attachment?);
    }
    Ok(Some(q))
}

pub async fn add_quote(
    handler: &Handler,
    ctx: &Context,
    guild_id: u64,
    message: &Message,
) -> anyhow::Result<Option<u64>> {
    let contents = message_to_quote_contents(handler, ctx, message).await?;
    let mut db = handler.db.lock().await;
    let tx = db.conn_mut().transaction()?;
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
    let quote_number = last_quote + 1;
    match tx.execute(
        r"INSERT INTO quote (
    guild_id, channel_id, message_id, ts, quote_number,
    author_id, author_name, contents
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            guild_id,
            channel_id,
            message.id.get(),
            ts.unix_timestamp(),
            quote_number,
            author_id,
            author_name.as_str(),
            contents.trim(),
        ],
    ) {
        Err(SqliteFailure(e, _)) if e.code == ErrorCode::ConstraintViolation => {
            return Ok(None); // Quote already exists
        }
        Ok(n) => Ok(Some(n)),
        Err(e) => Err(e),
    }?;
    let attachments = message.attachments.iter().map(|att| {
        (
            att.content_type.as_deref().unwrap_or_default(),
            att.url.as_str(),
            att.filename.as_str(),
        )
    });
    for (ndx, (ty, url, name)) in attachments.enumerate() {
        tx.execute(
            "INSERT INTO quote_attachments (guild_id, quote_number, type, url, ndx, name) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![guild_id, quote_number, ty, url, ndx, name],
        )?;
    }
    tx.commit()?;
    Ok(Some(last_quote + 1))
}

pub fn get_random_quote(
    db: &Db,
    guild_id: u64,
    user: Option<u64>,
) -> anyhow::Result<Option<Quote>> {
    let number = {
        let mut stmt = db.conn().prepare(
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
pub struct CaseInsensitiveString<'a>(Cow<'a, str>, u64);

impl CaseInsensitiveString<'_> {
    fn new<'a, T: Into<Cow<'a, str>>>(s: T) -> CaseInsensitiveString<'a> {
        let s = s.into();
        let mut hasher = std::hash::DefaultHasher::new();
        s.bytes()
            .filter(|b| !SEPARATORS.as_bytes().contains(b))
            .map(|b| b.to_ascii_lowercase())
            .for_each(|c| hasher.write_u8(c));
        CaseInsensitiveString(s, hasher.finish())
    }

    fn hash(&self) -> u64 {
        self.1
    }
}

impl Hash for CaseInsensitiveString<'_> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write_u64(self.1);
    }
}

impl PartialEq for CaseInsensitiveString<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.1 == other.1
    }
}

impl Eq for CaseInsensitiveString<'_> {}

pub async fn quotes_markov_chain(
    handler: &Handler,
    guild_id: u64,
    user: Option<u64>,
    order: Option<i64>,
) -> anyhow::Result<(markov::Chain<CaseInsensitiveString<'_>>, HashSet<u64>)> {
    let db = handler.db.lock().await;
    let mut stmt = db.conn().prepare(
        "SELECT contents FROM quote WHERE guild_id = ?1 AND (?2 IS NULL or author_id = ?2)",
    )?;
    let mut chain = markov::Chain::of_order(order.unwrap_or(1) as usize);
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
                quotes.insert(CaseInsensitiveString::new(msg.to_string()).hash());
                chain.feed(
                    msg.split_whitespace()
                        .map(|s| CaseInsensitiveString::new(s.to_string()))
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
    let res = db.conn().prepare(
            "SELECT quote_number, contents FROM quote WHERE guild_id = ?1 AND contents LIKE '%'||?2||'%' LIMIT 15",
        )?
            .query(params![guild_id, like])?
            .map(|row| Ok((row.get(0)?, row.get(1)?)))
            .collect()?;
    Ok(res)
}

args!(QUOTE_ARGS =
    "Number the quote was saved as (optional)"
    number[autocomplete]: Option<i64>,
     "Get a random quote from a specific user"
    user: Option<UserId>,
     "Hide the username for even more confusion"
    hide_author: Option<bool>,
);

const GET_QUOTE: CommandConst = CommandConst {
    description: "Retrieve a quote",
    ..command!(/quote QUOTE_ARGS(set_quote_options): get_quote)
};

fn set_quote_options(
    name: &str,
    opt: CreateCommandOption<'static>,
) -> CreateCommandOption<'static> {
    if name == "number" {
        opt.min_int_value(1)
    } else {
        opt
    }
}

pub struct GetQuote {
    pub number: Option<i64>,
    pub user: Option<UserId>,
    pub hide_author: Option<bool>,
}

async fn get_quote(
    (number, user, hide_author): QUOTE_ARGS,
    handler: &Handler,
    ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let guild_id = command.guild_id()?.get();
    let params = GetQuote {
        number,
        user,
        hide_author,
    };
    params.get_quote(handler, ctx, guild_id).await
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
        .to_guild_channel(http, Some(GuildId::new(quote.guild_id)))
        .await;
    let channel_name = channel
        .as_ref()
        .map(|c| c.base.name.as_str())
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
            .description(contents.clone())
            .url(message_url)
            .footer(CreateEmbedFooter::new(format!("in #{channel_name}")))
            .timestamp(model::Timestamp::parse(&quote.ts.format("%+").to_string()).unwrap());

        let mut has_image = false;
        if let Some(image) = quote.image {
            create = create.image(image);
            has_image = true;
        }
        let mut attachments = vec![];
        for (i, att) in quote.attachments.into_iter().enumerate() {
            if att.ty == AttachmentType::Image && !has_image {
                create = create.image(att.url.clone());
                has_image = true;
                continue;
            }
            let name = if let Some(name) = att.name {
                name
            } else {
                let ext = match att.ty {
                    AttachmentType::Audio => ".mp3",
                    AttachmentType::Video => ".mp4",
                    AttachmentType::Image => ".png",
                    AttachmentType::Other => "",
                };
                format!("attachment-{i}{ext}")
            };
            attachments.push((att.url.clone(), name));
        }
        Ok(CommandResponse::Public(
            serenity_command::ResponseType::WithAttachments(
                String::new(),
                vec![create],
                attachments,
            ),
        ))
    }
}

const SAVE_QUOTE: CommandConst = CommandConst {
    description: "Save a message as a quote",
    ..command!(/quote(Message): save_quote)
};

async fn save_quote(
    msg: &Message,
    handler: &Handler,
    ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let guild_id = command.guild_id()?.get();
    // messages received through command interactions are partial
    // retrieve full message to have referenced_message
    let message = ctx.http.get_message(msg.channel_id, msg.id).await?;
    let quote_number = add_quote(handler, ctx, guild_id, &message).await?;
    let link = msg.id.link(msg.channel_id, Some(GuildId::new(guild_id)));
    let resp_text = match quote_number {
        Some(n) => format!("Quote saved as #{n}: {link}"),
        None => "Quote already added".to_string(),
    };
    CommandResponse::public(resp_text)
}

args!(FAKE_QUOTE_ARGS =
    user: Option<UserId>,
    start: Option<String>,
    order: Option<i64>,
);

const FAKE_QUOTE: CommandConst = CommandConst {
    description: "Get a procedurally generated quote",
    ..command!(/fake_quote FAKE_QUOTE_ARGS(set_fake_quote_options): get_fake_quote)
};

fn set_fake_quote_options(
    name: &str,
    opt: CreateCommandOption<'static>,
) -> CreateCommandOption<'static> {
    if name == "order" {
        opt.min_int_value(1)
            .max_int_value(4)
            .description("Markov chain order. Higher = closer to real quotes but more coherent")
    } else {
        opt
    }
}

async fn get_fake_quote(
    (user, start, order): FAKE_QUOTE_ARGS,
    handler: &Handler,
    _ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let (chain, quotes) = quotes_markov_chain(
        handler,
        command
            .guild_id
            .ok_or_else(|| anyhow!("must be run in a guild"))?
            .get(),
        user.map(|u| u.get()),
        order,
    )
    .await?;
    let mut resp = String::new();
    for _ in 0..100 {
        resp = if let Some(start) = &start {
            chain.generate_from_token(CaseInsensitiveString::new(start))
            // chain.generate_str_from_token(&start)
        } else {
            chain.generate()
        }
        .into_iter()
        .map(|CaseInsensitiveString(s, _)| s)
        .join(" ");
        if !(resp.trim().is_empty()
            || quotes.contains(&CaseInsensitiveString::new(resp.as_str()).1))
        {
            break;
        }
        eprintln!("generated a real quote, trying again");
    }
    if resp.is_empty() {
        resp = "Failed to generate quote".to_string();
    } else if let Some(id) = user.map(UserId::get) {
        write!(&mut resp, "\n - <@{id}>").unwrap();
    }
    CommandResponse::public(resp)
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

    let mut has_image = false;
    if let Some(image) = qotd.image {
        embed = embed.image(image);
        has_image = true;
    }
    let mut attachments = vec![];
    for (i, att) in qotd.attachments.iter().enumerate() {
        if att.ty == AttachmentType::Image && !has_image {
            embed = embed.image(&att.url);
            has_image = true;
            continue;
        }
        attachments.push(CreateAttachment::url(http, &att.url, format!("att{i}.png")).await?);
    }
    let channel = ChannelId::new(channel_id);
    let msg = channel
        .widen()
        .send_message(http, CreateMessage::new().embed(embed).files(attachments))
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
                .conn()
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
                    resp.add_choice(AutocompleteChoice::new(q, num))
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
    async fn setup(&mut self, db: &mut crate::db::Db) -> anyhow::Result<()> {
        db.conn().execute(
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
        db.conn().execute(
            "CREATE TABLE IF NOT EXISTS quote_attachments (
                guild_id INTEGER,
                quote_number INTEGER,
                ndx INTEGER,
                type STRING,
                url STRING,
                name STRING,
                UNIQUE(guild_id, quote_number, ndx)
            )",
            [],
        )?;
        db.add_guild_field("qotd_channel_id", "INTEGER")?;
        db.add_guild_field("qotd_last_sent", "STRING")?;
        Ok(())
    }

    fn register_commands(&self, store: &mut dyn Storer) {
        store.register(GET_QUOTE);
        store.register(SAVE_QUOTE);
        store.register(FAKE_QUOTE);
        store.register(Quotes::complete_quotes as CompletionHandler);
    }
}

impl RegisterableModule for Quotes {
    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Ok(Quotes)
    }
}
