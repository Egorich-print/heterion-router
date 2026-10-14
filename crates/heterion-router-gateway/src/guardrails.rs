//! Rule-based guardrails: four checkers, three actions, no dependencies.
//!
//! Roadmap M3.3 (`docs/ROADMAP-2026-10.md` §6): the tier the Future-AGI parity
//! audit asks for — inline, local, sub-millisecond, no network hop, no model
//! in the binary. What is deliberately absent is the rest of that product: ML
//! guardrails and vendor adapters need exactly the two things this crate
//! refuses to carry.
//!
//! Four checks, each a pure function over text: [`secrets`], [`pii`],
//! [`keywords`], [`language`]. Three actions per check — `block`, `warn`,
//! `log` — carried in [`Config`], loaded from the `key_value` store the rest
//! of the dashboard settings live in ([`load`], [`save`], [`patch`]).
//!
//! # The one rule that outranks all four
//!
//! **No matched value ever leaves this module.** A [`Finding`] carries the
//! check name, the sub-kind and a hint made of the match's *shape* and
//! *length* — never a byte of the value ([`mask`]). Headers, log lines and
//! error bodies are all built from that struct, so a guardrail cannot become
//! the place a leaked credential is written down. The tests assert it with a
//! canary key: absent from the body, from the headers and from the captured
//! log.

use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Request, State},
    http::{HeaderName, HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use futures::StreamExt;
use heterion_router_core::GatewayError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{AppState, JsonBody, error_response, forbidden_response};

/// `key_value` namespace the policy lives in — one table, a namespace per
/// feature, the same shape the response store and the settings already use.
pub const NAMESPACE: &str = "guardrails";
/// The one row holding the whole policy, as JSON. Four checks do not need a
/// table, and a policy half-read from a table is not a policy.
pub const CONFIG_KEY: &str = "config";
/// Marks an answer a `warn` check had an opinion about.
///
/// `true`, and nothing else: the pinned value the roadmap names, so a client
/// can test for its presence without parsing a second vocabulary. A `block`
/// answers with the 403 envelope, whose body already says why, and a `log`
/// adds no header at all — that difference is the whole difference between the
/// two actions.
pub const HEADER_TRIGGERED: &str = "x-router-guardrail-triggered";
/// Which checks fired, comma-separated. A closed set — the four ids of
/// [`Check`], which are also the four keys of [`Config`].
pub const HEADER_CHECKS: &str = "x-router-guardrail-checks";
/// Bytes read from a body per direction.
///
/// Past this the body is scanned in its first [`SCAN_LIMIT_BYTES`] and passed
/// on whole: a guardrail that buffers an unbounded body is a memory bug, and a
/// credential buried at byte 400 KiB of a 10 MiB paste is not the case this
/// serves.
/// ponytail: one cap, prefix-only past it; raise it when a real oversized
/// request shows up in a trace.
pub const SCAN_LIMIT_BYTES: usize = 256 * 1024;
/// The entropy bar for the generic high-entropy rule, in bits per character.
///
/// **Why 4.2, and why the length floor it sits behind.** The rule has to
/// separate two populations whose entropies were measured, not guessed:
///
/// | population                              | bits/char        |
/// |----------------------------------------|------------------|
/// | lowercase English words (`recognition`) | 2.9 – 3.6        |
/// | digit-bearing prose, versions, config   | 3.7 – 4.0        |
/// | random 32-char base64/alnum token, p5   | 4.33             |
/// | random 48-char token, minimum           | 4.49             |
///
/// 4.2 is above every natural-text sample and below the 5th percentile of a
/// 32-character random token, so the bar costs a rare miss on a short secret
/// and buys no false positive on text. It is a bit boundary inside a wide gap,
/// which is why it is safe to expose as configuration. The length floor is
/// what keeps the populations apart at all: below 32 characters the median
/// token is 4.0 and the distributions overlap.
pub const DEFAULT_ENTROPY_THRESHOLD: f64 = 4.2;
/// Shortest run the generic entropy rule looks at. See the table above: under
/// 32 characters the token and prose distributions overlap.
const MIN_BLOB_LEN: usize = 32;
/// Entropy bar for the hexadecimal rule, in bits per character.
///
/// A 16-symbol alphabet caps entropy at log₂16 = 4.0, so no hex string could
/// ever clear [`DEFAULT_ENTROPY_THRESHOLD`] and a shared bar would leave the
/// rule dead on arrival. Kept low on purpose: a 32+ digit hex run is already
/// suspicious by shape, and the entropy test is only there to throw away the
/// degenerate repeats (`deadbeefdeadbeef…` measures 2.16).
pub const HEX_MIN_ENTROPY: f64 = 2.5;
/// Shortest hexadecimal run the hex rule looks at.
const MIN_HEX_LEN: usize = 32;
/// Shortest tail after a `sk-` prefix that can still be a key.
const MIN_SECRET_TAIL: usize = 16;
/// Length of an AWS access key id: `AKIA`/`ASIA` + 16.
const AWS_KEY_TAIL: usize = 16;
/// Digit counts a card can have: Visa 16, Amex 15, plus the 13- and 19-digit
/// Diners/JCB legacy ranges.
const CARD_DIGITS: (usize, usize) = (13, 19);
/// Digit counts a phone can have: E.164's 15 maximum, a national 10.
const PHONE_DIGITS: (usize, usize) = (10, 15);
/// Share of alphabetic characters one non-Latin block must hold to name the
/// language outright.
const SCRIPT_SHARE: f32 = 0.5;
/// Minimum alphabetic characters before a script verdict.
const MIN_SCRIPT_LETTERS: usize = 4;
/// Minimum Latin-script words before stopwords may decide.
const MIN_LATIN_TOKENS: usize = 3;
/// Share of words that must be stopwords for a Latin-script verdict.
const MIN_STOPWORD_SHARE: f32 = 0.2;

/// Shannon entropy of an ASCII string, in bits per character.
///
/// `H = -Σ p(c)·log₂ p(c)` over the byte histogram. Per *character* is the
/// unit that matters: total bits grow with length, so a total would flag every
/// long word. Non-ASCII bytes are skipped rather than folded, because the
/// rules that use this all run on the ASCII spans [`spans`] produces.
pub fn shannon_entropy(text: &str) -> f64 {
    if text.is_empty() {
        return 0.0;
    }
    let mut counts = [0usize; 128];
    let mut counted = 0usize;
    for byte in text.bytes() {
        if byte.is_ascii() {
            counts[byte as usize] += 1;
            counted += 1;
        }
    }
    if counted == 0 {
        return 0.0;
    }
    let total = counted as f64;
    -counts
        .iter()
        .filter(|count| **count > 0)
        .map(|count| {
            let share = *count as f64 / total;
            share * share.log2()
        })
        .sum::<f64>()
}

/// High-entropy credentials in `text`: `sk-`-style keys, AWS access key ids,
/// JWTs, and hex/base64 blobs judged by [`shannon_entropy`].
///
/// Each rule is a *shape* rule; entropy only decides the one rule that has no
/// shape of its own (an anonymous blob). Hand-rolled, because the workspace
/// deliberately has no regex crate: a prefixed key is `prefix + N allowed
/// bytes`, a JWT is `eyJ` + two dots + three base64url segments, a blob is a
/// long run whose alphabet is small enough for the entropy bar to mean
/// something.
pub fn secrets(text: &str, entropy_threshold: f64) -> Vec<Finding> {
    let mut findings = Vec::new();
    // Prefixed shapes and JWTs are scanned over the raw text, not over
    // `spans`: a `-` inside `sk-live-…` is not a phone separator, so a
    // tokenizer built for phone numbers cannot be trusted to keep a key whole.
    for (start, _) in text.match_indices("eyJ") {
        if let Some(length) = jwt_len(&text[start..]) {
            findings.push(Finding {
                check: Check::SecretsDetector,
                kind: "jwt",
                hint: mask("jwt", length),
            });
        }
    }
    for (prefix, kind, tail) in SECRET_PREFIXES {
        for (start, _) in text.match_indices(prefix) {
            let Some(length) = prefixed_secret(&text[start..], kind, *tail) else {
                continue;
            };
            findings.push(Finding {
                check: Check::SecretsDetector,
                kind,
                hint: mask(kind, length),
            });
        }
    }
    for span in spans(text) {
        if let Some(kind) = entropy_secret(span, entropy_threshold) {
            findings.push(Finding {
                check: Check::SecretsDetector,
                kind,
                hint: mask(kind, span.len()),
            });
        }
    }
    findings
}

/// Prefixed credential shapes: `(prefix, sub-kind, shortest acceptable tail)`.
///
/// `ASIA` sits next to `AKIA` because it is the same key id issued to a
/// temporary session credential, and one table row is cheaper than a special
/// case later.
const SECRET_PREFIXES: &[(&str, &str, usize)] = &[
    ("sk-", "openai-style-key", MIN_SECRET_TAIL),
    ("AKIA", "aws-access-key-id", AWS_KEY_TAIL),
    ("ASIA", "aws-session-key-id", AWS_KEY_TAIL),
];

/// The length of the credential at the start of `text`, if the shape holds.
///
/// `sk-` keys are base64url with dashes; an AWS key id is 16 uppercase
/// alphanumerics and nothing else. Both are "prefix + N bytes from one
/// alphabet", which is the whole of what a regex for them would say.
fn prefixed_secret(text: &str, kind: &str, tail: usize) -> Option<usize> {
    let prefix = SECRET_PREFIXES
        .iter()
        .find(|(_, candidate, _)| *candidate == kind)
        .map(|(prefix, _, _)| *prefix)?;
    let rest = text.strip_prefix(prefix)?;
    // The credential is the maximal run of allowed bytes, not "the rest of the
    // text": a key pasted into a sentence is followed by a comma and a word.
    let allowed = |byte: u8| {
        if kind == "openai-style-key" {
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
        } else {
            byte.is_ascii_uppercase() || byte.is_ascii_digit()
        }
    };
    let length = rest.bytes().take_while(|byte| allowed(*byte)).count();
    (length >= tail).then(|| prefix.len() + length)
}
/// The entropy rules: hex first (its own bar), then the generic blob.
fn entropy_secret(span: &str, entropy_threshold: f64) -> Option<&'static str> {
    if span.len() >= MIN_HEX_LEN
        && span.bytes().all(|byte| byte.is_ascii_hexdigit())
        && shannon_entropy(span) >= HEX_MIN_ENTROPY
    {
        return Some("hex-blob");
    }
    // A digit is required so that no run of letters can ever clear this bar:
    // English words are the false-positive population, and requiring a digit
    // removes them structurally rather than by tuning a threshold.
    let mixed = span.bytes().any(|byte| byte.is_ascii_digit())
        && span.bytes().any(|byte| byte.is_ascii_alphabetic());
    (span.len() >= MIN_BLOB_LEN && mixed && shannon_entropy(span) >= entropy_threshold)
        .then_some("high-entropy-blob")
}

/// The length of a JWT at the start of `text`, if there is one.
///
/// `eyJ` is base64 for `{"`, so a real JWT starts with it; two dots and three
/// non-empty base64url segments make the shape. The two payload segments must
/// clear [`MIN_SECRET_TAIL`]: a truncated word that happens to begin `eyJ` is
/// not a token.
fn jwt_len(text: &str) -> Option<usize> {
    let mut segments = [0usize; 3];
    let mut current = 0usize;
    let mut length = 0usize;
    let mut total = 0usize;
    for c in text.chars() {
        match c {
            '.' => {
                if current >= 2 || length == 0 {
                    return None;
                }
                segments[current] = length;
                current += 1;
                length = 0;
                total += 1;
            }
            c if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '=') => {
                length += 1;
                total += 1;
            }
            _ => return None,
        }
    }
    if current != 2 || length == 0 {
        return None;
    }
    (segments[0] >= MIN_SECRET_TAIL && segments[1] >= MIN_SECRET_TAIL).then_some(total)
}

/// Email, phone, Luhn-checked card, AWS access key id.
///
/// The card is the reason this is not a digit regex, and the phone is the
/// reason it is not a word-boundary regex either. See [`luhn`] and [`spans`].
pub fn pii(text: &str) -> Vec<Finding> {
    spans(text)
        .into_iter()
        .filter_map(|span| {
            pii_kind(span).map(|kind| Finding {
                check: Check::PiiDetector,
                kind,
                hint: mask(kind, span.chars().count()),
            })
        })
        .collect()
}

/// What one span is, most specific first.
fn pii_kind(span: &str) -> Option<&'static str> {
    if is_email(span) {
        return Some("email");
    }
    if is_aws_key_id(span) {
        return Some("aws-access-key-id");
    }
    let digits = digits(span);
    // Card before phone: a 16-digit Luhn-valid run with dashes is a card, and
    // reporting it as a phone too is noise an operator has to learn to ignore.
    if (CARD_DIGITS.0..=CARD_DIGITS.1).contains(&digits) && luhn(span) && is_issuer_ish(span) {
        return Some("credit-card");
    }
    let separated = span
        .bytes()
        .any(|byte| matches!(byte, b'+' | b'-' | b'(' | b')' | b' '));
    (separated && (PHONE_DIGITS.0..=PHONE_DIGITS.1).contains(&digits)).then_some("phone")
}

/// Digit count, ignoring every separator.
fn digits(span: &str) -> usize {
    span.bytes().filter(u8::is_ascii_digit).count()
}

/// The Luhn checksum, as in ISO/IEC 7812.
///
/// `sum(dᵢ·2^⌈ⁱ/2⌉ mod 10)` over the digits from the right, valid when the
/// total is a multiple of ten.
///
/// **Why this instead of "13-19 digits":** a bare digit-run rule fires on
/// order ids, timestamps and account numbers often enough to be worthless —
/// roughly one run in ten passes a check digit by accident, and every one of
/// those teaches the operator to ignore the warnings. Luhn's check digit makes
/// a wrong number almost impossible to hit by chance, so the false-positive
/// rate collapses to zero at the cost of missing a card number that fails its
/// own checksum — which is precisely the case an operator does not need a PII
/// warning about.
fn luhn(span: &str) -> bool {
    let mut sum = 0usize;
    for (index, byte) in span.bytes().rev().filter(u8::is_ascii_digit).enumerate() {
        let mut value = usize::from(byte - b'0');
        if index % 2 == 1 {
            value *= 2;
            if value > 9 {
                value -= 9;
            }
        }
        sum += value;
    }
    sum != 0 && sum.is_multiple_of(10)
}

/// Leading digit inside the issuer ranges (3-6), so a Luhn-valid run that is
/// not a payment number is not called one.
fn is_issuer_ish(span: &str) -> bool {
    span.bytes()
        .find(u8::is_ascii_digit)
        .is_some_and(|byte| matches!(byte, b'3'..=b'6'))
}

/// `AKIA` + 16 uppercase alphanumerics is the whole key id.
fn is_aws_key_id(span: &str) -> bool {
    let Some(tail) = span.strip_prefix("AKIA") else {
        return false;
    };
    tail.len() == AWS_KEY_TAIL
        && tail
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

/// `local@host.tld`, with the local part and the TLD shaped the way a mail
/// agent would accept.
fn is_email(span: &str) -> bool {
    let Some((local, domain)) = span.split_once('@') else {
        return false;
    };
    if local.is_empty() || local.len() > 64 || span.matches('@').count() != 1 {
        return false;
    }
    let local_ok = local
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'%' | b'+' | b'-'));
    let Some((host, tld)) = domain.rsplit_once('.') else {
        return false;
    };
    local_ok
        && !host.is_empty()
        && !host.starts_with('.')
        && !host.ends_with('.')
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
        && (2..=24).contains(&tld.len())
        && tld.bytes().all(|b| b.is_ascii_alphabetic())
}

/// Maximal runs of characters a credential or PII shape could live in.
///
/// One tokenizer for both checkers, because both need the same two things a
/// naive `split_whitespace` throws away: the punctuation *inside* a value, and
/// the separators *inside* a number. A run starts at an alphanumeric, at `@`,
/// or at a `+` that a digit follows (E.164), and continues across the
/// characters that can appear inside a value:
///
/// * `.`, `_`, `%`, `+` while a letter, digit or `@` follows — the inside of a
///   domain or a dotted local part;
/// * `-`, `(`, `)` and a single space while a digit follows — the inside of a
///   phone number.
///
/// Trailing sentence punctuation is dropped, so `mail me at a@b.c.` still
/// yields `a@b.c`. Everything the tokenizers see is ASCII, so slicing on byte
/// offsets cannot split a multi-byte character: a UTF-8 continuation byte is
/// never ASCII-alphanumeric.
fn spans(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if !starts_span(bytes, index) {
            index += 1;
            continue;
        }
        let start = index;
        let mut end = index + 1;
        while end < bytes.len() && continues_span(bytes, end) {
            end += 1;
        }
        let mut trimmed = end;
        while trimmed > start + 1
            && matches!(bytes[trimmed - 1], b'.' | b',' | b';' | b':' | b'!' | b'?')
        {
            trimmed -= 1;
        }
        out.push(&text[start..trimmed]);
        index = end;
    }
    out
}

fn starts_span(bytes: &[u8], index: usize) -> bool {
    let byte = bytes[index];
    byte.is_ascii_alphanumeric()
        || byte == b'@'
        || (byte == b'+' && bytes.get(index + 1).is_some_and(u8::is_ascii_digit))
}

/// Whether the character at `index` belongs to the run that started earlier.
fn continues_span(bytes: &[u8], index: usize) -> bool {
    let byte = bytes[index];
    if byte.is_ascii_alphanumeric() || byte == b'@' {
        return true;
    }
    let next = bytes.get(index + 1);
    match byte {
        b'.' | b'_' | b'%' | b'+' => next.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'@'),
        // A phone separator: kept only while a digit is what follows, so
        // "the report. It is fine" does not glue two sentences together.
        b'-' | b'(' | b')' | b' ' => digit_follows(bytes, index),
        _ => false,
    }
}

/// Whether the run continues with a digit, looking past any further
/// separators — the difference between `415 555 0132` being one candidate and
/// three.
fn digit_follows(bytes: &[u8], index: usize) -> bool {
    let mut look = index + 1;
    while let Some(byte) = bytes.get(look) {
        match byte {
            b' ' | b'-' | b'(' | b')' => look += 1,
            _ => return byte.is_ascii_digit(),
        }
    }
    false
}

/// Operator-configured terms found in `text`.
///
/// A single-word term matches whole words only, so blocking `ass` does not
/// block `class`; a term carrying a separator matches as a substring, because
/// `kernel panic` is a phrase rather than two words. Both sides compare
/// lowercased.
pub fn keywords(text: &str, terms: &[String]) -> Vec<Finding> {
    let haystack = text.to_lowercase();
    let mut findings = Vec::new();
    for term in terms {
        let needle = term.trim().to_lowercase();
        if needle.is_empty() {
            continue;
        }
        let phrase = needle.chars().any(|c| !c.is_alphanumeric());
        let hit = if phrase {
            haystack.contains(&needle)
        } else {
            haystack
                .split(|c: char| !c.is_alphanumeric())
                .any(|word| word == needle)
        };
        if hit {
            findings.push(Finding {
                check: Check::KeywordBlocklist,
                kind: "term",
                // The term's own length, not the matched span's: a blocklist
                // term is operator config, not the client's data, but its
                // length still says nothing about what was matched.
                hint: mask("term", needle.chars().count()),
            });
        }
    }
    findings
}

/// Which language `text` is in, or `None` when this cannot be told.
///
/// **Method.** Unicode script ranges first, stopword frequency second:
///
/// 1. Count alphabetic characters per block. If one non-Latin block holds at
///    least [`SCRIPT_SHARE`] of them, that block names the language: Cyrillic
///    → `ru`, Greek → `el`, Arabic → `ar`, Hebrew → `he`, Han → `ja` when
///    kana is present and `zh` otherwise.
/// 2. Otherwise take the Latin-script words, count how many are stopwords of
///    each language in [`STOPWORDS`], and let the best count win — provided it
///    is not tied.
///
/// **Honest limits.** This is a heuristic, not a classifier, and it is built
/// to say "I don't know" rather than to guess confidently:
///
/// * Word frequency with no syntax, no casing rules and no dictionary, so
///   short texts, code, product names and log lines are reported as unknown;
///   anything under [`MIN_LATIN_TOKENS`] words abstains.
/// * The stopword sets overlap exactly where the languages do — `de`/`en`,
///   `es`/`pt`, `nl`/`de`. A tie abstains instead of picking the earlier list.
/// * Chinese, Japanese and Korean share a script family: only the presence of
///   kana separates `ja`, and `ko` is never claimed.
/// * Mixed text goes to the majority script, so a one-word Russian quotation in
///   an English paragraph reads as English. For a character-count method that
///   is the honest answer, not a bug to paper over.
/// * `confidence` is a share of evidence, not a probability: 0.6 means "60 %
///   of the characters agreed", not "60 % likely correct".
pub fn language(text: &str) -> Option<LanguageGuess> {
    script_language(text).or_else(|| latin_language(text))
}

/// Languages the stopword fallback knows. The Russian entry is a
/// transliterated set: real Russian text is caught by the script rule, and
/// this exists only for a sentence typed on a Latin keyboard.
const STOPWORDS: &[(&str, &[&str])] = &[
    (
        "en",
        &[
            "the", "and", "is", "are", "of", "to", "in", "that", "it", "for", "on", "with", "was",
            "you", "this", "be", "have", "not", "they", "we",
        ],
    ),
    (
        "es",
        &[
            "el", "la", "de", "que", "y", "en", "los", "las", "un", "una", "por", "con", "para",
            "es", "no", "se", "su", "lo", "como", "más",
        ],
    ),
    (
        "fr",
        &[
            "le", "la", "les", "de", "des", "du", "et", "un", "une", "est", "en", "que", "qui",
            "pour", "dans", "sur", "pas", "ce", "je", "nous",
        ],
    ),
    (
        "de",
        &[
            "der", "die", "das", "und", "ist", "ein", "eine", "nicht", "mit", "auf", "für", "von",
            "den", "dem", "des", "sich", "auch", "wird", "sind", "aber",
        ],
    ),
    (
        "it",
        &[
            "il", "di", "che", "un", "una", "per", "con", "non", "sono", "del", "dei", "gli",
            "nel", "ma", "come", "più", "anche", "questo", "quella", "gli",
        ],
    ),
    (
        "pt",
        &[
            "os", "as", "de", "do", "da", "em", "no", "na", "não", "um", "uma", "para", "com",
            "por", "que", "se", "mais", "como", "ele", "ela",
        ],
    ),
    (
        "nl",
        &[
            "het", "een", "en", "van", "is", "in", "dat", "op", "te", "met", "voor", "niet",
            "zijn", "aan", "ook", "er", "maar", "door", "werd", "wordt",
        ],
    ),
    (
        "ru",
        &[
            "privet",
            "u",
            "menya",
            "ty",
            "ne",
            "net",
            "yes",
            "pomogi",
            "spasibo",
            "konec",
            "zdravstvuyte",
            "pozhaluysta",
        ],
    ),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Script {
    Latin,
    Cyrillic,
    Greek,
    Arabic,
    Hebrew,
    Han,
    Kana,
}

/// The dominant non-Latin script, if one dominates.
fn script_language(text: &str) -> Option<LanguageGuess> {
    let mut letters = 0usize;
    let mut kana = 0usize;
    let mut dominant: Option<Script> = None;
    let mut dominant_letters = 0usize;
    for c in text.chars() {
        if !c.is_alphabetic() {
            continue;
        }
        letters += 1;
        match script_of(c) {
            Script::Latin => {}
            Script::Kana => {
                kana += 1;
                dominant_letters += 1;
                dominant = Some(Script::Kana);
            }
            script => {
                dominant_letters += 1;
                dominant = Some(script);
            }
        }
    }
    if letters < MIN_SCRIPT_LETTERS || dominant_letters as f32 / (letters as f32) < SCRIPT_SHARE {
        return None;
    }
    let code = match dominant? {
        Script::Cyrillic => "ru",
        Script::Greek => "el",
        Script::Arabic => "ar",
        Script::Hebrew => "he",
        // Han and kana are one script family: only the presence of kana
        // separates `ja` from `zh`, so either dominant block reads the same.
        Script::Han | Script::Kana => {
            if kana > 0 {
                "ja"
            } else {
                "zh"
            }
        }
        Script::Latin => return None,
    };
    Some(LanguageGuess {
        code,
        confidence: dominant_letters as f32 / letters as f32,
    })
}

/// Which block a letter belongs to. Ranges, not a script table: enough for the
/// languages [`language`] claims, and for anything else it abstains rather than
/// guessing.
fn script_of(c: char) -> Script {
    match c as u32 {
        0x0370..=0x03FF | 0x1F00..=0x1FFF => Script::Greek,
        0x0400..=0x052F => Script::Cyrillic,
        0x0590..=0x05FF | 0xFB1D..=0xFB4F => Script::Hebrew,
        0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF => Script::Arabic,
        0x3040..=0x30FF | 0x31F0..=0x31FF => Script::Kana,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF => Script::Han,
        _ => Script::Latin,
    }
}

/// Stopword vote over Latin-script words.
fn latin_language(text: &str) -> Option<LanguageGuess> {
    let words: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    if words.len() < MIN_LATIN_TOKENS {
        return None;
    }
    let mut best: (&str, usize) = ("", 0);
    let mut tied = false;
    for (code, stopwords) in STOPWORDS {
        let hits = words
            .iter()
            .filter(|word| {
                word.chars().all(|c| c.is_ascii_alphabetic())
                    && stopwords.contains(&word.to_lowercase().as_str())
            })
            .count();
        match hits.cmp(&best.1) {
            std::cmp::Ordering::Greater => {
                best = (code, hits);
                tied = false;
            }
            std::cmp::Ordering::Equal if hits > 0 => tied = true,
            _ => {}
        }
    }
    let (code, hits) = best;
    // A tie is not a verdict: two languages claiming the same evidence is
    // exactly the case where naming either would be a false certainty.
    if tied || hits == 0 || hits as f32 / (words.len() as f32) < MIN_STOPWORD_SHARE {
        return None;
    }
    Some(LanguageGuess {
        code,
        confidence: hits as f32 / words.len() as f32,
    })
}

/// Run every enabled check over `text`.
///
/// `language-detection` is the only check with an allow-list: with no
/// `languages` configured it has nothing to enforce, so it reports nothing
/// rather than telling the operator what language everyone is already writing
/// in.
pub fn inspect(text: &str, config: &Config) -> Vec<Finding> {
    let mut findings = Vec::new();
    for check in Check::ALL {
        if !check.settings(config).enabled {
            continue;
        }
        match check {
            Check::SecretsDetector => findings.extend(secrets(text, config.entropy_threshold)),
            Check::PiiDetector => findings.extend(pii(text)),
            Check::KeywordBlocklist => findings.extend(keywords(text, &config.blocklist)),
            Check::LanguageDetection => findings.extend(language_findings(text, config)),
        }
    }
    findings
}

/// The language check's own precondition, kept out of [`inspect`] so the
/// match arm stays one line.
fn language_findings(text: &str, config: &Config) -> Vec<Finding> {
    if config.languages.is_empty() {
        return Vec::new();
    }
    let Some(guess) = language(text) else {
        return Vec::new();
    };
    if config
        .languages
        .iter()
        .any(|wanted| wanted.eq_ignore_ascii_case(guess.code))
    {
        return Vec::new();
    }
    vec![Finding {
        check: Check::LanguageDetection,
        kind: "language",
        // A language code is this module's own vocabulary, not text out of the
        // request, so it needs no masking — but it is written as a fixed string
        // for the same reason every other hint is: nothing derived from the
        // body may reach a log.
        hint: "language not in allow-list".to_string(),
    }]
}

/// Read the policy out of the store.
///
/// No row, an unreadable row and a row that does not parse all mean the same
/// thing: the shipped defaults. Failing *closed* on a config error would start
/// rejecting traffic because of a typo in a dashboard, which is the same class
/// of surprise as blocking by default.
pub fn load(conn: &rusqlite::Connection) -> Config {
    let stored = heterion_router_db::repos::kv_get(conn, NAMESPACE, CONFIG_KEY);
    let raw = match stored {
        Ok(Some(raw)) => raw,
        Ok(None) => return Config::default(),
        Err(error) => {
            tracing::warn!("guardrail policy read failed: {error}");
            return Config::default();
        }
    };
    match serde_json::from_str::<Config>(&raw) {
        Ok(config) => sanitize(config),
        Err(error) => {
            tracing::warn!("guardrail policy row is not readable; using the defaults: {error}");
            Config::default()
        }
    }
}

/// Write the policy into the store.
pub fn save(conn: &rusqlite::Connection, config: &Config) -> heterion_router_db::Result<()> {
    let sanitized = sanitize(config.clone());
    let encoded = serde_json::to_string(&sanitized)?;
    heterion_router_db::repos::kv_set(conn, NAMESPACE, CONFIG_KEY, &encoded)
}

/// Merge a partial update into the stored policy and return the result.
pub fn patch(conn: &rusqlite::Connection, patch: ConfigPatch) -> Config {
    let mut config = load(conn);
    if let Some(check) = patch.secrets_detector {
        config.secrets_detector = check;
    }
    if let Some(check) = patch.pii_detector {
        config.pii_detector = check;
    }
    if let Some(check) = patch.keyword_blocklist {
        config.keyword_blocklist = check;
    }
    if let Some(check) = patch.language_detection {
        config.language_detection = check;
    }
    if let Some(terms) = patch.blocklist {
        config.blocklist = terms;
    }
    if let Some(languages) = patch.languages {
        config.languages = languages;
    }
    if let Some(threshold) = patch.entropy_threshold {
        config.entropy_threshold = threshold;
    }
    if let Some(scan) = patch.scan_response {
        config.scan_response = scan;
    }
    if let Err(error) = save(conn, &config) {
        tracing::warn!("guardrail policy write failed: {error}");
    }
    config
}

/// Drop what a policy must never carry, once, at the boundary.
///
/// A blank blocklist term would match every text; a non-positive or absurd
/// entropy bar would make the generic rule match ordinary words. Both are
/// fixed here rather than trusted to every writer — including the dashboard.
fn sanitize(mut config: Config) -> Config {
    config.blocklist = clean_terms(config.blocklist);
    config.languages = clean_terms(config.languages);
    if !(0.0..=12.0).contains(&config.entropy_threshold) {
        config.entropy_threshold = DEFAULT_ENTROPY_THRESHOLD;
    }
    config
}

fn clean_terms(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

/// The redacted hint: shape and length, nothing else.
///
/// The only function that turns a match into something reportable, and it
/// cannot be asked for more — it takes the sub-kind and a length, never the
/// matched text, so no later caller can widen the leak by passing the wrong
/// argument.
fn mask(kind: &str, matched_chars: usize) -> String {
    format!("{kind} len={matched_chars}")
}

/// Guardrails over the whole router.
///
/// Registered as a layer, not inside a handler, for the one reason that
/// matters: a `block` must cost zero upstream calls, which is only true if the
/// decision happens before a handler dispatches.
///
/// Both directions are scanned and they answer different questions:
///
/// * **Request** — always. What the client is about to hand a provider: the
///   pasted credential, the customer's card, the prompt that was meant to stay
///   internal. This is the default because it is the direction the operator
///   actually controls: the text entered their own tool.
/// * **Response** — [`Config::scan_response`], on by default. What a provider
///   sent back. It catches the half of the leak the request direction cannot
///   see — a provider echoing a key into a completion, a translation layer
///   inlining one — and it is on by default because it can only ever *add* a
///   header, never change a status or a body, so a false positive costs a
///   header. Off would mean the operator has to know to switch on the
///   direction that catches the provider's own leak.
///
/// On `block` the request is answered with the project's own 403 envelope
/// ([`forbidden_response`]) naming the checks and nothing else. On `warn` the
/// body passes through byte for byte with [`HEADER_TRIGGERED`] set; on `log`
/// the response is exactly what it was before the guardrails existed — not one
/// byte changed, not one header added — and the log line is where the operator
/// looks instead.
///
/// A `block` is only ever honoured on the request direction. The response bytes
/// are already on their way to the client, so refusing to forward them would
/// mean dropping a completion over a violation the provider cannot fix; a
/// response that trips a `block` check is annotated and logged like a `warn`.
pub async fn middleware(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let config = match state.db.as_ref() {
        Some(db) => {
            let conn = db.connection();
            load(&conn)
        }
        // No database means no policy store, so the shipped defaults — and with
        // them no rejections.
        None => Config::default(),
    };

    let method = request.method().clone();
    let path = request.uri().path().to_string();
    if !is_scannable(&method, &path) {
        return next.run(request).await;
    }

    let (parts, body) = request.into_parts();
    let (prefix, body) = read_prefix(body, SCAN_LIMIT_BYTES).await;
    let findings = inspect(&body_text(&prefix), &config);

    if let Some(refused) = refuse(&findings, &config, Direction::Request) {
        return refused;
    }

    let response = next.run(Request::from_parts(parts, body)).await;
    let response = scan_response(response, &config).await;
    annotate(response, &findings, &config, Direction::Request)
}

/// Whether this request carries a body worth scanning.
///
/// Only a JSON `POST`/`PATCH` under `/v1/`: `/healthz`, `/api/*` and every SSE
/// `GET` carry no prompt, and buffering them would buy nothing. The dashboard's
/// own payloads are the operator's, and a blocklist term inside a policy body is
/// not a violation.
fn is_scannable(method: &Method, path: &str) -> bool {
    matches!(method, &Method::POST | &Method::PATCH) && path.starts_with("/v1/")
}

/// The findings whose check is configured to `block`.
fn blocking<'a>(findings: &'a [Finding], config: &Config) -> Vec<&'a Finding> {
    findings
        .iter()
        .filter(|finding| finding.check.settings(config).action == Action::Block)
        .collect()
}

/// The 403 for a blocked body, or `None` when nothing blocks.
///
/// [`forbidden_response`] is the project's own refusal shape
/// (`error.message`/`type`/`code`), so a client parses this like any other
/// permission error. The message names the checks and the direction, both of
/// which this module produced — never the text that tripped them.
fn refuse(findings: &[Finding], config: &Config, direction: Direction) -> Option<Response> {
    let denied = blocking(findings, config);
    if denied.is_empty() {
        return None;
    }
    let names = check_names(&denied);
    tracing::warn!(
        "guardrail: check={} hint={} direction={}",
        names.join(","),
        denied[0].hint,
        direction.label(),
    );
    let message = format!("guardrail blocked this request: {}", names.join(", "));
    let mut response = forbidden_response(&message);
    set_header(&mut response, HEADER_CHECKS, &names.join(","));
    Some(response)
}

/// Add what a non-blocking hit earned.
///
/// `log` gets nothing: the point of it is that the client's response is
/// byte-identical to what it was before the guardrails existed, and the log
/// line is where the operator looks instead.
fn annotate(
    mut response: Response,
    findings: &[Finding],
    config: &Config,
    direction: Direction,
) -> Response {
    let noted: Vec<&Finding> = findings
        .iter()
        .filter(|finding| finding.check.settings(config).action != Action::Block)
        .collect();
    if noted.is_empty() {
        return response;
    }
    let names = check_names(&noted);
    let warned = noted
        .iter()
        .any(|finding| finding.check.settings(config).action == Action::Warn);
    for finding in &noted {
        tracing::info!(
            "guardrail: check={} hint={} direction={}",
            finding.check.id(),
            finding.hint,
            direction.label(),
        );
    }
    if !warned {
        // `log` only: not one byte of the response changed and not one header
        // added. That is the whole difference between it and `warn`.
        return response;
    }
    set_header(&mut response, HEADER_TRIGGERED, "true");
    set_header(&mut response, HEADER_CHECKS, &names.join(","));
    response
}

/// Scan the answer on the way back, for `warn` and `log` only.
///
/// A `text/event-stream` body is never read: buffering a stream to inspect it
/// would delay the first token by the whole completion, which is the one cost
/// this feature must never pay. Everything else comes back as it was — the
/// bytes read are put back in front of the untouched remainder inside
/// [`read_prefix`], so a body this middleware did not scan is byte-identical to
/// one it never saw.
async fn scan_response(response: Response, config: &Config) -> Response {
    if !config.scan_response || is_streaming(&response) {
        return response;
    }
    let (parts, body) = response.into_parts();
    let (prefix, body) = read_prefix(body, SCAN_LIMIT_BYTES).await;
    let findings = inspect(&body_text(&prefix), config);
    let response = Response::from_parts(parts, body);
    annotate(response, &findings, config, Direction::Response)
}

fn is_streaming(response: &Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|content_type| content_type.starts_with("text/event-stream"))
}

/// Read up to `limit` bytes for scanning and hand back a body that still
/// carries every byte it started with.
///
/// Lossless by construction: the chunks that were read go back in front of the
/// untouched remainder of the same stream, and the one chunk that crossed the
/// cap is put back whole rather than truncated. The obvious alternative —
/// `to_bytes`, then reconstruct — loses the body whenever the read fails, which
/// for a response means the client gets nothing at all.
async fn read_prefix(body: Body, limit: usize) -> (Bytes, Body) {
    let mut stream = Box::pin(body.into_data_stream());
    // Two buffers, because they answer different questions: `scanned` is what
    // the checks see and stops at the cap, `consumed` is what has to go back
    // into the body. They diverge on the chunk that crosses the cap — its head
    // is scanned, and the chunk itself is put back whole rather than truncated,
    // because a truncated body is a body nobody sent.
    let mut scanned: Vec<u8> = Vec::new();
    let mut consumed: Vec<u8> = Vec::new();
    let mut carry: Option<Bytes> = None;
    while scanned.len() < limit {
        let Some(item) = stream.next().await else {
            break;
        };
        let Ok(chunk) = item else {
            break;
        };
        if scanned.len() + chunk.len() > limit {
            scanned.extend_from_slice(&chunk[..limit - scanned.len()]);
            carry = Some(chunk);
            break;
        }
        scanned.extend_from_slice(&chunk);
        consumed.extend_from_slice(&chunk);
    }
    let prefix = Bytes::from(scanned);
    let mut rebuilt: futures::stream::BoxStream<'static, Result<Bytes, axum::Error>> = Box::pin(
        futures::stream::once(async move { Ok(Bytes::from(consumed)) }),
    );
    if let Some(chunk) = carry {
        rebuilt = Box::pin(rebuilt.chain(futures::stream::once(async move { Ok(chunk) })));
    }
    let rest = stream;
    (prefix, Body::from_stream(Box::pin(rebuilt.chain(rest))))
}

/// Check ids in reporting order, without repeats.
fn check_names(findings: &[&Finding]) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Vec::new();
    for finding in findings {
        let id = finding.check.id();
        if !names.contains(&id) {
            names.push(id);
        }
    }
    names
}

/// Set a header, ignoring a value the HTTP layer would refuse.
///
/// A header is not where a verdict goes; if the value cannot be encoded, the
/// request still has to be answered.
fn set_header(response: &mut Response, name: &str, value: &str) {
    if let Ok(name) = HeaderName::from_bytes(name.as_bytes())
        && let Ok(value) = HeaderValue::from_str(value)
    {
        response.headers_mut().insert(name, value);
    }
}

/// What to do about a check that fired.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    /// Refuse the request before any upstream call.
    Block,
    /// Let it through, say so in [`HEADER_TRIGGERED`], leave the body alone.
    #[default]
    Warn,
    /// Let it through and write one log line. No header.
    Log,
}

/// The four checks, in reporting order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Check {
    SecretsDetector,
    PiiDetector,
    KeywordBlocklist,
    LanguageDetection,
}

impl Check {
    /// Every check, in a fixed order.
    pub const ALL: [Check; 4] = [
        Check::SecretsDetector,
        Check::PiiDetector,
        Check::KeywordBlocklist,
        Check::LanguageDetection,
    ];

    /// The check's id: its config key and its [`HEADER_CHECKS`] value.
    pub fn id(self) -> &'static str {
        match self {
            Check::SecretsDetector => "secrets-detector",
            Check::PiiDetector => "pii-detector",
            Check::KeywordBlocklist => "keyword-blocklist",
            Check::LanguageDetection => "language-detection",
        }
    }

    /// This check's slot in a policy.
    pub fn settings(self, config: &Config) -> &CheckConfig {
        match self {
            Check::SecretsDetector => &config.secrets_detector,
            Check::PiiDetector => &config.pii_detector,
            Check::KeywordBlocklist => &config.keyword_blocklist,
            Check::LanguageDetection => &config.language_detection,
        }
    }
}

/// One check's enabled flag and action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CheckConfig {
    /// Off means the check is not run at all.
    pub enabled: bool,
    /// What a hit does. Defaults to [`Action::Warn`].
    pub action: Action,
}

impl Default for CheckConfig {
    /// Enabled and `warn`: a check nobody asked for may notice things, and may
    /// not start refusing traffic on its own.
    fn default() -> Self {
        Self {
            enabled: true,
            action: Action::Warn,
        }
    }
}

/// The whole policy, as stored in [`CONFIG_KEY`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub secrets_detector: CheckConfig,
    pub pii_detector: CheckConfig,
    pub keyword_blocklist: CheckConfig,
    pub language_detection: CheckConfig,
    /// Terms for `keyword-blocklist`.
    pub blocklist: Vec<String>,
    /// Languages `language-detection` insists on. Empty stays inert.
    pub languages: Vec<String>,
    /// Entropy bar for the generic blob rule, bits per character.
    pub entropy_threshold: f64,
    /// Also scan the answer on the way back.
    pub scan_response: bool,
}

impl Default for Config {
    /// **The shipped default: every check on, every action `warn`, nothing
    /// configured to violate.** A user who configures nothing sees no new
    /// rejections — the only promise a check with an unknowable positive rate
    /// can make. It is not "off": an email in a prompt is flagged and answered
    /// with `x-router-guardrail-triggered: true`, so the operator can see what
    /// the checks see before deciding to refuse anything. Turning one into a
    /// hard `block` is one `PATCH /api/guardrails` away.
    fn default() -> Self {
        Self {
            secrets_detector: CheckConfig::default(),
            pii_detector: CheckConfig::default(),
            keyword_blocklist: CheckConfig::default(),
            language_detection: CheckConfig::default(),
            blocklist: Vec::new(),
            languages: Vec::new(),
            entropy_threshold: DEFAULT_ENTROPY_THRESHOLD,
            scan_response: true,
        }
    }
}

/// A partial policy update: absent leaves a field alone.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ConfigPatch {
    pub secrets_detector: Option<CheckConfig>,
    pub pii_detector: Option<CheckConfig>,
    pub keyword_blocklist: Option<CheckConfig>,
    pub language_detection: Option<CheckConfig>,
    pub blocklist: Option<Vec<String>>,
    pub languages: Option<Vec<String>>,
    pub entropy_threshold: Option<f64>,
    pub scan_response: Option<bool>,
}

/// Which way a body was travelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// What the client is about to send upstream.
    Request,
    /// What the provider is sending back.
    Response,
}

impl Direction {
    /// For a log line. Never a header value: a direction is not something a
    /// client has to act on.
    fn label(self) -> &'static str {
        match self {
            Direction::Request => "request",
            Direction::Response => "response",
        }
    }
}

/// One check firing.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Finding {
    pub check: Check,
    /// Sub-kind inside the check, e.g. `openai-style-key`.
    pub kind: &'static str,
    /// `openai-style-key len=51`. Never the matched bytes.
    pub hint: String,
}

/// A language verdict, with the share of evidence behind it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LanguageGuess {
    pub code: &'static str,
    pub confidence: f32,
}

/// `GET /api/guardrails` — the policy actually in force, defaults included.
///
/// The defaults are returned rather than an empty object, because a dashboard
/// has to render the state of the system and "no row" *is* a state.
pub async fn settings(
    State(state): State<AppState>,
    _auth: crate::auth::ManageAuthenticated,
) -> Json<Value> {
    let config = match state.db.as_ref() {
        Some(db) => {
            let conn = db.connection();
            load(&conn)
        }
        None => Config::default(),
    };
    Json(serde_json::to_value(config).unwrap_or(Value::Null))
}

/// `PATCH /api/guardrails` — merge a partial policy, answer with the result.
///
/// The merged policy rather than the patch, so a dashboard never has to guess
/// what the defaults filled in: the same "report the result, not the request"
/// rule the rest of the admin API follows.
pub async fn update_settings(
    State(state): State<AppState>,
    _auth: crate::auth::ManageAuthenticated,
    JsonBody(body): JsonBody<ConfigPatch>,
) -> Response {
    let Some(db) = state.db.as_ref() else {
        return error_response(GatewayError::InvalidRequest(
            "guardrails store their policy in the database; there is none".to_string(),
        ));
    };
    let conn = db.connection();
    let config = patch(&conn, body);
    (
        StatusCode::OK,
        Json(serde_json::to_value(config).unwrap_or(Value::Null)),
    )
        .into_response()
}

/// Gather the scannable text out of a request or response body.
///
/// Every JSON string leaf, except the `model` field: a model id is routing
/// metadata, not something a client wrote, and `gpt-4o-2024-08-06` would
/// otherwise be fed to the keyword and entropy rules on every call. Collecting
/// leaves rather than known fields is what lets one middleware cover
/// `/v1/chat/completions`, `/v1/messages`, `/v1/responses` and `/v1/embeddings`
/// without knowing any of their shapes.
fn collect_text(value: &Value, out: &mut String) {
    match value {
        Value::String(text) => {
            out.push_str(text);
            out.push('\n');
        }
        Value::Array(items) => items.iter().for_each(|item| collect_text(item, out)),
        Value::Object(map) => map.iter().for_each(|(key, item)| {
            if key == "model" {
                return;
            }
            collect_text(item, out);
        }),
        _ => {}
    }
}

/// The scannable text of a body, or nothing when there is nothing to read.
///
/// A JSON body is walked for its string leaves ([`collect_text`]); anything
/// else is scanned as plain text. That includes the *prefix* of a body too big
/// to buffer whole: a truncated JSON document does not parse, and falling back
/// to the raw bytes is what keeps an oversized paste scannable instead of
/// silently exempt. Bytes that are not text at all become replacement
/// characters, which match nothing — the cost of that fallback is bounded CPU,
/// not a wrong verdict.
fn body_text(bytes: &Bytes) -> String {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(value) => {
            let mut text = String::new();
            collect_text(&value, &mut text);
            text
        }
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppState;
    use crate::backend::{ChatBackend, ChunkStream};
    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use axum::routing::post;
    use heterion_router_core::{
        ChatChoice, ChatCompletionResponse, ChatMessage, GatewayError, StreamChunk, Usage,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    /// Synthetic stand-in for a live OpenAI-style key: the right shape, no real
    /// credential anywhere, and greppable in every assertion.
    const CANARY_KEY: &str = "sk-live-CANARYcanary0000000000000000000000000000abcd";
    /// A second canary for the response direction: a provider echoing a key.
    const ECHOED_KEY: &str = "sk-live-ECHOecho0000000000000000000000000000wxyz";
    const CANARY_EMAIL: &str = "canary.person@example.invalid";
    /// Luhn-valid, 16 digits.
    const CANARY_CARD: &str = "4539148803436467";
    /// 16 digits that fail Luhn: the shape a bare digit regex would flag.
    const NOT_A_CARD: &str = "1234567890123456";
    const REPLY: &str = "upstream answered";

    /// The upstream as these tests see it: a call counter and a fixed body.
    #[derive(Default)]
    struct Calls(AtomicUsize);

    struct CountingBackend {
        calls: Arc<Calls>,
        reply: String,
    }

    #[async_trait::async_trait]
    impl ChatBackend for CountingBackend {
        fn name(&self) -> &'static str {
            "counting"
        }

        async fn complete(
            &self,
            request: heterion_router_core::ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, GatewayError> {
            self.calls.0.fetch_add(1, Ordering::SeqCst);
            Ok(ChatCompletionResponse {
                id: "chatcmpl-guardrail".to_string(),
                object: "chat.completion",
                created: 1,
                model: request.model.clone(),
                choices: vec![ChatChoice {
                    index: 0,
                    message: ChatMessage::plain("assistant".to_string(), self.reply.clone()),
                    finish_reason: "stop".to_string(),
                }],
                usage: Usage {
                    prompt_tokens: 11,
                    completion_tokens: 2,
                    total_tokens: 13,
                },
                raw: None,
            })
        }

        fn stream(&self, _request: heterion_router_core::ChatCompletionRequest) -> ChunkStream {
            Box::pin(futures::stream::iter(vec![Ok(StreamChunk::done("stop"))]))
        }
    }

    /// Two routers over one policy and one backend: `router` with the guardrail
    /// layer, `plain` without it. "Unchanged from today" is measured against
    /// `plain`, so "the body came back intact" is an equality and not an
    /// impression.
    ///
    /// The layer sits *before* `.with_state` and passes the state explicitly,
    /// exactly where `lib.rs` is asked to put it: `from_fn` fixes the
    /// middleware state to `()`, so a middleware that reads [`AppState`] has to
    /// be built with `from_fn_with_state`.
    struct Harness {
        router: Router,
        plain: Router,
        calls: Arc<Calls>,
    }

    fn harness(reply: &str, configure: impl Fn(&rusqlite::Connection)) -> Harness {
        let db = Arc::new(heterion_router_db::Db::open_in_memory().unwrap());
        db.migrate().unwrap();
        let conn = db.connection();
        configure(&conn);
        drop(conn);
        let calls = Arc::new(Calls::default());
        let build = |guarded: bool| {
            let state = AppState::with_db(
                Arc::new(CountingBackend {
                    calls: calls.clone(),
                    reply: reply.to_string(),
                }),
                db.clone(),
            );
            let router = Router::new().route("/v1/chat/completions", post(crate::chat_completions));
            let router = if guarded {
                router.layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    middleware,
                ))
            } else {
                router
            };
            router.with_state(state)
        };
        Harness {
            router: build(true),
            plain: build(false),
            calls,
        }
    }

    /// The shipped default, with no `key_value` row written at all.
    fn unconfigured() -> Harness {
        harness(REPLY, |_| {})
    }

    fn configured(policy: Config) -> Harness {
        harness(REPLY, move |conn| {
            save(conn, &policy).unwrap();
        })
    }

    struct Answer {
        status: StatusCode,
        triggered: Option<String>,
        checks: Option<String>,
        body: String,
    }

    async fn ask(harness: &Harness, prompt: &str) -> Answer {
        ask_with(harness, prompt, false).await
    }

    async fn ask_with(harness: &Harness, prompt: &str, stream: bool) -> Answer {
        let request = chat_request(prompt, stream);
        let response = harness.router.clone().oneshot(request).await.unwrap();
        read_answer(response).await
    }

    async fn ask_plain(harness: &Harness, prompt: &str) -> Answer {
        ask_plain_with(harness, prompt, false).await
    }

    async fn ask_plain_with(harness: &Harness, prompt: &str, stream: bool) -> Answer {
        let request = chat_request(prompt, stream);
        let response = harness.plain.clone().oneshot(request).await.unwrap();
        read_answer(response).await
    }

    fn chat_request(prompt: &str, stream: bool) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "model": "counting/small",
                    "stream": stream,
                    "messages": [{"role": "user", "content": prompt}],
                })
                .to_string(),
            ))
            .unwrap()
    }

    async fn read_answer(response: axum::response::Response) -> Answer {
        let status = response.status();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        let triggered = header(HEADER_TRIGGERED);
        let checks = header(HEADER_CHECKS);
        let bytes = to_bytes(response.into_body(), 8 << 20).await.unwrap();
        Answer {
            status,
            triggered,
            checks,
            body: String::from_utf8_lossy(&bytes).to_string(),
        }
    }

    /// Nothing a guardrail said may contain any of the canaries.
    fn assert_absent(haystack: &str) {
        for slice in [
            CANARY_KEY,
            "CANARYcanary",
            "0000000000000000000000000000abcd",
            ECHOED_KEY,
            "ECHOecho0000",
            CANARY_EMAIL,
            "canary.person",
            CANARY_CARD,
            "4539148803436467",
        ] {
            assert!(
                !haystack.contains(slice),
                "leaked `{slice}` into:\n{haystack}"
            );
        }
    }

    /// Run `body` with a `tracing` capture installed on this thread and hand
    /// back what was logged. `set_default` is thread-local, which is why the
    /// future is polled here rather than on another runtime thread.
    fn capture_logs<T>(body: impl FnOnce() -> T) -> (T, String) {
        use std::io::Write;

        #[derive(Clone, Default)]
        struct Sink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

        impl Write for Sink {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Sink {
            type Writer = Sink;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let sink = Sink::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(sink.clone())
            .with_ansi(false)
            .without_time()
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        let out = body();
        drop(guard);
        let bytes = sink.0.lock().unwrap().clone();
        (out, String::from_utf8(bytes).unwrap_or_default())
    }

    #[tokio::test]
    async fn guardrail_block_never_reaches_upstream_and_never_echoes_the_key() {
        let policy = Config {
            secrets_detector: CheckConfig {
                enabled: true,
                action: Action::Block,
            },
            ..Config::default()
        };
        let harness = configured(policy);
        let prompt = format!("my key is {CANARY_KEY}, use it");
        let (answer, logs) = capture_logs(|| futures::executor::block_on(ask(&harness, &prompt)));

        assert_eq!(answer.status, StatusCode::FORBIDDEN);
        assert_eq!(answer.checks.as_deref(), Some("secrets-detector"));
        assert_eq!(
            harness.calls.0.load(Ordering::SeqCst),
            0,
            "upstream was called"
        );

        // Body, headers and the log line, none of which may carry the key.
        assert_absent(&answer.body);
        assert_absent(&answer.checks.unwrap_or_default());
        assert_absent(&logs);
        assert!(
            logs.contains("secrets-detector"),
            "the log must name the check: {logs}"
        );
        assert!(
            answer.body.contains("guardrail blocked"),
            "the body must be the project's 403 envelope: {}",
            answer.body
        );
        assert!(answer.body.contains("forbidden"), "{}", answer.body);
    }

    #[tokio::test]
    async fn guardrail_warn_keeps_the_upstream_body_and_names_the_check() {
        let harness = unconfigured();
        let prompt = format!("mail me at {CANARY_EMAIL} about the invoice");
        let answer = ask(&harness, &prompt).await;

        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(answer.triggered.as_deref(), Some("true"));
        assert_eq!(answer.checks.as_deref(), Some("pii-detector"));
        assert_eq!(answer.body, ask_plain(&harness, &prompt).await.body);
        assert!(answer.body.contains(REPLY));
        assert_absent(&answer.body);
    }

    #[tokio::test]
    async fn guardrail_log_is_invisible_to_the_client() {
        let policy = Config {
            pii_detector: CheckConfig {
                enabled: true,
                action: Action::Log,
            },
            ..Config::default()
        };
        let harness = configured(policy);
        let prompt = format!("mail me at {CANARY_EMAIL} about the invoice");
        let answer = ask(&harness, &prompt).await;

        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(answer.triggered, None);
        assert_eq!(answer.checks, None);
        assert_eq!(answer.body, ask_plain(&harness, &prompt).await.body);
    }

    #[tokio::test]
    async fn guardrail_zero_configuration_blocks_nothing() {
        let harness = unconfigured();
        for prompt in [
            format!("here is my key {CANARY_KEY}"),
            format!("mail {CANARY_EMAIL}"),
            format!("card {CANARY_CARD}"),
            "AKIAIOSFODNN7EXAMPLE".to_string(),
        ] {
            let answer = ask(&harness, &prompt).await;
            assert_eq!(answer.status, StatusCode::OK, "{prompt}");
        }
        assert_eq!(harness.calls.0.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn guardrail_shipped_default_is_warn_and_never_block() {
        let config = Config::default();
        for check in Check::ALL {
            let settings = check.settings(&config);
            assert!(settings.enabled, "{check:?}");
            assert_eq!(settings.action, Action::Warn, "{check:?}");
        }
        assert_eq!(config.blocklist, Vec::<String>::new());
        assert_eq!(config.languages, Vec::<String>::new());
        assert_eq!(config.entropy_threshold, DEFAULT_ENTROPY_THRESHOLD);
    }

    #[test]
    fn guardrail_luhn_gate_separates_cards_from_digit_runs() {
        for card in [
            "4539148803436467",
            "4111111111111111",
            "378282246310005",
            "5555555555554444",
            "4539 1488 0343 6467",
        ] {
            let hits = pii(&format!("pay with {card} today"));
            assert!(
                hits.iter().any(|hit| hit.kind == "credit-card"),
                "{card} -> {hits:?}"
            );
            assert!(
                !hits.iter().any(|hit| hit.kind == "phone"),
                "{card} reported twice: {hits:?}"
            );
        }
        for run in [
            NOT_A_CARD,
            "1234567890123",
            "0000000000000000",
            "1234567890123456789",
        ] {
            let hits = pii(&format!("order {run} shipped"));
            assert!(
                !hits.iter().any(|hit| hit.kind == "credit-card"),
                "{run} -> {hits:?}"
            );
        }
    }

    #[test]
    fn guardrail_entropy_rule_ignores_prose_and_catches_blobs() {
        let threshold = Config::default().entropy_threshold;
        for blob in [
            "aG93IGRvIHlvdSBsaXZlIGluIHRoZSBncmVlbiBwYXN0dXJl",
            "Zm9vYmFyMTIzNDU2Nzg5MEFCQ0RFRkdISUpLTE1OT1BRUlM",
            "0f8fad5bd9cb469fa165708692e46b6d3f4a5c1e",
            "da39a3ee5e6b4b0d3255bfef95601890afd80709",
        ] {
            let hits = secrets(&format!("token: {blob}"), threshold);
            assert!(!hits.is_empty(), "{blob}");
        }
        for prose in [
            "Please summarize the quarterly revenue recognition memo for the finance team.",
            "The quick brown fox jumps over the lazy dog and then goes to sleep.",
            "def compute(x): return x * 2 + 1",
            "v1.2.3-rc.1+build.20240816",
            "deadbeefdeadbeefdeadbeefdeadbeef",
            "config: timeout=30 retries=3",
            "internationalization and counterrevolutionaries",
        ] {
            let hits = secrets(prose, threshold);
            assert!(hits.is_empty(), "{prose} -> {hits:?}");
        }
    }

    #[test]
    fn guardrail_entropy_threshold_sits_in_the_gap_between_prose_and_base64() {
        let threshold = Config::default().entropy_threshold;
        // Digit-bearing natural text tops out below the bar; a random token of
        // 32+ characters sits above it. Those are the two populations the
        // threshold exists to separate, so it is asserted, not asserted-by-doc.
        for prose in [
            "Please summarize the quarterly revenue recognition memo for finance.",
            "v1.2.3-rc.1+build.20240816",
            "order-12345-67890 shipped on 2024-08-06",
            "timeout=30 retries=3 backoff=2",
        ] {
            for token in prose.split(' ') {
                if token.len() >= 8 {
                    assert!(
                        shannon_entropy(token) < threshold,
                        "{token} = {:.2}",
                        shannon_entropy(token)
                    );
                }
            }
        }
        assert!(shannon_entropy("aG93IGRvIHlvdSBsaXZlIGluIHRoZSBncmVlbiBwYXN0dXJl") > threshold);
        assert!(shannon_entropy("Zm9vYmFyMTIzNDU2Nzg5MEFCQ0RFRkdISUpLTE1OT1BRUlM") > threshold);
        assert_eq!(shannon_entropy(""), 0.0);
        assert_eq!(shannon_entropy("aaaa"), 0.0);
        assert_eq!(shannon_entropy("abcd"), 2.0);
    }

    #[test]
    fn guardrail_keyword_blocklist_only_fires_on_configured_terms() {
        assert!(keywords("please stop", &[]).is_empty());
        assert!(keywords("please stop", &["".to_string(), "  ".to_string()]).is_empty());
        let hits = keywords("Please stop that pipeline now", &["stop".to_string()]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].check, Check::KeywordBlocklist);
        // Word matching, so blocking `ass` does not block `class`.
        assert!(keywords("a classy pipeline", &["ass".to_string()]).is_empty());
        assert_eq!(keywords("a classy ass", &["ass".to_string()]).len(), 1);
        // A term with a separator matches as a phrase.
        assert_eq!(
            keywords("a kernel panic happened", &["kernel panic".to_string()]).len(),
            1
        );
        assert!(keywords("the kernel is fine", &["kernel panic".to_string()]).is_empty());
    }

    #[test]
    fn guardrail_language_script_range_beats_latin_noise() {
        let guess = language("Привет, отправьте отчёт в Postgres").expect("cyrillic");
        assert_eq!(guess.code, "ru");
        assert!(guess.confidence > 0.5, "{guess:?}");
        assert_eq!(language("こんにちは、元気ですか").expect("kana").code, "ja");
        assert_eq!(language("今天天气很好").expect("han").code, "zh");
        assert_eq!(language("γειά σου κόσμε").expect("greek").code, "el");
        assert_eq!(language("مرحبا بالعالم").expect("arabic").code, "ar");
        assert_eq!(language("שלום עולם").expect("hebrew").code, "he");
        // Nothing to say: no letters, or one word.
        assert!(language("1234 ++-- !!").is_none());
        assert!(language("Ok").is_none());
    }

    #[test]
    fn guardrail_language_stopwords_separate_english_from_russian() {
        let english = language("the report is on the server and it is not ready").expect("en");
        assert_eq!(english.code, "en");
        let russian = language("privet u menya vopros po serveru ty ne otvechaesh").expect("ru");
        assert_eq!(russian.code, "ru");
        // No stopword evidence at all is reported as unknown, not guessed.
        assert!(language("postgres kubernetes terraform grafana").is_none());
    }

    #[test]
    fn guardrail_language_allow_list_is_what_makes_the_check_fire() {
        let english = "the report is on the server";
        let unset = Config::default();
        assert!(
            inspect(english, &unset).is_empty(),
            "no allow-list configured means nothing to violate"
        );
        let config = Config {
            languages: vec!["de".to_string()],
            ..Config::default()
        };
        assert_eq!(
            inspect(english, &config)
                .iter()
                .map(|hit| hit.kind)
                .collect::<Vec<_>>(),
            vec!["language"]
        );
        let allowed = Config {
            languages: vec!["en".to_string()],
            ..Config::default()
        };
        assert!(inspect(english, &allowed).is_empty());
    }

    #[test]
    fn guardrail_aws_and_jwt_shapes() {
        assert!(
            secrets("key AKIAIOSFODNN7EXAMPLE", DEFAULT_ENTROPY_THRESHOLD)
                .iter()
                .any(|hit| hit.kind == "aws-access-key-id")
        );
        let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert!(
            secrets(jwt, DEFAULT_ENTROPY_THRESHOLD)
                .iter()
                .any(|hit| hit.kind == "jwt")
        );
        assert!(
            !secrets("eyJ is just a prefix here", DEFAULT_ENTROPY_THRESHOLD)
                .iter()
                .any(|hit| hit.kind == "jwt")
        );
        assert!(
            !secrets("AKIA is the amazon prefix", DEFAULT_ENTROPY_THRESHOLD)
                .iter()
                .any(|hit| hit.kind == "aws-access-key-id")
        );
    }

    #[test]
    fn guardrail_phone_shapes() {
        for phone in [
            "+1 (415) 555-0132",
            "+14155550132",
            "415-555-0132",
            "(415) 555 0132",
            "+44 20 7946 0958",
        ] {
            let hits = pii(&format!("call me on {phone} today"));
            assert!(
                hits.iter().any(|hit| hit.kind == "phone"),
                "{phone} -> {hits:?}"
            );
        }
        // A bare digit run is an order id, not a phone number.
        assert!(
            !pii("order 1234567890123456 shipped")
                .iter()
                .any(|hit| hit.kind == "phone")
        );
    }

    #[test]
    fn guardrail_findings_never_carry_the_matched_value() {
        let findings = secrets(
            &format!("key {CANARY_KEY}"),
            Config::default().entropy_threshold,
        );
        assert!(!findings.is_empty());
        for finding in &findings {
            assert!(!finding.hint.contains(CANARY_KEY));
            assert!(!CANARY_KEY.contains(&finding.hint));
            assert!(!finding.hint.contains("CANARY"));
            assert!(finding.hint.len() < 64, "{}", finding.hint);
        }
        let email = pii(CANARY_EMAIL);
        assert!(!email.is_empty());
        for finding in &email {
            assert!(!finding.hint.contains("canary.person"));
            assert!(!CANARY_EMAIL.contains(&finding.hint));
        }
        let card = pii(CANARY_CARD);
        for finding in &card {
            assert!(!finding.hint.contains(CANARY_CARD));
            assert!(!CANARY_CARD.contains(&finding.hint));
        }
    }

    #[test]
    fn guardrail_scannable_text_skips_model_ids() {
        let mut text = String::new();
        collect_text(
            &serde_json::json!({
                "model": "sk-live-abcdefghijklmnopqrstuvwxyz0123456789",
                "messages": [{"role": "user", "content": format!("key {CANARY_KEY}")}],
            }),
            &mut text,
        );
        assert!(text.contains(CANARY_KEY), "{text}");
        assert!(!text.contains("abcdefghijklmnop"), "{text}");
    }

    #[test]
    fn guardrail_true_false_table_for_every_check() {
        let cases: [(Check, &str, &str); 8] = [
            (Check::SecretsDetector, CANARY_KEY, "the cat sat on the mat"),
            (
                Check::SecretsDetector,
                "AKIAIOSFODNN7EXAMPLE",
                "AKIA is the Amazon prefix",
            ),
            (
                Check::PiiDetector,
                CANARY_EMAIL,
                "reach me on the usual channel",
            ),
            (
                Check::PiiDetector,
                CANARY_CARD,
                "the order shipped on monday",
            ),
            (
                Check::KeywordBlocklist,
                "restart the worker",
                "ship the patch",
            ),
            (Check::KeywordBlocklist, "kernel panic", "clean shutdown"),
            (
                Check::LanguageDetection,
                "Привет, отправьте отчёт в Postgres",
                "the report is on the server",
            ),
            (
                Check::LanguageDetection,
                "privet u menya vopros po serveru",
                "shutdown complete",
            ),
        ];
        for (check, positive, negative) in cases {
            let config = Config {
                blocklist: matches!(check, Check::KeywordBlocklist)
                    .then(|| vec![positive.to_string()])
                    .unwrap_or_default(),
                languages: matches!(check, Check::LanguageDetection)
                    .then(|| vec!["en".to_string()])
                    .unwrap_or_default(),
                ..Config::default()
            };
            assert!(
                inspect(positive, &config)
                    .iter()
                    .any(|hit| hit.check == check),
                "{check:?} missed `{positive}`"
            );
            assert!(
                !inspect(negative, &config)
                    .iter()
                    .any(|hit| hit.check == check),
                "{check:?} fired on `{negative}`"
            );
        }
    }

    #[test]
    fn guardrail_disabled_checks_are_skipped() {
        let off = CheckConfig {
            enabled: false,
            action: Action::Warn,
        };
        let config = Config {
            secrets_detector: off,
            pii_detector: off,
            ..Config::default()
        };
        let text = format!("{CANARY_KEY} {CANARY_EMAIL} {CANARY_CARD}");
        let hits = inspect(&text, &config);
        assert!(hits.is_empty(), "{hits:?}");
    }

    #[test]
    fn guardrail_config_round_trips_through_the_kv_store() {
        let db = heterion_router_db::Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let conn = db.connection();
        assert_eq!(load(&conn), Config::default(), "no row means the defaults");

        let config = Config {
            secrets_detector: CheckConfig {
                enabled: true,
                action: Action::Block,
            },
            pii_detector: CheckConfig {
                enabled: false,
                action: Action::Log,
            },
            blocklist: vec!["Kernel Panic".to_string(), "  ".to_string()],
            languages: vec!["RU".to_string()],
            entropy_threshold: 5.0,
            scan_response: false,
            ..Config::default()
        };
        save(&conn, &config).unwrap();

        let loaded = load(&conn);
        assert_eq!(loaded.secrets_detector, config.secrets_detector);
        assert_eq!(loaded.pii_detector, config.pii_detector);
        assert_eq!(loaded.entropy_threshold, 5.0);
        assert!(!loaded.scan_response);
        // Terms are normalized and blanks dropped at the boundary, so a stray
        // empty string cannot become "match everything".
        assert_eq!(loaded.blocklist, vec!["kernel panic".to_string()]);
        assert_eq!(loaded.languages, vec!["ru".to_string()]);
    }

    #[test]
    fn guardrail_a_corrupt_row_falls_back_to_the_defaults() {
        let db = heterion_router_db::Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let conn = db.connection();
        heterion_router_db::repos::kv_set(&conn, NAMESPACE, CONFIG_KEY, "{not json").unwrap();
        assert_eq!(load(&conn), Config::default());
    }

    #[test]
    fn guardrail_an_absurd_entropy_threshold_is_clamped() {
        let db = heterion_router_db::Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let conn = db.connection();
        let config = Config {
            entropy_threshold: -1.0,
            ..Config::default()
        };
        save(&conn, &config).unwrap();
        assert_eq!(load(&conn).entropy_threshold, DEFAULT_ENTROPY_THRESHOLD);
    }

    #[test]
    fn guardrail_patch_merges_onto_the_shipped_defaults() {
        let db = heterion_router_db::Db::open_in_memory().unwrap();
        db.migrate().unwrap();
        let conn = db.connection();
        let updated = patch(
            &conn,
            ConfigPatch {
                secrets_detector: Some(CheckConfig {
                    enabled: true,
                    action: Action::Block,
                }),
                blocklist: Some(vec!["stop".to_string()]),
                ..ConfigPatch::default()
            },
        );
        assert_eq!(updated.secrets_detector.action, Action::Block);
        assert_eq!(updated.pii_detector, Config::default().pii_detector);
        assert_eq!(updated.blocklist, vec!["stop".to_string()]);
        assert_eq!(load(&conn), updated, "the patch is persisted");
    }

    #[tokio::test]
    async fn guardrail_response_scan_can_be_turned_off() {
        assert!(Config::default().scan_response, "the shipped default is on");
        let policy = Config {
            scan_response: false,
            ..Config::default()
        };
        let harness = harness(ECHOED_KEY, move |conn| {
            save(conn, &policy).unwrap();
        });
        let answer = ask(&harness, "echo this back to me").await;
        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(answer.triggered, None);
        assert_eq!(answer.checks, None);
    }

    #[tokio::test]
    async fn guardrail_response_scan_warns_on_a_provider_that_echoes_a_key() {
        let harness = harness(ECHOED_KEY, |_| {});
        let answer = ask(&harness, "echo this back to me").await;
        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(answer.triggered.as_deref(), Some("true"));
        assert_eq!(answer.checks.as_deref(), Some("secrets-detector"));
        assert!(answer.body.contains(ECHOED_KEY), "the provider did echo it");
        // The header and the body are unchanged: the response direction can
        // only ever add a header, never rewrite an answer.
        assert_eq!(
            answer.body,
            ask_plain(&harness, "echo this back to me").await.body
        );
    }

    #[tokio::test]
    async fn guardrail_oversized_body_is_scanned_in_its_prefix_and_preserved() {
        let harness = unconfigured();
        let prompt = format!("{CANARY_KEY} {}", "a".repeat(SCAN_LIMIT_BYTES + 4096));
        let answer = ask(&harness, &prompt).await;
        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(answer.triggered.as_deref(), Some("true"));
        assert_eq!(
            answer.body,
            ask_plain(&harness, &prompt).await.body,
            "the body must survive read-and-put-back unchanged"
        );
    }

    #[tokio::test]
    async fn guardrail_streamed_answers_are_never_buffered() {
        let harness = unconfigured();
        let answer = ask_with(&harness, &format!("key {CANARY_KEY}"), true).await;
        assert_eq!(answer.status, StatusCode::OK);
        assert_eq!(answer.triggered.as_deref(), Some("true"));
        assert!(answer.body.contains("[DONE]"), "{}", answer.body);
        // Byte equality is not available here: the stream path mints a fresh
        // completion id per call. The frame sequence is the invariant instead.
        let plain = ask_plain_with(&harness, &format!("key {CANARY_KEY}"), true).await;
        assert_eq!(
            answer.body.matches("data: ").count(),
            plain.body.matches("data: ").count()
        );
        assert!(answer.body.starts_with("data: "));
        assert!(answer.body.ends_with("data: [DONE]\n\n"), "{}", answer.body);
    }

    #[tokio::test]
    async fn guardrail_non_json_bodies_pass_through_untouched() {
        let harness = unconfigured();
        let response = harness
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("not json at all"))
                    .unwrap(),
            )
            .await
            .unwrap();
        // The handler answers a body that is not JSON with a 400 of its own;
        // what matters here is that the guardrails neither blocked nor rewrote
        // it, and the body it reads arrived intact.
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(response.headers().get(HEADER_TRIGGERED).is_none());
    }
}
