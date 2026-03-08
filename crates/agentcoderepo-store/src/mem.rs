use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;

use crate::ObjectStore;

/// In-memory object store for testing.
#[derive(Debug, Clone, Default)]
pub struct MemStore {
    data: Arc<RwLock<HashMap<String, Bytes>>>,
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ObjectStore for MemStore {
    async fn put(&self, key: &str, data: Bytes) -> Result<()> {
        self.data.write().await.insert(key.to_string(), data);
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        Ok(self.data.read().await.get(key).cloned())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.data.write().await.remove(key);
        Ok(())
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>> {
        let data = self.data.read().await;
        Ok(data
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect())
    }
}
