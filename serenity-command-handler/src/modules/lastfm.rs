use anyhow::bail;
use chrono::{DateTime, Utc};
use fallible_iterator::FallibleIterator;
use futures::FutureExt;
use futures::future::BoxFuture;
use image::DynamicImage;

use itertools::Itertools;
use regex::Regex;
use reqwest::{Client, Method, StatusCode, Url};
use rspotify::ClientError;
use rusqlite::params;
use serenity::async_trait;
use serenity::builder::{CreateAutocompleteResponse, CreateInteractionResponse};
use serenity::model::prelude::CommandInteraction;
use serenity::model::prelude::CommandType;
use serenity::prelude::{Context, Mutex};
use serenity_command::{CommandKey, CommandResponse, args, command};

use std::env;
use std::fmt::Write;
use std::iter::IntoIterator;
use std::sync::Arc;
use std::time::Duration;

use crate::command_context::{get_focused_option, get_str_opt_ac};
use crate::db::Db;
use crate::modules::{Spotify, Tidal};
use crate::{RegisterableModule, prelude::*};

pub mod model;
use model::*;

mod aoty;
use aoty::GETAOTYS;

pub mod soty;

const API_ENDPOINT: &str = "http://ws.audioscrobbler.com/2.0/";

pub const CHART_SQUARE_SIZE: u32 = 300;

pub const TTL_DAYS: i64 = 30;

pub struct AlbumWithImage {
    album: TopAlbum,
    image: Option<DynamicImage>,
}

/// Lastfm API client
pub struct Lastfm {
    client: Client,
    api_key: String,
}

/// Scrape last.fm to find an album's release year
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
    let Some(cap) = re.captures(&text) else {
        return Ok(None);
    };
    cap.get(1)
        .expect("invalid regex")
        .as_str()
        .rsplit(' ')
        .next()
        .unwrap()
        .parse()
        .map_err(anyhow::Error::from)
        .map(Some)
}

impl Lastfm {
    /// Build a last.fm client using the `LFM_API_KEY` environment variable
    pub fn new() -> Self {
        let api_key = env::var("LFM_API_KEY").unwrap();
        let client = Client::new();
        Lastfm { client, api_key }
    }

    /// Send a last.fm API request
    async fn query<'a, T, I: IntoIterator<Item = (&'static str, &'a str)>>(
        &self,
        method: &str,
        params: I,
    ) -> anyhow::Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        async fn inner(
            client: &Lastfm,
            method: &str,
            params: &[(&str, &str)],
        ) -> anyhow::Result<reqwest::Response> {
            let url = {
                // build request
                let mut url = Url::parse(API_ENDPOINT)?;
                let mut pairs = url.query_pairs_mut();
                pairs
                    .append_pair("method", method)
                    .append_pair("api_key", &client.api_key)
                    .append_pair("format", "json");
                params
                    .iter()
                    .fold(&mut pairs, |pairs, (k, v)| pairs.append_pair(k, v));
                drop(pairs);
                url
            };
            let resp = client.client.get(url).send().await?;
            if resp.status() != StatusCode::OK {
                let text = resp.text().await?;
                bail!("Error getting calling API method {method}: {}", text);
            }
            Ok(resp)
        }
        let params = params.into_iter().collect_vec();
        let resp = inner(self, method, &params).await?;
        resp.json().await.map_err(anyhow::Error::from)
    }

    /// Fetch an artist's top genre tags
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

    /// Get a user's top tracks
    pub async fn get_top_tracks(&self, user: &str, page: Option<u64>) -> anyhow::Result<TopTracks> {
        let mut params: Vec<(&'static str, &str)> = vec![("user", user), ("limit", "200")];

        // format parameter
        let page_s = page.map(|p| p.to_string());
        if let Some(page) = page_s.as_deref() {
            params.push(("page", page));
        }

        // send query
        let top_tracks: TopTracksResponse = self.query("user.gettoptracks", params).await?;
        Ok(top_tracks.toptracks)
    }

    /// Get a user's listening history in the specified range
    pub async fn get_recent_tracks(
        &self,
        user: &str,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
        limit: Option<u64>,
        page: Option<u64>,
    ) -> anyhow::Result<RecentTracks> {
        let mut params: Vec<(&'static str, &str)> = vec![("user", user)];

        // format parameters
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

    /// Get metadata about a track
    pub async fn get_track_info(&self, artist: &str, name: &str) -> anyhow::Result<TrackInfo> {
        let resp: TrackInfoResponse = self
            .query("track.getInfo", [("artist", artist), ("track", name)])
            .await?;
        Ok(resp.track)
    }
}

impl Default for Lastfm {
    fn default() -> Self {
        Self::new()
    }
}

fn err_is_status_code(e: &anyhow::Error, expected: u16) -> bool {
    for err in e.chain() {
        if let Some(ClientError::Http(http_err)) = err.downcast_ref()
            && let rspotify_http::HttpError::StatusCode(code) = http_err.as_ref()
            && code.status() == expected
        {
            return true;
        }
    }
    false
}

/// Try to find an album's release year, scraping it from last.fm first,
/// and querying spotify if that does not work.
/// Once the release year is found, it is cached in the database.
pub async fn get_release_year(
    db: Arc<Mutex<Db>>,
    tidal: Arc<Tidal>,
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
        match tidal.get_album(&artist, &album).await {
            Ok(Some(crate::album::Album {
                release_date: Some(date),
                ..
            })) => {
                let year = date.split('-').next().unwrap().parse().unwrap();
                set_release_year(&db, &artist, &album, year).await?;
                break Ok(Some(year));
            }
            Ok(_) => {
                eprintln!("No release year found for {}", url);
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
                    eprintln!("query {} {} failed: {:?}", artist, album, e);
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

/// Get the release years of a batch of albums from the database.
pub async fn get_release_years<'a, I: IntoIterator<Item = (&'a str, &'a str, usize)>>(
    db: &Mutex<Db>,
    albums: I,
) -> anyhow::Result<Vec<(usize, Result<u64, u64>)>> {
    // use CTE to check many albums at once.
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
    let mut stmt = db.conn().prepare(&query)?;
    stmt.query([])?
        .map(|row| {
            let year: Option<u64> = row.get(1)?;
            let last_checked: Option<u64> = row.get(2)?;
            Ok((row.get(0)?, year.ok_or(last_checked.unwrap_or_default())))
        })
        .collect()
        .map_err(anyhow::Error::from)
}

/// Cache an album's release year in the database.
async fn set_release_year(
    db: &Mutex<Db>,
    artist: &str,
    album: &str,
    year: u64,
) -> anyhow::Result<()> {
    let db = db.lock().await;
    db.conn().execute("INSERT INTO album_cache (artist, album, year) VALUES (LOWER(?1), LOWER(?2), ?3) ON CONFLICT(artist, album) DO NOTHING",
    params![artist, album, year])?;
    Ok(())
}

/// Set the time the album's release year was last checked and not found in the cache.
/// This allows us to try to retrieve it after some time has passed.
async fn set_last_checked(db: &Mutex<Db>, artist: &str, album: &str) -> anyhow::Result<()> {
    let db = db.lock().await;
    db.conn().execute("INSERT INTO album_cache (artist, album, last_checked) VALUES (LOWER(?1), LOWER(?2), ?3) ON CONFLICT(artist, album) DO UPDATE SET last_checked = ?3",
    params![artist.to_lowercase(), album.to_lowercase(), Utc::now().timestamp()])?;
    Ok(())
}

/// Get an album's release year from the database.
/// If the release year is not cached, return the unix timestamp
/// of the last time it was checked as the Err value.
fn get_release_year_db(db: &Db, artist: &str, album: &str) -> Result<u64, u64> {
    let (year, last_checked): (Option<u64>, Option<u64>) = db
        .conn()
        .query_one(
            "SELECT year, last_checked FROM album_cache WHERE artist = LOWER(?1) AND album = LOWER(?2)",
            [artist, album],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .inspect_err(|err| {
            dbg!(err);
        })
        .unwrap_or((None, None));
    match (year, last_checked) {
        (Some(year), _) => Ok(year),
        (None, Some(last_checked)) => Err(last_checked),
        (None, None) => Err(0),
    }
}

args!(FIX_RELEASE_YEAR_ARGS =
    "Album artist"
    artist[autocomplete]: String,
    "Album title"
    album[autocomplete]: String,
    year: i64,
);

const FIX_RELEASE_YEAR: CommandConst = CommandConst {
    description: "Correct or set the release year of an album",
    ..command!(/fix_release_year FIX_RELEASE_YEAR_ARGS: fix_release_year)
};

/// Change the release year of the specified album in cache.
async fn fix_release_year(
    (artist, album, year): FIX_RELEASE_YEAR_ARGS,
    handler: &Handler,
    _ctx: &Context,
    _command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    let db = handler.db.lock().await;
    let current_value = match get_release_year_db(&db, &artist, &album) {
        Ok(y) if y == year as u64 => bail!("Release year is already {year}"),
        Ok(y) => Some(y),
        Err(0) => bail!("Album not found in database, check spelling?"),
        _ => None,
    };
    db.conn().execute(
        "UPDATE album_cache SET year = ?3, last_checked = 0 WHERE artist = ?1 AND album = ?2",
        params![artist.to_lowercase(), album.to_lowercase(), year],
    )?;
    let mut resp = format!("Updated release year of {artist} - {album} to {year}",);
    if let Some(prev) = current_value {
        resp.push_str(&format!(" (was {prev})"));
    }
    CommandResponse::public(resp)
}

/// Complete an album's title or artist, using the release year cache.
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

        // build query
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
            let mut stmt = db.conn().prepare(&qry)?;
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
                complete.add_choice(val.to_string())
            });
        ac.create_response(&ctx.http, CreateInteractionResponse::Autocomplete(complete))
            .await?;
        Ok(true)
    }
    .boxed()
}

#[async_trait]
impl Module for Lastfm {
    async fn setup(&mut self, db: &mut Db) -> anyhow::Result<()> {
        db.conn().execute(
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

    fn register_commands(&self, store: &mut dyn Storer) {
        store.register(GETAOTYS);
        store.register(FIX_RELEASE_YEAR);
        store.register(complete_album as CompletionHandler);
    }
}

impl RegisterableModule for Lastfm {
    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Ok(Lastfm::new())
    }

    async fn add_dependencies(builder: HandlerBuilder) -> anyhow::Result<HandlerBuilder> {
        builder.module::<Spotify>().await
    }
}
