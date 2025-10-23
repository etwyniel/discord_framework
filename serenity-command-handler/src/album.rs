use std::fmt::Write;
use std::sync::Arc;

use chrono::Duration;
use serenity::async_trait;

#[derive(Debug)]
pub struct Track {
    pub name: Option<String>,
    pub duration: Option<chrono::Duration>,
    pub uri: Option<String>,
}

#[derive(Debug, Default)]
pub struct Album {
    pub name: Option<String>,
    pub artist: Option<String>,
    pub genres: Vec<String>,
    pub release_date: Option<String>,
    pub url: Option<String>,
    pub is_playlist: bool,
    pub duration: Option<Duration>,
    pub cover: Option<String>,
    pub tracks: Vec<Track>,
    pub has_rich_embed: bool,
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
                out.push_str(" • ");
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

    pub fn format_tracks(&self, limit: Option<usize>) -> String {
        let n_tracks = self.tracks.len();
        let mut formatted = String::with_capacity(n_tracks * 15);
        self.tracks
            .iter()
            .take(limit.unwrap_or(100))
            .enumerate()
            .for_each(|(i, track)| {
                _ = write!(
                    &mut formatted,
                    "-# {}. {}",
                    i + 1,
                    track.name.as_deref().unwrap_or("<no title>"),
                );
                let Some(d) = &track.duration else {
                    formatted.push('\n');
                    return;
                };
                formatted.push_str(" (*");
                let h = d.num_hours();
                let m = d.num_minutes() % 60;
                let s = d.num_seconds() % 60;
                _ = if h > 0 {
                    write!(&mut formatted, "{h}:{m:02}:{s:02}")
                } else {
                    write!(&mut formatted, "{m}:{s:02}")
                };
                formatted.push_str("*)\n");
            });
        if let Some(limit) = limit
            && limit <= n_tracks
        {
            let remaining = n_tracks - limit;
            _ = write!(&mut formatted, "-# +{remaining} more")
        }
        formatted
    }

    pub fn as_link(&self, text: Option<&str>) -> String {
        let text = text
            .map(str::to_string)
            .unwrap_or_else(|| self.format_name());
        if let Some(link) = &self.url {
            format!("[**{text}**]({link})")
        } else {
            text
        }
    }

    pub fn as_linked_header(&self, text: Option<&str>) -> String {
        let linked_header = text.or(self.name.as_deref()).unwrap_or("this").to_string();
        let mut header = if let Some(url) = &self.url {
            if !self.has_rich_embed {
                format!("# [{linked_header}](<{url}>)")
            } else {
                format!("# [{linked_header}]({url})")
            }
        } else {
            linked_header
        };
        if let Some(artist) = &self.artist {
            _ = write!(&mut header, "\n**{artist}**");
        }
        header
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
