pub mod mem;
pub mod s3;

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;

/// Abstraction over object storage (Tigris in prod, in-memory in tests).
#[async_trait]
pub trait ObjectStore: Send + Sync + 'static {
    async fn put(&self, key: &str, data: Bytes) -> Result<()>;
    async fn get(&self, key: &str) -> Result<Option<Bytes>>;
    async fn delete(&self, key: &str) -> Result<()>;
    async fn list(&self, prefix: &str) -> Result<Vec<String>>;
}
