use serenity::all::CreateCommandOption;
use serenity::model::prelude::CommandInteraction;
use serenity::{async_trait, prelude::Context};
use serenity_command::{BotCommand, CommandResponse};
use serenity_command_derive::Command;

use std::fmt::Write;
use std::sync::Arc;

use crate::album::{Album, AlbumProvider};
use crate::modules::{Bandcamp, Lastfm, Spotify, Tidal};
use crate::{
    CommandStore, CompletionStore, Handler, HandlerBuilder, Module, ModuleMap, RegisterableModule,
};

use anyhow::Context as _;

#[derive(Command)]
#[cmd(name = "album", desc = "lookup an album")]
struct LookupAlbum {
    #[cmd(desc = "The album you are looking for (e.g. band - album)")]
    album: String,
    #[cmd(desc = "Where to look for album info (defaults to spotify)")]
    provider: Option<String>,
}

#[async_trait]
impl BotCommand for LookupAlbum {
    type Data = Handler;
    async fn run(
        self,
        handler: &Handler,
        _ctx: &Context,
        _opts: &CommandInteraction,
    ) -> anyhow::Result<CommandResponse> {
        let album_lookup = handler.module::<AlbumLookup>()?;
        let mut info = if self.album.starts_with("https://") {
            // command called with a URL, ignore provider param, find appropriate
            // album provider and fetch metadata
            let provider = album_lookup
                .providers
                .iter()
                .find(|p| p.url_matches(&self.album))
                .context("Unable to fetch metadata for this type of link")?;
            provider.get_from_url(&self.album).await?
        } else {
            let provider = album_lookup.get_provider(self.provider.as_deref());
            provider.query_album(&self.album).await?
        };
        let mut contents = info.as_linked_header(None);
        _ = writeln!(&mut contents);

        let mut add_sep = false;
        if let Some(duration) = info.duration {
            add_sep = true;
            contents.push('*');
            if duration.num_hours() > 0 {
                _ = write!(&mut contents, "{}h", duration.num_hours());
            }
            let minutes = duration.num_minutes() % 60;
            let seconds = duration.num_seconds();
            if minutes > 0 || seconds > 0 {
                _ = write!(&mut contents, "{minutes:02}m");
            }
            if seconds < 60 {
                _ = write!(&mut contents, "{seconds}s");
            }
            contents.push('*');
        }

        if let Some(release_date) = &info.release_date {
            if add_sep {
                _ = write!(&mut contents, " | ");
            }
            add_sep = true;
            _ = write!(&mut contents, "__*{release_date}*__");
        }

        if info.genres.is_empty()
            && let Some(artist) = &info.artist
        {
            info.genres = handler.module::<Lastfm>()?.artist_top_tags(artist).await?;
        }
        if let Some(genres) = info.format_genres() {
            if add_sep {
                _ = write!(&mut contents, " | ");
            }
            _ = writeln!(&mut contents, "{genres}");
        }
        let mut attachments = Vec::new();
        if !info.has_rich_embed {
            contents.push_str(&info.format_tracks(None));
            attachments.extend(info.cover);
        }
        Ok(CommandResponse::Public(
            serenity_command::ResponseType::WithAttachments(contents, Vec::new(), attachments),
        ))
    }

    fn setup_options(opt_name: &str, opt: CreateCommandOption) -> CreateCommandOption {
        if opt_name == "provider" {
            opt.add_string_choice("spotify", "spotify")
                .add_string_choice("bandcamp", "bandcamp")
                .add_string_choice("tidal", "tidal")
        } else {
            opt
        }
    }
}

pub struct AlbumLookup {
    providers: Vec<Arc<dyn AlbumProvider>>,
}

impl AlbumLookup {
    pub fn get_provider(&self, provider: Option<&str>) -> &dyn AlbumProvider {
        provider
            .and_then(|id| self.providers.iter().find(|p| p.id() == id))
            .or_else(|| self.providers.first())
            .unwrap()
            .as_ref()
    }

    pub fn providers(&self) -> &[Arc<dyn AlbumProvider>] {
        &self.providers
    }

    pub async fn get_album_info(&self, link: &str) -> anyhow::Result<Option<Album>> {
        if let Some(p) = self.providers.iter().find(|p| p.url_matches(link)) {
            let info = p.get_from_url(link).await?;
            return Ok(Some(info));
        }
        Ok(None)
    }

    pub async fn lookup_album(
        &self,
        query: &str,
        provider: Option<&str>,
    ) -> anyhow::Result<Option<Album>> {
        let p = self.get_provider(provider);
        p.query_album(query).await.map(Some)
    }

    pub async fn query_albums(
        &self,
        query: &str,
        provider: Option<&str>,
    ) -> anyhow::Result<Vec<(String, String)>> {
        let p = self.get_provider(provider);
        let mut choices = p.query_albums(query).await?;
        choices.iter_mut().for_each(|(name, _)| {
            if name.len() >= 100 {
                *name = name.chars().take(100).collect();
            }
        });
        Ok(choices)
    }

    pub fn add_provider<P: AlbumProvider + 'static>(&mut self, p: Arc<P>) {
        self.providers.push(p);
    }
}

#[async_trait]
impl Module for AlbumLookup {
    fn register_commands(&self, store: &mut CommandStore, _completions: &mut CompletionStore) {
        store.register::<LookupAlbum>();
    }
}

impl RegisterableModule for AlbumLookup {
    async fn add_dependencies(builder: HandlerBuilder) -> anyhow::Result<HandlerBuilder> {
        builder
            .module::<Lastfm>()
            .await?
            .module::<Spotify>()
            .await?
            .module::<Bandcamp>()
            .await?
            .module::<Tidal>()
            .await
    }

    async fn init(m: &ModuleMap) -> anyhow::Result<Self> {
        Ok(AlbumLookup {
            providers: vec![
                m.module_arc::<Spotify>()?,
                m.module_arc::<Bandcamp>()?,
                m.module_arc::<Tidal>()?,
            ],
        })
    }
}
