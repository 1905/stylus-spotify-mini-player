//! The anonymized log for a bug report (File → Show Anonymized Logs in Finder).
//! `anonymize` is pure: keep WARN/ERROR and auth INFO entries, mask private values,
//! collapse repeats, cap the size (spec: plans/2026-10-09-anonymized-logs, P3).

use regex::Regex;
use std::collections::HashMap;
use std::sync::LazyLock;

/// The anonymized text keeps its newest lines up to this size.
pub const CAP_BYTES: usize = 1_048_576;
/// A repeat of the same masked line within this time joins the episode of the line before it.
const COLLAPSE_GAP_MS: i64 = 600_000;
const DAY_MS: i64 = 86_400_000;

/// One line of `applog`: `[YYYY-MM-DD ]HH:MM:SS.mmmZ LEVEL target: message`.
static ENTRY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:(\d{4})-(\d{2})-(\d{2}) )?(\d{2}):(\d{2}):(\d{2})\.(\d{3})Z (TRACE|DEBUG|INFO|WARN|ERROR)\s+(\S+?): (.*)$").unwrap()
});
static DIGITS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+").unwrap());

pub struct Anonymized {
    /// The body, without the header.
    pub text: String,
    /// Entries after the keep filter, before the collapse.
    pub kept: usize,
    /// Entries read.
    pub total: usize,
    pub masked: usize,
}

pub struct Header {
    pub app: String,
    pub macos: String,
    pub made_at: String,
}

#[derive(Clone, Copy)]
struct Stamp {
    /// Days since 1970-01-01; `None` for an old stamp with no date.
    day: Option<i64>,
    tod_ms: i64,
}

impl Stamp {
    /// Milliseconds from `self` to `later`. With no date on one side, by time of day.
    fn until(self, later: Stamp) -> i64 {
        match (self.day, later.day) {
            (Some(a), Some(b)) => (b - a) * DAY_MS + later.tod_ms - self.tod_ms,
            _ => later.tod_ms - self.tod_ms,
        }
    }
}

struct Entry<'a> {
    /// The stamp, level and target text, up to the message.
    prefix: &'a str,
    level: &'a str,
    target: &'a str,
    msg: &'a str,
    /// Lines with no stamp after the first line.
    more: Vec<&'a str>,
    stamp: Stamp,
    /// `HH:MM:SS` of the stamp.
    hms: &'a str,
}

/// Days since 1970-01-01 of a civil date (Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn parse(raw: &str) -> Vec<Entry<'_>> {
    let mut entries: Vec<Entry> = Vec::new();
    for line in raw.lines() {
        let Some(c) = ENTRY.captures(line) else {
            if let Some(e) = entries.last_mut() {
                e.more.push(line);
            }
            continue;
        };
        let num = |i: usize| c.get(i).map(|m| m.as_str().parse::<i64>().unwrap_or(0));
        let day = match (num(1), num(2), num(3)) {
            (Some(y), Some(m), Some(d)) => Some(days_from_civil(y, m, d)),
            _ => None,
        };
        let (h, mi, s, ms) = (num(4).unwrap(), num(5).unwrap(), num(6).unwrap(), num(7).unwrap());
        let msg = c.get(10).unwrap();
        entries.push(Entry {
            prefix: &line[..msg.start()],
            level: c.get(8).unwrap().as_str(),
            target: c.get(9).unwrap().as_str(),
            msg: msg.as_str(),
            more: Vec::new(),
            stamp: Stamp { day, tod_ms: ((h * 60 + mi) * 60 + s) * 1000 + ms },
            hms: &line[c.get(4).unwrap().start()..c.get(6).unwrap().end()],
        });
    }
    entries
}

fn keep(level: &str, target: &str) -> bool {
    match level {
        "WARN" | "ERROR" => true,
        "INFO" => target == "stylus::auth" || target == "librespot_core::session",
        _ => false,
    }
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap()
}

static URI: LazyLock<Regex> = LazyLock::new(|| re(r#"spotify:([A-Za-z_-]+):[^\s"'<>()\[\],]+"#));
static URL: LazyLock<Regex> =
    LazyLock::new(|| re(r#"\b([A-Za-z][A-Za-z0-9+.-]*)://([^/\s?#"'<>]+)([/?#][^\s"'<>)\]]*)?"#));
static DOUBLE_QUOTED: LazyLock<Regex> = LazyLock::new(|| re(r#""(?:[^"\\]|\\.)+""#));
static ANGLED: LazyLock<Regex> = LazyLock::new(|| re(r"<[^<>]+>"));
static EMAIL: LazyLock<Regex> =
    LazyLock::new(|| re(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}"));
static HOME: LazyLock<Regex> = LazyLock::new(|| re(r#"/Users/[^/\s"'<>]+"#));
static UUID: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b"));
/// A run of hex digits and colons; `is_ipv6` decides.
static IPV6: LazyLock<Regex> = LazyLock::new(|| re(r"[0-9A-Fa-f:]{3,}"));
static IPV4: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:\d{1,3}\.){3}\d{1,3}\b"));
static TOKEN: LazyLock<Regex> = LazyLock::new(|| re(r"[A-Za-z0-9+/=_-]{16,}"));
static HEX: LazyLock<Regex> = LazyLock::new(|| re(r"[0-9A-Fa-f]{8,}"));
static DIGIT_RUN: LazyLock<Regex> = LazyLock::new(|| re(r"\d{8,}"));

/// Replaces each match that `with` gives a text for, and counts the replacements.
fn sub(s: &str, re: &Regex, n: &mut usize, with: impl Fn(&regex::Captures) -> Option<String>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for c in re.captures_iter(s) {
        let m = c.get(0).unwrap();
        if let Some(r) = with(&c) {
            out.push_str(&s[last..m.start()]);
            out.push_str(&r);
            last = m.end();
            *n += 1;
        }
    }
    out.push_str(&s[last..]);
    out
}

fn has_digit(s: &str) -> bool {
    s.bytes().any(|b| b.is_ascii_digit())
}

/// `'…'` after the start, a space, `(`, `=` or `:` and before the end, a space or `.,;:!?)`.
/// `Couldn't reach` has no such pair and stays.
fn single_quotes(s: &str, n: &mut usize) -> String {
    let b = s.as_bytes(); // quotes are ASCII: their byte offsets are char boundaries
    let (mut out, mut last, mut i) = (String::with_capacity(s.len()), 0, 0);
    while i < b.len() {
        if b[i] == b'\'' && (i == 0 || b" (=:".contains(&b[i - 1])) {
            let close = (i + 2..b.len()).find(|&j| b[j] == b'\'' && (j + 1 == b.len() || b" .,;:!?)".contains(&b[j + 1])));
            if let Some(j) = close {
                out.push_str(&s[last..i]);
                out.push_str("'•'");
                *n += 1;
                last = j + 1;
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&s[last..]);
    out
}

/// An IPv6 address: 2 to 7 colons, at most one `::`, groups of 1-4 hex digits, not inside a word.
/// A time of day (`14:03:16`) is not one.
fn is_ipv6(s: &str, start: usize, end: usize) -> bool {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    if word(s[..start].chars().next_back()) || word(s[end..].chars().next()) {
        return false;
    }
    let t = &s[start..end];
    let colons = t.matches(':').count();
    let double = t.matches("::").count();
    if !(2..=7).contains(&colons) || t.contains(":::") || double > 1 {
        return false;
    }
    if (t.starts_with(':') && !t.starts_with("::")) || (t.ends_with(':') && !t.ends_with("::")) {
        return false;
    }
    let groups: Vec<&str> = t.split(':').filter(|g| !g.is_empty()).collect();
    if groups.len() < 2 || groups.iter().any(|g| g.len() > 4) {
        return false;
    }
    double == 1 || colons >= 3 || t.bytes().any(|b| b.is_ascii_alphabetic())
}

/// One case-insensitive pattern for the known private values of 4+ characters, longest first.
fn known_values(private: &[String]) -> Option<Regex> {
    let mut vals: Vec<&str> = private.iter().map(|v| v.trim()).filter(|v| v.chars().count() >= 4).collect();
    if vals.is_empty() {
        return None;
    }
    vals.sort_by_key(|v| std::cmp::Reverse(v.len()));
    vals.dedup();
    let alts: Vec<String> = vals.iter().map(|v| regex::escape(v)).collect();
    Some(re(&format!("(?i){}", alts.join("|"))))
}

/// Masks private values in one message (or one line with no stamp): the pattern rules in the
/// locked sequence, then the known values. Gives the text and the count of replacements.
pub fn mask(msg: &str, private: &[String]) -> (String, usize) {
    mask_with(msg, known_values(private).as_ref())
}

fn mask_with(msg: &str, known: Option<&Regex>) -> (String, usize) {
    let mut n = 0;
    let s = sub(msg, &URI, &mut n, |c| Some(format!("spotify:{}:•", &c[1])));
    let s = sub(&s, &URL, &mut n, |c| match c.get(3) {
        Some(p) if p.as_str() != "/" => Some(format!("{}://{}/•", &c[1], &c[2])),
        _ => None,
    });
    let s = sub(&s, &DOUBLE_QUOTED, &mut n, |_| Some("\"•\"".into()));
    let s = single_quotes(&s, &mut n);
    let s = sub(&s, &ANGLED, &mut n, |_| Some("<•>".into()));
    let s = sub(&s, &EMAIL, &mut n, |_| Some("•@•".into()));
    let s = sub(&s, &HOME, &mut n, |_| Some("/Users/•".into()));
    let s = sub(&s, &UUID, &mut n, |_| Some("•".into()));
    let s = {
        let t = s.as_str();
        sub(t, &IPV6, &mut n, |c| {
            let m = c.get(0).unwrap();
            is_ipv6(t, m.start(), m.end()).then(|| "•".into())
        })
    };
    let s = sub(&s, &IPV4, &mut n, |_| Some("•".into()));
    let s = sub(&s, &TOKEN, &mut n, |c| has_digit(&c[0]).then(|| "•".into()));
    let s = sub(&s, &HEX, &mut n, |c| has_digit(&c[0]).then(|| "•".into()));
    let s = sub(&s, &DIGIT_RUN, &mut n, |_| Some("•".into()));
    let s = match known {
        Some(k) => sub(&s, k, &mut n, |_| Some("•".into())),
        None => s,
    };
    (s, n)
}

/// An episode of one masked line shape: its first entry and the repeats after it.
struct Episode<'a> {
    first: String,
    more: Vec<String>,
    count: usize,
    until: &'a str,
}

/// Keep, mask, collapse, cap. `private` = known private values; `cap_bytes` = `CAP_BYTES` in the app.
pub fn anonymize(raw: &str, private: &[String], cap_bytes: usize) -> Anonymized {
    let entries = parse(raw);
    let total = entries.len();
    let (mut kept, mut masked) = (0, 0);
    let known = known_values(private);
    let mut episodes: Vec<Episode> = Vec::new();
    // key → (episode index, stamp of the key's last line)
    let mut open: HashMap<String, (usize, Stamp)> = HashMap::new();
    for e in entries.iter().filter(|e| keep(e.level, e.target)) {
        kept += 1;
        let mut mask_count = |s: &str| {
            let (m, n) = mask_with(s, known.as_ref());
            masked += n;
            m
        };
        let msg = mask_count(e.msg);
        let more: Vec<String> = e.more.iter().map(|l| mask_count(l)).collect();
        let shape = format!("{msg}\n{}", more.join("\n"));
        let key = format!("{} {} {}", e.level, e.target, DIGITS.replace_all(&shape, "N"));
        match open.get_mut(&key) {
            Some((i, last)) if (0..=COLLAPSE_GAP_MS).contains(&last.until(e.stamp)) => {
                episodes[*i].count += 1;
                episodes[*i].until = e.hms;
                *last = e.stamp;
            }
            _ => {
                open.insert(key, (episodes.len(), e.stamp));
                episodes.push(Episode { first: format!("{}{msg}", e.prefix), more, count: 1, until: e.hms });
            }
        }
    }
    let mut text = String::new();
    for ep in &episodes {
        text.push_str(&ep.first);
        if ep.count >= 2 {
            text.push_str(&format!(" (×{} until {})", ep.count, ep.until));
        }
        text.push('\n');
        for l in &ep.more {
            text.push_str(l);
            text.push('\n');
        }
    }
    Anonymized { text: cap(text, cap_bytes), kept, total, masked }
}

/// Drops whole lines from the start until the text fits, then says so on a first line.
fn cap(text: String, cap_bytes: usize) -> String {
    if text.len() <= cap_bytes {
        return text;
    }
    let mut start = 0;
    while text.len() - start > cap_bytes {
        start = match text[start..].find('\n') {
            Some(i) => start + i + 1,
            None => text.len(),
        };
    }
    format!("… older lines cut (1 MB cap)\n{}", &text[start..])
}

/// The 3 header lines; for an empty body also `no log lines yet`.
pub fn header(h: &Header, a: &Anonymized) -> String {
    let mut s = format!(
        "Stylus anonymized log\napp {} · macOS {} · made {}\nkept {} of {} lines (WARN, ERROR, auth INFO) · {} values masked · repeats collapsed\n",
        h.app, h.macos, h.made_at, a.kept, a.total, a.masked
    );
    if a.text.is_empty() {
        s.push_str("no log lines yet\n");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(raw: &str) -> Anonymized {
        anonymize(raw, &[], CAP_BYTES)
    }

    #[test]
    fn keeps_warn_error_and_auth_info_only() {
        let raw = "\
2026-10-09 14:00:00.000Z INFO  stylus::ui: play click
2026-10-09 14:00:01.000Z INFO  stylus::auth: login: start
2026-10-09 14:00:02.000Z INFO  librespot_core::session: Connecting to AP
2026-10-09 14:00:03.000Z WARN  librespot_core::dealer: peer does not respond
2026-10-09 14:00:04.000Z ERROR stylus::player: engine down
2026-10-09 14:00:05.000Z INFO  librespot_playback::player: loading
2026-10-09 14:00:06.000Z DEBUG stylus::auth: debug detail
";
        let a = run(raw);
        assert_eq!(a.total, 7);
        assert_eq!(a.kept, 4);
        assert!(!a.text.contains("play click"));
        assert!(a.text.contains("INFO  stylus::auth: login: start"));
        assert!(a.text.contains("INFO  librespot_core::session: Connecting to AP"));
        assert!(a.text.contains("WARN  librespot_core::dealer: peer does not respond"));
        assert!(a.text.contains("ERROR stylus::player: engine down"));
        assert!(!a.text.contains("loading"));
        assert!(!a.text.contains("debug detail"));
    }

    #[test]
    fn line_without_stamp_belongs_to_the_entry_before() {
        let raw = "\
orphan before the first entry
2026-10-09 14:00:00.000Z WARN  stylus::player: panic
   at src/player.rs:12
2026-10-09 14:00:01.000Z INFO  stylus::cmd: pause
   cmd detail
";
        let a = run(raw);
        assert_eq!(a.total, 2);
        assert_eq!(a.text, "2026-10-09 14:00:00.000Z WARN  stylus::player: panic\n   at src/player.rs:12\n");
    }

    #[test]
    fn reads_both_stamp_forms() {
        let raw = "\
14:03:16.243Z WARN  stylus::old: no date
2026-10-09 14:03:17.243Z WARN  stylus::new: with date
";
        let a = run(raw);
        assert_eq!(a.total, 2);
        assert_eq!(a.kept, 2);
        assert_eq!(a.text, raw);
    }

    #[test]
    fn collapses_repeats_within_ten_minutes() {
        let raw = "\
2026-10-09 14:00:00.000Z WARN  librespot_core::dealer: peer does not respond
2026-10-09 14:02:00.000Z WARN  librespot_core::dealer: peer does not respond
2026-10-09 14:04:59.000Z WARN  librespot_core::dealer: peer does not respond
";
        let a = run(raw);
        assert_eq!(a.kept, 3);
        assert_eq!(a.text, "2026-10-09 14:00:00.000Z WARN  librespot_core::dealer: peer does not respond (×3 until 14:04:59)\n");
    }

    #[test]
    fn repeat_after_eleven_minutes_starts_a_new_episode() {
        let raw = "\
2026-10-09 14:00:00.000Z WARN  librespot_core::dealer: peer does not respond
2026-10-09 14:05:00.000Z WARN  librespot_core::dealer: peer does not respond
2026-10-09 14:16:00.000Z WARN  librespot_core::dealer: peer does not respond
";
        let a = run(raw);
        assert_eq!(
            a.text,
            "2026-10-09 14:00:00.000Z WARN  librespot_core::dealer: peer does not respond (×2 until 14:05:00)\n\
2026-10-09 14:16:00.000Z WARN  librespot_core::dealer: peer does not respond\n"
        );
    }

    #[test]
    fn lines_that_differ_only_in_digits_collapse() {
        let raw = "\
2026-10-09 14:00:00.000Z WARN  stylus::player: reconnect 1 in 2 s
2026-10-09 14:00:02.000Z WARN  stylus::player: reconnect 2 in 4 s
2026-10-09 14:00:06.000Z WARN  stylus::player: reconnect 3 in 8 s
";
        let a = run(raw);
        assert_eq!(a.text, "2026-10-09 14:00:00.000Z WARN  stylus::player: reconnect 1 in 2 s (×3 until 14:00:06)\n");
    }

    #[test]
    fn undated_stamps_compare_by_time_of_day_and_backwards_starts_new() {
        let raw = "\
23:59:00.000Z WARN  stylus::x: same
23:59:30.000Z WARN  stylus::x: same
00:00:10.000Z WARN  stylus::x: same
";
        let a = run(raw);
        assert_eq!(a.text, "23:59:00.000Z WARN  stylus::x: same (×2 until 23:59:30)\n00:00:10.000Z WARN  stylus::x: same\n");
    }

    #[test]
    fn dated_stamps_collapse_across_midnight() {
        let raw = "\
2026-10-09 23:59:00.000Z WARN  stylus::x: same
2026-10-10 00:01:00.000Z WARN  stylus::x: same
";
        let a = run(raw);
        assert_eq!(a.text, "2026-10-09 23:59:00.000Z WARN  stylus::x: same (×2 until 00:01:00)\n");
    }

    /// Letters only, so no two messages share a shape and no mask rule fires.
    fn letters(mut i: usize) -> String {
        let mut s = String::new();
        loop {
            s.push((b'k' + (i % 16) as u8) as char);
            i /= 16;
            if i == 0 {
                return s;
            }
        }
    }

    #[test]
    fn cap_keeps_the_newest_lines() {
        let mut raw = String::new();
        let mut i = 0;
        while raw.len() < 2 * 1024 * 1024 {
            raw.push_str(&format!("2026-10-09 14:00:00.000Z WARN  stylus::x: line {}\n", letters(i)));
            i += 1;
        }
        let last = format!("2026-10-09 14:00:00.000Z WARN  stylus::x: line {}\n", letters(i - 1));
        let a = anonymize(&raw, &[], 1024);
        let cut = "… older lines cut (1 MB cap)\n";
        assert!(a.text.starts_with(cut));
        assert!(a.text.len() <= 1024 + cut.len());
        assert!(a.text.ends_with(&last));
        assert_eq!(a.kept, i);
    }

    #[test]
    fn no_cap_under_the_limit() {
        let raw = "2026-10-09 14:00:00.000Z WARN  stylus::x: short\n";
        assert_eq!(anonymize(raw, &[], 1024).text, raw);
    }

    fn hdr() -> Header {
        Header { app: "0.1.0".into(), macos: "14.6".into(), made_at: "2026-10-09 14:20:05Z".into() }
    }

    #[test]
    fn header_has_the_three_locked_lines() {
        let raw = "\
2026-10-09 14:00:00.000Z INFO  stylus::ui: dropped
2026-10-09 14:00:01.000Z WARN  stylus::x: kept
";
        let a = run(raw);
        assert_eq!(
            header(&hdr(), &a),
            "Stylus anonymized log\n\
app 0.1.0 · macOS 14.6 · made 2026-10-09 14:20:05Z\n\
kept 1 of 2 lines (WARN, ERROR, auth INFO) · 0 values masked · repeats collapsed\n"
        );
    }

    #[test]
    fn header_for_empty_input_says_no_lines() {
        let a = run("");
        assert_eq!(a.text, "");
        assert_eq!(
            header(&hdr(), &a),
            "Stylus anonymized log\n\
app 0.1.0 · macOS 14.6 · made 2026-10-09 14:20:05Z\n\
kept 0 of 0 lines (WARN, ERROR, auth INFO) · 0 values masked · repeats collapsed\n\
no log lines yet\n"
        );
    }

    fn m(msg: &str) -> (String, usize) {
        mask(msg, &[])
    }

    #[test]
    fn mask_rule_01_spotify_uri() {
        assert_eq!(m("play spotify:track:4MzII8fszi8KkFl1ryv07L now"), ("play spotify:track:• now".into(), 1));
        assert_eq!(m("ctx spotify:user:someone:collection").0, "ctx spotify:user:•");
    }

    #[test]
    fn mask_rule_02_url() {
        assert_eq!(
            m("GET https://audio-ak.spotifycdn.com/audio/49ab?__token__=x failed"),
            ("GET https://audio-ak.spotifycdn.com/• failed".into(), 1)
        );
        assert_eq!(m("hm://collection/collection/someone/json"), ("hm://collection/•".into(), 1));
        assert_eq!(m("host https://example.com up"), ("host https://example.com up".into(), 0));
    }

    #[test]
    fn mask_rule_03_double_quotes() {
        assert_eq!(m(r#"now: "Interloper" by "X \"Y\" Z""#), (r#"now: "•" by "•""#.into(), 2));
    }

    #[test]
    fn mask_rule_04_single_quotes() {
        assert_eq!(m("Authenticated as 'kass' !"), ("Authenticated as '•' !".into(), 1));
        assert_eq!(m("x=('a b'), y:'c'."), ("x=('•'), y:'•'.".into(), 2));
        assert_eq!(m("Couldn't reach the server"), ("Couldn't reach the server".into(), 0));
        assert_eq!(m("Couldn't reach 'host'"), ("Couldn't reach '•'".into(), 1));
    }

    #[test]
    fn mask_rule_05_angle_brackets() {
        assert_eq!(m("Loading <Days Gone> with Spotify URI <x>"), ("Loading <•> with Spotify URI <•>".into(), 2));
    }

    #[test]
    fn mask_rule_06_email() {
        assert_eq!(m("for a@b.com done"), ("for •@• done".into(), 1));
    }

    #[test]
    fn mask_rule_07_home_path() {
        assert_eq!(m("read /Users/kass/Music failed"), ("read /Users/•/Music failed".into(), 1));
    }

    #[test]
    fn mask_rule_08_uuid() {
        assert_eq!(m("device 927e12e8-efcb-4c8e-838d-19de2e7a1231 gone"), ("device • gone".into(), 1));
    }

    #[test]
    fn mask_rule_09_ipv6() {
        assert_eq!(m("ap fe80::1c2b:3a4d:5e6f:7081 down"), ("ap • down".into(), 1));
        assert_eq!(m("ap [2001:db8:0:0:0:0:2:1]:443"), ("ap [•]:443".into(), 1));
        assert_eq!(m("at 14:03:16 ok"), ("at 14:03:16 ok".into(), 0));
        assert_eq!(m("in librespot_core::dealer::manager"), ("in librespot_core::dealer::manager".into(), 0));
    }

    #[test]
    fn mask_rule_10_ipv4() {
        assert_eq!(m("ap 192.168.8.214:4070 down"), ("ap •:4070 down".into(), 1));
    }

    #[test]
    fn mask_rule_11_token_run() {
        assert_eq!(m("id M2RlZDRiZGItYWFjZS00NWU4 x"), ("id • x".into(), 1));
        assert_eq!(m("no digit librespot_connect_state stays"), ("no digit librespot_connect_state stays".into(), 0));
    }

    #[test]
    fn mask_rule_12_hex_run() {
        assert_eq!(m("file 65b708ab x"), ("file • x".into(), 1));
        assert_eq!(m("word deadbeefcafe stays"), ("word deadbeefcafe stays".into(), 0));
    }

    #[test]
    fn mask_rule_13_digit_run() {
        assert_eq!(m("ts 12345678 n 1234567"), ("ts • n 1234567".into(), 1));
    }

    #[test]
    fn mask_rule_14_known_values() {
        let private = ["Alex Canary iPhone".to_string(), "Bob".to_string(), "  ".to_string()];
        assert_eq!(
            mask("device Alex Canary iPhone and alex canary iphone, Bob", &private),
            ("device • and •, Bob".into(), 2)
        );
    }

    #[test]
    fn mask_keeps_ordinary_values() {
        for s in ["Couldn't reach", "headphones WH-1000XM6 ok", "play_request_id: 1", "version 0.8.0", "reconnect 3 in 8 s"] {
            assert_eq!(m(s), (s.to_string(), 0), "{s}");
        }
    }

    #[test]
    fn mask_never_touches_stamp_or_target() {
        let raw = "2026-10-09 14:03:16.243Z WARN  librespot_core::dealer: peer dealer gone\n";
        let private = ["dealer".to_string(), "2026".to_string(), "librespot".to_string()];
        let a = anonymize(raw, &private, CAP_BYTES);
        assert_eq!(a.text, "2026-10-09 14:03:16.243Z WARN  librespot_core::dealer: peer • gone\n");
        assert_eq!(a.masked, 1);
    }

    #[test]
    fn canary_values_do_not_survive() {
        let fixture = r#"2026-10-09 14:00:00.000Z INFO  librespot_core::session: Authenticated as 'canaryuser42' !
2026-10-09 14:00:01.000Z WARN  librespot_playback::player: Loading <Velvet Canary Song> with Spotify URI <spotify:track:4MzII8fszi8KkFl1ryv07L>
2026-10-09 14:00:02.000Z ERROR stylus::spotify: now: "Velvet Canary Song" by "Zed Canary" from "My Secret Mix"
2026-10-09 14:00:03.000Z WARN  librespot_core::mercury: hm://collection/collection/canaryuser42/json failed
2026-10-09 14:00:04.000Z WARN  librespot_audio::fetch: GET https://audio-ak.spotifycdn.com/audio/65b708073fc0480ea92a077233ca87bd?__token__=exp=1791285544~hmac=56aa7079cd404764aca6069730b588e0d51015b4 failed
2026-10-09 14:00:05.000Z WARN  librespot_connect::spirc: unknown SpotifyUri("spotify:track:4MzII8fszi8KkFl1ryv07L")
2026-10-09 14:00:06.000Z WARN  librespot_core::dealer: connection_id: "M2RlZDRiZGItYWFjZS00NWU4LTg3NDctOTQxNDQwOTg5ZDlj"
2026-10-09 14:00:07.000Z INFO  stylus::auth: token: client_id: "65b708073fc0480ea92a077233ca87bd" for alex@example.com
2026-10-09 14:00:08.000Z WARN  stylus::player: device Alex Canary iPhone (927e12e8-efcb-4c8e-838d-19de2e7a1231) left
2026-10-09 14:00:09.000Z ERROR stylus::paths: cannot read /Users/alex/Library/x: denied
2026-10-09 14:00:10.000Z WARN  librespot_core::apresolve: ap 192.168.1.23:4070 and fe80::1c2b:3a4d:5e6f:7081 down
2026-10-09 14:00:11.000Z WARN  stylus::x: bare 4MzII8fszi8KkFl1ryv07L M2RlZDRiZGItYWFjZS00NWU4LTg3NDctOTQxNDQwOTg5ZDlj 56aa7079cd404764aca6069730b588e0d51015b4 exp=1791285544 65b708073fc0480ea92a077233ca87bd
2026-10-09 14:00:12.000Z ERROR stylus::x: names CANARYUSER42 velvet canary song ZED CANARY alex canary iphone
   continued: Alex Canary iPhone owned by canaryuser42 at 192.168.1.23
2026-10-09 14:00:13.000Z INFO  stylus::auth: ui: device Alex Canary iPhone picked for Velvet Canary Song
"#;
        let private: Vec<String> =
            ["canaryuser42", "Alex Canary iPhone", "Velvet Canary Song", "Zed Canary"].iter().map(|s| s.to_string()).collect();
        let a = anonymize(fixture, &private, CAP_BYTES);
        assert_eq!(a.kept, 14);
        let out = a.text.to_lowercase();
        for v in [
            "canaryuser42",
            "Velvet Canary Song",
            "Zed Canary",
            "My Secret Mix",
            "Alex Canary iPhone",
            "alex@example.com",
            "192.168.1.23",
            "fe80::1c2b:3a4d:5e6f:7081",
            "4MzII8fszi8KkFl1ryv07L",
            "927e12e8-efcb-4c8e-838d-19de2e7a1231",
            "M2RlZDRiZGItYWFjZS00NWU4LTg3NDctOTQxNDQwOTg5ZDlj",
            "56aa7079cd404764aca6069730b588e0d51015b4",
            "exp=1791285544",
            "/Users/alex",
            "65b708073fc0480ea92a077233ca87bd",
        ] {
            assert!(!out.contains(&v.to_lowercase()), "a canary value survived (index {})", v.len());
        }
        // the lines stay readable: stamps, levels and targets are all there
        assert!(a.text.contains("2026-10-09 14:00:10.000Z WARN  librespot_core::apresolve: ap •:4070 and • down"));
    }
}
