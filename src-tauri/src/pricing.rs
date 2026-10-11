// Token pricing. OpenAI-published prices are the first source for OpenAI/Codex
// model ids. Fallbacks then fill gaps from models.dev, LiteLLM, and a bundled
// LiteLLM snapshot.
//
// Matching is layered: exact id → normalized id (strip provider path prefix +
// unify the ".'↔'p" version separator, e.g. "glm-5.1" ⇄ "glm-5p1").
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, SystemTime};

// Process-wide memoized price table. Loaded once off the main thread (see
// reload_shared) and refreshed every 24h, so build_dashboard — which holds
// BUILD_LOCK — only ever does a cheap Arc clone, never JSON parsing or network.
static PRICING: OnceLock<RwLock<Arc<Pricing>>> = OnceLock::new();

const MODELSDEV_URL: &str = "https://models.dev/api.json";
const LITELLM_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60); // 24h
                                                             // Bundled LiteLLM price table snapshot — offline fallback so a first launch
                                                             // with no network (and no prior cache) still prices the common third-party
                                                             // models, not just the few hardcoded in `ingest_builtin`. Live sources, when
                                                             // reachable, are ingested first and win.
const LITELLM_SNAPSHOT: &str = include_str!("../snapshots/litellm.json");

#[derive(Clone, Default)]
pub struct ModelPrice {
    pub input: f64,        // per-token USD
    pub output: f64,       // per-token USD
    pub cache_create: f64, // per-token USD
    pub cache_read: f64,   // per-token USD
    // Some models charge a higher rate when the complete input context for a
    // request crosses a threshold. Zero means the model has no tiered rate.
    pub long_context_threshold: f64,
    pub long_input: f64,
    pub long_output: f64,
    pub long_cache_create: f64,
    pub long_cache_read: f64,
}

impl ModelPrice {
    fn is_zero(&self) -> bool {
        self.input == 0.0
            && self.output == 0.0
            && self.cache_create == 0.0
            && self.cache_read == 0.0
    }
}

pub struct Pricing {
    exact: HashMap<String, ModelPrice>,
    norm: HashMap<String, ModelPrice>,
}

/// Strip provider path prefix (after last '/') and unify version separators
/// so "z-ai/glm-5.1", "glm-5p1" and "glm-5.1" all collapse to one key.
fn normalize_key(s: &str) -> String {
    let base = s.rsplit('/').next().unwrap_or(s);
    base.to_lowercase().replace('.', "p")
}

fn bare(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

fn cache_dir() -> Option<PathBuf> {
    let dir = dirs::cache_dir()?.join("codexscope");
    let _ = fs::create_dir_all(&dir);
    Some(dir)
}

/// A models.dev payload: at least one provider with a non-empty `models` map.
fn valid_modelsdev(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| {
            v.as_object().map(|root| {
                root.values().any(|p| {
                    p.get("models")
                        .and_then(|m| m.as_object())
                        .map(|m| !m.is_empty())
                        .unwrap_or(false)
                })
            })
        })
        .unwrap_or(false)
}

/// A LiteLLM payload: at least one entry carrying a per-token cost field.
fn valid_litellm(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| {
            v.as_object().map(|root| {
                root.values().filter_map(|m| m.as_object()).any(|m| {
                    m.contains_key("input_cost_per_token")
                        || m.contains_key("output_cost_per_token")
                })
            })
        })
        .unwrap_or(false)
}

/// Read a fresh (<24h) cache for `name`, else fetch `url` & cache it, else fall
/// back to any stale cache. Returns the raw JSON text. `valid` gates what gets
/// written to the cache: a 200 carrying a JSON error envelope (CDN/proxy/rate
/// limit) would otherwise poison the cache for 24h with zero usable prices, so
/// we only persist a body that actually parses as a price table — and keep the
/// previous good cache otherwise.
fn fetch_cached(name: &str, url: &str, valid: impl Fn(&str) -> bool) -> Option<String> {
    let path = cache_dir()?.join(format!("{name}.json"));
    if let Ok(meta) = fs::metadata(&path) {
        let fresh = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .map(|age| age < MAX_AGE)
            .unwrap_or(false);
        if fresh {
            if let Ok(t) = fs::read_to_string(&path) {
                return Some(t);
            }
        }
    }
    // fetch fresh — only overwrite the cache if the body validates as a table
    if let Ok(resp) = ureq::get(url).timeout(Duration::from_secs(10)).call() {
        if let Ok(text) = resp.into_string() {
            if valid(&text) {
                let _ = fs::write(&path, &text);
                return Some(text);
            }
        }
    }
    // stale cache as last resort
    fs::read_to_string(&path).ok()
}

impl Pricing {
    pub fn load() -> Self {
        let mut p = Pricing {
            exact: HashMap::new(),
            norm: HashMap::new(),
        };
        // 1. OpenAI official pricing snapshot - inserted first, so it wins on
        //    conflict for OpenAI/Codex model ids.
        p.ingest_openai_official();
        // 2. models.dev - fills gaps the official snapshot doesn't cover.
        if let Some(text) = fetch_cached("modelsdev", MODELSDEV_URL, valid_modelsdev) {
            p.ingest_modelsdev(&text);
        }
        // 3. LiteLLM - fills gaps models.dev doesn't cover.
        if let Some(text) = fetch_cached("litellm", LITELLM_URL, valid_litellm) {
            p.ingest_litellm(&text);
        }
        // 4. bundled LiteLLM snapshot - offline fallback for anything the live
        //    sources didn't supply (only fills gaps; live prices already won).
        p.ingest_litellm(LITELLM_SNAPSHOT);
        // 5. legacy non-OpenAI backstop (last resort).
        p.ingest_legacy_fallbacks();
        p
    }

    /// Just the built-in snapshot — no disk, no network. Returned by `shared()`
    /// before the background loader has run, so common bundled models still
    /// price during the first moments after launch.
    fn builtin_only() -> Self {
        let mut p = Pricing {
            exact: HashMap::new(),
            norm: HashMap::new(),
        };
        p.ingest_openai_official();
        p.ingest_legacy_fallbacks();
        p
    }

    /// The process-wide memoized price table (cheap Arc clone). Never blocks on
    /// disk/network — until `reload_shared` has populated the cell it returns the
    /// built-in snapshot, so callers holding BUILD_LOCK are never stalled.
    pub fn shared() -> Arc<Pricing> {
        if let Some(lock) = PRICING.get() {
            if let Ok(g) = lock.read() {
                return g.clone();
            }
        }
        Arc::new(Pricing::builtin_only())
    }

    /// Load the full table (cache read + network on cold/stale cache) and swap it
    /// into the shared cell. MUST run on a background thread — never the main
    /// thread or a BUILD_LOCK holder — since the fetch can block up to ~20s.
    pub fn reload_shared() {
        let p = Arc::new(Pricing::load());
        match PRICING.get() {
            Some(lock) => {
                if let Ok(mut g) = lock.write() {
                    *g = p;
                }
            }
            None => {
                let _ = PRICING.set(RwLock::new(p));
            }
        }
    }

    fn insert(&mut self, id: &str, price: ModelPrice) {
        if price.is_zero() {
            return;
        }
        self.exact
            .entry(id.to_string())
            .or_insert_with(|| price.clone());
        self.exact
            .entry(bare(id).to_string())
            .or_insert_with(|| price.clone());
        self.norm.entry(normalize_key(id)).or_insert(price);
    }

    // models.dev: { provider: { models: { id: { cost: {input,output,cache_read,cache_write} } } } }
    // cost is per-1M tokens → divide by 1e6 for per-token.
    fn ingest_modelsdev(&mut self, text: &str) {
        let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
            return;
        };
        let Some(root) = json.as_object() else { return };
        // gather (id, price); bare ids (no '/') first so official-vendor prices win
        let mut entries: Vec<(String, ModelPrice)> = Vec::new();
        for prov in root.values() {
            let Some(models) = prov.get("models").and_then(|m| m.as_object()) else {
                continue;
            };
            for (id, m) in models {
                let Some(c) = m.get("cost").and_then(|c| c.as_object()) else {
                    continue;
                };
                let g = |k: &str| c.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
                let price = ModelPrice {
                    input: g("input") / 1e6,
                    output: g("output") / 1e6,
                    cache_create: g("cache_write") / 1e6,
                    cache_read: g("cache_read") / 1e6,
                    ..Default::default()
                };
                entries.push((id.clone(), price));
            }
        }
        entries.sort_by_key(|(id, _)| id.contains('/')); // false(0)=bare first
        for (id, price) in entries {
            self.insert(&id, price);
        }
    }

    // LiteLLM: { key: { input_cost_per_token, output_cost_per_token, ... } } — already per-token.
    fn ingest_litellm(&mut self, text: &str) {
        let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
            return;
        };
        let Some(root) = json.as_object() else { return };
        let mut entries: Vec<(String, ModelPrice)> = Vec::new();
        for (id, m) in root {
            let Some(o) = m.as_object() else { continue };
            let g = |k: &str| o.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
            let price = ModelPrice {
                input: g("input_cost_per_token"),
                output: g("output_cost_per_token"),
                cache_create: g("cache_creation_input_token_cost"),
                cache_read: g("cache_read_input_token_cost"),
                ..Default::default()
            };
            entries.push((id.clone(), price));
        }
        entries.sort_by_key(|(id, _)| id.contains('/'));
        for (id, price) in entries {
            self.insert(&id, price);
        }
    }

    // OpenAI API pricing, standard processing / short context, USD per token.
    // Source: https://developers.openai.com/api/docs/pricing
    fn ingest_openai_official(&mut self) {
        let mk = |i: f64, o: f64, cc: f64, cr: f64| ModelPrice {
            input: i,
            output: o,
            cache_create: cc,
            cache_read: cr,
            ..Default::default()
        };
        let tiered = |i: f64, o: f64, cc: f64, cr: f64| ModelPrice {
            input: i,
            output: o,
            cache_create: cc,
            cache_read: cr,
            long_context_threshold: 272_000.0,
            long_input: i * 2.0,
            long_output: o * 1.5,
            long_cache_create: cc * 2.0,
            long_cache_read: cr * 2.0,
        };
        let b: &[(&str, ModelPrice)] = &[
            ("gpt-6-astra", tiered(10e-6, 50e-6, 12.5e-6, 1e-6)),
            ("gpt-6.1-sol", tiered(2e-6, 10e-6, 2.5e-6, 0.1e-6)),
            ("gpt-6-sol", tiered(2e-6, 10e-6, 2.5e-6, 0.2e-6)),
            ("gpt-6-luna", tiered(0.1e-6, 0.5e-6, 0.125e-6, 0.01e-6)),
            ("gpt-5.6", mk(5e-6, 30e-6, 0.0, 0.5e-6)),
            ("gpt-5.6-sol", mk(5e-6, 30e-6, 0.0, 0.5e-6)),
            ("gpt-5.5", mk(5e-6, 30e-6, 0.0, 0.5e-6)),
            ("gpt-5.5-2026-04-23", mk(5e-6, 30e-6, 0.0, 0.5e-6)),
            ("gpt-5.4", mk(2.5e-6, 15e-6, 0.0, 0.25e-6)),
            ("gpt-5.4-2026-03-05", mk(2.5e-6, 15e-6, 0.0, 0.25e-6)),
            ("gpt-5.4-mini", mk(0.75e-6, 4.5e-6, 0.0, 0.075e-6)),
            (
                "gpt-5.4-mini-2026-03-17",
                mk(0.75e-6, 4.5e-6, 0.0, 0.075e-6),
            ),
            ("gpt-5.4-nano", mk(0.20e-6, 1.25e-6, 0.0, 0.02e-6)),
            (
                "gpt-5.4-nano-2026-03-17",
                mk(0.20e-6, 1.25e-6, 0.0, 0.02e-6),
            ),
            ("chat-latest", mk(5e-6, 30e-6, 0.0, 0.5e-6)),
            ("gpt-5.3-codex", mk(1.75e-6, 14e-6, 0.0, 0.175e-6)),
        ];
        for (id, price) in b {
            self.insert(id, price.clone());
        }
    }

    fn ingest_legacy_fallbacks(&mut self) {
        let mk = |i: f64, o: f64, cc: f64, cr: f64| ModelPrice {
            input: i,
            output: o,
            cache_create: cc,
            cache_read: cr,
            ..Default::default()
        };
        let b: &[(&str, ModelPrice)] = &[
            ("claude-opus-4-7", mk(5e-6, 25e-6, 6.25e-6, 0.5e-6)),
            ("claude-opus-4-8", mk(5e-6, 25e-6, 6.25e-6, 0.5e-6)),
            ("claude-sonnet-4-5", mk(3e-6, 15e-6, 3.75e-6, 0.3e-6)),
            ("claude-sonnet-4-6", mk(3e-6, 15e-6, 3.75e-6, 0.3e-6)),
            ("claude-haiku-4-5", mk(1e-6, 5e-6, 1.25e-6, 0.1e-6)),
        ];
        for (id, price) in b {
            self.insert(id, price.clone());
        }
    }

    fn lookup(&self, model: &str) -> Option<&ModelPrice> {
        if let Some(p) = self.exact.get(model) {
            return Some(p);
        }
        self.norm.get(&normalize_key(model))
    }

    /// Exact-or-normalized cost in USD. None = no pricing data for this model.
    pub fn cost(
        &self,
        model: &str,
        input: f64,
        output: f64,
        cache_create: f64,
        cache_read: f64,
    ) -> Option<f64> {
        let p = self.lookup(model)?;
        let long_context = p.long_context_threshold > 0.0
            && input + cache_create + cache_read > p.long_context_threshold;
        let (input_rate, output_rate, cache_create_rate, cache_read_rate) = if long_context {
            (
                p.long_input,
                p.long_output,
                p.long_cache_create,
                p.long_cache_read,
            )
        } else {
            (p.input, p.output, p.cache_create, p.cache_read)
        };
        Some(
            input * input_rate
                + output * output_rate
                + cache_create * cache_create_rate
                + cache_read * cache_read_rate,
        )
    }

    #[allow(dead_code)]
    pub fn known(&self, model: &str) -> bool {
        self.lookup(model).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_prices_openai_official_models_without_network() {
        let pricing = Pricing::builtin_only();

        let astra_standard = pricing
            .cost("gpt-6-astra", 100_000.0, 100_000.0, 0.0, 100_000.0)
            .unwrap();
        assert!((astra_standard - 6.1).abs() < 1e-9);

        let astra_long = pricing
            .cost("gpt-6-astra", 100_000.0, 100_000.0, 0.0, 200_000.0)
            .unwrap();
        assert!((astra_long - 9.9).abs() < 1e-9);

        let sol61_standard = pricing
            .cost("gpt-6.1-sol", 100_000.0, 100_000.0, 0.0, 100_000.0)
            .unwrap();
        assert!((sol61_standard - 1.21).abs() < 1e-9);
        let sol61_long = pricing
            .cost("gpt-6.1-sol", 100_000.0, 100_000.0, 0.0, 200_000.0)
            .unwrap();
        assert!((sol61_long - 1.94).abs() < 1e-9);

        let sol6_standard = pricing
            .cost("gpt-6-sol", 100_000.0, 100_000.0, 0.0, 100_000.0)
            .unwrap();
        assert!((sol6_standard - 1.22).abs() < 1e-9);
        let sol6_long = pricing
            .cost("gpt-6-sol", 100_000.0, 100_000.0, 0.0, 200_000.0)
            .unwrap();
        assert!((sol6_long - 1.98).abs() < 1e-9);

        let luna_standard = pricing
            .cost("gpt-6-luna", 100_000.0, 100_000.0, 0.0, 100_000.0)
            .unwrap();
        assert!((luna_standard - 0.061).abs() < 1e-9);
        let luna_long = pricing
            .cost("gpt-6-luna", 100_000.0, 100_000.0, 0.0, 200_000.0)
            .unwrap();
        assert!((luna_long - 0.099).abs() < 1e-9);

        let sol = pricing
            .cost(
                "gpt-5.6-sol",
                1_000_000.0,
                1_000_000.0,
                0.0,
                1_000_000.0,
            )
            .unwrap();
        assert!((sol - 35.5).abs() < 1e-9);

        let gpt55 = pricing
            .cost("gpt-5.5", 1_000_000.0, 1_000_000.0, 0.0, 1_000_000.0)
            .unwrap();
        assert!((gpt55 - 35.5).abs() < 1e-9);

        let codex = pricing
            .cost("gpt-5.3-codex", 1_000_000.0, 1_000_000.0, 0.0, 1_000_000.0)
            .unwrap();
        assert!((codex - 15.925).abs() < 1e-9);
    }
}
