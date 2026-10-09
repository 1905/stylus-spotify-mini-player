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

/// Masks private values in one message (stub until the mask rules land).
pub fn mask(msg: &str, _private: &[String]) -> (String, usize) {
    (msg.to_string(), 0)
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
    let mut episodes: Vec<Episode> = Vec::new();
    // key → (episode index, stamp of the key's last line)
    let mut open: HashMap<String, (usize, Stamp)> = HashMap::new();
    for e in entries.iter().filter(|e| keep(e.level, e.target)) {
        kept += 1;
        let mut mask_count = |s: &str| {
            let (m, n) = mask(s, private);
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
}
