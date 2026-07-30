//! HTTP client for the Docling embedding service.
//!
//! Communicates with the Python Docling service to generate text embeddings
//! using local sentence-transformers models.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Client for the embedding service.
#[derive(Debug, Clone)]
pub struct EmbeddingClient {
    /// Base URL of the Docling service (e.g., "http://localhost:50060")
    service_url: String,
    /// Expected embedding dimension
    dimension: usize,
    /// HTTP client
    client: reqwest::Client,
}

/// Request body for the /embed endpoint.
#[derive(Debug, Serialize)]
struct EmbedRequest {
    texts: Vec<String>,
}

/// Response from the /embed endpoint.
#[derive(Debug, Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
    dimension: usize,
}

/// Response from the /health endpoint.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct HealthResponse {
    status: String,
    embedding_loaded: bool,
    embedding_dim: Option<usize>,
}

impl EmbeddingClient {
    /// Create a new embedding client.
    ///
    /// # Arguments
    /// * `service_url` - Base URL of the Docling service
    /// * `dimension` - Expected embedding dimension (e.g., 384)
    pub fn new(service_url: &str, dimension: usize) -> Self {
        Self {
            service_url: service_url.trim_end_matches('/').to_string(),
            dimension,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("Failed to build HTTP client"),
        }
    }

    /// Create from environment variables.
    ///
    /// Reads `DOCLING_SERVICE_URL` and `EMBEDDING_DIM` from the environment.
    pub fn from_env() -> Result<Self> {
        let service_url = std::env::var("DOCLING_SERVICE_URL")
            .unwrap_or_else(|_| "http://localhost:50060".to_string());
        let dimension: usize = std::env::var("EMBEDDING_DIM")
            .unwrap_or_else(|_| "384".to_string())
            .parse()
            .context("Invalid EMBEDDING_DIM")?;
        
        Ok(Self::new(&service_url, dimension))
    }

    /// Check if the embedding service is available.
    /// Uses a short timeout (3 seconds) to fail fast.
    pub async fn health_check(&self) -> Result<bool> {
        let url = format!("{}/health", self.service_url);
        
        // Wrap entire operation in timeout (send + parse)
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            async {
                let resp = self.client.get(&url).send().await?;
                if !resp.status().is_success() {
                    return Ok::<bool, anyhow::Error>(false);
                }
                let health: HealthResponse = resp.json().await?;
                Ok(health.status == "ok" && health.embedding_loaded)
            }
        )
        .await;
        
        match result {
            Ok(Ok(ok)) => Ok(ok),
            _ => Ok(false),  // Timeout or error = service unavailable
        }
    }

    /// Generate embeddings for a batch of texts.
    ///
    /// Returns a vector of embedding vectors, each of dimension `self.dimension`.
    /// The order of embeddings matches the order of input texts.
    pub async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        
        // Batch size limit (service enforces max 100)
        const BATCH_SIZE: usize = 50;
        
        let mut all_embeddings = Vec::with_capacity(texts.len());
        
        for chunk in texts.chunks(BATCH_SIZE) {
            let url = format!("{}/embed", self.service_url);
            
            let request = EmbedRequest {
                texts: chunk.iter().map(|s| s.to_string()).collect(),
            };
            
            // Wrap entire request + response parsing in timeout
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                async {
                    let response = self.client
                        .post(&url)
                        .json(&request)
                        .send()
                        .await
                        .context("Failed to send request")?;
                    
                    if !response.status().is_success() {
                        let status = response.status();
                        let body = response.text().await.unwrap_or_default();
                        anyhow::bail!("Embedding service error ({}): {}", status, body);
                    }
                    
                    let embed_resp: EmbedResponse = response.json().await?;
                    Ok(embed_resp)
                }
            )
            .await
            .context("Embedding request timed out")??;
            
            // Validate dimension
            if result.dimension != self.dimension {
                tracing::warn!(
                    expected = self.dimension,
                    actual = result.dimension,
                    "Embedding dimension mismatch"
                );
            }
            
            all_embeddings.extend(result.embeddings);
        }
        
        Ok(all_embeddings)
    }

    /// Generate embedding for a single query text.
    pub async fn embed_query(&self, query: &str) -> Result<Vec<f32>> {
        let mut embeddings = self.embed_batch(&[query]).await?;
        
        if embeddings.is_empty() {
            anyhow::bail!("No embedding returned for query");
        }
        
        Ok(embeddings.remove(0))
    }

    /// Get the expected embedding dimension.
    pub fn dimension(&self) -> usize {
        self.dimension
    }
}

/// Convert a vector of f32 to raw bytes (for storage).
pub fn embedding_to_bytes(embedding: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(embedding.len() * 4);
    for &val in embedding {
        bytes.extend_from_slice(&val.to_le_bytes());
    }
    bytes
}

/// Convert raw bytes back to a vector of f32.
pub fn bytes_to_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| {
            let arr: [u8; 4] = chunk.try_into().expect("Invalid chunk size");
            f32::from_le_bytes(arr)
        })
        .collect()
}

/// Compute cosine similarity between two vectors.
///
/// Assumes vectors are L2-normalized (dot product = cosine similarity).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    
    // Dot product (for normalized vectors, this is cosine similarity)
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_embedding_bytes_roundtrip() {
        let original = vec![0.1, 0.2, 0.3, -0.5, 1.0];
        let bytes = embedding_to_bytes(&original);
        let recovered = bytes_to_embedding(&bytes);
        
        assert_eq!(original.len(), recovered.len());
        for (a, b) in original.iter().zip(recovered.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }
    
    #[test]
    fn test_cosine_similarity_identical() {
        let a = vec![0.5, 0.5, 0.5, 0.5];
        let sim = cosine_similarity(&a, &a);
        assert!((sim - 1.0).abs() < 1e-6, "Identical vectors should have similarity 1.0");
    }
    
    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6, "Orthogonal vectors should have similarity 0.0");
    }
}
