//! Spotify share links and URIs → `(kind, id)`: `https://open.spotify.com/playlist/<id>?si=…`,
//! `…/intl-de/album/<id>`, the old `…/user/<name>/playlist/<id>`, `spotify:track:<id>`,
//! `spotify:user:<name>:playlist:<id>`. Pure; the UI has the same rules in src/lib/links.js.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Playlist,
    Album,
    Artist,
    Track,
}

impl Kind {
    fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "playlist" => Kind::Playlist,
            "album" => Kind::Album,
            "artist" => Kind::Artist,
            "track" => Kind::Track,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Playlist => "playlist",
            Kind::Album => "album",
            Kind::Artist => "artist",
            Kind::Track => "track",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Link {
    pub kind: Kind,
    pub id: String,
}

impl Link {
    pub fn uri(&self) -> String {
        format!("spotify:{}:{}", self.kind.as_str(), self.id)
    }
}

/// A Spotify id: base62, 22 characters.
pub fn is_id(s: &str) -> bool {
    s.len() == 22 && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// The playlist, album, artist or track a share link or URI names; None for anything else.
pub fn parse(text: &str) -> Option<Link> {
    let t = text.trim();
    let parts: Vec<&str> = if let Some(rest) = t.strip_prefix("spotify:") {
        rest.split(':').collect()
    } else {
        let rest = t.strip_prefix("https://").or_else(|| t.strip_prefix("http://")).unwrap_or(t);
        let (host, path) = rest.split_once('/')?;
        if !matches!(host.to_ascii_lowercase().as_str(), "open.spotify.com" | "play.spotify.com") {
            return None;
        }
        let path = path.split(['?', '#']).next().unwrap_or("");
        path.split('/').filter(|p| !p.is_empty()).collect()
    };
    // the kind is the last kind word followed by an id: skips `intl-xx/` and `user/<name>/`
    parts.windows(2).rev().find_map(|w| {
        let kind = Kind::parse(w[0])?;
        is_id(w[1]).then(|| Link { kind, id: w[1].to_string() })
    })
}

/// Spotify's own playlists (editorial, Made For You, radio): their ids start with 37i9.
pub fn is_spotify_playlist(id: &str) -> bool {
    id.starts_with("37i9")
}

/// A personal Made For You mix (Daily Mix, Discover Weekly, Release Radar, artist/genre radio and
/// mixes, daylist): 37i9dQZF1E… and 37i9dQZEVX…; editorial and "This Is" lists are 37i9dQZF1D….
pub fn is_personal_mix(id: &str) -> bool {
    id.starts_with("37i9dQZF1E") || id.starts_with("37i9dQZEVX")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(kind: Kind, id: &str) -> Option<Link> {
        Some(Link { kind, id: id.into() })
    }

    #[test]
    fn share_links() {
        assert_eq!(parse("https://open.spotify.com/playlist/37i9dQZEVXcVV9hd3iqSgp?si=384f379cb4d54126"), l(Kind::Playlist, "37i9dQZEVXcVV9hd3iqSgp"));
        assert_eq!(parse("  https://open.spotify.com/playlist/37i9dQZF1E4qxgJU46pFLr?si=83348eaf08b543d1\n"), l(Kind::Playlist, "37i9dQZF1E4qxgJU46pFLr"));
        assert_eq!(parse("https://open.spotify.com/intl-de/album/6dVIqQ8qmQ5GBnJ9shOYGE"), l(Kind::Album, "6dVIqQ8qmQ5GBnJ9shOYGE"));
        assert_eq!(parse("open.spotify.com/artist/4Z8W4fKeB5YxbusRsdQVPb#x"), l(Kind::Artist, "4Z8W4fKeB5YxbusRsdQVPb"));
        assert_eq!(parse("https://open.spotify.com/track/7c378mlmubSu7NGkLFa4sN?si=a&context=b"), l(Kind::Track, "7c378mlmubSu7NGkLFa4sN"));
        assert_eq!(parse("https://open.spotify.com/user/spotify/playlist/37i9dQZF1DXcBWIGoYBM5M"), l(Kind::Playlist, "37i9dQZF1DXcBWIGoYBM5M"));
    }

    #[test]
    fn uris() {
        assert_eq!(parse("spotify:playlist:37i9dQZEVXcVV9hd3iqSgp"), l(Kind::Playlist, "37i9dQZEVXcVV9hd3iqSgp"));
        assert_eq!(parse("spotify:user:someone:playlist:37i9dQZEVXcVV9hd3iqSgp"), l(Kind::Playlist, "37i9dQZEVXcVV9hd3iqSgp"));
        assert_eq!(parse("spotify:track:7c378mlmubSu7NGkLFa4sN").unwrap().uri(), "spotify:track:7c378mlmubSu7NGkLFa4sN");
    }

    #[test]
    fn not_links() {
        for bad in [
            "",
            "bonobo",
            "https://example.com/playlist/37i9dQZEVXcVV9hd3iqSgp",
            "https://open.spotify.com/show/37i9dQZEVXcVV9hd3iqSgp",
            "https://open.spotify.com/playlist/short",
            "spotify:playlist:",
            "spotify:episode:7c378mlmubSu7NGkLFa4sN",
            "https://spotify.link/abc",
        ] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn mix_ids() {
        assert!(is_personal_mix("37i9dQZF1E4yLltmVk3nyb")); // Bonobo Radio
        assert!(is_personal_mix("37i9dQZEVXcVV9hd3iqSgp")); // Discover Weekly
        assert!(!is_personal_mix("37i9dQZF1DXcBWIGoYBM5M")); // editorial
        assert!(is_spotify_playlist("37i9dQZF1DXcBWIGoYBM5M"));
        assert!(!is_spotify_playlist("1A2b3C4d5E6f7G8h9I0jKl"));
    }
}
