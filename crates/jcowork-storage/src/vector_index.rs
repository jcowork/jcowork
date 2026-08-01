//! HNSW vector index for fast approximate nearest neighbor search.
//!
//! Wraps the `usearch` library to provide efficient vector similarity search
//! over document chunk embeddings. Replaces brute-force linear scan with
//! sub-linear HNSW graph traversal.

use anyhow::Result;
use std::path::Path;
use std::sync::Mutex;
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

/// HNSW vector index wrapping usearch.
///
/// Thread-safe via Mutex. Stores chunk_id -> embedding mappings.
pub struct VectorIndex {
    inner: Mutex<Index>,
    dimensions: usize,
}

impl std::fmt::Debug for VectorIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VectorIndex")
            .field("dimensions", &self.dimensions)
            .field("count", &self.count())
            .finish()
    }
}

/// Search result from the vector index.
#[derive(Debug, Clone)]
pub struct VectorSearchResult {
    pub chunk_id: i64,
    pub score: f32,
}

impl VectorIndex {
    /// Create a new empty vector index.
    ///
    /// # Arguments
    /// * `dimensions` - Embedding dimension (e.g., 384 for all-MiniLM-L6-v2)
    /// * `capacity` - Initial capacity hint (number of vectors expected)
    pub fn new(dimensions: usize, capacity: usize) -> Result<Self> {
        let mut options = IndexOptions::default();
        options.dimensions = dimensions;
        options.metric = MetricKind::Cos;
        options.quantization = ScalarKind::F32;

        let index = Index::new(&options)
            .map_err(|e| anyhow::anyhow!("Failed to create HNSW index: {}", e))?;

        if capacity > 0 {
            let _ = index.reserve(capacity);
        }

        Ok(Self {
            inner: Mutex::new(index),
            dimensions,
        })
    }

    /// Load an existing index from disk.
    pub fn load(path: &Path, dimensions: usize) -> Result<Self> {
        let mut options = IndexOptions::default();
        options.dimensions = dimensions;
        options.metric = MetricKind::Cos;
        options.quantization = ScalarKind::F32;

        let index = Index::new(&options)
            .map_err(|e| anyhow::anyhow!("Failed to create HNSW index: {}", e))?;

        let path_str = path.to_string_lossy().to_string();
        index
            .load(&path_str)
            .map_err(|e| anyhow::anyhow!("Failed to load HNSW index: {}", e))?;

        tracing::info!(
            path = %path_str,
            size = index.size(),
            "Loaded HNSW vector index from disk"
        );

        Ok(Self {
            inner: Mutex::new(index),
            dimensions,
        })
    }

    /// Save the index to disk.
    pub fn save(&self, path: &Path) -> Result<()> {
        let index = self.inner.lock().map_err(|_| anyhow::anyhow!("Lock poisoned"))?;
        let path_str = path.to_string_lossy().to_string();
        index
            .save(&path_str)
            .map_err(|e| anyhow::anyhow!("Failed to save HNSW index: {}", e))?;
        tracing::info!(path = %path_str, size = index.size(), "Saved HNSW vector index");
        Ok(())
    }

    /// Add a vector to the index.
    ///
    /// # Arguments
    /// * `chunk_id` - Unique identifier for the chunk (must be positive)
    /// * `embedding` - The embedding vector
    pub fn add(&self, chunk_id: i64, embedding: &[f32]) -> Result<()> {
        debug_assert!(chunk_id > 0, "chunk_id must be positive for usearch key");
        let index = self.inner.lock().map_err(|_| anyhow::anyhow!("Lock poisoned"))?;
        index
            .add(chunk_id as u64, embedding)
            .map_err(|e| anyhow::anyhow!("Failed to add vector: {}", e))?;
        Ok(())
    }

    /// Remove a vector from the index.
    pub fn remove(&self, chunk_id: i64) -> Result<()> {
        let index = self.inner.lock().map_err(|_| anyhow::anyhow!("Lock poisoned"))?;
        index
            .remove(chunk_id as u64)
            .map_err(|e| anyhow::anyhow!("Failed to remove vector: {}", e))?;
        Ok(())
    }

    /// Search for the nearest neighbors.
    ///
    /// Returns up to `top_k` results sorted by similarity (highest first).
    /// Cosine distance is converted to similarity score: score = 1.0 - distance.
    pub fn search(&self, query: &[f32], top_k: u32) -> Result<Vec<VectorSearchResult>> {
        let index = self.inner.lock().map_err(|_| anyhow::anyhow!("Lock poisoned"))?;
        let results = index
            .search(query, top_k as usize)
            .map_err(|e| anyhow::anyhow!("Search failed: {}", e))?;

        let scored = results
            .keys
            .iter()
            .zip(results.distances.iter())
            .map(|(&key, &distance)| VectorSearchResult {
                chunk_id: key as i64,
                // Cos distance: 0 = identical, 2 = opposite → similarity = 1 - dist/2
                // But usearch Cos metric returns values in [0, 2], we want [0, 1] similarity
                score: (1.0 - distance / 2.0).max(0.0),
            })
            .collect();

        Ok(scored)
    }

    /// Get the number of vectors in the index.
    pub fn count(&self) -> usize {
        self.inner
            .lock()
            .map(|idx| idx.size())
            .unwrap_or(0)
    }

    /// Get the embedding dimensions.
    pub fn dimensions(&self) -> usize {
        self.dimensions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vector_index_basic() {
        let idx = VectorIndex::new(4, 10).unwrap();

        let v1 = vec![1.0, 0.0, 0.0, 0.0];
        let v2 = vec![0.0, 1.0, 0.0, 0.0];
        let v3 = vec![1.0, 1.0, 0.0, 0.0];

        idx.add(1, &v1).unwrap();
        idx.add(2, &v2).unwrap();
        idx.add(3, &v3).unwrap();

        assert_eq!(idx.count(), 3);

        // Search for something close to v1
        let query = vec![0.9, 0.1, 0.0, 0.0];
        let results = idx.search(&query, 3).unwrap();

        assert_eq!(results.len(), 3);
        // v1 should be most similar
        assert_eq!(results[0].chunk_id, 1);
    }

    #[test]
    fn test_vector_index_remove() {
        let idx = VectorIndex::new(4, 10).unwrap();

        idx.add(1, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        idx.add(2, &[0.0, 1.0, 0.0, 0.0]).unwrap();
        assert_eq!(idx.count(), 2);

        idx.remove(1).unwrap();
        assert_eq!(idx.count(), 1);

        let results = idx.search(&[1.0, 0.0, 0.0, 0.0], 5).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk_id, 2);
    }

    #[test]
    fn test_vector_index_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.usearch");

        let idx = VectorIndex::new(4, 10).unwrap();
        idx.add(1, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        idx.add(2, &[0.0, 1.0, 0.0, 0.0]).unwrap();
        idx.save(&path).unwrap();

        let idx2 = VectorIndex::load(&path, 4).unwrap();
        assert_eq!(idx2.count(), 2);

        let results = idx2.search(&[1.0, 0.0, 0.0, 0.0], 5).unwrap();
        assert_eq!(results[0].chunk_id, 1);
    }
}
