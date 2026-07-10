use std::{collections::HashMap, io::Cursor, ops::RangeInclusive, sync::Arc};

use anyhow::Context as _;
use chrono::{Datelike, TimeZone, Utc};
use image::{
    DynamicImage, GenericImage, ImageFormat, ImageReader, RgbaImage, imageops::FilterType,
};
use serenity::all::{
    CommandInteraction, Context, CreateAttachment, CreateInteractionResponseFollowup,
};
use serenity::futures::{FutureExt, Stream, StreamExt, TryStreamExt};
use tokio::sync::Mutex;

use super::{
    AlbumWithImage, TTL_DAYS, get_release_year, get_release_years,
    model::{TopAlbums, TopAlbumsResp},
};
use super::{CHART_SQUARE_SIZE, Lastfm, TopAlbum};
use serenity_command_handler::{db::Db, prelude::*};
use tidal::Tidal;

args!(GETAOTYS_ARGS =
    "Last.fm username"
    username: String,
    year: Option<i64>,
    year_range: Option<String>,
    "Skip albums without album art"
    skip: Option<bool>,
);

pub const GETAOTYS: CommandConst = CommandConst {
    description: "Get your albums of the year",
    ..command!(/aoty GETAOTYS_ARGS: get_aotys)
};

#[derive(Debug)]
pub struct GetAotys {
    pub username: String,
    pub year: Option<i64>,
    pub year_range: Option<String>,
    pub skip: Option<bool>,
}

/// Respond with a chart of a lastfm user's top albums of the specified year,
/// or of the current year.
async fn get_aotys(
    (username, year, year_range, skip): GETAOTYS_ARGS,
    handler: &Handler,
    ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    // long running command, defer response
    command.defer(&ctx.http).await?;
    let params = GetAotys {
        username,
        year,
        year_range,
        skip,
    };
    if let Err(e) = params.get_aotys(handler, ctx, command).await {
        eprintln!("get aotys failed: {:?}", e);
        // send error message as a followup
        command
            .create_followup(
                &ctx.http,
                CreateInteractionResponseFollowup::new().content(e.to_string()),
            )
            .await?;
    }
    Ok(CommandResponse::None)
}

impl GetAotys {
    /// Respond with a chart of a lastfm user's top albums of the specified year,
    /// or of the current year.
    async fn get_aotys(
        self,
        handler: &Handler,
        ctx: &Context,
        opts: &CommandInteraction,
    ) -> anyhow::Result<()> {
        // get required modules
        let lastfm: Arc<Lastfm> = handler.module_arc()?;
        let tidal: Arc<Tidal> = handler.module_arc()?;
        let db = Arc::clone(&handler.db);
        // determine year range from parameters
        let year_range = self
            .year_range
            .as_deref()
            .and_then(|range| range.split_once('-'))
            .and_then(|(start, end)| {
                // year range supplied, parse it
                start
                    .parse::<u64>()
                    .and_then(|start| end.parse::<u64>().map(|end| start..=end))
                    .ok()
            })
            .unwrap_or_else(|| {
                // use supplied year or current year
                let y = self
                    .year
                    .map(|yr| yr as u64)
                    .unwrap_or_else(|| Utc::now().year() as u64);
                y..=y
            });
        let start = year_range.start();
        let end = year_range.end();
        let year_fmt = if end - start <= 1 {
            // single year, display it as is
            start.to_string()
        } else {
            // year range
            format!("{start}-{end}")
        };
        // fetch top albums in that range for that user
        let mut aotys = lastfm
            .get_albums_of_the_year(db, tidal, &self.username, &year_range)
            .await?;
        let http = &ctx.http;
        if aotys.is_empty() {
            opts.create_followup(
                http,
                CreateInteractionResponseFollowup::new().content(format!(
                    "No {} albums found for user {}",
                    year_fmt, self.username
                )),
            )
            .await?;
            return Ok(());
        }
        // only keep the first 25 albums, to build a 5x5 chart
        aotys.truncate(25);
        // build chart image
        let image = create_aoty_chart(&aotys, self.skip.unwrap_or(false)).await?;
        // build response text content
        let mut content = format!("**Top albums of {} for {}**", year_fmt, self.username);
        aotys
            .iter()
            .map(|ab| &ab.album)
            .map(|ab| format!("{} - {} ({} plays)", ab.artist.name, ab.name, ab.playcount))
            .for_each(|line| {
                content.push('\n');
                content.push_str(&line);
            });
        content.push_str(
            "\n-# Something missing? Fix incorrect release years with /fix_release_year.",
        );

        opts.create_followup(
            http,
            CreateInteractionResponseFollowup::new()
                .content(content)
                .add_file(CreateAttachment::bytes(
                    image,
                    format!("{}_aoty_{}.png", self.username, year_fmt),
                )),
        )
        .await?;
        Ok(())
    }
}

impl TopAlbum {
    /// Fetch an album's cover from last.fm
    pub fn get_image(
        &self,
    ) -> impl 'static + Future<Output = anyhow::Result<Option<DynamicImage>>> {
        let image = self.image.iter().last().map(|img| img.url.clone());

        async move {
            let Some(image_url) = image else {
                return Ok(None);
            };
            let reader = match reqwest::get(&image_url).await {
                Ok(resp) => ImageReader::new(Cursor::new(
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

/// Build a image chart from a list of album covers
pub async fn create_aoty_chart(albums: &[AlbumWithImage], skip: bool) -> anyhow::Result<Vec<u8>> {
    // determine chart size
    let n = (albums.len() as f32).sqrt().ceil() as u32;
    eprintln!("Creating {n}x{n} chart");
    let len = n * CHART_SQUARE_SIZE;
    let mut height = n;
    while (height - 1) * n >= albums.len() as u32 {
        height -= 1;
    }
    // copy album covers to output image
    let mut out = RgbaImage::new(len, height * CHART_SQUARE_SIZE);
    let mut offset = 0;
    for (mut i, ab) in albums.iter().enumerate() {
        let Some(img) = ab.image.as_ref() else {
            offset += 1;
            continue;
        };
        if skip {
            // skipping albums with no cover, offsetting byt the number of skipped albums
            i -= offset;
        }
        let y = (i as u32 / n) * CHART_SQUARE_SIZE;
        let x = (i as u32 % n) * CHART_SQUARE_SIZE;
        out.copy_from(img, x, y)?;
    }
    // encode image
    let buf = Vec::new();
    let mut writer = Cursor::new(buf);
    out.write_to(&mut writer, ImageFormat::Png)?;
    Ok(writer.into_inner())
}

impl Lastfm {
    /// Get a user's top albums.
    /// If `current_year` is true, only get top albums within the past 12 months.
    pub async fn get_top_albums(
        self: Arc<Self>,
        user: String,
        page: Option<u64>,
        current_year: bool,
    ) -> anyhow::Result<TopAlbums> {
        // using a limit of 500 because somewhere above that number lastfm stops including
        // image links. this limit seems to vary somehow?
        let mut params: Vec<(&'static str, &str)> = vec![("user", &user), ("limit", "500")];

        // format parameter
        let page_s = page.map(|p| p.to_string());
        if let Some(page) = page_s.as_deref() {
            params.push(("page", page));
        }

        if current_year {
            params.push(("period", "12month"))
        }

        // send query
        let top_albums: TopAlbumsResp = self.query("user.gettopalbums", params).await?;
        Ok(top_albums.topalbums)
    }

    /// Fetch a user's top albums, page by page, as a stream of pages
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
        tidal: Arc<Tidal>,
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
            let fetches = serenity::futures::stream::iter(
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
                                Arc::clone(&tidal),
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
        for (album, fut) in aotys.into_iter().zip(img_futures) {
            let image = fut.await?.ok().flatten();
            out.push(AlbumWithImage { album, image })
        }
        Ok(out)
    }
}
