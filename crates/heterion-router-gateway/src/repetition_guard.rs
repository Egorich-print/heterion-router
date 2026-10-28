//! Repetition guard: stop a model that has stopped saying anything new.
//!
//! A model that degenerates repeats the same words until it runs into the
//! token cap. Measured on the agent transcripts of 2026-10-26: Mistral
//! Codestral did it in 3 of 7 sessions — a 110 KB answer that printed
//! `(clippy::unused_variables)` 4090 times, or `(os error 2)` hundreds of
//! times, always ending at exactly the 32768-token cap with
//! `finish_reason: length`. Nothing upstream produced that text: the tool
//! output it "quoted" contained the lint name zero times.
//!
//! Such an answer is not a long answer, it is a loop with a prefix. Left
//! alone it costs the full token budget, fills the agent's context with
//! junk, and — because the runtime then asks the model to continue the
//! truncated turn — invites a second, identical answer.
//!
//! The guard watches the text a model produces and answers with a number,
//! not a guess: the share of eight-word windows inside a trailing window
//! that have already been seen. Healthy prose scores 0.0; a degenerate
//! loop scores above 0.99. The cut keeps the text before the loop and
//! reports the answer as truncated, because that is what it now is.

use std::collections::HashMap;

use heterion_router_core::StreamChunk;

/// A window of words wide enough that a genuine paraphrase never repeats.
const NGRAM: usize = 8;

/// How many trailing words the guard looks at.
const WINDOW_WORDS: usize = 256;

/// Share of the window's windows that must be repeats before the text is
/// called a loop. Measured answers score 0.0; degenerate ones above 0.99.
const REPEAT_RATIO: f64 = 0.5;

/// How often the most repeated window must occur. A code listing may repeat
/// a line twice; a loop repeats it hundreds of times.
const MIN_REPEATS: usize = 4;

/// How many different words the window may hold. This is what separates a
/// degenerate answer from a structured repetition: a model that has stopped
/// thinking repeats a handful of words forever, while a model that quotes a
/// list the prompt itself repeated uses twenty or more distinct words — and
/// that answer is not broken, only repetitive.
const MAX_DISTINCT_WORDS: usize = 16;

/// How long an answer must be before a *structured* repetition counts as a
/// loop. A model that repeats a list of twenty names three times is quoting,
/// not failing; the same model repeating that list for twenty thousand words
/// is failing. Measured on the transcripts of 2026-10-26: the rustc
/// error-kind loop ran 23 867 words with 37 distinct words per window, and
/// the harmless echo of a repeated prompt ran 400.
const LARGE_ANSWER_WORDS: usize = 2000;

/// What the guard found.
#[derive(Debug, Clone, PartialEq)]
pub struct RepetitionHit {
    /// What looped: the visible answer or the reasoning block.
    pub source: &'static str,
    /// Share of the trailing window's eight-word windows that were repeats.
    pub repeat_ratio: f64,
    /// How often the most repeated window occurred.
    pub repeats: usize,
    /// Word index where the loop starts — the text before it is kept.
    pub cut_word: usize,
    /// Word count of the whole answer.
    pub total_words: usize,
}

/// The trailing window of a text, as words.
fn tail_window(text: &str) -> Vec<&str> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() > WINDOW_WORDS {
        words[words.len() - WINDOW_WORDS..].to_vec()
    } else {
        words
    }
}

/// Share of the window's eight-word windows that already occurred earlier,
/// plus how often the most repeated window occurred.
fn window_repetition(words: &[&str]) -> (f64, usize, usize) {
    if words.len() < NGRAM {
        return (0.0, 0, words.len());
    }
    let mut seen: HashMap<&[&str], usize> = HashMap::new();
    let mut counts: HashMap<&[&str], usize> = HashMap::new();
    let mut repeats = 0usize;
    let mut total = 0usize;
    for window in words.windows(NGRAM) {
        total += 1;
        if seen.contains_key(window) {
            repeats += 1;
        } else {
            seen.insert(window, total);
        }
        *counts.entry(window).or_insert(0) += 1;
    }
    let best = counts.values().copied().max().unwrap_or(0);
    let distinct = words.iter().collect::<std::collections::HashSet<_>>().len();
    if total == 0 {
        return (0.0, best, distinct);
    }
    (repeats as f64 / total as f64, best, distinct)
}

/// Where the loop starts: the first occurrence of the most repeated
/// eight-word window.
fn loop_start(words: &[&str]) -> usize {
    let mut counts: HashMap<&[&str], usize> = HashMap::new();
    let mut best: Option<(&[&str], usize)> = None;
    for window in words.windows(NGRAM) {
        let count = counts.entry(window).or_insert(0);
        *count += 1;
        if best.is_none_or(|(_, best_count)| *count > best_count) {
            best = Some((window, *count));
        }
    }
    let Some((window, _)) = best else {
        return 0;
    };
    words
        .windows(NGRAM)
        .position(|candidate| candidate == window)
        .unwrap_or(0)
}

/// Is this repetition a loop?
///
/// Two families, two rules. A model that stopped thinking repeats a handful
/// of words: few distinct words in the window, and the answer is cut at once.
/// A model that repeats a *list* keeps many distinct words; that is only a
/// loop once the answer has grown past every reasonable length.
fn is_loop(repeat_ratio: f64, repeats: usize, distinct: usize, total_words: usize) -> bool {
    if repeat_ratio < REPEAT_RATIO || repeats < MIN_REPEATS {
        return false;
    }
    distinct <= MAX_DISTINCT_WORDS || total_words >= LARGE_ANSWER_WORDS
}

/// Analyse a complete answer. `None` when the tail is not a loop.
pub fn analyze(text: &str) -> Option<RepetitionHit> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    let window = tail_window(text);
    let (repeat_ratio, repeats, distinct) = window_repetition(&window);
    if !is_loop(repeat_ratio, repeats, distinct, words.len()) {
        return None;
    }
    Some(RepetitionHit {
        source: "content",
        repeat_ratio,
        repeats,
        cut_word: loop_start(&words),
        total_words: words.len(),
    })
}

/// Cut a looping answer to the text before the loop.
pub fn trim(text: &str) -> Option<(String, RepetitionHit)> {
    let hit = analyze(text)?;
    let words: Vec<&str> = text.split_whitespace().collect();
    let cut = hit.cut_word.min(words.len());
    let kept = words[..cut].join(" ");
    Some((kept, hit))
}

/// Streaming counterpart of [`analyze`]: the guard is fed the text as it
/// arrives and reports the moment the answer turns into a loop.
#[derive(Debug, Default)]
pub struct StreamGuard {
    words: Vec<String>,
    triggered: bool,
}

impl StreamGuard {
    /// A guard that has seen nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed the next piece of text. `Some` once the text is a loop.
    pub fn push(&mut self, text: &str) -> Option<RepetitionHit> {
        if self.triggered {
            return None;
        }
        for word in text.split_whitespace() {
            self.words.push(word.to_string());
        }
        if self.words.len() < NGRAM {
            return None;
        }
        let start = self.words.len().saturating_sub(WINDOW_WORDS);
        let window: Vec<&str> = self.words[start..].iter().map(String::as_str).collect();
        let (repeat_ratio, repeats, distinct) = window_repetition(&window);
        if !is_loop(repeat_ratio, repeats, distinct, self.words.len()) {
            return None;
        }
        self.triggered = true;
        let all: Vec<&str> = self.words.iter().map(String::as_str).collect();
        Some(RepetitionHit {
            source: "content",
            repeat_ratio,
            repeats,
            cut_word: loop_start(&all),
            total_words: all.len(),
        })
    }
}

/// The terminal chunk a guarded stream ends with.
pub fn truncated_chunk() -> StreamChunk {
    StreamChunk::done("length")
}

/// The visible text of a message content value: a plain string, or the
/// concatenation of the text parts of a multi-part content.
fn content_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter(|part| part.get("type").and_then(serde_json::Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// Replace a looping message content with the text that came before the
/// loop. Multi-part content keeps its non-text parts — an image must not
/// disappear because the prose around it looped.
fn set_trimmed_content(content: &mut serde_json::Value, trimmed: &str) {
    match content {
        serde_json::Value::String(text) => *text = trimmed.to_string(),
        serde_json::Value::Array(parts) => {
            let mut written = false;
            for part in parts.iter_mut() {
                if part.get("type").and_then(serde_json::Value::as_str) != Some("text") {
                    continue;
                }
                if written {
                    part["text"] = serde_json::Value::String(String::new());
                } else {
                    part["text"] = serde_json::Value::String(trimmed.to_string());
                    written = true;
                }
            }
        }
        _ => {}
    }
}

/// Cut a looping answer out of a finished response.
///
/// The answer keeps the text before the loop and is reported as truncated,
/// because that is what it now is. Returns what was cut, for the log.
pub fn apply_to_response(
    response: &mut heterion_router_core::ChatCompletionResponse,
) -> Option<RepetitionHit> {
    let mut found: Option<RepetitionHit> = None;
    for choice in &mut response.choices {
        if let Some(content) = choice.message.content.as_mut() {
            let text = content_text(content);
            if let Some((trimmed, hit)) = trim(&text) {
                set_trimmed_content(content, &trimmed);
                choice.finish_reason = "length".to_string();
                found = Some(hit);
            }
        }

        if let Some(reasoning) = choice.message.reasoning_content.as_mut()
            && let Some((trimmed, mut hit)) = trim(reasoning)
        {
            hit.source = "reasoning";
            *reasoning = trimmed;
            found = found.or(Some(hit));
        }
    }
    found
}

/// Watch a stream and stop it when the answer turns into a loop.
///
/// The stream ends with a `length` chunk: the answer is truncated, and the
/// client must not read the loop as a finished thought. Stopping here also
/// stops the upstream — the tokens it would have spent on the rest of the
/// loop are never billed.
pub fn guard_stream(
    stream: crate::backend::ChunkStream,
    model: &str,
) -> crate::backend::ChunkStream {
    use futures::StreamExt;

    let model = model.to_string();

    struct Guarded {
        stream: crate::backend::ChunkStream,
        content: StreamGuard,
        reasoning: StreamGuard,
        terminal: Option<StreamChunk>,
        finished: bool,
    }

    let state = Guarded {
        stream,
        content: StreamGuard::new(),
        reasoning: StreamGuard::new(),
        terminal: None,
        finished: false,
    };

    Box::pin(futures::stream::unfold(state, move |mut state| {
        // The model name is only used in the log line; a clone per item keeps
        // the future free of a borrow of the closure's environment.
        let model = model.clone();
        async move {
            loop {
                // The terminal chunk is the last one: the upstream must not
                // keep talking after the answer was cut.
                if state.finished {
                    return None;
                }
                if let Some(chunk) = state.terminal.take() {
                    state.finished = true;
                    return Some((Ok(chunk), state));
                }
                let chunk = match state.stream.next().await {
                    Some(Ok(chunk)) => chunk,
                    Some(Err(error)) => return Some((Err(error), state)),
                    None => return None,
                };
                let mut hit: Option<RepetitionHit> = None;
                if let Some(text) = &chunk.content {
                    hit = state.content.push(text);
                }
                if hit.is_none()
                    && let Some(text) = &chunk.reasoning
                {
                    hit = state.reasoning.push(text).map(|mut found| {
                        found.source = "reasoning";
                        found
                    });
                }
                match hit {
                    Some(hit) => {
                        tracing::warn!(
                            "repetition guard model={model} source={} ratio={} repeats={} words={} — answer truncated",
                            hit.source,
                            hit.repeat_ratio,
                            hit.repeats,
                            hit.total_words
                        );
                        // The loop is here: the answer ends, truncated.
                        state.terminal = Some(truncated_chunk());
                    }
                    None => return Some((Ok(chunk), state)),
                }
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit() -> String {
        "(clippy::unused_variables) ".repeat(200)
    }

    #[test]
    fn degenerate_loop_is_detected() {
        let text = format!("warning: unused variable here. {}", unit());
        let hit = analyze(&text).expect("a loop must be detected");
        assert!(hit.repeat_ratio > 0.9, "ratio was {}", hit.repeat_ratio);
        assert!(hit.repeats > 100, "repeats were {}", hit.repeats);
    }

    #[test]
    fn healthy_answer_is_not_detected() {
        let text = "Rust owns its memory. \
            The borrow checker rejects data races at compile time. \
            Cargo builds and publishes packages. \
            Errors are values, so they compose. \
            The standard library avoids unsafe code.";
        assert!(analyze(text).is_none());
    }

    #[test]
    fn a_repeated_paragraph_twice_is_not_a_loop() {
        let paragraph = "The router forwards the request to the provider and books the usage row.";
        let text = format!("{paragraph} {paragraph}");
        assert!(analyze(&text).is_none());
    }

    #[test]
    fn code_listing_with_repeated_lines_is_not_a_loop() {
        let mut text = String::new();
        for index in 0..40 {
            text.push_str(&format!(
                "    let value_{index} = load_configuration_entry(\"section_{index}\", \"key_{index}\")?;\n"
            ));
        }
        assert!(analyze(&text).is_none());
    }

    #[test]
    fn trim_keeps_the_text_before_the_loop() {
        let text = format!(
            "warning: unused variable: `cargo_cmd` at host-test.sh:32:5. {}",
            unit()
        );
        let (kept, hit) = trim(&text).expect("a loop must be trimmed");
        assert!(
            kept.starts_with("warning: unused variable"),
            "kept was {kept}"
        );
        assert!(kept.len() < 200, "kept {} bytes", kept.len());
        assert!(hit.cut_word > 0);
    }

    #[test]
    fn a_long_list_repeated_by_the_prompt_is_not_a_loop() {
        // The prompt asks for a list of twenty names and repeats it three
        // times; a model that echoes it back faithfully is repetitive, not
        // degenerate.
        let mut names: Vec<&str> = Vec::new();
        for index in 0..20 {
            names.push(format!("clippy_lint_number_{index}").leak());
        }
        let list = names.join(" ");
        let text = format!("{list} {list} {list}");
        assert!(analyze(&text).is_none());
    }

    #[test]
    fn a_long_list_repeated_until_the_answer_is_huge_is_a_loop() {
        // The same list, repeated until the answer runs to thousands of
        // words: now it is a loop, and the guard cuts it.
        let mut names: Vec<&str> = Vec::new();
        for index in 0..20 {
            names.push(format!("clippy_lint_number_{index}").leak());
        }
        let list = names.join(" ");
        let mut text = String::new();
        while text.split_whitespace().count() < 2500 {
            text.push_str(&list);
            text.push(' ');
        }
        let hit = analyze(&text).expect("a huge repeated list is a loop");
        assert!(hit.total_words >= LARGE_ANSWER_WORDS);
    }

    #[test]
    fn stream_guard_stays_quiet_until_the_loop_grows() {
        let mut guard = StreamGuard::new();
        assert!(guard.push("The answer begins normally. ").is_none());
        for _ in 0..40 {
            if guard.push("(clippy::unused_variables) ").is_some() {
                return;
            }
        }
        panic!("the stream guard never reported the loop");
    }

    #[test]
    fn stream_guard_never_fires_on_healthy_text() {
        let mut guard = StreamGuard::new();
        for index in 0..200 {
            assert!(
                guard
                    .push(&format!(
                        "Fact number {index} about Rust and its type system. "
                    ))
                    .is_none()
            );
        }
    }
}
