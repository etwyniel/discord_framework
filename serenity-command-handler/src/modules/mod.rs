pub mod spotify;
use rspotify::ClientCredsSpotify;
pub type Spotify = spotify::Spotify<ClientCredsSpotify>;
pub use spotify::SpotifyOAuth;

pub mod bandcamp;
pub use bandcamp::Bandcamp;

pub mod tidal;
pub use tidal::Tidal;

pub mod lastfm;
pub use lastfm::Lastfm;

pub mod polls;
pub use polls::ModPoll;

pub mod autoreact;
pub use autoreact::ModAutoreacts;

pub mod quotes;
pub use quotes::Quotes;

pub mod pinboard;
pub use pinboard::Pinboard;

pub mod lp;
pub use lp::ModLp;

pub mod album_lookup;
pub use album_lookup::AlbumLookup;

pub mod bdays;

pub mod sql;

pub mod forms;
pub use forms::Forms;

pub mod playlist_builder;
pub use playlist_builder::PlaylistBuilder;

pub mod complete;

pub mod spotify_activity;
