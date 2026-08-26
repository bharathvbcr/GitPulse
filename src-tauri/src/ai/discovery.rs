//! Finding the local model server, and what it serves.
//!
//! The ports are the ones the runtimes actually bind: Ollama 11434, vLLM 8000,
//! LM Studio 1234, llama.cpp and `mlx_lm.server` 8080, Jan 1337. An unused
//! loopback port refuses a connection immediately rather than hanging, so the
//! whole sweep costs about as much as its slowest live server.
//!
//! A port that answers something other than a model listing — anything else on
//! 8080, say — is reported as "not a model server" rather than silently
//! dropped, because "nothing found" and "something is there and it is not a
//! model server" send an operator to two different places.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::http::{self, Endpoint};

/// Ports probed when the user has named no endpoint.
pub const CANDIDATE_PORTS: [u16; 5] = [11434, 8000, 1234, 8080, 1337];

/// How long one candidate may take. Long enough for a loaded server to answer
/// a listing, short enough that a five-port sweep is not felt.
const PROBE_TIMEOUT: Duration = Duration::from_millis(1200);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredEndpoint {
    pub base_url: String,
    /// Models the server listed, in the order it listed them.
    pub models: Vec<String>,
    /// True when the endpoint answered a well-formed model listing.
    pub reachable: bool,
    /// Why it did not, when it did not.
    pub detail: String,
}

#[derive(Debug, Deserialize)]
struct ModelListing {
    #[serde(default)]
    data: Vec<ModelEntry>,
    /// Ollama's native listing uses `models` with `name`; accepted so a server
    /// reached on its native root still identifies itself.
    #[serde(default)]
    models: Vec<NativeModelEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    #[serde(default)]
    id: String,
}

#[derive(Debug, Deserialize)]
struct NativeModelEntry {
    #[serde(default)]
    name: String,
}

/// Asks one endpoint what it serves.
pub fn list_models(base_url: &str) -> DiscoveredEndpoint {
    let endpoint = match http::parse_base_url(base_url) {
        Ok(e) => e,
        Err(detail) => {
            return DiscoveredEndpoint {
                base_url: base_url.to_string(),
                models: Vec::new(),
                reachable: false,
                detail,
            }
        }
    };
    list_models_at(&endpoint)
}

fn list_models_at(endpoint: &Endpoint) -> DiscoveredEndpoint {
    let base_url = endpoint.base_url();
    match http::request(endpoint, "GET", "/models", None, PROBE_TIMEOUT) {
        Ok(res) if res.status == 200 => match serde_json::from_str::<ModelListing>(&res.body) {
            Ok(listing) => {
                let mut models: Vec<String> = listing
                    .data
                    .into_iter()
                    .map(|m| m.id)
                    .filter(|id| !id.is_empty())
                    .collect();
                models.extend(
                    listing
                        .models
                        .into_iter()
                        .map(|m| m.name)
                        .filter(|n| !n.is_empty()),
                );
                let empty = models.is_empty();
                DiscoveredEndpoint {
                    base_url,
                    models,
                    reachable: !empty,
                    detail: if empty {
                        "server answered a listing with no models".into()
                    } else {
                        String::new()
                    },
                }
            }
            Err(e) => DiscoveredEndpoint {
                base_url,
                models: Vec::new(),
                reachable: false,
                detail: format!("not a model server: {}", e),
            },
        },
        Ok(res) => DiscoveredEndpoint {
            base_url,
            models: Vec::new(),
            reachable: false,
            detail: format!("not a model server: HTTP {}", res.status),
        },
        Err(detail) => DiscoveredEndpoint {
            base_url,
            models: Vec::new(),
            reachable: false,
            detail,
        },
    }
}

/// Sweeps the candidate ports, plus any endpoint the caller names.
///
/// Every candidate is reported, reachable or not; a caller that only wants the
/// live ones filters on `reachable`, and one that wants to explain an empty
/// result has the reasons.
pub fn sweep(explicit: Option<&str>) -> Vec<DiscoveredEndpoint> {
    let mut candidates: Vec<String> = Vec::new();
    if let Some(url) = explicit {
        if !url.trim().is_empty() {
            candidates.push(url.trim().to_string());
        }
    }
    for port in CANDIDATE_PORTS {
        let url = format!("http://127.0.0.1:{}/v1", port);
        if !candidates.iter().any(|c| c == &url) {
            candidates.push(url);
        }
    }

    let handles: Vec<_> = candidates
        .into_iter()
        .map(|url| {
            let probe_url = url.clone();
            let handle = std::thread::spawn(move || list_models(&probe_url));
            (url, handle)
        })
        .collect();

    let mut endpoints = Vec::with_capacity(handles.len());
    for (url, handle) in handles {
        match handle.join() {
            Ok(endpoint) => endpoints.push(endpoint),
            Err(_) => log::warn!(
                target: "ai::discovery",
                "model-discovery probe thread panicked for endpoint {url}"
            ),
        }
    }
    endpoints
}

/// The first reachable endpoint, preferring one that serves `preferred_model`.
pub fn choose<'a>(
    endpoints: &'a [DiscoveredEndpoint],
    preferred_model: Option<&str>,
) -> Option<(&'a DiscoveredEndpoint, String)> {
    if let Some(model) = preferred_model.filter(|m| !m.trim().is_empty()) {
        if let Some(ep) = endpoints
            .iter()
            .find(|e| e.reachable && e.models.iter().any(|m| m == model))
        {
            return Some((ep, model.to_string()));
        }
    }
    endpoints
        .iter()
        .find(|e| e.reachable && !e.models.is_empty())
        .map(|e| (e, e.models[0].clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(url: &str, models: &[&str], reachable: bool) -> DiscoveredEndpoint {
        DiscoveredEndpoint {
            base_url: url.into(),
            models: models.iter().map(|m| m.to_string()).collect(),
            reachable,
            detail: String::new(),
        }
    }

    #[test]
    fn choose_prefers_the_named_model_over_position() {
        let endpoints = vec![
            endpoint("http://127.0.0.1:11434/v1", &["gemma4:31b"], true),
            endpoint("http://127.0.0.1:1234/v1", &["qwen3.8:27b"], true),
        ];
        let (ep, model) = choose(&endpoints, Some("qwen3.8:27b")).unwrap();
        assert_eq!(ep.base_url, "http://127.0.0.1:1234/v1");
        assert_eq!(model, "qwen3.8:27b");
    }

    #[test]
    fn choose_falls_back_to_the_first_reachable_endpoint() {
        let endpoints = vec![
            endpoint("http://127.0.0.1:8000/v1", &[], false),
            endpoint("http://127.0.0.1:11434/v1", &["gemma4:31b"], true),
        ];
        let (ep, model) = choose(&endpoints, Some("not-served")).unwrap();
        assert_eq!(ep.base_url, "http://127.0.0.1:11434/v1");
        assert_eq!(model, "gemma4:31b");
    }

    #[test]
    fn choose_reports_nothing_when_no_endpoint_is_reachable() {
        let endpoints = vec![endpoint("http://127.0.0.1:8000/v1", &[], false)];
        assert!(choose(&endpoints, None).is_none());
    }

    #[test]
    fn an_unusable_base_url_is_reported_not_swallowed() {
        let result = list_models("https://api.example.com/v1");
        assert!(!result.reachable);
        assert!(
            result.detail.contains("https is refused"),
            "{}",
            result.detail
        );
    }
}
