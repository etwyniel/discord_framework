use serenity::all::{CreateAttachment, CreateCommandOption, CreateInteractionResponseFollowup};
use serenity::model::prelude::CommandInteraction;
use serenity::{async_trait, prelude::Context};
use serenity_command::{CommandResponse, args, command};

use std::fmt::Write;
use std::sync::Arc;

use serenity_command_handler::album::{Album, AlbumProvider};
use serenity_command_handler::prelude::*;

use bandcamp::Bandcamp;
use lastfm::Lastfm;
use tidal::Tidal;
type Spotify = spotify::Spotify<spotify::rspotify::ClientCredsSpotify>;

use anyhow::Context as _;

args!(LOOKUP_ALBUM_ARGS =
     "The album you are looking for (e.g. band - album)"
    album: String,
     "Where to look for album info (defaults to spotify)"
    provider: Option<String>,
);

/// configure options for the /album command
fn set_lookup_options(
    name: &str,
    opt: CreateCommandOption<'static>,
) -> CreateCommandOption<'static> {
    if name == "provider" {
        opt.add_string_choice("spotify", "spotify")
            .add_string_choice("bandcamp", "bandcamp")
            .add_string_choice("tidal", "tidal")
    } else {
        opt
    }
}

const LOOKUP_ALBUM: CommandConst = CommandConst {
    description: "Look up an album",
    ..command!(/album LOOKUP_ALBUM_ARGS(set_lookup_options): lookup_album)
};

/// look up an album using the specified provider
async fn lookup_album(
    (album, provider): LOOKUP_ALBUM_ARGS,
    handler: &Handler,
    ctx: &Context,
    command: &CommandInteraction,
) -> anyhow::Result<CommandResponse> {
    command.defer(&ctx.http).await?;
    let album_lookup = handler.module::<AlbumLookup>()?;
    let mut info = if album.starts_with("https://") {
        // command called with a URL, ignore provider param, find appropriate
        // album provider and fetch metadata
        let provider = album_lookup
            .providers
            .iter()
            .find(|p| p.url_matches(&album))
            .context("Unable to fetch metadata for this type of link")?;
        provider.get_from_url(&album).await?
    } else {
        // use specified provider to find album metadata
        let provider = album_lookup.get_provider(provider.as_deref());
        provider.query_album(&album).await?
    };
    // start message content with basic information and link
    let mut contents = info.as_linked_header(None);
    contents.push('\n');

    // add extra metadata if present

    let mut add_sep = false;
    if let Some(duration) = info.format_duration() {
        add_sep = true;
        _ = write!(&mut contents, "*{duration}*");
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
    let mut attachment = None;
    if !info.has_rich_embed {
        // album provider doesn't provide nice embeds,
        // add track list and album cover if present
        contents.push_str(&info.format_tracks(None));
        if let Some(url) = info.cover {
            attachment = CreateAttachment::url(&ctx.http, url, "cover.jpg")
                .await
                .ok();
        }
    }
    let mut resp = CreateInteractionResponseFollowup::new().content(contents);
    if let Some(attachment) = attachment {
        resp = resp.add_file(attachment);
    }
    command.create_followup(&ctx.http, resp).await?;

    Ok(CommandResponse::None)
}

/// album lookup module
pub struct AlbumLookup {
    providers: Vec<Arc<dyn AlbumProvider>>,
}

impl AlbumLookup {
    /// get album provider from provider name
    pub fn get_provider(&self, provider: Option<&str>) -> &dyn AlbumProvider {
        provider
            .and_then(|id| self.providers.iter().find(|p| p.id() == id))
            .or_else(|| self.providers.first())
            .unwrap()
            .as_ref()
    }

    /// list loaded providers
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
    fn register_commands(&self, store: &mut dyn Storer) {
        store.register(LOOKUP_ALBUM);
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
                m.module_arc::<Tidal>()?,
                m.module_arc::<Bandcamp>()?,
                m.module_arc::<Spotify>()?,
            ],
        })
    }
}
