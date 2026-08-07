//! `{{variable}}` substitution for paths, params, headers and bodies.
//!
//! Two kinds of variable resolve here:
//!
//! - **Collection variables** — looked up by (trimmed) name in the Variables tab.
//! - **Dynamic variables** — computed at send time: `{{uuid}}`, `{{timestamp}}`,
//!   `{{randomInt(1,100)}}` … see [`DYNAMIC_VARS`].
//!
//! A collection variable shadows a dynamic one of the same name, so `uuid` can
//! be pinned to a fixed value for a debugging session. Prefixing with `$`
//! (`{{$uuid}}`, Postman's spelling) always takes the dynamic one.
//!
//! Unknown variables are left untouched so the user can see what failed to
//! resolve.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use uuid::Uuid;

/// Dynamic variable names and their help text, in the order the help popup
/// lists them. Names are matched case-insensitively, ignoring `_`, so
/// `isoTimestamp`, `iso_timestamp` and `ISOTIMESTAMP` are the same variable.
pub const DYNAMIC_VARS: [(&str, &str); 8] = [
    ("uuid", "UUID v4, fresh per occurrence"),
    ("timestamp", "Unix time in seconds"),
    ("timestampMs", "Unix time in milliseconds"),
    ("isoTimestamp", "RFC 3339 UTC, e.g. 2026-08-06T12:34:56Z"),
    ("randomInt", "0–1000, or randomInt(min,max) inclusive"),
    ("randomHex", "16 hex chars, or randomHex(n)"),
    ("randomString", "16 alphanumerics, or randomString(n)"),
    ("randomBool", "true or false"),
];

pub fn substitute(input: &str, vars: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let key = after[..end].trim();
                match resolve(key, vars) {
                    Some(v) => out.push_str(&v),
                    // Unknown variable: keep the placeholder as-is.
                    None => out.push_str(&rest[..start + 2 + end + 2]),
                }
                rest = &after[end + 2..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// A `$` prefix forces the dynamic variable; otherwise the collection wins.
fn resolve(key: &str, vars: &HashMap<String, String>) -> Option<String> {
    match key.strip_prefix('$') {
        Some(name) => dynamic(name.trim()),
        None => vars.get(key).cloned().or_else(|| dynamic(key)),
    }
}

/// Evaluate a dynamic variable, with optional `name(arg,arg)` arguments.
/// Returns `None` for an unknown name or unusable arguments — the caller then
/// leaves the placeholder visible rather than silently emitting junk.
fn dynamic(spec: &str) -> Option<String> {
    let (name, args) = split_call(spec)?;
    let name = normalize(&name);
    Some(match (name.as_str(), args.as_slice()) {
        ("uuid", []) => Uuid::new_v4().to_string(),
        ("timestamp", []) => unix_secs().to_string(),
        ("timestampms", []) => unix_millis().to_string(),
        ("isotimestamp", []) => iso_timestamp(unix_secs()),
        ("randomint", []) => random_int(0, 1000).to_string(),
        ("randomint", [min, max]) => {
            let (min, max) = (min.parse::<i64>().ok()?, max.parse::<i64>().ok()?);
            if min > max {
                return None;
            }
            random_int(min, max).to_string()
        }
        ("randomhex", []) => random_hex(16),
        ("randomhex", [n]) => random_hex(parse_len(n)?),
        ("randomstring", []) => random_string(16),
        ("randomstring", [n]) => random_string(parse_len(n)?),
        ("randombool", []) => (random_int(0, 1) == 1).to_string(),
        _ => return None,
    })
}

/// `name` or `name(a, b)` → `("name", ["a", "b"])`. Empty args are rejected so
/// `randomHex()` doesn't quietly mean `randomHex`.
fn split_call(spec: &str) -> Option<(String, Vec<String>)> {
    let Some(open) = spec.find('(') else {
        return Some((spec.to_string(), Vec::new()));
    };
    let inner = spec.strip_suffix(')')?.get(open + 1..)?;
    let args: Vec<String> = inner.split(',').map(|a| a.trim().to_string()).collect();
    if args.iter().any(|a| a.is_empty()) {
        return None;
    }
    Some((spec[..open].trim().to_string(), args))
}

fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| *c != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

/// Length arguments are capped: a stray `randomString(999999999)` shouldn't
/// build a gigabyte of request body.
fn parse_len(s: &str) -> Option<usize> {
    let n = s.parse::<usize>().ok()?;
    (1..=4096).contains(&n).then_some(n)
}

// ----- clock -----

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn unix_secs() -> i64 {
    (unix_millis() / 1000) as i64
}

/// RFC 3339 in UTC. Hand-rolled rather than pulling in a date crate: the only
/// calendar work cielago does is stamping a request.
fn iso_timestamp(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let time = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (h, min, s) = (time / 3600, (time % 3600) / 60, time % 60);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}Z")
}

/// Days since 1970-01-01 → (year, month, day). Hinnant's civil-from-days.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11], March-based
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ----- randomness -----

/// Random bytes borrowed from UUID v4 generation, so no extra RNG dependency.
/// Bytes 6 and 8 carry the version/variant bits and are dropped.
fn random_bytes(n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n + 14);
    while out.len() < n {
        let id = *Uuid::new_v4().as_bytes();
        out.extend(
            id.iter()
                .enumerate()
                .filter(|(i, _)| *i != 6 && *i != 8)
                .map(|(_, b)| *b),
        );
    }
    out.truncate(n);
    out
}

fn random_u64() -> u64 {
    let b = random_bytes(8);
    u64::from_le_bytes(b.try_into().expect("8 bytes requested"))
}

/// Uniform-ish over `[min, max]`; the modulo bias is irrelevant for test data.
/// Widened to i128 so a full-range `randomInt(i64::MIN, i64::MAX)` can't wrap.
fn random_int(min: i64, max: i64) -> i64 {
    let span = (max as i128 - min as i128 + 1) as u128;
    (min as i128 + (random_u64() as u128 % span) as i128) as i64
}

fn random_hex(n: usize) -> String {
    random_bytes(n.div_ceil(2))
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()
        .chars()
        .take(n)
        .collect()
}

fn random_string(n: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    random_bytes(n)
        .iter()
        .map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> HashMap<String, String> {
        HashMap::from([
            ("tenant".to_string(), "acme".to_string()),
            ("version".to_string(), "v2".to_string()),
        ])
    }

    #[test]
    fn substitutes_named_vars() {
        assert_eq!(
            substitute("/{{tenant}}/{{ version }}/x", &vars()),
            "/acme/v2/x"
        );
    }

    #[test]
    fn uuid_is_fresh_per_occurrence() {
        let out = substitute("{{uuid}}-{{uuid}}", &vars());
        assert_eq!(out.len(), 36 + 1 + 36);
        let (first, rest) = out.split_at(36);
        let second = &rest[1..];
        assert_eq!(&rest[..1], "-");
        assert!(uuid::Uuid::parse_str(first).is_ok());
        assert!(uuid::Uuid::parse_str(second).is_ok());
        assert_ne!(first, second);
    }

    #[test]
    fn unknown_vars_left_intact() {
        assert_eq!(substitute("{{nope}}", &vars()), "{{nope}}");
        assert_eq!(substitute("{{$nope}}", &vars()), "{{$nope}}");
    }

    #[test]
    fn unclosed_brace_left_intact() {
        assert_eq!(substitute("a {{oops", &vars()), "a {{oops");
    }

    #[test]
    fn collection_var_shadows_dynamic_unless_dollar_prefixed() {
        let vars = HashMap::from([("uuid".to_string(), "pinned".to_string())]);
        assert_eq!(substitute("{{uuid}}", &vars), "pinned");
        assert_eq!(substitute("{{$uuid}}", &vars).len(), 36);
    }

    #[test]
    fn timestamps_are_plausible() {
        let secs: i64 = substitute("{{timestamp}}", &vars()).parse().unwrap();
        // Somewhere after 2020 and before 2100.
        assert!((1_577_836_800..4_102_444_800).contains(&secs));
        let ms: i64 = substitute("{{timestampMs}}", &vars()).parse().unwrap();
        assert_eq!(ms / 1000, secs);

        let iso = substitute("{{isoTimestamp}}", &vars());
        assert_eq!(iso.len(), 20, "{iso}");
        assert!(iso.ends_with('Z'));
        assert_eq!(&iso[4..5], "-");
        assert_eq!(&iso[10..11], "T");
    }

    #[test]
    fn iso_timestamp_matches_known_instants() {
        assert_eq!(iso_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_timestamp(1_000_000_000), "2001-09-09T01:46:40Z");
        // Leap day.
        assert_eq!(iso_timestamp(1_709_164_800), "2024-02-29T00:00:00Z");
        assert_eq!(iso_timestamp(1_754_484_896), "2025-08-06T12:54:56Z");
    }

    #[test]
    fn name_matching_ignores_case_and_underscores() {
        for name in ["isoTimestamp", "iso_timestamp", "ISO_TIMESTAMP"] {
            assert!(
                substitute(&format!("{{{{{name}}}}}"), &vars()).ends_with('Z'),
                "{name}"
            );
        }
    }

    #[test]
    fn random_int_respects_bounds() {
        for _ in 0..200 {
            let n: i64 = substitute("{{randomInt(5, 7)}}", &vars()).parse().unwrap();
            assert!((5..=7).contains(&n), "{n}");
        }
        assert_eq!(substitute("{{randomInt(-1,-1)}}", &vars()), "-1");
        let d: i64 = substitute("{{randomInt}}", &vars()).parse().unwrap();
        assert!((0..=1000).contains(&d));
    }

    #[test]
    fn random_strings_have_requested_length() {
        assert_eq!(substitute("{{randomHex}}", &vars()).len(), 16);
        assert_eq!(substitute("{{randomHex(7)}}", &vars()).len(), 7);
        assert_eq!(substitute("{{randomString}}", &vars()).len(), 16);
        assert_eq!(substitute("{{randomString(40)}}", &vars()).len(), 40);
        assert!(
            substitute("{{randomHex(9)}}", &vars())
                .chars()
                .all(|c| c.is_ascii_hexdigit())
        );
        assert!(
            substitute("{{randomString(64)}}", &vars())
                .chars()
                .all(|c| c.is_ascii_alphanumeric())
        );
    }

    #[test]
    fn random_bool_is_a_bool_and_varies() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..100 {
            let v = substitute("{{randomBool}}", &vars());
            assert!(v == "true" || v == "false", "{v}");
            seen.insert(v);
        }
        assert_eq!(seen.len(), 2, "randomBool never flipped");
    }

    #[test]
    fn bad_arguments_leave_the_placeholder() {
        for bad in [
            "{{randomInt(9,1)}}",
            "{{randomInt(a,b)}}",
            "{{randomInt(1)}}",
            "{{randomHex(0)}}",
            "{{randomHex(99999)}}",
            "{{randomString()}}",
            "{{uuid(2)}}",
        ] {
            assert_eq!(substitute(bad, &vars()), bad);
        }
    }

    #[test]
    fn every_documented_dynamic_var_resolves() {
        for (name, _) in DYNAMIC_VARS {
            let out = substitute(&format!("{{{{${name}}}}}"), &vars());
            assert!(!out.contains("{{"), "{name} did not resolve: {out}");
        }
    }
}
