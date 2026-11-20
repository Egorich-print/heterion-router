//! Weighted spread: the traffic of one class does not always start at the
//! same model.
//!
//! A combo — and the `auto` ladder — holds several models that can serve the
//! same request. When the plan always starts at the same entry, that model
//! burns its quota while its siblings stand idle. The spread draws the head
//! of the plan with a probability proportional to the weights the author set,
//! so the traffic is shared without changing *which* models may serve.
//!
//! Everything here is deterministic: the same seed and the same items always
//! give the same permutation. That is what makes one request reproducible —
//! its failover retries walk the same order — while the next request gets a
//! new one.

use std::sync::atomic::{AtomicU64, Ordering};

use heterion_router_core::ChatCompletionRequest;

/// The counter that keeps two identical requests apart.
///
/// The seed of a request must differ from the seed of the next one, or every
/// request would walk the same order and the spread would spread nothing.
static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// One item with the weight it should be picked by.
#[derive(Debug, Clone)]
pub struct Weighted<T> {
    /// The thing that is ordered.
    pub value: T,
    /// How much of the traffic it should take.
    ///
    /// Zero means "not in the head": the item stays available as a fallback.
    pub weight: f64,
}

impl<T> Weighted<T> {
    /// An item with the weight it should be picked by.
    pub fn new(value: T, weight: f64) -> Self {
        Self { value, weight }
    }
}

/// A tiny deterministic generator (SplitMix64).
///
/// The router needs randomness it can reproduce — a request that is retried
/// must walk the same order — so it carries its own generator instead of
/// pulling in a dependency.
#[derive(Debug, Clone)]
pub struct Rng {
    /// The state of the generator.
    state: u64,
}

impl Rng {
    /// A generator that starts at `seed`.
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in `(0, 1]`.
    ///
    /// Never zero on purpose: a weighted key of zero is how "this item has no
    /// weight" is spelled, and a random zero would fake it.
    pub fn next_unit(&mut self) -> f64 {
        1.0 - (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Shuffle `items` so that the first one is drawn with a probability
/// proportional to its weight.
///
/// The algorithm is Efraimidis–Spirakis: every item gets the key
/// `u^(1/w)` and the keys are sorted descending. That makes the head
/// proportional to the weights while the rest of the order stays a
/// permutation of the input.
///
/// Items with weight zero get the key zero and therefore never come first
/// as long as one item carries a weight. A list where nothing carries a
/// weight has no opinion to spread by and comes back unchanged.
pub fn weighted_shuffle<T: Clone>(items: &[Weighted<T>], seed: u64) -> Vec<T> {
    let mut rng = Rng::new(seed);
    let mut keyed: Vec<(f64, usize, T)> = items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let key = if item.weight > 0.0 {
                rng.next_unit().powf(1.0 / item.weight)
            } else {
                0.0
            };
            (key, index, item.value.clone())
        })
        .collect();
    // Descending by key; the index keeps the order of equal keys stable.
    keyed.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.cmp(&b.1))
    });
    keyed.into_iter().map(|(_, _, value)| value).collect()
}

/// Spread the entries of a combo by the weight the author gave them.
///
/// Entries with weight zero are fallbacks: they keep their listed order but
/// move behind the entries that carry the traffic, so a zero-weight entry is
/// never the head of the plan. A combo where no entry carries a weight has
/// no opinion to spread by and keeps its listed order — that is what a combo
/// written before the weights existed looks like.
pub fn spread_by_weight<T: Clone>(items: &[Weighted<T>], seed: u64) -> Vec<T> {
    let traffic: Vec<Weighted<T>> = items
        .iter()
        .filter(|item| item.weight > 0.0)
        .cloned()
        .collect();
    if traffic.is_empty() {
        return items.iter().map(|item| item.value.clone()).collect();
    }
    let mut spread: Vec<T> = weighted_shuffle(&traffic, seed);
    spread.extend(
        items
            .iter()
            .filter(|item| item.weight <= 0.0)
            .map(|item| item.value.clone()),
    );
    spread
}

/// The seed one request is spread by.
///
/// A client that sends a seed gets the order it asked for: the same seed
/// always gives the same plan. Without one the router derives the seed from
/// the request itself and from a process-wide counter, so the failover
/// retries of one request keep the same order while the next request gets a
/// new one.
pub fn request_seed(request: &ChatCompletionRequest) -> u64 {
    if let Some(seed) = request.seed {
        return seed as u64;
    }
    let counter = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    mix(counter, fingerprint(request))
}

/// What the request *is*, without its bytes.
///
/// The seed has to depend on the request, or two different requests would
/// walk the same order; it has to stay cheap, because it is computed on the
/// way to every plan.
fn fingerprint(request: &ChatCompletionRequest) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    eat(request.model.as_bytes());
    for message in &request.messages {
        eat(message.role.as_bytes());
        if let Some(content) = &message.content {
            eat(content.to_string().as_bytes());
        }
    }
    hash
}

/// Mix the counter and the fingerprint into one seed.
fn mix(counter: u64, fingerprint: u64) -> u64 {
    let mut z = counter.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ fingerprint.wrapping_add(0xD1B5_4A32_D192_ED03);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weighted(values: &[(&str, f64)]) -> Vec<Weighted<String>> {
        values
            .iter()
            .map(|(value, weight)| Weighted::new((*value).to_string(), *weight))
            .collect()
    }

    #[test]
    fn the_same_seed_gives_the_same_permutation() {
        let items = weighted(&[("a", 1.0), ("b", 2.0), ("c", 3.0)]);
        let first = weighted_shuffle(&items, 42);
        let second = weighted_shuffle(&items, 42);
        assert_eq!(first, second, "the same seed must replay the same order");
        assert_ne!(
            first,
            weighted_shuffle(&items, 43),
            "another seed must be able to give another order"
        );
    }

    #[test]
    fn the_head_follows_the_weights() {
        // Three times the weight means three times the chance to be first.
        let items = weighted(&[("a", 1.0), ("b", 3.0)]);
        let mut first_a = 0usize;
        let samples = 20_000;
        for seed in 0..samples {
            let order = weighted_shuffle(&items, seed);
            if order[0] == "a" {
                first_a += 1;
            }
        }
        let share = first_a as f64 / samples as f64;
        assert!(
            (0.22..0.28).contains(&share),
            "the light item must lead about a quarter of the time, got {share}"
        );
    }

    #[test]
    fn a_zero_weight_item_is_never_first() {
        let items = weighted(&[("a", 0.0), ("b", 1.0), ("c", 2.0)]);
        for seed in 0..500 {
            let order = weighted_shuffle(&items, seed);
            assert_ne!(
                order[0], "a",
                "an item without weight must not take the head: {order:?}"
            );
        }
    }

    #[test]
    fn a_list_without_any_weight_keeps_its_order() {
        let items = weighted(&[("a", 0.0), ("b", 0.0)]);
        assert_eq!(
            weighted_shuffle(&items, 7),
            vec!["a".to_string(), "b".to_string()],
            "without a weight there is no opinion to spread by"
        );
    }

    #[test]
    fn one_item_stays_put() {
        let items = weighted(&[("only", 5.0)]);
        assert_eq!(
            weighted_shuffle(&items, 3),
            vec!["only".to_string()],
            "a single candidate has nothing to swap with"
        );
    }

    #[test]
    fn an_empty_list_is_handled() {
        let items: Vec<Weighted<String>> = Vec::new();
        assert!(weighted_shuffle(&items, 1).is_empty());
        assert!(spread_by_weight(&items, 1).is_empty());
    }

    #[test]
    fn fallbacks_go_behind_the_traffic() {
        let items = weighted(&[("fallback", 0.0), ("light", 1.0), ("heavy", 3.0)]);
        for seed in 0..200 {
            let order = spread_by_weight(&items, seed);
            assert_ne!(
                order[0], "fallback",
                "an entry without weight must not take the head: {order:?}"
            );
            assert_eq!(
                order.last().map(String::as_str),
                Some("fallback"),
                "a fallback stays available at the back: {order:?}"
            );
        }
    }

    #[test]
    fn a_combo_without_weights_keeps_its_order() {
        let items = weighted(&[("a", 0.0), ("b", 0.0), ("c", 0.0)]);
        assert_eq!(
            spread_by_weight(&items, 11),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn the_client_seed_wins() {
        let request = ChatCompletionRequest {
            model: "combo".into(),
            seed: Some(1234),
            ..Default::default()
        };
        assert_eq!(request_seed(&request), 1234);
    }

    #[test]
    fn two_requests_get_two_seeds() {
        let request = ChatCompletionRequest {
            model: "combo".into(),
            messages: vec![heterion_router_core::ChatMessage::plain("user", "hi")],
            ..Default::default()
        };
        assert_ne!(
            request_seed(&request),
            request_seed(&request),
            "the next request must not walk the order of the previous one"
        );
    }
}
