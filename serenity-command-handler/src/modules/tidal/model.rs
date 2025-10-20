use iso8601_duration::Duration;
use serde::Deserialize;

use crate::album::Album;

pub const BASE: &str = "https://openapi.tidal.com/v2";
pub const ALBUM_URL: &str = "https://tidal.com/album";

pub fn album_share_url(album_id: &str) -> String {
    format!("{ALBUM_URL}/{album_id}/u")
}

#[derive(Deserialize)]
pub struct AuthResponse {
    pub access_token: String,
    pub expires_in: u64,
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
    pub fn artist(self) -> Option<(String, ArtistAttributes)> {
        match self.entity {
            IncludedEntity::Artist(a) => Some((self.id, a)),
            _ => None,
        }
    }

    pub fn album(self) -> Option<(String, AlbumAttributes)> {
        match self.entity {
            IncludedEntity::Album(a) => Some((self.id, a)),
            _ => None,
        }
    }

    pub fn artwork(self) -> Option<(String, ArtworkAttributes)> {
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

#[derive(Deserialize, Debug)]
pub struct Error {
    pub code: String,
    pub detail: String,
}

#[derive(Deserialize, Debug)]
pub struct ErrorResponse {
    pub errors: Vec<Error>,
}

impl AlbumAttributes {
    pub fn into_album(
        self,
        id: String,
        artists: &[Relationship],
        included: Vec<IncludedItem>,
    ) -> Album {
        let duration = Duration::parse(&self.duration)
            .ok()
            .and_then(|dur| Duration::to_chrono(&dur));
        let artist = artists
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
            name: Some(self.title),
            release_date: self.release_date,
            duration,
            url: Some(album_share_url(&id)),
            artist,

            is_playlist: false,
            cover,
            genres: Vec::new(),
        }
    }
}
