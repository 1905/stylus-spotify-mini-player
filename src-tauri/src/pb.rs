//! Protobuf answers of Spotify's internal endpoints → the UI's JSON shapes (see parse.rs).
//! librespot-protocol has the playlist4, extended-metadata, metadata and connect messages;
//! collection v2 (Liked Songs paging, save/unsave) isn't compiled there, so its four small
//! messages are encoded and decoded by hand here.

use librespot_core::SpotifyId;
use librespot_protocol::{
    connect::Cluster,
    extended_metadata::{BatchedEntityRequest, BatchedExtensionResponse, EntityRequest, ExtensionQuery},
    extension_kind::ExtensionKind,
    metadata::{self, image::Size},
    playlist4_external::SelectedListContent,
};
use protobuf::{EnumOrUnknown, Message};
use serde_json::{json, Value};

// ---- hand-rolled wire format (collection v2) ------------------------------------------

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_bytes(out: &mut Vec<u8>, field: u32, b: &[u8]) {
    put_varint(out, u64::from(field << 3 | 2));
    put_varint(out, b.len() as u64);
    out.extend_from_slice(b);
}

fn put_str(out: &mut Vec<u8>, field: u32, s: &str) {
    if !s.is_empty() {
        put_bytes(out, field, s.as_bytes());
    }
}

fn put_int(out: &mut Vec<u8>, field: u32, v: u64) {
    if v != 0 {
        put_varint(out, u64::from(field << 3));
        put_varint(out, v);
    }
}

/// A wire value: a varint, or a length-delimited slice.
enum Wire<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
}

/// The (field, value) pairs of one message. Fixed-width fields are skipped; a broken message errors.
fn fields(mut b: &[u8]) -> Result<Vec<(u32, Wire<'_>)>, String> {
    fn varint(b: &mut &[u8]) -> Result<u64, String> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let (&byte, rest) = b.split_first().ok_or("truncated varint")?;
            *b = rest;
            v |= u64::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                return Ok(v);
            }
        }
        Err("varint too long".into())
    }
    let mut out = Vec::new();
    while !b.is_empty() {
        let key = varint(&mut b)?;
        let field = (key >> 3) as u32;
        match key & 7 {
            0 => out.push((field, Wire::Varint(varint(&mut b)?))),
            2 => {
                let len = varint(&mut b)? as usize;
                if len > b.len() {
                    return Err("truncated field".into());
                }
                let (v, rest) = b.split_at(len);
                b = rest;
                out.push((field, Wire::Bytes(v)));
            }
            1 => b = b.get(8..).ok_or("truncated fixed64")?,
            5 => b = b.get(4..).ok_or("truncated fixed32")?,
            t => return Err(format!("wire type {t}")),
        }
    }
    Ok(out)
}

/// collection2v2 `CollectionItem`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CollectionItem {
    pub uri: String,
    pub added_at: i64,
    pub is_removed: bool,
}

fn encode_item(i: &CollectionItem) -> Vec<u8> {
    let mut out = Vec::new();
    put_str(&mut out, 1, &i.uri);
    put_int(&mut out, 2, i.added_at as u32 as u64);
    put_int(&mut out, 3, u64::from(i.is_removed));
    out
}

/// `PageRequest {username=1, set=2, pagination_token=3, limit=4}`.
pub fn page_request(username: &str, set: &str, token: &str, limit: u32) -> Vec<u8> {
    let mut out = Vec::new();
    put_str(&mut out, 1, username);
    put_str(&mut out, 2, set);
    put_str(&mut out, 3, token);
    put_int(&mut out, 4, u64::from(limit));
    out
}

/// `WriteRequest {username=1, set=2, items=3, client_update_id=4}`.
pub fn write_request(username: &str, set: &str, items: &[CollectionItem], client_update_id: &str) -> Vec<u8> {
    let mut out = Vec::new();
    put_str(&mut out, 1, username);
    put_str(&mut out, 2, set);
    for i in items {
        put_bytes(&mut out, 3, &encode_item(i));
    }
    put_str(&mut out, 4, client_update_id);
    out
}

/// `PageResponse {items=1, next_page_token=2, sync_token=3}` → (items, next page token).
pub fn page_response(b: &[u8]) -> Result<(Vec<CollectionItem>, String), String> {
    let mut items = Vec::new();
    let mut next = String::new();
    for (f, w) in fields(b)? {
        match (f, w) {
            (1, Wire::Bytes(item)) => {
                let mut i = CollectionItem::default();
                for (f, w) in fields(item)? {
                    match (f, w) {
                        (1, Wire::Bytes(s)) => i.uri = String::from_utf8_lossy(s).into_owned(),
                        (2, Wire::Varint(v)) => i.added_at = v as u32 as i32 as i64,
                        (3, Wire::Varint(v)) => i.is_removed = v != 0,
                        _ => {}
                    }
                }
                items.push(i);
            }
            (2, Wire::Bytes(s)) => next = String::from_utf8_lossy(s).into_owned(),
            _ => {}
        }
    }
    Ok((items, next))
}

/// Liked Songs from collection pages: tracks only, removed ones dropped, newest first.
pub fn liked_uris(items: Vec<CollectionItem>) -> Vec<String> {
    liked_uris_of(items, "spotify:track:")
}

/// The uris starting with `prefix` of collection pages, removed ones dropped, newest first.
pub fn liked_uris_of(mut items: Vec<CollectionItem>, prefix: &str) -> Vec<String> {
    items.retain(|i| !i.is_removed && i.uri.starts_with(prefix));
    items.sort_by_key(|i| std::cmp::Reverse(i.added_at));
    items.into_iter().map(|i| i.uri).collect()
}

// ---- extended metadata ------------------------------------------------------------------

/// A batched extended-metadata request for one kind.
pub fn ext_request(uris: &[String], kind: ExtensionKind) -> Vec<u8> {
    let req = BatchedEntityRequest {
        entity_request: uris
            .iter()
            .map(|u| EntityRequest {
                entity_uri: u.clone(),
                query: vec![ExtensionQuery { extension_kind: EnumOrUnknown::new(kind), ..Default::default() }],
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    req.write_to_bytes().unwrap_or_default()
}

/// A batched answer → (entity uri, payload bytes). The answer is unordered.
pub fn ext_response(b: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let parsed = BatchedExtensionResponse::parse_from_bytes(b).map_err(|e| format!("extended metadata: {e}"))?;
    Ok(parsed
        .extended_metadata
        .into_iter()
        .flat_map(|arr| arr.extension_data)
        .filter_map(|d| d.extension_data.into_option().map(|any| (d.entity_uri, any.value)))
        .collect())
}

fn b62(gid: &[u8]) -> String {
    SpotifyId::from_raw(gid).ok().and_then(|i| i.to_base62().ok()).unwrap_or_default()
}

/// The UI's cover url of an image list: LARGE (640 px), else the biggest.
fn cover(images: &[metadata::Image]) -> Option<String> {
    let rank = |s: Size| match s {
        Size::SMALL => 0,
        Size::DEFAULT => 1,
        Size::LARGE => 3,
        Size::XLARGE => 2,
    };
    images
        .iter()
        .filter(|i| !i.file_id().is_empty())
        .max_by_key(|i| rank(i.size()))
        .map(|i| format!("https://i.scdn.co/image/{}", hex(i.file_id())))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn album_cover(a: &metadata::Album) -> Option<String> {
    cover(&a.cover_group.image).or_else(|| cover(&a.cover))
}

/// TRACK_V4 payload → the UI's Track (uri from the request: the payload's gid can be a relinked one).
pub fn track(uri: &str, b: &[u8]) -> Option<Value> {
    let t = metadata::Track::parse_from_bytes(b).ok()?;
    if t.name().is_empty() {
        return None;
    }
    let artists: Vec<Value> = t.artist.iter().map(|a| json!({ "id": b62(a.gid()), "name": a.name() })).collect();
    let names: Vec<&str> = t.artist.iter().map(|a| a.name()).collect();
    Some(json!({
        "id": crate::parse::id_of(uri),
        "uri": uri,
        "name": t.name(),
        "artists": names.join(", "),
        "artist_list": artists,
        "album": t.album.name(),
        "cover": album_cover(&t.album),
        "duration_ms": t.duration().max(0),
    }))
}

/// Tracks in `uris` order from TRACK_V4 payloads; uris without metadata are skipped.
pub fn tracks_in_order(uris: &[String], payloads: Vec<(String, Vec<u8>)>) -> Vec<Value> {
    let by_uri: std::collections::HashMap<String, Vec<u8>> = payloads.into_iter().collect();
    uris.iter().filter_map(|u| by_uri.get(u).and_then(|b| track(u, b))).collect()
}

/// ALBUM_V4 payload → (album name, cover, track uris in disc order).
pub fn album(b: &[u8]) -> Option<(String, Option<String>, Vec<String>)> {
    let a = metadata::Album::parse_from_bytes(b).ok()?;
    let uris = a.disc.iter().flat_map(|d| d.track.iter()).map(|t| format!("spotify:track:{}", b62(t.gid()))).collect();
    Some((a.name().to_string(), album_cover(&a), uris))
}

/// ALBUM_V4 payload → `get_artist_albums`' `{id, name, cover, year, kind}`.
pub fn album_tile(uri: &str, b: &[u8], kind: &str) -> Option<Value> {
    let a = metadata::Album::parse_from_bytes(b).ok()?;
    let year = a.date.as_ref().map(|d| d.year()).filter(|y| *y > 0).map(|y| y.to_string());
    Some(json!({ "id": crate::parse::id_of(uri), "name": a.name(), "cover": album_cover(&a), "year": year, "kind": kind }))
}

/// `{id, name, image}`, top track uris, (album uri, "album" | "single").
pub type ArtistMeta = (Value, Vec<String>, Vec<(String, &'static str)>);

/// ARTIST_V4 payload → (`{id, name, image}`, top track uris for `country` (else the first list),
/// album uris with their kind: albums first, then singles, newest group first).
pub fn artist(uri: &str, b: &[u8], country: &str) -> Option<ArtistMeta> {
    let a = metadata::Artist::parse_from_bytes(b).ok()?;
    let image = cover(&a.portrait_group.image).or_else(|| cover(&a.portrait));
    let info = json!({ "id": crate::parse::id_of(uri), "name": a.name(), "image": image });
    let top = a.top_track.iter().find(|t| t.country() == country).or(a.top_track.first());
    let top_uris = top.map(|t| t.track.iter().map(|tr| format!("spotify:track:{}", b62(tr.gid()))).collect()).unwrap_or_default();
    let group = |g: &[metadata::AlbumGroup], kind: &'static str| -> Vec<(String, &'static str)> {
        // each group is one release; its first album is the main edition
        g.iter().filter_map(|grp| grp.album.first()).map(|al| (format!("spotify:album:{}", b62(al.gid())), kind)).collect()
    };
    let mut albums = group(&a.album_group, "album");
    albums.extend(group(&a.single_group, "single"));
    Some((info, top_uris, albums))
}

// ---- playlists ---------------------------------------------------------------------------

/// A rootlist entry: what the playlist list needs that pathfinder lacks (count, order).
#[derive(Debug, Clone, PartialEq)]
pub struct RootEntry {
    pub uri: String,
    pub name: String,
    pub length: i64,
    pub owner: String,
    /// The revision as the Web API's `snapshot_id` (base64).
    pub snapshot_id: Option<String>,
    pub picture: Option<String>,
}

fn b64(b: &[u8]) -> Option<String> {
    use base64::Engine;
    (!b.is_empty()).then(|| base64::engine::general_purpose::STANDARD.encode(b))
}

/// `playlist/v2/user/{u}/rootlist` → its playlists in the user's order (folder markers skipped).
pub fn rootlist(b: &[u8]) -> Result<Vec<RootEntry>, String> {
    let c = SelectedListContent::parse_from_bytes(b).map_err(|e| format!("rootlist: {e}"))?;
    Ok(c.contents
        .items
        .iter()
        .zip(c.contents.meta_items.iter())
        .filter(|(i, _)| i.uri().starts_with("spotify:playlist:"))
        .map(|(i, m)| RootEntry {
            uri: i.uri().to_string(),
            name: m.attributes.name().to_string(),
            length: i64::from(m.length()),
            owner: m.owner_username().to_string(),
            snapshot_id: b64(m.revision()),
            picture: picture(&m.attributes),
        })
        .collect())
}

/// A playlist's cover from its attributes: the biggest named size, else the picture id.
fn picture(a: &librespot_protocol::playlist4_external::ListAttributes) -> Option<String> {
    let sizes = &a.picture_size;
    let by = |name: &str| sizes.iter().find(|p| p.target_name() == name).map(|p| p.url().to_string());
    by("large").or_else(|| by("default")).or_else(|| sizes.first().map(|p| p.url().to_string())).filter(|u| !u.is_empty()).or_else(|| {
        (!a.picture().is_empty()).then(|| format!("https://i.scdn.co/image/{}", hex(a.picture())))
    })
}

/// A rootlist entry as the UI's playlist (`get_playlists`' shape).
pub fn root_playlist(e: &RootEntry) -> Value {
    let images: Vec<Value> = e.picture.iter().map(|u| json!({ "url": u, "width": null, "height": null })).collect();
    json!({
        "id": crate::parse::id_of(&e.uri),
        "uri": e.uri,
        "name": e.name,
        "images": images,
        "tracks": { "total": e.length },
        "snapshot_id": e.snapshot_id,
        "owner": { "id": e.owner, "display_name": e.owner },
    })
}

/// `playlist/v2/playlist/{id}` → (track uris of this page, total length, snapshot id, name, cover).
pub struct PlaylistPage {
    pub uris: Vec<String>,
    pub length: i64,
    pub snapshot_id: Option<String>,
    pub name: String,
    pub cover: Option<String>,
}

pub fn playlist(b: &[u8]) -> Result<PlaylistPage, String> {
    let c = SelectedListContent::parse_from_bytes(b).map_err(|e| format!("playlist: {e}"))?;
    Ok(PlaylistPage {
        uris: c.contents.items.iter().map(|i| i.uri().to_string()).filter(|u| u.starts_with("spotify:track:")).collect(),
        length: i64::from(c.length()),
        snapshot_id: b64(c.revision()),
        name: c.attributes.name().to_string(),
        cover: picture(&c.attributes),
    })
}

// ---- Spotify Connect cluster ----------------------------------------------------------------

/// The Web API's device `type` for a Connect device type.
fn device_type(t: librespot_protocol::devices::DeviceType) -> &'static str {
    use librespot_protocol::devices::DeviceType as D;
    match t {
        D::COMPUTER | D::CHROMEBOOK => "Computer",
        D::TABLET => "Tablet",
        D::SMARTPHONE => "Smartphone",
        D::SPEAKER => "Speaker",
        D::TV => "TV",
        D::AVR => "AVR",
        D::STB => "STB",
        D::AUDIO_DONGLE => "AudioDongle",
        D::GAME_CONSOLE => "GameConsole",
        D::CAST_VIDEO => "CastVideo",
        D::CAST_AUDIO => "CastAudio",
        D::AUTOMOBILE => "Automobile",
        D::SMARTWATCH => "Smartwatch",
        _ => "Unknown",
    }
}

/// A cluster's devices as `list_devices` gives them (the Web API's device objects), hidden and
/// offline ones dropped, sorted by name for a stable menu.
pub fn devices(c: &Cluster) -> Vec<Value> {
    let mut out: Vec<Value> = c
        .device
        .iter()
        .filter(|(_, d)| !d.capabilities.hidden && !d.is_offline)
        .map(|(id, d)| {
            let id = if d.device_id.is_empty() { id.clone() } else { d.device_id.clone() };
            json!({
                "id": id,
                "name": d.name,
                "type": device_type(d.device_type.enum_value_or_default()),
                "is_active": c.active_device_id == id,
                "is_private_session": d.is_private_session,
                "is_restricted": !d.capabilities.is_controllable,
                "supports_volume": !d.capabilities.disable_volume,
                "volume_percent": (u64::from(d.volume) * 100 + 32767) / 65535,
            })
        })
        .collect();
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()).then_with(|| a["id"].as_str().cmp(&b["id"].as_str())));
    out
}

/// The up-next track uris of the cluster's active player (the queue first, as Spotify sends it).
pub fn next_track_uris(c: &Cluster, max: usize) -> Vec<String> {
    crate::nowplaying::next_uris(c.player_state.next_tracks.iter().map(|t| t.uri.as_str())).into_iter().take(max).collect()
}

/// What the cluster's active device plays, at `now_ms` (unix ms): `{device_id, track_uri,
/// context_uri, is_playing, position_ms, duration_ms, shuffle, repeat}`. None when no device is active.
pub fn cluster_state(c: &Cluster, now_ms: i64) -> Option<Value> {
    if c.active_device_id.is_empty() {
        return None;
    }
    let p = &c.player_state;
    let playing = p.is_playing && !p.is_paused;
    // a speed of 0 while playing is a missing field: real time
    let speed = if p.playback_speed > 0.0 { p.playback_speed.min(4.0) } else { 1.0 };
    let elapsed = if playing && p.timestamp > 0 { ((now_ms - p.timestamp).max(0) as f64 * speed) as i64 } else { 0 };
    let mut position = p.position_as_of_timestamp.max(0) + elapsed;
    if p.duration > 0 {
        position = position.min(p.duration);
    }
    let o = &p.options;
    let repeat = if o.repeating_track { "track" } else if o.repeating_context { "context" } else { "off" };
    let track_uri = Some(p.track.uri.as_str()).filter(|u| !u.is_empty());
    Some(json!({
        "device_id": c.active_device_id,
        "track_uri": track_uri,
        "context_uri": Some(p.context_uri.as_str()).filter(|u| !u.is_empty()),
        "is_playing": playing,
        "position_ms": position,
        "duration_ms": p.duration,
        "shuffle": o.shuffling_context,
        "repeat": repeat,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use librespot_protocol::connect::{Capabilities, DeviceInfo};
    use librespot_protocol::player::{PlayerState, ProvidedTrack};

    fn fx(name: &str) -> Vec<u8> {
        let path = format!("{}/src/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    #[test]
    fn cluster_state_position() {
        use librespot_protocol::player::ContextPlayerOptions;
        let mut c = Cluster::new();
        assert!(cluster_state(&c, 0).is_none(), "no active device");
        c.active_device_id = "phone".into();
        let mut ps = PlayerState::new();
        ps.timestamp = 1_000;
        ps.position_as_of_timestamp = 5_000;
        ps.duration = 6_000;
        ps.playback_speed = 1.0;
        ps.is_playing = true;
        ps.context_uri = "spotify:playlist:p".into();
        let mut t = ProvidedTrack::new();
        t.uri = "spotify:track:t".into();
        ps.track = Some(t).into();
        let mut o = ContextPlayerOptions::new();
        o.repeating_context = true;
        ps.options = Some(o).into();
        c.player_state = Some(ps).into();
        let s = cluster_state(&c, 1_500).unwrap();
        assert_eq!(s["position_ms"], 5_500);
        assert_eq!(s["repeat"], "context");
        assert_eq!(s["track_uri"], "spotify:track:t");
        assert_eq!(s["device_id"], "phone");
        // past the end: capped at the duration
        assert_eq!(cluster_state(&c, 9_000).unwrap()["position_ms"], 6_000);
        // paused: the position stands still
        c.player_state.mut_or_insert_default().is_paused = true;
        let s = cluster_state(&c, 3_000).unwrap();
        assert_eq!((s["position_ms"].as_i64(), s["is_playing"].as_bool()), (Some(5_000), Some(false)));
    }

    #[test]
    fn varints() {
        let mut out = Vec::new();
        put_varint(&mut out, 300);
        assert_eq!(out, [0xac, 0x02]);
        assert!(fields(&[0x08]).is_err(), "truncated");
        assert!(fields(&[0x0a, 0x05, 0x01]).is_err(), "length past the end");
    }

    #[test]
    fn page_request_bytes() {
        // username "u", set "collection", no token, limit 50
        let b = page_request("u", "collection", "", 50);
        let mut want = vec![0x0a, 1, b'u', 0x12, 10];
        want.extend_from_slice(b"collection");
        want.extend_from_slice(&[0x20, 50]);
        assert_eq!(b, want);
    }

    #[test]
    fn write_request_round_trip_through_page_decoder() {
        let items = vec![
            CollectionItem { uri: "spotify:track:a".into(), added_at: 1_790_000_000, is_removed: false },
            CollectionItem { uri: "spotify:track:b".into(), added_at: 0, is_removed: true },
        ];
        let b = write_request("u", "collection", &items, "id1");
        // a WriteRequest's items sit at field 3, a PageResponse's at 1: re-tag to reuse the decoder
        let items_only: Vec<u8> = {
            let mut out = Vec::new();
            for (f, w) in fields(&b).unwrap() {
                if let (3, Wire::Bytes(i)) = (f, w) {
                    put_bytes(&mut out, 1, i);
                }
            }
            put_str(&mut out, 2, "next");
            out
        };
        let (got, next) = page_response(&items_only).unwrap();
        assert_eq!(got, items);
        assert_eq!(next, "next");
    }

    #[test]
    fn collection_page_fixture() {
        // a real first page (limit 3) of Liked Songs
        let (items, next) = page_response(&fx("collectionPage.bin")).unwrap();
        assert_eq!(items.len(), 3);
        assert!(items.iter().all(|i| i.uri.starts_with("spotify:") && i.added_at > 1_400_000_000));
        assert!(!next.is_empty());
    }

    #[test]
    fn liked_order() {
        let i = |u: &str, t, r| CollectionItem { uri: u.into(), added_at: t, is_removed: r };
        let got = liked_uris(vec![i("spotify:track:old", 1, false), i("spotify:track:new", 9, false), i("spotify:track:gone", 5, true), i("spotify:episode:e", 7, false)]);
        assert_eq!(got, ["spotify:track:new", "spotify:track:old"]);
    }

    #[test]
    fn ext_request_round_trip() {
        let b = ext_request(&["spotify:track:x".into()], ExtensionKind::TRACK_V4);
        let req = BatchedEntityRequest::parse_from_bytes(&b).unwrap();
        assert_eq!(req.entity_request[0].entity_uri, "spotify:track:x");
        assert_eq!(req.entity_request[0].query[0].extension_kind.enum_value(), Ok(ExtensionKind::TRACK_V4));
    }

    #[test]
    fn track_metadata_fixture() {
        // TRACK_V4 for Creep and Airbag (real answer)
        let uris = vec!["spotify:track:70LcF31zb1H0PyJoS1Sx1r".to_string(), "spotify:track:7c378mlmubSu7NGkLFa4sN".to_string(), "spotify:track:missing".to_string()];
        let tracks = tracks_in_order(&uris, ext_response(&fx("extTracks.bin")).unwrap());
        assert_eq!(tracks.len(), 2, "the uri without metadata is skipped");
        assert_eq!(tracks[0]["name"], "Creep");
        assert_eq!(tracks[1]["name"], "Airbag");
        assert_eq!(tracks[1]["album"], "OK Computer");
        assert_eq!(tracks[0]["artist_list"][0], json!({"id": "4Z8W4fKeB5YxbusRsdQVPb", "name": "Radiohead"}));
        assert!(tracks[0]["cover"].as_str().unwrap().starts_with("https://i.scdn.co/image/ab67616d0000b273"));
        assert!(tracks[0]["duration_ms"].as_i64().unwrap() > 200_000);
    }

    #[test]
    fn album_and_artist_metadata_fixtures() {
        let a = ext_response(&fx("extAlbum.bin")).unwrap();
        let (name, cover, uris) = album(&a[0].1).unwrap();
        assert_eq!(name, "OK Computer");
        assert!(cover.unwrap().starts_with("https://i.scdn.co/image/"));
        assert_eq!(uris.len(), 12);
        assert_eq!(uris[0], "spotify:track:7c378mlmubSu7NGkLFa4sN");
        let tile = album_tile("spotify:album:6dVIqQ8qmQ5GBnJ9shOYGE", &a[0].1, "album").unwrap();
        assert_eq!(tile["year"], "1997");
        let r = ext_response(&fx("extArtist.bin")).unwrap();
        let (info, top, albums) = artist("spotify:artist:4Z8W4fKeB5YxbusRsdQVPb", &r[0].1, "").unwrap();
        assert_eq!(info["name"], "Radiohead");
        assert!(!top.is_empty() && top[0].starts_with("spotify:track:"));
        assert!(albums.iter().any(|(_, k)| *k == "album") && albums.iter().any(|(_, k)| *k == "single"));
    }

    #[test]
    fn playlist_fixture() {
        // Today's Top Hits, first 5 items
        let p = playlist(&fx("playlistV2.bin")).unwrap();
        assert_eq!(p.uris.len(), 5);
        assert!(p.length >= 50);
        assert!(p.snapshot_id.is_some());
        assert!(!p.name.is_empty());
    }

    #[test]
    fn rootlist_entries() {
        use librespot_protocol::playlist4_external::{Item, ListAttributes, ListItems, MetaItem, PictureSize};
        let mut c = SelectedListContent::new();
        let mut items = ListItems { pos: Some(0), truncated: Some(false), ..Default::default() };
        let item = |u: &str| Item { uri: Some(u.into()), ..Default::default() };
        let meta = |name: &str, len: i32, pic: Option<&str>| {
            let mut attributes = ListAttributes { name: Some(name.into()), ..Default::default() };
            if let Some(u) = pic {
                attributes.picture_size.push(PictureSize { target_name: Some("large".into()), url: Some(u.into()), ..Default::default() });
            }
            MetaItem { revision: Some(vec![1, 2, 3]), attributes: Some(attributes).into(), length: Some(len), owner_username: Some("me".into()), ..Default::default() }
        };
        items.items = vec![item("spotify:playlist:aaaaaaaaaaaaaaaaaaaaaa"), item("spotify:start-group:x:Folder"), item("spotify:playlist:bbbbbbbbbbbbbbbbbbbbbb")];
        items.meta_items = vec![meta("A", 12, Some("https://mosaic.scdn.co/640/x")), meta("", 0, None), meta("B", 3, None)];
        c.contents = Some(items).into();
        let got = rootlist(&c.write_to_bytes().unwrap()).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].snapshot_id.as_deref(), Some("AQID"));
        let p = root_playlist(&got[0]);
        assert_eq!(p["id"], "aaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(p["tracks"]["total"], 12);
        assert_eq!(p["images"][0]["url"], "https://mosaic.scdn.co/640/x");
        assert_eq!(root_playlist(&got[1])["images"], json!([]));
    }

    #[test]
    fn cluster_devices_and_queue() {
        let mut c = Cluster::new();
        c.active_device_id = "phone".into();
        let dev = |name: &str, controllable: bool, hidden: bool, volume: u32| {
            let caps = Capabilities { is_controllable: controllable, hidden, ..Default::default() };
            DeviceInfo { name: name.into(), volume, capabilities: Some(caps).into(), device_type: EnumOrUnknown::new(librespot_protocol::devices::DeviceType::SMARTPHONE), ..Default::default() }
        };
        c.device.insert("phone".into(), dev("Phone", true, false, 65535));
        c.device.insert("mac".into(), dev("This Mac", true, false, 32768));
        c.device.insert("ghost".into(), dev("Hidden", true, true, 0));
        c.device.insert("tv".into(), dev("TV", false, false, 0));
        let d = devices(&c);
        assert_eq!(d.len(), 3);
        assert_eq!(d[0], json!({"id": "phone", "name": "Phone", "type": "Smartphone", "is_active": true, "is_private_session": false,
            "is_restricted": false, "supports_volume": true, "volume_percent": 100}));
        assert_eq!(d[2]["name"], "This Mac");
        assert_eq!(d[2]["volume_percent"], 50);
        assert_eq!(d[1]["is_restricted"], true);
        let mut ps = PlayerState::new();
        for u in ["spotify:track:a", "spotify:delimiter", "spotify:track:b"] {
            ps.next_tracks.push(ProvidedTrack { uri: u.into(), ..Default::default() });
        }
        c.player_state = Some(ps).into();
        assert_eq!(next_track_uris(&c, 10), ["spotify:track:a", "spotify:track:b"]);
        assert_eq!(next_track_uris(&c, 1), ["spotify:track:a"]);
    }
}
