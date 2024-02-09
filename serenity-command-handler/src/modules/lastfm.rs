use anyhow::{bail, Context as _};
use chrono::{DateTime, Datelike, TimeZone, Utc};
use fallible_iterator::FallibleIterator;
use futures::future::BoxFuture;
use futures::{Future, FutureExt, Stream, StreamExt, TryStreamExt};
use image::imageops::FilterType;
use image::io::Reader;
use image::{DynamicImage, GenericImage, ImageOutputFormat, RgbaImage};
use itertools::Itertools;
use regex::Regex;
use reqwest::{Client, Method, StatusCode, Url};
use rspotify::ClientError;
use rusqlite::params;
use serde::Deserialize;
use serenity::async_trait;
use serenity::builder::{
    CreateAttachment, CreateAutocompleteResponse, CreateEmbed, CreateInteractionResponse,
    CreateInteractionResponseFollowup, EditInteractionResponse,
};
use serenity::json::JsonMap;
use serenity::model::prelude::CommandInteraction;
use serenity::model::prelude::CommandType;
use serenity::prelude::{Context, Mutex};
use serenity_command::{BotCommand, CommandKey, CommandResponse};

use std::borrow::Cow;
use std::collections::HashMap;
use std::env;
use std::fmt::Write;
use std::io::Cursor;
use std::iter::IntoIterator;
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::Duration;

use crate::command_context::{get_focused_option, get_str_opt_ac};
use crate::db::Db;
use crate::modules::Spotify;
use crate::prelude::*;
use serenity_command_derive::Command;

const API_ENDPOINT: &str = "http://ws.audioscrobbler.com/2.0/";

const CHART_SQUARE_SIZE: u32 = 300;

const TTL_DAYS: i64 = 30;

pub struct Lastfm {
    client: Client,
    api_key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Tag {
    pub count: u64,
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopTags {
    pub tag: Vec<Tag>,
    #[serde(rename = "@attr")]
    pub attributes: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtistTopTags {
    pub toptags: TopTags,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Date {
    pub uts: String,
    #[serde(rename = "#text")]
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtistId {
    pub mbid: String,
    #[serde(rename = "#text")]
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlbumId {
    pub mbid: String,
    #[serde(rename = "#text")]
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Image {
    pub size: String,
    #[serde(rename = "#text")]
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecentTrackAttrs {
    pub nowplaying: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Track {
    pub name: String,
    pub url: String,
    pub mbid: String,
    pub date: Option<Date>,
    pub artist: ArtistId,
    pub album: AlbumId,
    pub image: Vec<Image>,
    #[serde(rename = "@attr")]
    pub attr: Option<RecentTrackAttrs>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecentTracksAttrs {
    pub user: String,
    #[serde(rename = "totalPages")]
    pub total_pages: String,
    pub total: String,
    pub page: String,
    #[serde(rename = "perPage")]
    pub per_page: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecentTracks {
    pub track: Vec<Track>,
    #[serde(rename = "@attr")]
    pub attr: RecentTracksAttrs,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecentTracksResp {
    pub recenttracks: RecentTracks,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Album {
    pub name: String,
    pub url: String,
    pub mbid: String,
    pub playcount: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtistShort {
    pub url: String,
    pub name: String,
    pub mbid: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopAlbum {
    pub name: String,
    pub mbid: String,
    pub url: String,
    pub artist: ArtistShort,
    pub image: Vec<Image>,
    pub playcount: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopAlbumsAttr {
    pub user: String,
    #[serde(rename = "totalPages")]
    pub total_pages: String,
    pub page: String,
    pub total: String,
    #[serde(rename = "perPage")]
    pub per_page: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopAlbums {
    pub album: Vec<TopAlbum>,
    #[serde(rename = "@attr")]
    pub attr: TopAlbumsAttr,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopAlbumsResp {
    pub topalbums: TopAlbums,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopTrack {
    pub name: String,
    pub mbid: Option<String>,
    pub artist: ArtistShort,
    pub url: String,
    pub playcount: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopTracks {
    pub track: Vec<TopTrack>,
    #[serde(rename = "@attr")]
    pub attr: TopAlbumsAttr,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopTracksResponse {
    pub toptracks: TopTracks,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlbumShort {
    pub artist: String,
    pub title: String,
    pub mbid: Option<String>,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TrackInfo {
    pub name: String,
    pub mbid: Option<String>,
    pub url: String,
    pub artist: ArtistShort,
    pub album: Option<AlbumShort>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TrackInfoResponse {
    pub track: TrackInfo,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MbReleaseInfo {
    pub id: String,
    pub title: String,
    pub date: String,
}

#[derive(Command, Debug)]
#[cmd(name = "aoty", desc = "Get your albums of the year")]
pub struct GetAotys {
    #[cmd(desc = "Last.fm username")]
    pub username: String,
    pub year: Option<i64>,
    pub year_range: Option<String>,
    #[cmd(desc = "Skip albums without album art")]
    pub skip: Option<bool>,
}

#[async_trait]
impl BotCommand for GetAotys {
    type Data = Handler;

    async fn run(
        self,
        handler: &Handler,
        ctx: &Context,
        opts: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        opts.create_response(
            &ctx.http,
            CreateInteractionResponse::Defer(Default::default()),
        )
        .await?;
        if let Err(e) = self.get_aotys(handler, ctx, opts).await {
            eprintln!("get aotys failed: {:?}", &e);
            opts.create_followup(
                &ctx.http,
                CreateInteractionResponseFollowup::new().content(e.to_string()),
            )
            .await?;
        }
        Ok(CommandResponse::None)
    }
}

impl GetAotys {
    async fn get_aotys(
        self,
        handler: &Handler,
        ctx: &Context,
        opts: &CommandInteraction,
    ) -> anyhow::Result<()> {
        let lastfm: Arc<Lastfm> = handler.module_arc()?;
        let spotify: Arc<Spotify> = handler.module_arc()?;
        let db = Arc::clone(&handler.db);
        let year_range = self
            .year_range
            .as_deref()
            .and_then(|range| range.split_once('-'))
            .and_then(|(start, end)| {
                start
                    .parse::<u64>()
                    .and_then(|start| end.parse::<u64>().map(|end| start..=end))
                    .ok()
            })
            .unwrap_or_else(|| {
                let y = self
                    .year
                    .map(|yr| yr as u64)
                    .unwrap_or_else(|| Utc::now().year() as u64);
                y..=y
            });
        let start = year_range.start();
        let end = year_range.end();
        let year_fmt = if end - start <= 1 {
            start.to_string()
        } else {
            format!("{start}-{end}")
        };
        let mut aotys = lastfm
            .get_albums_of_the_year(db, spotify, &self.username, &year_range)
            .await?;
        let http = &ctx.http;
        if aotys.is_empty() {
            opts.create_followup(
                http,
                CreateInteractionResponseFollowup::new().content(format!(
                    "No {} albums found for user {}",
                    &year_fmt, &self.username
                )),
            )
            .await?;
            return Ok(());
        }
        aotys.truncate(25);
        let image = create_aoty_chart(&aotys, self.skip.unwrap_or(false)).await?;
        let mut content = format!("**Top albums of {} for {}**", &year_fmt, &self.username);
        aotys
            .iter()
            .map(|ab| &ab.album)
            .map(|ab| {
                format!(
                    "{} - {} ({} plays)",
                    &ab.artist.name, &ab.name, &ab.playcount
                )
            })
            .for_each(|line| {
                content.push('\n');
                content.push_str(&line);
            });
        opts.create_followup(
            http,
            CreateInteractionResponseFollowup::new()
                .content(content)
                .add_file(CreateAttachment::bytes(
                    Cow::Owned(image),
                    format!("{}_aoty_{}.png", &self.username, &year_fmt),
                )),
        )
        .await?;
        Ok(())
    }
}

pub struct AlbumWithImage {
    album: TopAlbum,
    image: Option<DynamicImage>,
}

impl TopAlbum {
    fn get_image(&self) -> impl 'static + Future<Output = anyhow::Result<Option<DynamicImage>>> {
        let image = self.image.iter().last().map(|img| img.url.clone());

        async move {
            let Some(image_url) = image else {
                return Ok(None);
            };
            let reader = match reqwest::get(&image_url).await {
                Ok(resp) => Reader::new(Cursor::new(
                    resp.bytes().await.context("Error getting album cover")?,
                )),
                Err(_) => return Ok(None),
            };
            let img = reader.with_guessed_format()?.decode()?.resize(
                CHART_SQUARE_SIZE,
                CHART_SQUARE_SIZE,
                FilterType::Triangle,
            );
            Ok(Some(img))
        }
        .boxed()
    }
}

pub async fn create_aoty_chart(albums: &[AlbumWithImage], skip: bool) -> anyhow::Result<Vec<u8>> {
    let n = (albums.len() as f32).sqrt().ceil() as u32;
    eprintln!("Creating {n}x{n} chart");
    let len = n * CHART_SQUARE_SIZE;
    let mut height = n;
    while (height - 1) * n >= albums.len() as u32 {
        height -= 1;
    }
    let mut out = RgbaImage::new(len, height * CHART_SQUARE_SIZE);
    let mut offset = 0;
    for (mut i, ab) in albums.iter().enumerate() {
        let Some(img) = ab.image.as_ref() else {
            offset += 1;
            continue;
        };
        if skip {
            i -= offset;
        }
        let y = (i as u32 / n) * CHART_SQUARE_SIZE;
        let x = (i as u32 % n) * CHART_SQUARE_SIZE;
        out.copy_from(img, x, y)?;
    }
    let buf = Vec::new();
    let mut writer = Cursor::new(buf);
    out.write_to(&mut writer, ImageOutputFormat::Png)?;
    Ok(writer.into_inner())
}

#[derive(Command, Debug)]
#[cmd(name = "soty", desc = "Get your songs of the year")]
pub struct GetSotys {
    #[cmd(desc = "Last.fm username")]
    pub username: String,
    pub year: Option<i64>,
    #[cmd(desc = "Skip albums without album art")]
    pub skip: Option<bool>,
}

#[async_trait]
impl BotCommand for GetSotys {
    type Data = Handler;

    async fn run(
        self,
        handler: &Handler,
        ctx: &Context,
        opts: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        opts.create_response(
            &ctx.http,
            CreateInteractionResponse::Defer(Default::default()),
        )
        .await?;
        self.get_soty(handler, ctx, opts).await?;
        Ok(CommandResponse::None)
    }
}

impl GetSotys {
    async fn get_soty(
        self,
        handler: &Handler,
        ctx: &Context,
        opts: &CommandInteraction,
    ) -> anyhow::Result<()> {
        let year = self
            .year
            .map(|yr| yr as u64)
            .unwrap_or_else(|| Utc::now().year() as u64);
        let lastfm: Arc<Lastfm> = handler.module_arc()?;
        let spotify: Arc<Spotify> = handler.module_arc()?;
        let mut songs = lastfm
            .get_songs_of_the_year(
                Arc::clone(&handler.db),
                spotify,
                self.username.clone(),
                year,
            )
            .await?;
        songs.truncate(25);
        let content = songs
            .iter()
            .map(|song| {
                format!(
                    "**{}** - *{}* ({} plays)",
                    &song.artist.name, &song.name, &song.playcount
                )
            })
            .join("\n");
        let embed = CreateEmbed::default()
            .description(content)
            .title(format!("Top songs of {year} for {}", &self.username));
        opts.edit_response(&ctx.http, EditInteractionResponse::new().embed(embed))
            .await?;
        Ok(())
    }
}

async fn retrieve_release_year(url: &str) -> anyhow::Result<Option<u64>> {
    let client = reqwest::Client::new();
    let resp = client
        .request(Method::GET, url)
        .header("accept", "text/html")
        .header("user-agent", "lpbot (0.1.0)")
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        bail!("{}", status.canonical_reason().unwrap_or_default());
    }
    let text = resp.text().await?;
    let re = Regex::new(r"(?m)<dt.+>Release Date</dt>\s*<dd[^>]+>([^<]+)<").unwrap();
    if let Some(cap) = re.captures(&text) {
        cap.get(1)
            .unwrap()
            .as_str()
            .rsplit(' ')
            .next()
            .unwrap()
            .parse()
            .map_err(anyhow::Error::from)
            .map(Some)
    } else {
        Ok(None)
    }
}

impl Lastfm {
    pub fn new() -> Self {
        let api_key = env::var("LFM_API_KEY").unwrap();
        let client = Client::new();
        Lastfm { client, api_key }
    }

    async fn query<'a, T, I: IntoIterator<Item = (&'static str, &'a str)>>(
        &self,
        method: &str,
        params: I,
    ) -> anyhow::Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let mut url = Url::parse(API_ENDPOINT)?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs
                .append_pair("method", method)
                .append_pair("api_key", &self.api_key)
                .append_pair("format", "json");
            params
                .into_iter()
                .fold(&mut pairs, |pairs, (k, v)| pairs.append_pair(k, v));
        }
        let resp = self.client.get(url).send().await?;
        if resp.status() != StatusCode::OK {
            let map: JsonMap = resp.json().await?;
            bail!("Error getting top albums: {:?}", map);
        }
        resp.json().await.map_err(anyhow::Error::from)
    }

    pub async fn artist_top_tags(&self, artist: &str) -> anyhow::Result<Vec<String>> {
        let top_tags: ArtistTopTags = self
            .query("artist.getTopTags", [("artist", artist)])
            .await?;
        Ok(top_tags
            .toptags
            .tag
            .into_iter()
            .take(5)
            .map(|t| t.name)
            .collect())
    }

    pub async fn get_recent_tracks(
        &self,
        user: &str,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        limit: Option<u64>,
        page: Option<u64>,
    ) -> anyhow::Result<RecentTracks> {
        let mut params: Vec<(&'static str, &str)> = vec![("user", user)];

        let from_s = from.map(|from| from.timestamp().to_string());
        if let Some(from) = from_s.as_deref() {
            params.push(("from", from));
        }
        let to_s = to.map(|to| to.timestamp().to_string());
        if let Some(to) = to_s.as_deref() {
            params.push(("to", to));
        }
        let limit_s = limit.map(|limit| limit.to_string());
        if let Some(limit) = limit_s.as_deref() {
            params.push(("limit", limit));
        }
        let page_s = page.map(|page| page.to_string());
        if let Some(page) = page_s.as_deref() {
            params.push(("page", page));
        }

        let recent_tracks: RecentTracksResp = self.query("user.getrecenttracks", params).await?;
        Ok(recent_tracks.recenttracks)
    }

    pub async fn get_track_info(&self, artist: &str, name: &str) -> anyhow::Result<TrackInfo> {
        let resp: TrackInfoResponse = self
            .query("track.getInfo", [("artist", artist), ("track", name)])
            .await?;
        Ok(resp.track)
    }

    pub async fn get_top_albums(
        self: Arc<Self>,
        user: String,
        page: Option<u64>,
        current_year: bool,
    ) -> anyhow::Result<TopAlbums> {
        // using a limit of 500 because somewhere above that number lastfm stops including
        // image links. this limit seems to vary somehow?
        let mut params: Vec<(&'static str, &str)> = vec![("user", &user), ("limit", "500")];

        let page_s = page.map(|p| p.to_string());
        if let Some(page) = page_s.as_deref() {
            params.push(("page", page));
        }

        if current_year {
            params.push(("period", "12month"))
        }

        let top_albums: TopAlbumsResp = self.query("user.gettopalbums", params).await?;
        Ok(top_albums.topalbums)
    }

    pub async fn get_top_tracks(&self, user: &str, page: Option<u64>) -> anyhow::Result<TopTracks> {
        let mut params: Vec<(&'static str, &str)> = vec![("user", user), ("limit", "200")];

        let page_s = page.map(|p| p.to_string());
        if let Some(page) = page_s.as_deref() {
            params.push(("page", page));
        }

        let top_tracks: TopTracksResponse = self.query("user.gettoptracks", params).await?;
        Ok(top_tracks.toptracks)
    }

    pub fn top_albums_stream_inner(
        self: Arc<Self>,
        user: String,
        current_year: bool,
    ) -> impl Stream<Item = impl Future<Output = anyhow::Result<TopAlbums>>> {
        tokio_stream::iter(1..).map(move |i| {
            let user = user.clone();
            let lfm = Arc::clone(&self);
            eprintln!("querying page {i}");
            lfm.get_top_albums(user, Some(i), current_year)
        })
    }

    pub fn top_albums_stream(
        self: Arc<Self>,
        user: String,
        current_year: bool,
    ) -> impl Stream<Item = anyhow::Result<TopAlbums>> {
        self.top_albums_stream_inner(user, current_year)
            .buffered(2)
            .try_take_while(|ta| {
                let total_pages = ta.attr.total_pages.parse::<u64>().unwrap();
                let page = ta.attr.page.parse::<u64>().unwrap();
                async move { Ok(page <= total_pages) }
            })
    }

    pub async fn get_albums_of_the_year(
        self: Arc<Self>,
        db: Arc<Mutex<Db>>,
        spotify: Arc<Spotify>,
        user: &str,
        year_range: &RangeInclusive<u64>,
    ) -> anyhow::Result<Vec<AlbumWithImage>> {
        let mut aotys = Vec::<TopAlbum>::new();
        let mut img_futures = Vec::new();
        let current_year = *year_range.start() == Utc::now().year() as u64;
        let mut stream = Arc::clone(&self)
            .top_albums_stream(user.to_string(), current_year)
            .try_take_while(|ta| {
                let first_plays = ta
                    .album
                    .first()
                    .map(|ab| ab.playcount.parse::<u64>().unwrap())
                    .unwrap_or_default();
                async move { Ok(first_plays >= 4) }
            })
            .boxed();
        while let Some(res) = stream.next().await {
            eprintln!("Retrieved page");
            let top_albums = res?;
            let tuples = top_albums
                .album
                .iter()
                .enumerate()
                .map(|(i, ab)| (ab.artist.name.as_str(), ab.name.as_str(), i));
            let res = get_release_years(&db, tuples).await?;
            eprintln!(
                "Found {}/{} release years in db",
                res.len(),
                top_albums.album.len()
            );
            let mut years: Vec<Result<u64, u64>> = vec![Err(0); top_albums.album.len()];
            res.into_iter().for_each(|(i, year)| years[i] = year);
            let fetches = futures::stream::iter(
                top_albums
                    .album
                    .iter()
                    .cloned()
                    .enumerate()
                    .filter(|(_, ab)| ab.playcount.parse::<u64>().unwrap() >= 4)
                    .filter_map(|(i, ab)| years[i].err().map(|last_checked| (i, ab, last_checked)))
                    .map(|(i, ab, last_checked)| {
                        tokio::spawn({
                            let year_fut = get_release_year(
                                Arc::clone(&db),
                                Arc::clone(&spotify),
                                ab.artist.name.clone(),
                                ab.name.clone(),
                                ab.url,
                            );
                            async move {
                                let last_checked = Utc
                                    .timestamp_opt(last_checked as i64, 0)
                                    .earliest()
                                    .unwrap_or_default();
                                if (Utc::now() - last_checked).num_days() < TTL_DAYS {
                                    return Ok((i, None));
                                }
                                year_fut.await.map(|yr| (i, yr))
                            }
                        })
                    }),
            )
            .buffer_unordered(50)
            .map(|res| match res {
                Ok(inner) => inner,
                Err(e) => Err(anyhow::Error::from(e)),
            })
            .map(|res| match res {
                Ok((i, yr)) => Ok((i, yr.map(|yr| year_range.contains(&yr)).unwrap_or(false))),
                Err(e) => Err(e),
            })
            .try_collect::<HashMap<usize, bool>>();
            let mut album_infos = fetches.await?;
            for (i, yr) in years.iter().enumerate() {
                if let Ok(year) = yr {
                    album_infos.entry(i).or_insert(year_range.contains(year));
                }
            }
            aotys.extend(
                top_albums
                    .album
                    .into_iter()
                    .enumerate()
                    .filter(|(i, _)| album_infos.get(i).copied() == Some(true))
                    .map(|(_, ab)| ab)
                    .inspect(|ab| img_futures.push(tokio::spawn(ab.get_image()))),
            );
            if aotys.len() > 25 {
                break;
            }
        }
        let mut out = Vec::with_capacity(aotys.len());
        for (album, fut) in aotys.into_iter().zip(img_futures.into_iter()) {
            let image = fut.await?.ok().flatten();
            out.push(AlbumWithImage { album, image })
        }
        Ok(out)
    }

    pub async fn get_songs_of_the_year(
        self: Arc<Self>,
        db: Arc<Mutex<Db>>,
        spotify: Arc<Spotify>,
        user: String,
        year: u64,
    ) -> anyhow::Result<Vec<TopTrack>> {
        let mut sotys = Vec::<TopTrack>::new();
        let mut page = 1;
        let mut top_songs_fut = Some(tokio::spawn({
            let user = user.to_string();
            let lastfm = Arc::clone(&self);
            let page = page;
            async move { lastfm.get_top_tracks(&user, Some(page)).await }
        }));
        loop {
            eprintln!("Querying page {page}");
            let top_songs = match top_songs_fut.take() {
                Some(fut) => fut.await?.context("Error getting top albums")?,
                None => break,
            };
            let last_plays: Option<u64> = top_songs
                .track
                .last()
                .map(|ab| ab.playcount.parse().unwrap());
            let total_pages = top_songs
                .attr
                .total_pages
                .parse::<u64>()
                .context("Invalid response from last.fm")?;
            if page < total_pages && last_plays.unwrap_or_default() >= 5 {
                page += 1;
                top_songs_fut = Some(tokio::spawn({
                    let user = user.to_string();
                    let lastfm = Arc::clone(&self);
                    let page = page;
                    async move { lastfm.get_top_tracks(&user, Some(page)).await }
                }));
            }
            for song in &top_songs.track {
                let info = self.get_track_info(&song.artist.name, &song.name).await?;
                let Some(album) = info.album else { continue };
                let cached_year = {
                    let db = db.lock().await;
                    get_release_year_db(&db, &album.artist, &album.title)
                };
                let Some(yr) = (match cached_year {
                    Ok(year) => Some(year),
                    Err(last_checked) => {
                        let last_checked = Utc
                            .timestamp_opt(last_checked as i64, 0)
                            .earliest()
                            .unwrap_or_default();
                        if (Utc::now() - last_checked).num_days() < TTL_DAYS {
                            None
                        } else {
                            get_release_year(
                                Arc::clone(&db),
                                Arc::clone(&spotify),
                                album.artist,
                                album.title,
                                album.url,
                            )
                            .await?
                        }
                    }
                }) else {
                    continue;
                };
                if yr != year {
                    continue;
                };
                sotys.push(song.clone());
                if sotys.len() >= 25 {
                    break;
                }
            }
            if top_songs_fut.is_none() || sotys.len() >= 25 {
                break;
            }
        }
        Ok(sotys)
    }
}

impl Default for Lastfm {
    fn default() -> Self {
        Self::new()
    }
}

fn err_is_status_code(e: &anyhow::Error, expected: u16) -> bool {
    for err in e.chain() {
        if let Some(ClientError::Http(http_err)) = err.downcast_ref() {
            if let rspotify_http::HttpError::StatusCode(code) = http_err.as_ref() {
                if code.status() == expected {
                    return true;
                }
            }
        }
    }
    false
}

async fn get_release_year(
    db: Arc<Mutex<Db>>,
    spotify: Arc<Spotify>,
    artist: String,
    album: String,
    url: String,
) -> anyhow::Result<Option<u64>> {
    let lastfm_release_year = retrieve_release_year(&url).await;
    match lastfm_release_year {
        Ok(Some(year)) => {
            set_release_year(&db, &artist, &album, year).await?;
            return Ok(Some(year));
        }
        Err(e) => eprintln!("Error getting release year from lastfm: {e}"),
        _ => (),
    }
    // Backoff loop
    loop {
        match spotify.get_album(&artist, &album).await {
            Ok(Some(crate::album::Album {
                release_date: Some(date),
                ..
            })) => {
                let year = date.split('-').next().unwrap().parse().unwrap();
                set_release_year(&db, &artist, &album, year).await?;
                break Ok(Some(year));
            }
            Ok(_) => {
                eprintln!("No release year found for {}", &url);
                set_last_checked(&db, &artist, &album).await?;
                break Ok(None);
            }
            Err(e) => {
                let retry = err_is_status_code(&e, 429);
                if &e.to_string() == "Not found" {
                    set_last_checked(&db, &artist, &album).await?;
                    break Ok(None);
                }
                if !retry {
                    eprintln!("query {} {} failed: {:?}", &artist, &album, &e);
                    set_last_checked(&db, &artist, &album).await?;
                    // discard error, best effort
                    break Ok(None);
                }
                // Wait before retrying
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

pub async fn get_release_years<'a, I: IntoIterator<Item = (&'a str, &'a str, usize)>>(
    db: &Mutex<Db>,
    albums: I,
) -> anyhow::Result<Vec<(usize, Result<u64, u64>)>> {
    let mut query = "WITH albums_in(artist, album, pos) AS(VALUES".to_string();
    albums.into_iter().enumerate().for_each(|(i, ab)| {
        if i > 0 {
            query.push(',');
        }
        write!(
            &mut query,
            "(lower('{}'), lower('{}'), {})",
            crate::db::escape_str(ab.0),
            crate::db::escape_str(ab.1),
            ab.2
        )
        .unwrap();
    });
    query.push_str(
        ")
        SELECT albums_in.pos, album_cache.year, album_cache.last_checked
        FROM album_cache JOIN albums_in
        ON albums_in.artist = album_cache.artist
        AND albums_in.album = album_cache.album",
    );
    let db = db.lock().await;
    let mut stmt = db.conn.prepare(&query)?;
    let res = stmt
        .query([])?
        .map(|row| {
            let year: Option<u64> = row.get(1)?;
            let last_checked: Option<u64> = row.get(2)?;
            Ok((row.get(0)?, year.ok_or(last_checked.unwrap_or_default())))
        })
        .collect()
        .map_err(anyhow::Error::from);
    res
}

async fn set_release_year(
    db: &Mutex<Db>,
    artist: &str,
    album: &str,
    year: u64,
) -> anyhow::Result<()> {
    let db = db.lock().await;
    db.conn.execute("INSERT INTO album_cache (artist, album, year) VALUES (lower(?1), lower(?2), ?3) ON CONFLICT(artist, album) DO NOTHING",
    params![artist, album, year])?;
    Ok(())
}

async fn set_last_checked(db: &Mutex<Db>, artist: &str, album: &str) -> anyhow::Result<()> {
    let db = db.lock().await;
    db.conn.execute("INSERT INTO album_cache (artist, album, last_checked) VALUES (?1, ?2, ?3) ON CONFLICT(artist, album) DO UPDATE SET last_checked = ?3",
    params![artist.to_lowercase(), album.to_lowercase(), Utc::now().timestamp()])?;
    Ok(())
}

fn get_release_year_db(db: &Db, artist: &str, album: &str) -> Result<u64, u64> {
    let (year, last_checked): (Option<u64>, Option<u64>) = db
        .conn
        .query_row(
            "SELECT year, last_checked FROM album_cache WHERE artist = ?1 AND album = ?2",
            [artist.to_lowercase(), album.to_lowercase()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap_or((None, None));
    match (year, last_checked) {
        (Some(year), _) => Ok(year),
        (None, Some(last_checked)) => Err(last_checked),
        (None, None) => Err(0),
    }
}

#[derive(Command, Debug)]
#[cmd(
    name = "fix_release_year",
    desc = "Correct or set the release year of an album"
)]
pub struct FixReleaseYear {
    #[cmd(desc = "Album artist", autocomplete)]
    pub artist: String,
    #[cmd(desc = "Album title", autocomplete)]
    pub album: String,
    pub year: i64,
}

#[async_trait]
impl BotCommand for FixReleaseYear {
    type Data = Handler;

    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        _opts: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let db = handler.db.lock().await;
        let current_value = match get_release_year_db(&db, &self.artist, &self.album) {
            Ok(year) if year == self.year as u64 => bail!("Release year is already {year}"),
            Ok(year) => Some(year),
            Err(0) => bail!("Album not found in database, check spelling?"),
            _ => None,
        };
        db.conn.execute(
            "UPDATE album_cache SET year = ?3, last_checked = 0 WHERE artist = ?1 AND album = ?2",
            params![
                self.artist.to_lowercase(),
                self.album.to_lowercase(),
                self.year
            ],
        )?;
        let mut resp = format!(
            "Updated release year of {} - {} to {}",
            &self.artist, &self.album, self.year
        );
        if let Some(prev) = current_value {
            resp.push_str(&format!(" (was {prev})"));
        }
        CommandResponse::public(resp)
    }
}

#[allow(clippy::let_and_return)] // doesn't compile if the lint is obeyed....
fn complete_album<'a>(
    handler: &'a Handler,
    ctx: &'a Context,
    key: CommandKey<'a>,
    ac: &'a CommandInteraction,
) -> BoxFuture<'a, anyhow::Result<bool>> {
    async move {
        if key != ("fix_release_year", CommandType::ChatInput) {
            return Ok(false);
        }

        let options = &ac.data.options;
        let Some(focused) = get_focused_option(options) else {
            return Ok(false);
        };

        let artist = get_str_opt_ac(options, "artist").unwrap_or_default();
        let album = get_str_opt_ac(options, "album").unwrap_or_default();

        let field = match focused {
            "artist" | "album" => focused,
            _ => bail!("Invalid option '{focused}'"),
        };
        let qry = format!(
            "SELECT {field} FROM album_cache
                          WHERE artist LIKE '%' || ?1 || '%' AND album LIKE '%' || ?2 || '%'
                          GROUP BY {field}
                          LIMIT 15"
        );

        let values: Vec<String> = {
            let db = handler.db.lock().await;
            let mut stmt = db.conn.prepare(&qry)?;
            let values = stmt
                .query_map([artist.to_lowercase(), album.to_lowercase()], |row| {
                    row.get(0)
                })?
                .collect::<Result<_, _>>()?;
            values
        };

        let complete = values
            .iter()
            .fold(CreateAutocompleteResponse::new(), |complete, val| {
                complete.add_string_choice(val, val)
            });
        ac.create_response(&ctx.http, CreateInteractionResponse::Autocomplete(complete))
            .await?;
        Ok(true)
    }
    .boxed()
}

#[async_trait]
impl Module for Lastfm {
    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Ok(Lastfm::new())
    }

    async fn add_dependencies(builder: HandlerBuilder) -> anyhow::Result<HandlerBuilder> {
        builder.module::<Spotify>().await
    }

    async fn setup(&mut self, db: &mut Db) -> anyhow::Result<()> {
        db.conn.execute(
            "CREATE TABLE IF NOT EXISTS album_cache (
            artist STRING NOT NULL,
            album STRING NOT NULL,
            year INTEGER,
            last_checked INTEGER,
            UNIQUE(artist, album)
        )",
            [],
        )?;
        Ok(())
    }

    fn register_commands(&self, store: &mut CommandStore, completions: &mut CompletionStore) {
        store.register::<GetAotys>();
        store.register::<FixReleaseYear>();
        completions.push(complete_album);
    }
}
