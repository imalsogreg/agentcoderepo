pub mod mock;
pub mod openai;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    pub content: String,
}

/// Abstraction over LLM providers.
///
/// Used for repo indexing, SemVer verification, doc quality scoring,
/// and in tests to simulate agent traffic.
#[async_trait]
pub trait LlmClient: Send + Sync + 'static {
    /// Chat-style completion.
    async fn complete(&self, messages: &[Message]) -> Result<LlmResponse>;

    /// Batch text embedding. Returns one vector per input string.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}
