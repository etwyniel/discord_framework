use std::{collections::HashMap, env};

use anyhow::{anyhow, bail, Context};
use chrono::{DateTime, Local};
use iso8601_duration::Duration;
use reqwest::{self, Client};
use serde::Deserialize;
use serenity::async_trait;
use tokio::sync::RwLock;

use crate::{
    album::{Album, AlbumProvider},
    Module, ModuleMap, RegisterableModule,
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
            if let Some(Token { value, expiration }) = token.as_ref() {
                if *expiration - Local::now() > chrono::Duration::hours(1) {
                    return Ok(value.to_owned());
                }
            }
        }

        let resp: AuthResponse = self
            .client
            .post("https://auth.tidal.com/v1/oauth2/token")
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(&HashMap::from([("grant_type", "client_credentials")]))
            .send()
            .await?
            .json()
            .await?;
        let token = Token {
            value: resp.access_token,
            expiration: Local::now() + chrono::Duration::seconds(resp.expires_in as i64),
        };
        Ok(self.token.write().await.insert(token).value.to_owned())
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

#[derive(Deserialize)]
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

#[derive(Deserialize)]
pub struct Response<T> {
    pub data: ResponseData<T>,
    pub included: Vec<IncludedItem>,
}

impl Response<AlbumAttributes> {
    fn to_album(self) -> Album {
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
            .iter()
            .filter_map(|inc| match &inc.entity {
                IncludedEntity::Artwortk(ArtworkAttributes { files }) => Some(files),
                _ => None,
            })
            .flat_map(|files| files.first())
            .next()
            .map(|file| file.href.clone());

        Album {
            name: Some(attributes.title),
            release_date: attributes.release_date,
            duration,
            url: Some(format!("{ALBUM_URL}/{id}/u")),
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
        let token = self.get_token().await?;
        let resp = self
            .client
            .get(request_url)
            .query(&[
                ("countryCode", "FR"),
                ("include", "artists"),
                ("include", "coverArt"),
            ])
            .bearer_auth(token)
            .send()
            .await
            .context("tidal API request failed")?
            .text()
            .await?;

        let album = serde_json::from_str::<'_, Response<AlbumAttributes>>(&resp)
            .context("failed to parse album data")?
            .to_album();
        Ok(album)
    }

    async fn query_album(&self, _q: &str) -> anyhow::Result<Album> {
        unimplemented!()
    }

    fn url_matches(&self, url: &str) -> bool {
        url.starts_with("https://tidal.com/")
    }

    async fn query_albums(&self, _q: &str) -> anyhow::Result<Vec<(String, String)>> {
        unimplemented!()
    }
}

#[async_trait]
impl Module for Tidal {}

impl RegisterableModule for Tidal {
    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Tidal::from_env()
    }
}
