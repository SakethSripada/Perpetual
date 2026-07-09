use chrono::{DateTime, Duration, Utc};

pub(crate) fn detect_usage_limit(text: &str) -> Option<Option<DateTime<Utc>>> {
    if !looks_like_limit(text) {
        return None;
    }
    Some(parse_reset_at(text))
}

fn looks_like_limit(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("usage limit")
        || lower.contains("rate limit")
        || lower.contains("limit reached")
        || lower.contains("spend limit")
        || lower.contains("too many requests")
        || lower.contains("quota")
        || lower.contains("429")
}

fn parse_reset_at(text: &str) -> Option<DateTime<Utc>> {
    parse_rfc3339(text)
        .or_else(|| parse_relative(text))
        .or_else(|| parse_epoch_seconds(text))
        .or_else(|| parse_clock_time(text))
}

fn parse_rfc3339(text: &str) -> Option<DateTime<Utc>> {
    for raw in text.split_whitespace() {
        let token = raw
            .trim_matches(|c: char| {
                !(c.is_ascii_alphanumeric() || matches!(c, '-' | ':' | '.' | '+' | 'T' | 'Z'))
            })
            .trim_end_matches([',', '.', ';', ')', ']', '}']);
        if let Ok(dt) = DateTime::parse_from_rfc3339(token) {
            return Some(dt.with_timezone(&Utc));
        }
    }
    None
}

fn parse_relative(text: &str) -> Option<DateTime<Utc>> {
    let lower = text.to_lowercase();
    for marker in [
        "try again in",
        "retry in",
        "retry after",
        "resets in",
        "reset in",
        "available in",
    ] {
        if let Some(idx) = lower.find(marker) {
            let after = &lower[idx + marker.len()..];
            if let Some(duration) = parse_duration(after) {
                return Some(Utc::now() + duration);
            }
        }
    }
    None
}

/// Parse `1 hour`, `30 minutes`, `90s`, and compounds like `1 hour 30 minutes`
/// or `1h30m`.
fn parse_duration(input: &str) -> Option<Duration> {
    let mut rest = input.trim_start();
    let mut total: Option<Duration> = None;
    for _ in 0..3 {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            break;
        }
        let amount = digits.parse::<i64>().ok()?;
        rest = rest[digits.len()..].trim_start();
        let unit_len = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        let unit = &rest[..unit_len];
        let piece = if unit.starts_with("sec") || unit == "s" {
            Duration::seconds(amount)
        } else if unit.starts_with("min") || unit == "m" {
            Duration::minutes(amount)
        } else if unit.starts_with("hour") || unit.starts_with("hr") || unit == "h" {
            Duration::hours(amount)
        } else if unit.starts_with("day") || unit == "d" {
            Duration::days(amount)
        } else {
            break;
        };
        total = Some(total.unwrap_or_else(Duration::zero) + piece);
        rest = rest[unit_len..]
            .trim_start()
            .trim_start_matches("and")
            .trim_start();
    }
    total
}

/// A 10-digit unix timestamp in the message (e.g. `resets at 1751522400`),
/// bounded to the plausible near future so version strings don't match.
fn parse_epoch_seconds(text: &str) -> Option<DateTime<Utc>> {
    let now = Utc::now();
    for raw in text.split(|c: char| !c.is_ascii_digit()) {
        if raw.len() != 10 {
            continue;
        }
        let Ok(secs) = raw.parse::<i64>() else {
            continue;
        };
        let Some(candidate) = DateTime::from_timestamp(secs, 0) else {
            continue;
        };
        // Sanity window: a limit reset lands between now and 8 days out.
        if candidate > now && candidate < now + Duration::days(8) {
            return Some(candidate);
        }
    }
    None
}

/// Wall-clock phrasings the CLIs actually emit, e.g. Claude Code's
/// "Your limit will reset at 7pm (America/Chicago)" or Codex's
/// "Try again at 2:30 PM". Interpreted in this machine's local time (the CLI
/// producing the message runs on the same host); if that instant already
/// passed today, it means tomorrow.
fn parse_clock_time(text: &str) -> Option<DateTime<Utc>> {
    use chrono::{Local, NaiveTime, TimeZone};

    let lower = text.to_lowercase();
    let idx = ["reset at", "resets at", "again at", "available at"]
        .iter()
        .find_map(|marker| lower.find(marker).map(|idx| idx + marker.len()))?;
    let after = lower[idx..].trim_start();

    let hour_digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    if hour_digits.is_empty() || hour_digits.len() > 2 {
        return None;
    }
    let hour_raw = hour_digits.parse::<u32>().ok()?;
    let mut rest = &after[hour_digits.len()..];
    let mut minute = 0u32;
    if let Some(stripped) = rest.strip_prefix(':') {
        let minute_digits: String = stripped
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        minute = minute_digits.parse().ok()?;
        rest = &stripped[minute_digits.len()..];
    }
    let rest = rest.trim_start();
    let hour = if rest.starts_with("pm") && hour_raw < 12 {
        hour_raw + 12
    } else if rest.starts_with("am") && hour_raw == 12 {
        0
    } else if rest.starts_with("am") || rest.starts_with("pm") {
        hour_raw
    } else if hour_raw <= 23 && after[hour_digits.len()..].starts_with(':') {
        hour_raw // 24h form like "at 19:30"
    } else {
        return None; // bare number without am/pm or minutes is too ambiguous
    };

    let time = NaiveTime::from_hms_opt(hour, minute, 0)?;
    let now_local = Local::now();
    let mut candidate = now_local.date_naive().and_time(time);
    if candidate <= now_local.naive_local() {
        candidate += Duration::days(1);
    }
    Local
        .from_local_datetime(&candidate)
        .earliest()
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_limit_without_reset_time() {
        assert!(matches!(
            detect_usage_limit("Rate limit reached. Try again later."),
            Some(None)
        ));
    }

    #[test]
    fn detects_monthly_spend_limit() {
        assert!(matches!(
            detect_usage_limit(
                "You've hit your monthly spend limit · raise it at claude.ai/settings/usage"
            ),
            Some(None)
        ));
    }

    #[test]
    fn parses_rfc3339_reset_time() {
        let reset =
            detect_usage_limit("Usage limit reached. Try again after 2026-06-24T12:30:00Z.");

        assert_eq!(
            reset.flatten().map(|dt| dt.to_rfc3339()),
            Some("2026-06-24T12:30:00+00:00".to_string())
        );
    }

    #[test]
    fn parses_relative_reset_time() {
        let before = Utc::now() + Duration::minutes(29);
        let reset = detect_usage_limit("Too many requests. Retry in 30 minutes.")
            .flatten()
            .unwrap();
        let after = Utc::now() + Duration::minutes(31);

        assert!(reset >= before);
        assert!(reset <= after);
    }

    #[test]
    fn parses_compound_and_retry_after_durations() {
        // Codex-style "retry after".
        let reset = detect_usage_limit("Rate limit exceeded, retry after 90s")
            .flatten()
            .unwrap();
        assert!(reset <= Utc::now() + Duration::seconds(95));

        // Compound duration.
        let reset = detect_usage_limit("Usage limit reached. Try again in 1 hour 30 minutes.")
            .flatten()
            .unwrap();
        let expected = Utc::now() + Duration::minutes(90);
        assert!((reset - expected).num_seconds().abs() < 10);
    }

    #[test]
    fn parses_epoch_reset_timestamp() {
        let target = Utc::now() + Duration::hours(3);
        let message = format!(
            "429 too many requests; quota resets at {}",
            target.timestamp()
        );
        let reset = detect_usage_limit(&message).flatten().unwrap();
        assert_eq!(reset.timestamp(), target.timestamp());
    }

    #[test]
    fn epoch_parsing_ignores_past_and_far_future_numbers() {
        // A past timestamp (e.g. a build id) must not be treated as a reset.
        let message = "usage limit hit, request id 1500000000".to_string();
        assert_eq!(detect_usage_limit(&message), Some(None));
    }

    #[test]
    fn parses_claude_style_clock_reset() {
        // "Your limit will reset at 7pm (America/Chicago)." — the clock is
        // interpreted in host-local time and always lands in the future.
        let reset = detect_usage_limit(
            "You've reached your usage limit. Your limit will reset at 7pm (America/Chicago).",
        )
        .flatten()
        .expect("clock time parsed");
        assert!(reset > Utc::now());
        assert!(reset <= Utc::now() + Duration::days(1));
    }

    #[test]
    fn parses_codex_style_clock_reset_with_minutes() {
        let reset = detect_usage_limit("You've hit your usage limit. Try again at 2:30 pm.")
            .flatten()
            .expect("clock time parsed");
        assert!(reset > Utc::now());
        assert_eq!(reset.timestamp() % 60, 0);
    }

    #[test]
    fn bare_ambiguous_numbers_do_not_parse_as_clock() {
        // "at 3" with no am/pm or minutes is too ambiguous to schedule on.
        assert_eq!(
            detect_usage_limit("quota exceeded, docs at 3 for details"),
            Some(None)
        );
    }
}
