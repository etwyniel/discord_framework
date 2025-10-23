use std::{
    borrow::Cow,
    collections::HashSet,
    sync::{LazyLock, atomic::AtomicU64},
};

use crate::{CommandStore, CompletionStore, Handler, Module, ModuleMap, RegisterableModule};
use anyhow::{Context as _, anyhow, bail};
use regex::Regex;
use reqwest::redirect::Policy;
use rspotify::{
    AuthCodeSpotify, ClientCredsSpotify, Config, Credentials,
    clients::{BaseClient, OAuthClient},
    model::{
        AlbumId, FullEpisode, FullTrack, Id, PlayableItem, PlaylistId, SearchType,
        SimplifiedArtist, TrackId,
    },
    scopes,
};
use serenity::{
    all::{
        CreateActionRow, CreateButton, CreateInteractionResponse, CreateInteractionResponseMessage,
    },
    async_trait,
    model::{
        channel::Message,
        prelude::{CommandInteraction, Reaction},
    },
};
use serenity::{http::Http, model::prelude::ReactionType, prelude::*};
use serenity_command::{BotCommand, CommandResponse};
use serenity_command_derive::Command;

use crate::album::{Album, AlbumProvider};

const SPOTIFY_URL_PATTERN: &str =
    r"https://open.spotify.com/(intl-.{2}/)?(track|album|playlist)/(\w+)(\?.*)?";
const SHORTENED_URL_START: &str = "https://spotify.link/";

const CACHE_PATH: &str = "rspotify_cache";

const UNLINK_REACT: &str = "🔗";

const INTERACTION_AUTH_PROMPT: &str = "spotify-oauth-prompt";

static SPOTIFY_URL_MATCHER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(SPOTIFY_URL_PATTERN).unwrap());

pub struct Spotify<C: BaseClient> {
    // client: ClientCredsSpotify,
    pub client: C,
}

pub type SpotifyOAuth = Spotify<AuthCodeSpotify>;

async fn resolve_redirect(url: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .redirect(Policy::none())
        .build()
        .unwrap();
    let resp = client
        .head(url)
        .send()
        .await
        .context("Failed to resolve shortened spotify URL")?;
    resp.headers()
        .get("location")
        .and_then(|val| val.to_str().map(String::from).ok())
        .ok_or_else(|| anyhow!("Not a valid spotify URL"))
}

impl<C: BaseClient> Spotify<C> {
    async fn get_album_from_id(&self, id: &str) -> anyhow::Result<Album> {
        let album = self.client.album(AlbumId::from_id(id)?, None).await?;
        let name = album.name.clone();
        let artist = album
            .artists
            .iter()
            .map(|a| a.name.as_ref())
            .collect::<Vec<_>>()
            .join(", ");
        let genres = album.genres.clone();
        let release_date = Some(album.release_date);
        let duration = album.tracks.items.iter().map(|track| track.duration).sum();
        Ok(Album {
            name: Some(name),
            artist: Some(artist),
            genres,
            release_date,
            url: Some(album.id.url()),
            duration: Some(duration),
            cover: album.images.first().map(|img| img.url.clone()),
            has_rich_embed: true,
            ..Default::default()
        })
    }

    async fn get_playlist_from_id(&self, id: &str) -> anyhow::Result<Album> {
        let playlist = self
            .client
            .playlist(PlaylistId::from_id(id)?, None, None)
            .await?;
        let name = playlist.name.clone();
        let artist = playlist.owner.display_name;
        let duration = playlist
            .tracks
            .items
            .iter()
            .flat_map(|item| item.track.as_ref())
            .map(|track| match track {
                PlayableItem::Track(FullTrack { duration, .. })
                | PlayableItem::Episode(FullEpisode { duration, .. }) => *duration,
                PlayableItem::Unknown(_) => Default::default(),
            })
            .sum();
        Ok(Album {
            name: Some(name),
            artist,
            url: Some(playlist.id.url()),
            duration: Some(duration),
            is_playlist: true,
            ..Default::default()
        })
    }

    pub async fn get_song_from_id(&self, id: &str) -> anyhow::Result<FullTrack> {
        Ok(self.client.track(TrackId::from_id(id)?, None).await?)
    }

    pub async fn get_song_from_url(&self, url: &str) -> anyhow::Result<FullTrack> {
        let mut url = Cow::Borrowed(url);
        if url.starts_with(SHORTENED_URL_START) {
            let location = resolve_redirect(url.as_ref()).await?;
            url = Cow::Owned(location);
        }
        let Some(captures) = SPOTIFY_URL_MATCHER.captures(&url) else {
            bail!("Invalid Spotify URL");
        };
        match captures.get(2).map(|m| m.as_str()) {
            Some("track") => {
                self.get_song_from_id(captures.get(3).unwrap().as_str())
                    .await
            }
            _ => bail!("Expected a Spotify track URL"),
        }
    }

    pub fn artists_to_string(artists: &[SimplifiedArtist]) -> String {
        artists
            .iter()
            .map(|a| a.name.as_ref())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn sanitize_string(s: &str) -> String {
    s.chars()
        .filter(|&c| !r#""'+()[]"#.contains(c))
        .take(30)
        .collect()
}

#[async_trait]
impl<C: BaseClient> AlbumProvider for Spotify<C> {
    fn id(&self) -> &'static str {
        "spotify"
    }

    async fn get_from_url(&self, url: &str) -> anyhow::Result<Album> {
        let mut url = Cow::Borrowed(url);
        if url.starts_with(SHORTENED_URL_START) {
            let location = resolve_redirect(url.as_ref()).await?;
            url = Cow::Owned(location);
        }
        let Some(captures) = Regex::new(SPOTIFY_URL_PATTERN).unwrap().captures(&url) else {
            bail!("Invalid Spotify URL");
        };
        let id = captures.get(3).unwrap().as_str();
        match captures.get(2).map(|m| m.as_str()) {
            Some("album") => self.get_album_from_id(id).await,
            Some("playlist") => self.get_playlist_from_id(id).await,
            _ => bail!("Expected a Spotify album/playlist URL"),
        }
    }

    fn url_matches(&self, url: &str) -> bool {
        SPOTIFY_URL_MATCHER.is_match(url) || url.starts_with(SHORTENED_URL_START)
    }

    async fn query_album(&self, query: &str) -> anyhow::Result<Album> {
        let res = self
            .client
            .search(query, SearchType::Album, None, None, Some(1), None)
            .await?;
        if let rspotify::model::SearchResult::Albums(albums) = res {
            Ok(albums
                .items
                .first()
                .map(|a| Album {
                    name: Some(a.name.clone()),
                    artist: a.artists.first().map(|ar| ar.name.clone()),
                    url: a.id.as_ref().map(|i| i.url()),
                    release_date: a.release_date.clone(),
                    has_rich_embed: true,
                    ..Default::default()
                })
                .ok_or_else(|| anyhow!("Not found"))?)
        } else {
            Err(anyhow!("Not an album"))
        }
    }

    async fn query_albums(&self, query: &str) -> anyhow::Result<Vec<(String, String)>> {
        let res = self
            .client
            .search(query, SearchType::Album, None, None, Some(10), None)
            .await?;
        if let rspotify::model::SearchResult::Albums(albums) = res {
            Ok(albums
                .items
                .into_iter()
                .map(|a| {
                    (
                        format!(
                            "{} - {}",
                            a.artists
                                .into_iter()
                                .next()
                                .map(|ar| ar.name)
                                .unwrap_or_default(),
                            a.name,
                        ),
                        a.id.map(|id| id.url()).unwrap_or_default(),
                    )
                })
                .collect())
        } else {
            Err(anyhow!("Not an album"))
        }
    }
}

impl<C: BaseClient> Spotify<C> {
    pub async fn get_album(&self, artist: &str, name: &str) -> anyhow::Result<Option<Album>> {
        let query = format!(
            r#"album:"{}" artist:"{}""#,
            &sanitize_string(name),
            &sanitize_string(artist)
        );
        let res = self
            .client
            .search(&query, SearchType::Album, None, None, Some(5), None)
            .await?;
        let rspotify::model::SearchResult::Albums(albums) = res else {
            return Err(anyhow!("Not an album"));
        };
        let album = albums
            .items
            .iter()
            .find(|ab| ab.name == name)
            .or_else(|| albums.items.first());
        Ok(album.map(|a| Album {
            name: Some(a.name.clone()),
            artist: a.artists.first().map(|ar| ar.name.clone()),
            url: a.id.as_ref().map(|i| i.url()),
            release_date: a.release_date.clone(),
            has_rich_embed: true,
            ..Default::default()
        }))
    }

    pub async fn query_songs(&self, query: &str) -> anyhow::Result<Vec<(String, String)>> {
        let res = self
            .client
            .search(query, SearchType::Track, None, None, Some(10), None)
            .await?;
        let rspotify::model::SearchResult::Tracks(songs) = res else {
            return Err(anyhow!("Not an album"));
        };
        Ok(songs
            .items
            .into_iter()
            .map(|a| {
                //dbg!(a.available_markets.len());
                (
                    format!(
                        "{} - {}",
                        a.artists
                            .into_iter()
                            .next()
                            .map(|ar| ar.name)
                            .unwrap_or_default(),
                        a.name,
                    ),
                    a.id.map(|id| id.url()).unwrap_or_default(),
                )
            })
            .collect())
    }
}

impl Spotify<ClientCredsSpotify> {
    pub async fn new() -> anyhow::Result<Self> {
        let creds = Credentials::from_env().ok_or_else(|| anyhow!("No spotify credentials"))?;
        let config = Config {
            token_refreshing: true,
            ..Default::default()
        };
        let spotify = ClientCredsSpotify::with_config(creds, config);

        // Obtaining the access token
        spotify.request_token().await?;
        Ok(Spotify { client: spotify })
    }
}

impl Spotify<AuthCodeSpotify> {
    pub async fn new_auth_code(scopes: HashSet<String>) -> anyhow::Result<Self> {
        let creds = Credentials::from_env().ok_or_else(|| anyhow!("No spotify credentials"))?;
        let oauth =
            rspotify::OAuth::from_env(scopes).ok_or_else(|| anyhow!("No oauth information"))?;
        let mut client = AuthCodeSpotify::new(creds, oauth);
        client.config.token_cached = true;
        client.config.cache_path = CACHE_PATH.into();
        // let prev_token = Token::from_cache(CACHE_PATH).ok();
        // if let Some(tok) = prev_token {
        //     *client.token.lock().await.unwrap() = Some(tok);
        // } else {
        let url = client
            .get_authorize_url(false)
            .context("failed to generate authorization url")?;
        // eprintln!("url: {url}");
        // }
        client
            .prompt_for_token(&url)
            .await
            .context("failed to prompt for token")?;
        Ok(Spotify { client })
    }
}

#[derive(Command)]
#[cmd(name = "spotify_authenticate", desc = "Authenticate Spotify user")]
pub struct SpotifyAuthenticate;

#[async_trait]
impl BotCommand for SpotifyAuthenticate {
    type Data = Handler;

    async fn run(
        self,
        _: &Handler,
        ctx: &Context,
        interaction: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let scopes = scopes!(
            "playlist-modify-public",
            "playlist-read-private",
            "playlist-read-collaborative",
            "user-library-read",
            "user-read-private",
            "playlist-modify-private"
        );
        let creds = Credentials::from_env().ok_or_else(|| anyhow!("No spotify credentials"))?;
        let oauth =
            rspotify::OAuth::from_env(scopes).ok_or_else(|| anyhow!("No oauth information"))?;
        let mut client = AuthCodeSpotify::new(creds, oauth);
        client.config.token_cached = true;
        client.config.cache_path = CACHE_PATH.into();
        let url = client
            .get_authorize_url(false)
            .context("failed to generate authorization url")?;
        interaction
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .ephemeral(true)
                        .content(format!("authentication URL: {url}"))
                        .components(vec![CreateActionRow::Buttons(vec![
                            CreateButton::new(INTERACTION_AUTH_PROMPT).label("Submit token"),
                        ])]),
                ),
            )
            .await?;
        Ok(CommandResponse::None)
    }
}

#[derive(Command)]
#[cmd(name = "unlink", message, desc = "Resolve a spotify.link URL")]
pub struct Unlink(Message);

#[async_trait]
impl Module for Spotify<ClientCredsSpotify> {
    fn register_commands(&self, store: &mut CommandStore, _: &mut CompletionStore) {
        store.register::<Unlink>();
    }
}

impl RegisterableModule for Spotify<ClientCredsSpotify> {
    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Spotify::new().await
    }
}

pub async fn resolve_spotify_links(message: &str) -> anyhow::Result<Vec<String>> {
    let re = Regex::new("https://spotify.link/[a-zA-Z0-9]+").unwrap();
    let client = reqwest::Client::builder()
        .redirect(Policy::none())
        .build()
        .unwrap();
    let mut urls = Vec::new();
    for cap in re.captures_iter(message) {
        let url = cap.get(0).unwrap().as_str();
        let resp = client
            .head(url)
            .send()
            .await
            .context("Failed to resolve shortened spotify URL")?;
        let location = resp
            .headers()
            .get("location")
            .and_then(|val| val.to_str().ok())
            .ok_or_else(|| anyhow!("Not a valid spotify URL"))?;
        urls.push(location.split('?').next().unwrap().to_string());
    }
    Ok(urls)
}

static UNLINK_CACHE: AtomicU64 = AtomicU64::new(0);

pub async fn handle_message(http: &Http, message: &Message) -> anyhow::Result<()> {
    if !message.content.contains(SHORTENED_URL_START) {
        return Ok(());
    }
    let offset = message.id.get() % 64;
    let mask = !(1 << offset);
    UNLINK_CACHE.fetch_and(mask, std::sync::atomic::Ordering::AcqRel);
    message
        .react(http, ReactionType::Unicode(UNLINK_REACT.to_string()))
        .await?;
    Ok(())
}

pub async fn handle_reaction(
    handler: &Handler,
    http: &Http,
    react: &Reaction,
) -> anyhow::Result<()> {
    if !react.emoji.unicode_eq(UNLINK_REACT) || handler.self_id.get().copied() == react.user_id {
        return Ok(());
    }
    let offset = react.message_id.get() % 64;
    let mask = 1 << offset;
    let previous = UNLINK_CACHE.fetch_or(mask, std::sync::atomic::Ordering::AcqRel);
    if previous & mask != 0 {
        // already unlinked
        return Ok(());
    }
    let message = react.message(http).await?;
    let urls = resolve_spotify_links(&message.content).await?;
    if urls.is_empty() {
        return Ok(());
    }
    let plural_s = if urls.len() > 1 { "s" } else { "" };
    let mut resp = format!("Resolved spotify link{plural_s}");
    urls.into_iter().for_each(|url| {
        resp.push('\n');
        resp.push_str(&url);
    });
    _ = message.reply(http, resp).await;
    Ok(())
}

#[async_trait]
impl BotCommand for Unlink {
    type Data = Handler;

    async fn run(
        self,
        _: &Handler,
        _: &Context,
        _: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let urls = resolve_spotify_links(&self.0.content).await?;
        if urls.is_empty() {
            bail!("No shortened spotify links found in message");
        }
        let plural_s = if urls.len() > 1 { "s" } else { "" };
        let mut resp = format!("Resolved spotify link{plural_s} from {}", self.0.link());
        urls.into_iter().for_each(|url| {
            resp.push('\n');
            resp.push_str(&url)
        });
        CommandResponse::public(resp)
    }
}

#[async_trait]
impl Module for Spotify<AuthCodeSpotify> {
    async fn setup(&mut self, db: &mut crate::db::Db) -> anyhow::Result<()> {
        db.add_guild_field("spotify_user_id", "STRING")
    }

    fn register_commands(&self, store: &mut CommandStore, _: &mut CompletionStore) {
        store.register::<SpotifyAuthenticate>();
    }
    //
    // async fn handle_component_interaction(
    //     &self,
    //     handler: &Handler,
    //     ctx: &Context,
    //     interaction: &ComponentInteraction,
    // ) -> Option<anyhow::Result<()>> {
    //     if interaction.data.custom_id != INTERACTION_AUTH_PROMPT {
    //         return None;
    //     }
    //
    //     Some(
    //         interaction
    //             .create_response(
    //                 &ctx.http,
    //                 CreateInteractionResponse::Modal(
    //                     CreateModal::new("spotify-auth-submit", "Spotify Authentication")
    //                         .components(vec![CreateActionRow::InputText(CreateInputText::new(
    //                             InputTextStyle::Short,
    //                             "Redirection URL",
    //                             "spotify-auth-token",
    //                         ))]),
    //                 ),
    //             )
    //             .await
    //             .map_err(|err| err.into()),
    //     )
    // }
}

impl RegisterableModule for Spotify<AuthCodeSpotify> {
    async fn init(_: &ModuleMap) -> anyhow::Result<Self> {
        Err(anyhow!(
            "Must be initialized with new_auth_code and added using with_module"
        ))
    }
}
