use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde::Deserialize;
use serde_json::Value;

fn encode(s: &str) -> String {
    utf8_percent_encode(s, NON_ALPHANUMERIC).to_string()
}

/// Fetches lyrics for a track from lyrics.ovh (free, keyless). Returns
/// `Ok(None)` if the service has no lyrics for this artist/title rather
/// than treating that as an error.
pub fn fetch_lyrics(artist: &str, title: &str) -> Result<Option<String>, String> {
    let url = format!(
        "https://api.lyrics.ovh/v1/{}/{}",
        encode(artist),
        encode(title)
    );

    let response = match ureq::get(&url).call() {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };

    let mut response = response;
    let json: Value = response
        .body_mut()
        .read_json()
        .map_err(|e| e.to_string())?;

    Ok(json
        .get("lyrics")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty()))
}

#[derive(Deserialize)]
struct WikiSummary {
    extract: Option<String>,
    #[serde(rename = "content_urls")]
    content_urls: Option<WikiContentUrls>,
}

#[derive(Deserialize)]
struct WikiContentUrls {
    desktop: Option<WikiDesktopUrl>,
}

#[derive(Deserialize)]
struct WikiDesktopUrl {
    page: Option<String>,
}

pub struct ArtistBio {
    pub summary: String,
    pub source_url: Option<String>,
}

/// Fetches a short artist bio from Wikipedia's free REST summary API (no
/// key required). Returns `Ok(None)` if there's no matching page.
pub fn fetch_artist_bio(artist: &str) -> Result<Option<ArtistBio>, String> {
    let url = format!(
        "https://en.wikipedia.org/api/rest_v1/page/summary/{}",
        encode(artist)
    );

    let response = match ureq::get(&url).call() {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };

    let mut response = response;
    let summary: WikiSummary = response
        .body_mut()
        .read_json()
        .map_err(|e| e.to_string())?;

    let Some(extract) = summary.extract.filter(|s| !s.is_empty()) else {
        return Ok(None);
    };

    Ok(Some(ArtistBio {
        summary: extract,
        source_url: summary.content_urls.and_then(|u| u.desktop).and_then(|d| d.page),
    }))
}
