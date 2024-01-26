use std::sync::Arc;

use chrono::Duration;
use serenity::async_trait;

#[derive(Debug, Default)]
pub struct Album {
    pub name: Option<String>,
    pub artist: Option<String>,
    pub genres: Vec<String>,
    pub release_date: Option<String>,
    pub url: Option<String>,
    pub is_playlist: bool,
    pub duration: Option<Duration>,
}

#[async_trait]
pub trait AlbumProvider: Send + Sync {
    fn url_matches(&self, _url: &str) -> bool;

    fn id(&self) -> &'static str;

    async fn get_from_url(&self, url: &str) -> anyhow::Result<Album>;

    async fn query_album(&self, _q: &str) -> anyhow::Result<Album>;

    async fn query_albums(&self, q: &str) -> anyhow::Result<Vec<(String, String)>>;
}

impl Album {
    pub fn format_genres(&self) -> Option<String> {
        if self.genres.is_empty() {
            return None;
        }
        Some(self.genres.iter().fold(String::new(), |mut out, s| {
            if !out.is_empty() {
                out.push_str("  ");
            }
            out.push('`');
            out.push_str(&s.to_lowercase());
            out.push('`');
            out
        }))
    }

    pub fn format_name(&self) -> String {
        match (&self.name, &self.artist) {
            (Some(n), Some(a)) => format!("{a} - {n}"),
            (Some(n), None) => n.to_string(),
            _ => "this".to_string(),
        }
    }
}

#[async_trait]
impl<P: AlbumProvider + Send> AlbumProvider for Arc<P> {
    fn url_matches(&self, url: &str) -> bool {
        self.as_ref().url_matches(url)
    }

    fn id(&self) -> &'static str {
        self.as_ref().id()
    }

    async fn get_from_url(&self, url: &str) -> anyhow::Result<Album> {
        self.as_ref().get_from_url(url).await
    }

    async fn query_album(&self, q: &str) -> anyhow::Result<Album> {
        self.as_ref().query_album(q).await
    }

    async fn query_albums(&self, q: &str) -> anyhow::Result<Vec<(String, String)>> {
        self.as_ref().query_albums(q).await
    }
}
