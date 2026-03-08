use std::sync::Arc;
use tokio::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;

use crate::{LlmClient, LlmResponse, Message};

/// Mock LLM for testing. Returns scripted responses and records requests.
///
/// When no embedding responses are queued, falls back to keyword-based
/// embeddings — see [`keyword_embed`] for details.
#[derive(Debug, Clone)]
pub struct MockLlm {
    responses: Arc<Mutex<Vec<LlmResponse>>>,
    embed_responses: Arc<Mutex<Vec<Vec<Vec<f32>>>>>,
    requests: Arc<Mutex<Vec<Vec<Message>>>>,
    embed_requests: Arc<Mutex<Vec<Vec<String>>>>,
}

impl MockLlm {
    pub fn new() -> Self {
        Self {
            responses: Arc::new(Mutex::new(Vec::new())),
            embed_responses: Arc::new(Mutex::new(Vec::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
            embed_requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Queue a response to be returned on the next call to `complete`.
    pub async fn queue_response(&self, content: impl Into<String>) {
        self.responses
            .lock()
            .await
            .push(LlmResponse {
                content: content.into(),
            });
    }

    /// Queue an embedding response to be returned on the next call to `embed`.
    pub async fn queue_embed_response(&self, embeddings: Vec<Vec<f32>>) {
        self.embed_responses.lock().await.push(embeddings);
    }

    /// Get all recorded completion requests for assertions.
    pub async fn recorded_requests(&self) -> Vec<Vec<Message>> {
        self.requests.lock().await.clone()
    }

    /// Get all recorded embedding requests for assertions.
    pub async fn recorded_embed_requests(&self) -> Vec<Vec<String>> {
        self.embed_requests.lock().await.clone()
    }
}

impl Default for MockLlm {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LlmClient for MockLlm {
    async fn complete(&self, messages: &[Message]) -> Result<LlmResponse> {
        self.requests.lock().await.push(messages.to_vec());
        let mut queue = self.responses.lock().await;
        let response = if queue.is_empty() {
            LlmResponse {
                content: "mock response".to_string(),
            }
        } else {
            queue.remove(0)
        };
        Ok(response)
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.embed_requests.lock().await.push(texts.to_vec());
        let mut queue = self.embed_responses.lock().await;
        let response = if queue.is_empty() {
            texts.iter().map(|t| keyword_embed(t)).collect()
        } else {
            queue.remove(0)
        };
        Ok(response)
    }
}

// ---------------------------------------------------------------------------
// Keyword-based mock embeddings
// ---------------------------------------------------------------------------

/// Keywords and their assigned dimension in the embedding vector.
/// Each keyword "lights up" one dimension, so texts about similar
/// concepts produce similar vectors.
const KEYWORDS: &[&str] = &[
    "sort",     // 0
    "order",    // 1
    "filter",   // 2
    "select",   // 3
    "map",      // 4
    "transform",// 5
    "read",     // 6
    "write",    // 7
    "parse",    // 8
    "format",   // 9
    "send",     // 10
    "receive",  // 11
    "create",   // 12
    "delete",   // 13
    "search",   // 14
    "find",     // 15
];

/// The dimensionality of mock embeddings.
pub const EMBED_DIM: usize = KEYWORDS.len();

/// Produce a deterministic embedding vector from keyword presence in text.
///
/// Scans the lowercased text for each keyword. Each match sets the
/// corresponding dimension to 1.0. The result is L2-normalized so
/// cosine similarity works correctly.
///
/// This is intentionally silly but useful for testing: texts mentioning
/// "sort" and "order" will be similar to each other, and dissimilar
/// to texts mentioning "parse" and "format".
///
/// ```
/// use agentcoderepo_llm::mock::keyword_embed;
///
/// let sort_vec = keyword_embed("sort a list in ascending order");
/// let also_sort = keyword_embed("order elements by value, sorting them");
/// let parse_vec = keyword_embed("parse a JSON format string");
///
/// // sort and also_sort should be more similar than sort and parse
/// let sim_sort = cosine_sim(&sort_vec, &also_sort);
/// let sim_diff = cosine_sim(&sort_vec, &parse_vec);
/// assert!(sim_sort > sim_diff);
///
/// fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
///     let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
///     let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
///     let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
///     if na == 0.0 || nb == 0.0 { 0.0 } else { dot / (na * nb) }
/// }
/// ```
pub fn keyword_embed(text: &str) -> Vec<f32> {
    let lower = text.to_lowercase();
    let mut vec = vec![0.0f32; EMBED_DIM];

    for (i, keyword) in KEYWORDS.iter().enumerate() {
        if lower.contains(keyword) {
            vec[i] = 1.0;
        }
    }

    // L2 normalize
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut vec {
            *x /= norm;
        }
    }

    vec
}

/// Compute cosine similarity between two vectors. Useful in test assertions.
pub fn cosine_sim(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similar_concepts_are_close() {
        let sort1 = keyword_embed("sort a list in order");
        let sort2 = keyword_embed("order the elements by sorting");
        let parse = keyword_embed("parse a JSON format string");

        let sim_same = cosine_sim(&sort1, &sort2);
        let sim_diff = cosine_sim(&sort1, &parse);

        assert!(
            sim_same > sim_diff,
            "sort/order similarity ({sim_same}) should exceed sort/parse similarity ({sim_diff})"
        );
    }

    #[test]
    fn no_keywords_gives_zero_vector() {
        let vec = keyword_embed("hello world");
        assert!(vec.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn normalized_to_unit_length() {
        let vec = keyword_embed("sort and filter");
        let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "should be unit vector, got norm={norm}");
    }

    #[test]
    fn identical_texts_have_similarity_one() {
        let v = keyword_embed("read and write data");
        let sim = cosine_sim(&v, &v);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn orthogonal_concepts() {
        let sort = keyword_embed("sort");
        let parse = keyword_embed("parse");
        let sim = cosine_sim(&sort, &parse);
        assert!(
            sim.abs() < 1e-6,
            "sort and parse should be orthogonal, got {sim}"
        );
    }
}
