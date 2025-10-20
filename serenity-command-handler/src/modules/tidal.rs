use std::{collections::HashMap, env};

use anyhow::{Context, anyhow, bail};
use chrono::{DateTime, Local};
use iso8601_duration::Duration;
use reqwest::{self, Client, Method};
use serde::Deserialize;
use serenity::async_trait;
use tokio::sync::RwLock;

use crate::{
    Module, ModuleMap, RegisterableModule,
    album::{Album, AlbumProvider},
};

struct Token {
    value: String,
    expiration: DateTime<Local>,
}

pub struct Tidal {
    client_id: String,
    client_secret: String,
    token: RwLock<Option<Token>>,
    client: Client,
}

#[derive(Deserialize)]
pub struct AuthResponse {
    access_token: String,
    expires_in: u64,
}

const BASE: &str = "https://openapi.tidal.com/v2";
const ALBUM_URL: &str = "https://tidal.com/album";

fn album_share_url(album_id: &str) -> String {
    format!("{ALBUM_URL}/{album_id}/u")
}

impl Tidal {
    pub fn from_env() -> anyhow::Result<Self> {
        let client_id = env::var("TIDAL_CLIENT_ID").context("missing TIDAL_CLIENT_ID")?;
        let client_secret =
            env::var("TIDAL_CLIENT_SECRET").context("missing TIDAL_CLIENT_SECRET")?;

        Ok(Self::new(client_id, client_secret))
    }

    pub async fn get_token(&self) -> anyhow::Result<String> {
        {
            let token = self.token.read().await;
            if let Some(Token { value, expiration }) = token.as_ref()
                && *expiration - Local::now() > chrono::Duration::hours(1)
            {
                return Ok(value.to_owned());
            }
        }

        let body = self
            .client
            .post("https://auth.tidal.com/v1/oauth2/token")
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(&HashMap::from([("grant_type", "client_credentials")]))
            .send()
            .await?
            .text()
            .await?;
        dbg!(&body);

        let resp: AuthResponse = serde_json::from_str(&body)?;
        let token = Token {
            value: resp.access_token,
            expiration: Local::now() + chrono::Duration::seconds(resp.expires_in as i64),
        };
        Ok(self.token.write().await.insert(token).value.to_owned())
    }

    async fn request(&self, method: Method, url: &str) -> anyhow::Result<reqwest::RequestBuilder> {
        let token = self.get_token().await?;
        let req = self
            .client
            .request(method, url)
            .bearer_auth(token)
            .query(&[("countryCode", "FR")]);
        Ok(req)
    }

    pub fn new(client_id: String, client_secret: String) -> Self {
        let client = reqwest::Client::new();
        Tidal {
            client_id,
            client_secret,
            client,
            token: RwLock::default(),
        }
    }
}

#[derive(Deserialize, Debug)]
pub struct Relationship {
    pub id: String,
    #[serde(rename = "type")]
    pub typ: String,
}

#[derive(Deserialize, Debug)]
pub struct RelationshipsData {
    #[serde(default)]
    pub data: Vec<Relationship>,
}

#[derive(Deserialize, Debug)]
pub struct AlbumRelationships {
    pub artists: RelationshipsData,
    pub genres: RelationshipsData,
    #[serde(rename = "coverArt")]
    pub cover_art: RelationshipsData,
    pub items: RelationshipsData,
}

#[derive(Deserialize, Debug)]
pub struct AlbumAttributes {
    pub title: String,
    pub duration: String,
    #[serde(rename = "releaseDate")]
    pub release_date: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct ArtistAttributes {
    pub name: String,
}

#[derive(Deserialize, Debug)]
pub struct ArtworkFile {
    href: String,
}

#[derive(Deserialize, Debug)]
pub struct ArtworkAttributes {
    files: Vec<ArtworkFile>,
}

#[derive(Deserialize, Debug)]
pub struct ResponseData<T> {
    pub id: String,
    #[serde(rename = "type")]
    pub typ: String,
    pub attributes: T,
    pub relationships: AlbumRelationships,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "type", content = "attributes")]
pub enum IncludedEntity {
    #[serde(rename = "artists")]
    Artist(ArtistAttributes),
    #[serde(rename = "albums")]
    Album(AlbumAttributes),
    #[serde(rename = "artworks")]
    Artwortk(ArtworkAttributes),
}

#[derive(Deserialize, Debug)]
pub struct IncludedItem {
    pub id: String,
    #[serde(flatten)]
    pub entity: IncludedEntity,
}

impl IncludedItem {
    fn artist(self) -> Option<(String, ArtistAttributes)> {
        match self.entity {
            IncludedEntity::Artist(a) => Some((self.id, a)),
            _ => None,
        }
    }

    fn album(self) -> Option<(String, AlbumAttributes)> {
        match self.entity {
            IncludedEntity::Album(a) => Some((self.id, a)),
            _ => None,
        }
    }

    fn artwork(self) -> Option<(String, ArtworkAttributes)> {
        match self.entity {
            IncludedEntity::Artwortk(a) => Some((self.id, a)),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
pub struct Response<T> {
    pub data: ResponseData<T>,
    pub included: Vec<IncludedItem>,
}

#[derive(Deserialize, Debug)]
pub struct MultiResponse<T> {
    #[serde(default = "Vec::new")]
    pub data: Vec<T>,
    #[serde(default)]
    pub included: Vec<IncludedItem>,
}

impl Response<AlbumAttributes> {
    fn into_album(self) -> Album {
        let Response {
            data:
                ResponseData {
                    id,
                    attributes,
                    relationships,
                    ..
                },
            included,
        } = self;

        let duration = Duration::parse(&attributes.duration)
            .ok()
            .and_then(|dur| Duration::to_chrono(&dur));
        let artist = relationships
            .artists
            .data
            .first()
            .and_then(|Relationship { id, .. }| included.iter().find(|inc| &inc.id == id))
            .and_then(|inc| match &inc.entity {
                IncludedEntity::Artist(ArtistAttributes { name }) => Some(name.to_owned()),
                _ => None,
            });

        let cover = included
            .into_iter()
            .filter_map(IncludedItem::artwork)
            .flat_map(|(_, artwork)| artwork.files)
            .next()
            .map(|file| file.href.clone());

        Album {
            name: Some(attributes.title),
            release_date: attributes.release_date,
            duration,
            url: Some(album_share_url(&id)),
            artist,

            is_playlist: false,
            cover,
            genres: Vec::new(),
        }
    }
}

#[async_trait]
impl AlbumProvider for Tidal {
    fn id(&self) -> &'static str {
        "tidal"
    }

    async fn get_from_url(&self, url: &str) -> anyhow::Result<Album> {
        let Some(id) = url.strip_prefix("https://tidal.com/album/") else {
            bail!("not an album URL")
        };
        let id = id
            .split('/')
            .next()
            .ok_or_else(|| anyhow!("invalid album ID"))?;
        let request_url = format!("{BASE}/albums/{id}");
        let resp = self
            .request(Method::GET, &request_url)
            .await?
            .query(&[("include", "artists"), ("include", "coverArt")])
            .send()
            .await
            .context("tidal API request failed")?
            .text()
            .await?;

        let album = serde_json::from_str::<'_, Response<AlbumAttributes>>(&resp)
            .context("failed to parse album data")?
            .into_album();
        Ok(album)
    }

    async fn query_album(&self, q: &str) -> anyhow::Result<Album> {
        let request_url = format!("{BASE}/searchResults/{q}/relationships/albums");
        let resp: Response<Vec<Relationship>> = self
            .request(Method::GET, &request_url)
            .await?
            .query(&[("include", "albums")])
            .send()
            .await?
            .json()
            .await?;
        let (album_id, album) = resp
            .included
            .into_iter()
            .find_map(IncludedItem::album)
            .context("album not found")?;
        // get artist name
        let request_url = format!("{BASE}/albums/{album_id}/relationships/artists");
        let resp: Response<Vec<Relationship>> = self
            .request(Method::GET, &request_url)
            .await?
            .query(&[("include", "artists")])
            .send()
            .await?
            .json()
            .await?;
        let (_, artist) = resp
            .included
            .into_iter()
            .find_map(IncludedItem::artist)
            .context("failed to find album artist")?;
        Ok(Album {
            name: Some(album.title),
            artist: Some(artist.name),
            release_date: album.release_date,
            url: Some(album_share_url(&album_id)),
            ..Default::default()
        })
    }

    fn url_matches(&self, url: &str) -> bool {
        url.starts_with("https://tidal.com/")
    }

    async fn query_albums(&self, q: &str) -> anyhow::Result<Vec<(String, String)>> {
        // get album IDs based on search query
        let request_url = format!("{BASE}/searchResults/{q}/relationships/albums");
        let resp: MultiResponse<Relationship> = self
            .request(Method::GET, &request_url)
            .await?
            .send()
            .await?
            .json()
            .await?;
        let filter = resp
            .data
            .into_iter()
            .map(|rel| ("filter[id]", rel.id))
            .collect::<Vec<_>>();
        // get artist and album info
        let request_url = format!("{BASE}/albums");
        let MultiResponse { data, included }: MultiResponse<ResponseData<AlbumAttributes>> = self
            .request(Method::GET, &request_url)
            .await?
            .query(&[("include", "artists")])
            .query(&filter)
            .send()
            .await?
            .json()
            .await?;

        Ok(data
            .into_iter()
            .flat_map(|album| {
                let artist_id = &album.relationships.artists.data.first()?.id;
                let artist = included
                    .iter()
                    .find(|inc| &inc.id == artist_id)
                    .and_then(|inc| match &inc.entity {
                        IncludedEntity::Artist(a) => Some(a),
                        _ => None,
                    })?;
                let name = format!("{} - {}", artist.name, album.attributes.title);
                Some((name, album_share_url(&album.id)))
            })
            .collect())
    }
}

#[async_trait]
impl Module for Tidal {}

impl RegisterableModule for Tidal {
    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Tidal::from_env()
    }
}
