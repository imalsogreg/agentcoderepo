//! OpenAI-backed LLM client.
//!
//! Reads `OPENAI_API_KEY` from the environment.
//! Uses `gpt-4o` for completions and `text-embedding-3-small` for embeddings.

use anyhow::{Context, Result};
use async_openai::config::OpenAIConfig;
use async_openai::types::chat::{
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
    ChatCompletionRequestSystemMessageContent, ChatCompletionRequestUserMessage,
    ChatCompletionRequestUserMessageContent, CreateChatCompletionRequestArgs,
};
use async_openai::types::embeddings::{CreateEmbeddingRequestArgs, EmbeddingInput};
use async_openai::Client;
use async_trait::async_trait;

use crate::{LlmClient, LlmResponse, Message};

/// OpenAI-backed LLM client.
///
/// Reads the API key from `OPENAI_API_KEY` environment variable.
pub struct OpenAiClient {
    client: Client<OpenAIConfig>,
    chat_model: String,
    embed_model: String,
}

impl OpenAiClient {
    /// Create a new client using `OPENAI_API_KEY` from the environment.
    pub fn from_env() -> Result<Self> {
        // async-openai reads OPENAI_API_KEY automatically
        let client = Client::new();
        Ok(Self {
            client,
            chat_model: "gpt-4o".to_string(),
            embed_model: "text-embedding-3-small".to_string(),
        })
    }

    /// Create a client with explicit configuration.
    pub fn new(
        api_key: impl Into<String>,
        chat_model: impl Into<String>,
        embed_model: impl Into<String>,
    ) -> Self {
        let config = OpenAIConfig::new().with_api_key(api_key);
        Self {
            client: Client::with_config(config),
            chat_model: chat_model.into(),
            embed_model: embed_model.into(),
        }
    }
}

fn to_oai_message(m: &Message) -> ChatCompletionRequestMessage {
    match m.role.as_str() {
        "system" => ChatCompletionRequestMessage::System(ChatCompletionRequestSystemMessage {
            content: ChatCompletionRequestSystemMessageContent::Text(m.content.clone()),
            name: None,
        }),
        _ => ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
            content: ChatCompletionRequestUserMessageContent::Text(m.content.clone()),
            name: None,
        }),
    }
}

#[async_trait]
impl LlmClient for OpenAiClient {
    async fn complete(&self, messages: &[Message]) -> Result<LlmResponse> {
        let oai_messages: Vec<ChatCompletionRequestMessage> =
            messages.iter().map(to_oai_message).collect();

        let request = CreateChatCompletionRequestArgs::default()
            .model(&self.chat_model)
            .messages(oai_messages)
            .build()
            .context("failed to build chat completion request")?;

        tracing::debug!(model = %self.chat_model, "sending chat completion request");

        let response = self
            .client
            .chat()
            .create(request)
            .await
            .context("OpenAI chat completion failed")?;

        let content = response
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .unwrap_or_default();

        tracing::debug!(
            tokens = ?response.usage.map(|u| u.total_tokens),
            "chat completion complete"
        );

        Ok(LlmResponse { content })
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let request = CreateEmbeddingRequestArgs::default()
            .model(&self.embed_model)
            .input(EmbeddingInput::StringArray(texts.to_vec()))
            .build()
            .context("failed to build embedding request")?;

        tracing::debug!(
            model = %self.embed_model,
            count = texts.len(),
            "sending embedding request"
        );

        let response = self
            .client
            .embeddings()
            .create(request)
            .await
            .context("OpenAI embedding request failed")?;

        let embeddings: Vec<Vec<f32>> = response
            .data
            .into_iter()
            .map(|e| e.embedding)
            .collect();

        tracing::debug!(
            vectors = embeddings.len(),
            dim = embeddings.first().map(|v: &Vec<f32>| v.len()).unwrap_or(0),
            "embedding complete"
        );

        Ok(embeddings)
    }
}
