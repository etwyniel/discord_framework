use std::{collections::HashMap, env};

use anyhow::{Context, anyhow, bail};
use chrono::{DateTime, Local};
use futures::TryFutureExt;
use reqwest::{self, Client, Method, Url};
use serde::de::DeserializeOwned;
use serenity::async_trait;
use tokio::sync::RwLock;

use crate::{
    Module, ModuleMap, RegisterableModule,
    album::{Album, AlbumProvider},
};

mod model;
pub use model::*;

/// Tidal authentication token
struct Token {
    value: String,
    expiration: DateTime<Local>,
}

/// Tidal API client
pub struct Tidal {
    client_id: String,
    client_secret: String,
    token: RwLock<Option<Token>>,
    client: Client,
}

/// Parse an HTTP response as an error or as a `T`
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
    /// Create a new Tidal API client using the `TIDAL_CLIENT_ID` and
    /// `TIDAL_CLIENT_SECRET` environment variables.
    pub fn from_env() -> anyhow::Result<Self> {
        let client_id = env::var("TIDAL_CLIENT_ID").context("missing TIDAL_CLIENT_ID")?;
        let client_secret =
            env::var("TIDAL_CLIENT_SECRET").context("missing TIDAL_CLIENT_SECRET")?;

        Ok(Self::new(client_id, client_secret))
    }

    /// Get an API token for tidal. Performs authentication if
    /// the stored token is expired or absent.
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

    /// Creates a request to a Tidal API endpoint with the specified method.
    async fn request(&self, method: Method, url: &str) -> anyhow::Result<reqwest::RequestBuilder> {
        let token = self.get_token().await?;
        let req = self
            .client
            .request(method, url)
            .bearer_auth(token)
            .query(&[("countryCode", "FR")]);
        Ok(req)
    }

    /// Create a Tidal API client
    pub fn new(client_id: String, client_secret: String) -> Self {
        let client = reqwest::Client::new();
        Tidal {
            client_id,
            client_secret,
            client,
            token: RwLock::default(),
        }
    }

    pub fn search_uri(query: &str, ty: Option<&str>) -> Url {
        let mut uri = Url::parse(&format!("{BASE}/searchResults")).unwrap();
        {
            let mut path = uri.path_segments_mut().unwrap();
            path.push(query);
            if let Some(ty) = ty {
                path.push("relationships").push(ty);
            }
        }
        if let Some(ty) = ty {
            uri.set_query(Some(&format!("include={ty}")));
        }
        uri
    }

    pub async fn get_album(&self, artist: &str, name: &str) -> anyhow::Result<Option<Album>> {
        let albums_uri = Self::search_uri(&format!("{artist} {name}"), Some("albums"));
        let resp = self
            .request(Method::GET, albums_uri.as_str())
            .await?
            .send()
            .await?
            .error_for_status()?;
        let albums_resp: MultiResponse<Relationship> = resp.json().await?;
        for rel in &albums_resp.data {
            let Some(album) = albums_resp.included.iter().find_map(|inc| {
                if inc.id != rel.id {
                    return None;
                }
                let IncludedEntity::Album(album) = &inc.entity else {
                    return None;
                };
                Some(album)
            }) else {
                continue;
            };
            if album.title == name {
                let ab = Album {
                    name: Some(album.title.to_string()),
                    release_date: album.release_date.clone(),
                    ..Default::default()
                };
                return Ok(Some(ab));
            }
        }
        Ok(None)
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
            .query(&[
                ("include", "artists"),
                ("include", "coverArt"),
                ("include", "items"),
            ])
            .send()
            .map_err(anyhow::Error::from)
            .and_then(parse_response::<Response<AlbumAttributes>>)
            .await
            .context("failed to fetch album metadata from Tidal")
            .map(Response::into_album)
    }

    async fn query_album(&self, q: &str) -> anyhow::Result<Album> {
        let request_url = format!("{BASE}/searchResults/{q}/relationships/albums");
        let resp: MultiResponse<Relationship> = self
            .request(Method::GET, &request_url)
            .await?
            .send()
            .map_err(anyhow::Error::from)
            .and_then(parse_response)
            .await?;
        let album_id = &resp.data.first().context("album not found")?.id;
        // get artist name and track info
        let request_url = format!("{BASE}/albums/{album_id}");
        self.request(Method::GET, &request_url)
            .await?
            .query(&[
                ("include", "artists"),
                ("include", "coverArt"),
                ("include", "items"),
            ])
            .send()
            .map_err(anyhow::Error::from)
            .and_then(parse_response::<Response<AlbumAttributes>>)
            .await
            .context("failed to fetch album metadata from Tidal")
            .map(Response::into_album)
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
