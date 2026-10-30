//! Topic names and topic filters, as the MQTT specification defines them.
//!
//! Pure functions on strings: no hardware and no crate types, so they can be tested on a computer
//! (`rustc --test src/mqtt/topic.rs`).

/// The longest topic or filter in bytes (a 16-bit length field in the packet).
const MAX_LEN: usize = 65_535;

/// Whether `topic` is a name that can be published to: not empty, not too long, no wildcards and
/// no NUL character.
pub fn is_valid_name(topic: &str) -> bool {
    !topic.is_empty() && topic.len() <= MAX_LEN && !topic.contains(['+', '#', '\0'])
}

/// Whether `filter` can be subscribed to: `+` stands for exactly one whole level, `#` for all the
/// remaining levels and only as the last one. MQTT 5 shared subscriptions
/// (`$share/<group>/<filter>`) are accepted.
pub fn is_valid_filter(filter: &str) -> bool {
    if filter.is_empty() || filter.len() > MAX_LEN || filter.contains('\0') {
        return false;
    }
    let Some(filter) = strip_share(filter) else {
        return false;
    };

    let mut levels = filter.split('/').peekable();
    while let Some(level) = levels.next() {
        let valid = match level {
            "#" => levels.peek().is_none(),
            "+" => true,
            _ => !level.contains(['+', '#']),
        };
        if !valid {
            return false;
        }
    }
    true
}

/// Whether a message published to `topic` is delivered to a subscription with `filter`.
pub fn matches(filter: &str, topic: &str) -> bool {
    let filter = strip_share(filter).unwrap_or(filter);

    // Topics that start with `$` are for the broker itself (`$SYS/...`): a wildcard in the first
    // level of the filter never matches them.
    if topic.starts_with('$') && filter.starts_with(['+', '#']) {
        return false;
    }

    let mut filter_levels = filter.split('/');
    let mut topic_levels = topic.split('/');
    loop {
        match (filter_levels.next(), topic_levels.next()) {
            // `a/#` also matches `a` itself.
            (Some("#"), _) | (None, None) => return true,
            (Some("+"), Some(_)) => {}
            (Some(wanted), Some(level)) if wanted == level => {}
            _ => return false,
        }
    }
}

/// The filter inside `$share/<group>/<filter>`. A plain filter is returned as it is, and `None`
/// means a shared filter that is malformed (no group, a wildcard in the group, or no filter).
fn strip_share(filter: &str) -> Option<&str> {
    let Some(rest) = filter.strip_prefix("$share/") else {
        return Some(filter);
    };
    let (group, filter) = rest.split_once('/')?;
    (!group.is_empty() && !group.contains(['+', '#']) && !filter.is_empty()).then_some(filter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(is_valid_name("a"));
        assert!(is_valid_name("/"));
        assert!(is_valid_name("pulsar/pulsar-a1b2c3/state"));
        assert!(is_valid_name("$SYS/broker/uptime"));
        assert!(!is_valid_name(""));
        assert!(!is_valid_name("a/+/b"));
        assert!(!is_valid_name("a/#"));
        assert!(!is_valid_name("a\0b"));
        assert!(!is_valid_name(&"x".repeat(MAX_LEN + 1)));
        assert!(is_valid_name(&"x".repeat(MAX_LEN)));
    }

    #[test]
    fn filters() {
        for valid in [
            "#", "+", "a", "a/b", "a/+/b", "a/#", "+/+", "/", "a//b", "+/#",
        ] {
            assert!(is_valid_filter(valid), "{valid}");
        }
        for invalid in [
            "",
            "a/#/b",
            "a#",
            "a+",
            "a/b+",
            "#/a",
            "a/\0",
            "sport/tennis#",
        ] {
            assert!(!is_valid_filter(invalid), "{invalid}");
        }
    }

    #[test]
    fn shared_filters() {
        assert!(is_valid_filter("$share/g/a/b"));
        assert!(is_valid_filter("$share/g/#"));
        assert!(!is_valid_filter("$share/g"));
        assert!(!is_valid_filter("$share//a"));
        assert!(!is_valid_filter("$share/g+/a"));
        assert!(!is_valid_filter("$share/g/"));
        assert!(matches("$share/g/a/+", "a/b"));
        assert!(!matches("$share/g/a/+", "a/b/c"));
    }

    #[test]
    fn matching_examples_from_the_specification() {
        assert!(matches("sport/tennis/player1/#", "sport/tennis/player1"));
        assert!(matches(
            "sport/tennis/player1/#",
            "sport/tennis/player1/ranking"
        ));
        assert!(matches(
            "sport/tennis/player1/#",
            "sport/tennis/player1/score/wimbledon"
        ));
        assert!(matches("sport/#", "sport"));
        assert!(matches("sport/tennis/+", "sport/tennis/player1"));
        assert!(!matches("sport/tennis/+", "sport/tennis/player1/ranking"));
        assert!(matches("+", "finance"));
        assert!(!matches("+", "/finance"));
        assert!(matches("/+", "/finance"));
        assert!(matches("+/+", "/finance"));
        assert!(matches("#", "any/topic/at/all"));
    }

    #[test]
    fn matching_edge_cases() {
        assert!(matches("a/b", "a/b"));
        assert!(!matches("a/b", "a/b/c"));
        assert!(!matches("a/b/c", "a/b"));
        assert!(!matches("a/+", "a"));
        assert!(matches("a/+/c", "a/b/c"));
        assert!(!matches("a/+/c", "a/b/d"));
        assert!(matches("a//b", "a//b"));
        assert!(matches("a/+/b", "a//b"));
    }

    #[test]
    fn system_topics_need_an_explicit_filter() {
        assert!(!matches("#", "$SYS/uptime"));
        assert!(!matches("+/uptime", "$SYS/uptime"));
        assert!(matches("$SYS/#", "$SYS/uptime"));
        assert!(matches("$SYS/+", "$SYS/uptime"));
    }
}
