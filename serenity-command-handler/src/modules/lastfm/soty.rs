use std::sync::Arc;

use anyhow::Context as _;
use chrono::{Datelike, TimeZone, Utc};
use itertools::Itertools;
use serenity::all::{
    CommandInteraction, Context, CreateEmbed, CreateInteractionResponse, EditInteractionResponse,
};
use tokio::sync::Mutex;

use super::Lastfm;
use crate::{
    db::Db,
    modules::{
        Tidal,
        lastfm::{TTL_DAYS, get_release_year, get_release_year_db, model::TopTrack},
    },
    prelude::*,
};

args!(SOTY_ARGS =
"Last.fm username"
username: String,
year: Option<i64>,
"Skip albums without album art"
skip: Option<bool>,
);

pub const SOTY: CommandConst = CommandConst {
    description: "Get your songs of the year",
    ..command!(/soty SOTY_ARGS: get_soty)
};

#[derive(Debug)]
pub struct GetSotys {
    pub username: String,
    pub year: Option<i64>,
    pub skip: Option<bool>,
}

async fn get_soty(
    (username, year, skip): SOTY_ARGS,
    handler: &Handler,
    ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    command
        .create_response(
            &ctx.http,
            CreateInteractionResponse::Defer(Default::default()),
        )
        .await?;
    let params = GetSotys {
        username,
        year,
        skip,
    };
    params.get_soty(handler, ctx, command).await?;
    Ok(CommandResponse::None)
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
        let tidal: Arc<Tidal> = handler.module_arc()?;
        let mut songs = lastfm
            .get_songs_of_the_year(Arc::clone(&handler.db), tidal, self.username.clone(), year)
            .await?;
        songs.truncate(25);
        let content = songs
            .iter()
            .map(|song| {
                format!(
                    "**{}** - *{}* ({} plays)",
                    song.artist.name, song.name, song.playcount
                )
            })
            .join("\n");
        let embed = CreateEmbed::default()
            .description(content)
            .title(format!("Top songs of {year} for {}", self.username));
        opts.edit_response(&ctx.http, EditInteractionResponse::new().embed(embed))
            .await?;
        Ok(())
    }
}

impl Lastfm {
    pub async fn get_songs_of_the_year(
        self: Arc<Self>,
        db: Arc<Mutex<Db>>,
        tidal: Arc<Tidal>,
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
                                Arc::clone(&tidal),
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
