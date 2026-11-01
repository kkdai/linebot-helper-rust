//! 記憶體版 KvStore，測試用。

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::Value;

use super::{KvStore, sanitize_key};

pub struct MemoryKv {
    docs: Mutex<BTreeMap<String, Value>>,
    available: bool,
}

impl MemoryKv {
    pub fn new() -> Self {
        Self {
            docs: Mutex::default(),
            available: true,
        }
    }

    pub fn unavailable() -> Self {
        Self {
            docs: Mutex::default(),
            available: false,
        }
    }

    pub fn len(&self) -> usize {
        self.docs.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for MemoryKv {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl KvStore for MemoryKv {
    fn is_available(&self) -> bool {
        self.available
    }

    async fn save(&self, key: &str, doc: &Value) {
        if self.available {
            self.docs
                .lock()
                .unwrap()
                .insert(sanitize_key(key), doc.clone());
        }
    }

    async fn load(&self, key: &str) -> Option<Value> {
        self.docs.lock().unwrap().get(&sanitize_key(key)).cloned()
    }

    async fn delete(&self, key: &str) {
        self.docs.lock().unwrap().remove(&sanitize_key(key));
    }

    async fn load_all(&self) -> Vec<(String, Value)> {
        self.docs
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}
