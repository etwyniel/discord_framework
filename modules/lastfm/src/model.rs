use serde_derive::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
pub struct Tag {
    pub count: u64,
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopTags {
    pub tag: Vec<Tag>,
    #[serde(rename = "@attr")]
    pub attributes: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtistTopTags {
    pub toptags: TopTags,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Date {
    pub uts: String,
    #[serde(rename = "#text")]
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtistId {
    pub mbid: String,
    #[serde(rename = "#text")]
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlbumId {
    pub mbid: String,
    #[serde(rename = "#text")]
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Image {
    pub size: String,
    #[serde(rename = "#text")]
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecentTrackAttrs {
    pub nowplaying: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Track {
    pub name: String,
    pub url: String,
    pub mbid: String,
    pub date: Option<Date>,
    pub artist: ArtistId,
    pub album: AlbumId,
    pub image: Vec<Image>,
    #[serde(rename = "@attr")]
    pub attr: Option<RecentTrackAttrs>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecentTracksAttrs {
    pub user: String,
    #[serde(rename = "totalPages")]
    pub total_pages: String,
    pub total: String,
    pub page: String,
    #[serde(rename = "perPage")]
    pub per_page: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecentTracks {
    pub track: Vec<Track>,
    #[serde(rename = "@attr")]
    pub attr: RecentTracksAttrs,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecentTracksResp {
    pub recenttracks: RecentTracks,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Album {
    pub name: String,
    pub url: String,
    pub mbid: String,
    pub playcount: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtistShort {
    pub url: String,
    pub name: String,
    pub mbid: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopAlbum {
    pub name: String,
    pub mbid: String,
    pub url: String,
    pub artist: ArtistShort,
    pub image: Vec<Image>,
    pub playcount: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopAlbumsAttr {
    pub user: String,
    #[serde(rename = "totalPages")]
    pub total_pages: String,
    pub page: String,
    pub total: String,
    #[serde(rename = "perPage")]
    pub per_page: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopAlbums {
    pub album: Vec<TopAlbum>,
    #[serde(rename = "@attr")]
    pub attr: TopAlbumsAttr,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopAlbumsResp {
    pub topalbums: TopAlbums,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopTrack {
    pub name: String,
    pub mbid: Option<String>,
    pub artist: ArtistShort,
    pub url: String,
    pub playcount: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopTracks {
    pub track: Vec<TopTrack>,
    #[serde(rename = "@attr")]
    pub attr: TopAlbumsAttr,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TopTracksResponse {
    pub toptracks: TopTracks,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlbumShort {
    pub artist: String,
    pub title: String,
    pub mbid: Option<String>,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TrackInfo {
    pub name: String,
    pub mbid: Option<String>,
    pub url: String,
    pub artist: ArtistShort,
    pub album: Option<AlbumShort>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TrackInfoResponse {
    pub track: TrackInfo,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MbReleaseInfo {
    pub id: String,
    pub title: String,
    pub date: String,
}
