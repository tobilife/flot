//! Credentials by their shape, so evidence that is about to leave the host can
//! be checked for them first.
//!
//! Only shapes a credential issuer fixed are recognised: a literal prefix and
//! a run of the characters that follow it, bounded on both sides. Nothing here
//! guesses from entropy, which would flag every digest a receipt holds. What
//! does not look like one of these passes, so a clean scan is not a promise.

use serde::Serialize;

/// One credential-shaped token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    /// What it looks like, such as "GitHub token".
    pub kind: &'static str,
    /// The prefix that identified it; the rest is never shown.
    pub shown: String,
    /// Byte offset in the scanned text.
    pub offset: usize,
}

struct Shape {
    kind: &'static str,
    prefix: &'static [u8],
    /// Characters the body may contain after the prefix.
    body: fn(u8) -> bool,
    min: usize,
    /// `None`: any length from `min` up.
    max: Option<usize>,
}

fn alnum(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
}

fn word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn dashed(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

fn upper(byte: u8) -> bool {
    byte.is_ascii_uppercase() || byte.is_ascii_digit()
}

const SHAPES: &[Shape] = &[
    Shape {
        kind: "GitHub token",
        prefix: b"ghp_",
        body: alnum,
        min: 36,
        max: Some(36),
    },
    Shape {
        kind: "GitHub token",
        prefix: b"gho_",
        body: alnum,
        min: 36,
        max: Some(36),
    },
    Shape {
        kind: "GitHub token",
        prefix: b"ghu_",
        body: alnum,
        min: 36,
        max: Some(36),
    },
    Shape {
        kind: "GitHub token",
        prefix: b"ghs_",
        body: alnum,
        min: 36,
        max: Some(36),
    },
    Shape {
        kind: "GitHub token",
        prefix: b"ghr_",
        body: alnum,
        min: 36,
        max: Some(36),
    },
    Shape {
        kind: "GitHub token",
        prefix: b"github_pat_",
        body: word,
        min: 22,
        max: None,
    },
    Shape {
        kind: "GitLab token",
        prefix: b"glpat-",
        body: dashed,
        min: 20,
        max: None,
    },
    Shape {
        kind: "Anthropic key",
        prefix: b"sk-ant-",
        body: dashed,
        min: 20,
        max: None,
    },
    Shape {
        kind: "OpenAI key",
        prefix: b"sk-proj-",
        body: dashed,
        min: 20,
        max: None,
    },
    Shape {
        kind: "OpenAI key",
        prefix: b"sk-",
        body: alnum,
        min: 40,
        max: None,
    },
    Shape {
        kind: "AWS access key",
        prefix: b"AKIA",
        body: upper,
        min: 16,
        max: Some(16),
    },
    Shape {
        kind: "AWS access key",
        prefix: b"ASIA",
        body: upper,
        min: 16,
        max: Some(16),
    },
    Shape {
        kind: "Google API key",
        prefix: b"AIza",
        body: dashed,
        min: 35,
        max: Some(35),
    },
    Shape {
        kind: "Slack token",
        prefix: b"xoxb-",
        body: dashed,
        min: 10,
        max: None,
    },
    Shape {
        kind: "Slack token",
        prefix: b"xoxp-",
        body: dashed,
        min: 10,
        max: None,
    },
    Shape {
        kind: "Slack token",
        prefix: b"xoxa-",
        body: dashed,
        min: 10,
        max: None,
    },
    Shape {
        kind: "npm token",
        prefix: b"npm_",
        body: alnum,
        min: 36,
        max: Some(36),
    },
];

const PRIVATE_KEY_HEAD: &[u8] = b"-----BEGIN ";
const PRIVATE_KEY_TAIL: &[u8] = b"PRIVATE KEY-----";

/// Every credential-shaped token in `text`, in order.
pub fn scan(text: &[u8]) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut at = 0;
    while at < text.len() {
        match token_at(text, at) {
            Some((finding, end)) => {
                findings.push(finding);
                at = end;
            }
            None => at += 1,
        }
    }
    findings
}

fn token_at(text: &[u8], at: usize) -> Option<(Finding, usize)> {
    // A token starts where a word does: inside a longer run of word
    // characters (a digest, base64) a prefix is a coincidence.
    if at > 0 && dashed(text[at - 1]) {
        return None;
    }
    let rest = &text[at..];
    if let Some(after) = rest.strip_prefix(PRIVATE_KEY_HEAD) {
        // `-----BEGIN [A-Z ]*PRIVATE KEY-----` on one line.
        let label = after
            .iter()
            .take_while(|byte| byte.is_ascii_uppercase() || **byte == b' ')
            .count();
        let tail = PRIVATE_KEY_TAIL.len() - "-----".len();
        if label >= tail && label <= 40 && after[label - tail..].starts_with(PRIVATE_KEY_TAIL) {
            let end = at + PRIVATE_KEY_HEAD.len() + label + "-----".len();
            return Some((
                Finding {
                    kind: "private key",
                    shown: String::from_utf8_lossy(&text[at..end]).into_owned(),
                    offset: at,
                },
                end,
            ));
        }
    }
    for shape in SHAPES {
        let Some(after) = rest.strip_prefix(shape.prefix) else {
            continue;
        };
        let length = after.iter().take_while(|byte| (shape.body)(**byte)).count();
        let fits = length >= shape.min && shape.max.is_none_or(|max| length == max);
        // A token ends where a word does, or the run was longer than the
        // issuer's length and is something else.
        let bounded = after.get(length).is_none_or(|byte| !dashed(*byte));
        if fits && bounded {
            let end = at + shape.prefix.len() + length;
            return Some((
                Finding {
                    kind: shape.kind,
                    shown: format!("{}…", String::from_utf8_lossy(shape.prefix)),
                    offset: at,
                },
                end,
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tokens are assembled at run time so no credential-shaped literal sits in
    // the source for a scanner (or a push protection) to trip over.
    fn token(prefix: &str, body: &str) -> String {
        format!("{prefix}{body}")
    }

    fn kinds(text: &str) -> Vec<&'static str> {
        scan(text.as_bytes()).iter().map(|f| f.kind).collect()
    }

    #[test]
    fn recognises_the_shapes_issuers_fixed() {
        let cases = [
            (
                token("gh", &format!("p_{}", "a1".repeat(18))),
                "GitHub token",
            ),
            (
                token("github_", &format!("pat_{}", "B2_c".repeat(10))),
                "GitHub token",
            ),
            (
                token("glp", &format!("at-{}", "x".repeat(20))),
                "GitLab token",
            ),
            (
                token("sk-", &format!("ant-api03-{}", "Zz9-".repeat(10))),
                "Anthropic key",
            ),
            (
                token("sk-", &format!("proj-{}", "q".repeat(30))),
                "OpenAI key",
            ),
            (token("sk-", &"T3".repeat(24)), "OpenAI key"),
            (
                token("AK", &format!("IA{}", "Q7".repeat(8))),
                "AWS access key",
            ),
            (
                token("AI", &format!("za{}", "k".repeat(35))),
                "Google API key",
            ),
            (token("xo", &format!("xb-{}", "1234-abcd-")), "Slack token"),
            (token("np", &format!("m_{}", "r".repeat(36))), "npm token"),
            (
                token("-----BEGIN ", "OPENSSH PRIVATE KEY-----"),
                "private key",
            ),
            (token("-----BEGIN ", "PRIVATE KEY-----"), "private key"),
        ];
        for (text, kind) in cases {
            let line = format!("token: {text}\n");
            assert_eq!(kinds(&line), [kind], "{line}");
        }
    }

    #[test]
    fn only_the_prefix_is_ever_shown() {
        let secret = token("gh", &format!("p_{}", "s".repeat(36)));
        let found = scan(format!("GH_TOKEN={secret}").as_bytes());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].shown, "ghp_…");
        assert_eq!(found[0].offset, 9);
    }

    #[test]
    fn near_misses_and_digests_pass() {
        let near = [
            // One short, one long, inside a longer word.
            token("gh", &format!("p_{}", "a".repeat(35))),
            token("gh", &format!("p_{}", "a".repeat(37))),
            token("xgh", &format!("p_{}", "a".repeat(36))),
            token("AK", &format!("IA{}", "Q".repeat(15))),
            token("AK", &format!("IA{}", "q".repeat(16))),
            token("AK", &format!("IA{}x", "Q".repeat(16))),
            token("sk-", &"a".repeat(39)),
            token("-----BEGIN ", "PUBLIC KEY-----"),
            token("-----BEGIN ", "CERTIFICATE-----"),
            // What receipts are full of.
            "sha256:5c8d316a79ba7f6e0d1b2c3a4f5e6d7c8b9a0f1e2d3c4b5a69788796a5b4c3d2".into(),
            "ed25519:3a9f3136fc4fc8b7d3d96d45cf987653063fbf0ca394dcc75cac2424da9097c9".into(),
            "test result: ok. 4 passed; 0 failed; task-skip_ok".into(),
        ];
        for text in near {
            assert_eq!(kinds(&text), Vec::<&str>::new(), "{text}");
        }
    }

    #[test]
    fn finds_every_token_in_binary_text() {
        let mut text = b"\x00\xff".to_vec();
        text.extend(token("AK", &format!("IA{}", "Z".repeat(16))).as_bytes());
        text.extend(b"\n\xfe");
        text.extend(token("np", &format!("m_{}", "9".repeat(36))).as_bytes());
        let found: Vec<_> = scan(&text).iter().map(|f| (f.kind, f.offset)).collect();
        assert_eq!(found, [("AWS access key", 2), ("npm token", 24)]);
    }
}
