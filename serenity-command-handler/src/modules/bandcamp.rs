use crate::{Module, ModuleMap, RegisterableModule};
use anyhow::anyhow;
use reqwest::{Client, Url};
use scraper::{Html, Selector};
use serenity::async_trait;

use crate::album::{Album, AlbumProvider};

const SEARCH_URL: &str = "https://bandcamp.com/search";

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

pub struct Bandcamp {
    client: Client,
}

#[async_trait]
impl AlbumProvider for Bandcamp {
    fn id(&self) -> &'static str {
        "bandcamp"
    }

    async fn get_from_url(&self, url: &str) -> anyhow::Result<Album> {
        let mut url = Url::parse(url)?;
        url.query_pairs_mut().clear();
        let page = self.client.get(url.clone()).send().await?.text().await?;
        let html = Html::parse_document(&page);

        let title_selector = Selector::parse(".trackTitle").unwrap();
        let title = contents(&html, &title_selector).ok_or_else(|| anyhow!("Not an album page"))?;

        let artist_selector = Selector::parse("#name-section>h3>span>a").unwrap();
        let artist = contents(&html, &artist_selector);

        let genres_selector = Selector::parse(".tralbum-tags>.tag").unwrap();
        let genres = html
            .select(&genres_selector)
            .map(|e| e.text().collect::<String>().trim().to_string())
            .collect::<Vec<_>>();

        let release_selector = Selector::parse(".tralbum-credits").unwrap();
        let release_date = html
            .select(&release_selector)
            .next()
            .and_then(|e| e.text().next())
            .and_then(|s| s.trim().split_once(' '))
            .map(|(_, date)| date.to_string());

        Ok(Album {
            name: Some(title),
            artist,
            genres,
            url: Some(url.to_string()),
            release_date,
            ..Default::default()
        })
    }

    async fn query_album(&self, q: &str) -> anyhow::Result<Album> {
        let mut query_url = Url::parse(SEARCH_URL).unwrap();
        query_url
            .query_pairs_mut()
            .append_pair("q", q)
            .append_pair("item_type", "a");
        let page = self.client.get(query_url).send().await?.text().await?;

        let url_selector = Selector::parse(".result-info>.heading>a").unwrap();
        let url = Html::parse_document(&page)
            .select(&url_selector)
            .next()
            .ok_or_else(|| anyhow!("Not found"))?
            .value()
            .attr("href")
            .ok_or_else(|| anyhow!("Not found"))?
            .to_string();
        self.get_from_url(&url).await
    }

    fn url_matches(&self, url: &str) -> bool {
        url.starts_with("https://") && url.contains(".bandcamp.com")
    }

    async fn query_albums(&self, q: &str) -> anyhow::Result<Vec<(String, String)>> {
        let mut query_url = Url::parse(SEARCH_URL).unwrap();
        query_url
            .query_pairs_mut()
            .append_pair("q", q)
            .append_pair("item_type", "a");
        let page = self.client.get(query_url).send().await?.text().await?;

        let url_selector = Selector::parse(".result-info>.heading>a").unwrap();
        let artist_selector = Selector::parse(".result-info>.subhead").unwrap();
        let html = Html::parse_document(&page);
        Ok(html
            .select(&url_selector)
            .zip(html.select(&artist_selector))
            .take(10)
            .map(|(a, h)| {
                (
                    format!(
                        "{} - {}",
                        h.text()
                            .next()
                            .unwrap_or_default()
                            .trim()
                            .split_once(' ')
                            .map(|(_, artist)| artist)
                            .unwrap_or_default(),
                        a.text().collect::<String>().trim()
                    ),
                    a.value()
                        .attr("href")
                        .map(|s| s.split('?').next().unwrap().to_string())
                        .unwrap_or_default(),
                )
            })
            .collect())
    }
}

impl Bandcamp {
    pub fn new() -> Self {
        Bandcamp {
            client: Client::new(),
        }
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
