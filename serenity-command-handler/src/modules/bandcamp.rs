use crate::{Module, ModuleMap, RegisterableModule, album::Track};
use anyhow::{Context, anyhow};
use chrono::TimeDelta;
use itertools::Itertools;
use reqwest::{Client, Url};
use scraper::{ElementRef, Html, Selector};
use serenity::async_trait;

use crate::album::{Album, AlbumProvider};

const SEARCH_URL: &str = "https://bandcamp.com/search";

/// helper to get the contents of an HTML tag matched by the provided selector
fn contents(html: &Html, selector: &Selector) -> Option<String> {
    Some(
        html.select(selector)
            .next()?
            .text()
            .next()?
            .trim()
            .to_string(),
    )
}

fn extract_track_info(track: ElementRef<'_>) -> Option<Track> {
    let (title, duration) = track
        .text()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .tuples()
        .next()?;
    let duration = duration
        .split(':')
        .map(|s| s.parse::<i64>().unwrap_or_default())
        .tuples()
        .next()
        .map(|(m, s)| TimeDelta::seconds(s) + TimeDelta::minutes(m));
    Some(Track {
        name: Some(title.to_string()),
        duration,
        uri: None,
    })
}

/// format a search result from an album query.
/// returns a tuple of `<artist> - <album>` and the album URL
fn format_search_result(
    (album_link, subheading): (ElementRef, ElementRef),
) -> Option<(String, String)> {
    let url = album_link
        .value()
        .attr("href")
        .map(|s| s.split('?').next().unwrap().to_string())?;

    // formatted as `by <artist>`
    let artist = subheading.text().next()?.trim().strip_prefix("by ")?;
    let album_name = album_link.text().collect::<String>();
    Some((format!("{} - {}", artist.trim(), album_name.trim()), url))
}

pub struct Bandcamp {
    client: Client,
}

impl Bandcamp {
    pub fn new() -> Self {
        Bandcamp {
            client: Client::new(),
        }
    }

    async fn query_albums(&self, q: &str) -> anyhow::Result<Html> {
        let mut query_url = Url::parse(SEARCH_URL).unwrap();
        query_url
            .query_pairs_mut()
            .append_pair("q", q)
            .append_pair("item_type", "a"); // album type
        let page = self.client.get(query_url).send().await?.text().await?;
        Ok(Html::parse_document(&page))
    }
}

#[async_trait]
impl AlbumProvider for Bandcamp {
    fn id(&self) -> &'static str {
        "bandcamp"
    }

    async fn get_from_url(&self, url: &str) -> anyhow::Result<Album> {
        let url = {
            let mut url = Url::parse(url)?;
            // URL cleanup, remove tracking parameters
            url.query_pairs_mut().clear();
            url
        };
        let url_string = url.to_string();

        let page = self.client.get(url).send().await?.text().await?;
        let html = Html::parse_document(&page);

        // extract metadata

        // extract album title
        let title_selector = Selector::parse(".trackTitle").unwrap();
        let title = contents(&html, &title_selector).ok_or_else(|| anyhow!("Not an album page"))?;

        // extract album artist
        let artist_selector = Selector::parse("#name-section>h3>span>a").unwrap();
        let artist = contents(&html, &artist_selector);

        // extract genre list
        let genres_selector = Selector::parse(".tralbum-tags>.tag").unwrap();
        let genres = html
            .select(&genres_selector)
            .map(|e| e.text().collect::<String>().trim().to_string())
            .collect::<Vec<_>>();

        // extract album release date
        let release_selector = Selector::parse(".tralbum-credits").unwrap();
        let release_date = html
            .select(&release_selector)
            .next()
            .and_then(|e| e.text().next())
            .and_then(|s| s.trim().split_once(' '))
            .map(|(_, date)| date.to_string());

        // extract album tracklist
        let track_selector = Selector::parse(".title-col > .title").unwrap();
        let tracks = html
            .select(&track_selector)
            .flat_map(extract_track_info)
            .collect::<Vec<_>>();

        // extract album cover
        let cover_selector = Selector::parse("#tralbumArt > .popupImage").unwrap();
        let cover = html
            .select(&cover_selector)
            .next()
            .and_then(|a| a.attr("href"))
            .map(|s| s.to_string());

        Ok(Album {
            name: Some(title),
            artist,
            genres,
            url: Some(url_string),
            release_date,
            tracks,
            cover,
            ..Default::default()
        })
    }

    async fn query_album(&self, q: &str) -> anyhow::Result<Album> {
        let url = {
            let search_results = self.query_albums(q).await?;

            // extract URL first result
            let url_selector = Selector::parse(".result-info>.heading>a").unwrap();
            search_results
                .select(&url_selector)
                .next()
                .context("Not found")?
                .value()
                .attr("href")
                .context("Not found")?
                .to_string()
        };
        self.get_from_url(&url).await
    }

    fn url_matches(&self, url: &str) -> bool {
        url.starts_with("https://") && url.contains(".bandcamp.com")
    }

    async fn query_albums(&self, q: &str) -> anyhow::Result<Vec<(String, String)>> {
        let search_results = self.query_albums(q).await?;

        // extract album link and artist for each result
        let url_selector = Selector::parse(".result-info>.heading>a").unwrap();
        let artist_selector = Selector::parse(".result-info>.subhead").unwrap();
        Ok(search_results
            .select(&url_selector)
            .zip(search_results.select(&artist_selector))
            .flat_map(format_search_result)
            .take(10)
            .collect())
    }
}

impl Default for Bandcamp {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Module for Bandcamp {}

impl RegisterableModule for Bandcamp {
    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Ok(Bandcamp::new())
    }
}
