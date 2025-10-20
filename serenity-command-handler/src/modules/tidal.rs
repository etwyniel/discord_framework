use std::{collections::HashMap, env};

use anyhow::{Context, anyhow, bail};
use chrono::{DateTime, Local};
use futures::TryFutureExt;
use reqwest::{self, Client, Method};
use serde::de::DeserializeOwned;
use serenity::async_trait;
use tokio::sync::RwLock;

use crate::{
    Module, ModuleMap, RegisterableModule,
    album::{Album, AlbumProvider},
};

mod model;
pub use model::*;

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

async fn parse_response<T: DeserializeOwned>(resp: reqwest::Response) -> anyhow::Result<T> {
    if resp.status().is_client_error() {
        let err_resp = resp.json::<ErrorResponse>().await?;
        let msg = err_resp
            .errors
            .into_iter()
            .next()
            .map(|e| e.detail)
            .unwrap_or("unknown error".to_string());
        bail!(msg);
    }
    Ok(resp.json().await?)
}

impl Tidal {
    pub fn from_env() -> anyhow::Result<Self> {
        let client_id = env::var("TIDAL_CLIENT_ID").context("missing TIDAL_CLIENT_ID")?;
        let client_secret =
            env::var("TIDAL_CLIENT_SECRET").context("missing TIDAL_CLIENT_SECRET")?;

        Ok(Self::new(client_id, client_secret))
    }

    pub async fn get_token(&self) -> anyhow::Result<String> {
        if let Some(Token { value, expiration }) = self.token.read().await.as_ref()
            && *expiration - Local::now() > chrono::Duration::hours(1)
        {
            // token exists and is still valid for over an hour
            return Ok(value.to_owned());
        }

        let resp: AuthResponse = self
            .client
            .post("https://auth.tidal.com/v1/oauth2/token")
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(&HashMap::from([("grant_type", "client_credentials")]))
            .send()
            .map_err(anyhow::Error::from)
            .and_then(parse_response)
            .await
            .context("failed to fetch Tidal API authorization token")?;

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

#[async_trait]
impl AlbumProvider for Tidal {
    fn id(&self) -> &'static str {
        "tidal"
    }

    fn url_matches(&self, url: &str) -> bool {
        url.starts_with("https://tidal.com/")
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
        self.request(Method::GET, &request_url)
            .await?
            .query(&[("include", "artists"), ("include", "coverArt")])
            .send()
            .map_err(anyhow::Error::from)
            .and_then(parse_response::<Response<AlbumAttributes>>)
            .await
            .context("failed to fetch album metadata from Tidal")
            .map(
                |Response {
                     data:
                         ResponseData {
                             id,
                             attributes,
                             relationships,
                             ..
                         },
                     included,
                 }| {
                    attributes.into_album(id, &relationships.artists.data, included)
                },
            )
    }

    async fn query_album(&self, q: &str) -> anyhow::Result<Album> {
        let request_url = format!("{BASE}/searchResults/{q}/relationships/albums");
        let resp: MultiResponse<Relationship> = self
            .request(Method::GET, &request_url)
            .await?
            .query(&[("include", "albums")])
            .send()
            .map_err(anyhow::Error::from)
            .and_then(parse_response)
            .await?;
        let (album_id, album) = resp
            .included
            .into_iter()
            .find_map(IncludedItem::album)
            .context("album not found")?;
        // get artist name
        let request_url = format!("{BASE}/albums/{album_id}");
        let Response { data, included } = self
            .request(Method::GET, &request_url)
            .await?
            .query(&[("include", "artists")])
            .send()
            .map_err(anyhow::Error::from)
            .and_then(parse_response::<Response<AlbumAttributes>>)
            .await?;

        let album = album.into_album(album_id, &data.relationships.artists.data, included);
        Ok(album)
    }

    async fn query_albums(&self, q: &str) -> anyhow::Result<Vec<(String, String)>> {
        // get album IDs based on search query
        let request_url = format!("{BASE}/searchResults/{q}/relationships/albums");
        let resp: MultiResponse<Relationship> = self
            .request(Method::GET, &request_url)
            .await?
            .send()
            .map_err(anyhow::Error::from)
            .and_then(parse_response)
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
            .map_err(anyhow::Error::from)
            .and_then(parse_response)
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
