//! Client-side sentence embeddings for ChromaDB.
//!
//! The `chromadb` Rust client (2.x) requires embeddings to be computed
//! client-side: it refuses documents without embeddings (upsert) and refuses
//! `query_texts` without an embedding function (query). ChromaDB 1.x also
//! removed the server's default embedding function, so there is no
//! server-side fallback anymore — this module closes that gap with a local
//! ONNX sentence-transformers model.
//!
//! Model: `paraphrase-multilingual-MiniLM-L12-v2` (quantized ONNX) — the
//! reader's library is largely Chinese, and this model covers 50+ languages
//! (unlike the English-only all-MiniLM-L6-v2). The architecture-specific
//! model is downloaded once (~120 MB) into `~/.rss-reader/models/`, from an
//! immutable revision and verified by SHA-256. hf-mirror.com is a fallback
//! where huggingface.co is blocked. Override the model dir with
//! `CHROMA_MODEL_DIR`.
//!
//! Everything is lazy: constructing the function does zero I/O, and the
//! download + model load happen on the FIRST embed call (cached afterwards).
//! This keeps `ChromaService::new` (and thus health checks, which never
//! embed) fast even on a fresh machine.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use async_trait::async_trait;
use chromadb::embeddings::EmbeddingFunction;
use ndarray::Array2;
use ort::session::Session;
use ort::value::{DynTensor, Tensor, Value};
use sha2::{Digest, Sha256};
use tokenizers::tokenizer::{
    PaddingDirection, PaddingParams, PaddingStrategy, Tokenizer, TruncationDirection,
    TruncationParams, TruncationStrategy,
};
use tokio::io::AsyncWriteExt;

/// Sentence-transformers model used for embedding articles and queries.
const MODEL_REPO: &str = "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2";
/// Immutable upstream revision reviewed for this build. Never execute a model
/// fetched from the mutable `main` ref.
const MODEL_REVISION: &str = "e8f8c211226b894fcb81acc59f3b34ba3efd5f42";
const TOKENIZER_FILE: &str = "tokenizer.json";
const TOKENIZER_SHA256: &str = "2c3387be76557bd40970cec13153b3bbf80407865484b209e655e5e4729076b8";

#[derive(Clone, Copy)]
struct ModelSpec {
    file: &'static str,
    sha256: &'static str,
}

fn model_spec() -> ModelSpec {
    // Exactly one cfg block is active per target; it is the function's value.
    #[cfg(target_arch = "aarch64")]
    {
        ModelSpec {
            file: "onnx/model_qint8_arm64.onnx",
            sha256: "783fea82d71a58179b830a4dbd2d58447e640609e98eedf9ffa12622d375a672",
        }
    }
    #[cfg(target_arch = "x86_64")]
    {
        ModelSpec {
            file: "onnx/model_quint8_avx2.onnx",
            sha256: "98a01d88b7de996cdea58c32ca71208c09968d143798814b2ea09d3439dc334f",
        }
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        ModelSpec {
            file: "onnx/model.onnx",
            sha256: "10f7a088420252b26caf819236ca2c9d2987afd0fc06fec7553b542a5655a05a",
        }
    }
}
/// Truncate every document/query to this many tokens — long enough for an
/// article's opening, short enough for fast inference.
const MAX_SEQ_LEN: usize = 256;
const EMBEDDING_DIM: usize = 384;

/// Hosts tried in order when downloading model files.
fn model_mirrors() -> Vec<String> {
    let mut mirrors = Vec::new();
    if let Ok(endpoint) = std::env::var("HF_ENDPOINT") {
        if !endpoint.is_empty() {
            mirrors.push(endpoint.trim_end_matches('/').to_string());
        }
    }
    mirrors.push("https://huggingface.co".to_string());
    mirrors.push("https://hf-mirror.com".to_string());
    mirrors
}

/// `~/.rss-reader/models/` unless `CHROMA_MODEL_DIR` overrides it.
fn model_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CHROMA_MODEL_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".rss-reader")
        .join("models")
}

/// Tokenizer + session once the model files exist. Stored behind a
/// `OnceCell` so the (expensive) load happens exactly once per process.
struct Loaded {
    tokenizer: Tokenizer,
    /// ort's `run` takes `&mut self`; a Mutex makes the session usable
    /// through the `&self` trait method. `run` is synchronous CPU work, so
    /// the lock is never held across an await.
    session: Mutex<Session>,
}

/// ONNX sentence-transformers pipeline: tokenize → BERT → mean-pool → L2
/// normalize, producing one 384-dim vector per document.
#[derive(Clone)]
pub struct OnnxEmbeddingFunction {
    /// Directory holding the model + tokenizer files (shared between clones,
    /// all of which race on the same `OnceCell` init).
    model_dir: Arc<PathBuf>,
    loaded: Arc<tokio::sync::OnceCell<Loaded>>,
}

impl OnnxEmbeddingFunction {
    /// Cheap constructor — no I/O. The model is downloaded and loaded on the
    /// first `embed` call (and cached for the process lifetime).
    pub fn new() -> Self {
        Self {
            model_dir: Arc::new(model_dir().join(MODEL_REPO)),
            loaded: Arc::new(tokio::sync::OnceCell::new()),
        }
    }

    /// Ensure the model files exist (downloading them if necessary) and load
    /// tokenizer + session. Only runs once; concurrent first calls race on
    /// the `OnceCell` and a failure is retried by the next caller.
    async fn loaded(&self) -> anyhow::Result<&Loaded> {
        self.loaded
            .get_or_try_init(|| async {
                let t0 = std::time::Instant::now();
                let spec = model_spec();
                let tokenizer_path = self.model_dir.join(TOKENIZER_FILE);
                let model_path = self.model_dir.join(spec.file);
                let files_pre_present = tokenizer_path.exists() && model_path.exists();
                std::fs::create_dir_all(&*self.model_dir)
                    .with_context(|| format!("create model dir {}", self.model_dir.display()))?;
                ensure_model_file(&tokenizer_path, TOKENIZER_FILE, TOKENIZER_SHA256).await?;
                ensure_model_file(&model_path, spec.file, spec.sha256).await?;
                // tokenizers' error type is `Box<dyn StdError + Send + Sync>`,
                // which anyhow's Context trait can't blanket-cover — map
                // errors manually instead.
                let mut tokenizer = Tokenizer::from_file(&tokenizer_path).map_err(|e| {
                    anyhow::anyhow!("load tokenizer {}: {}", tokenizer_path.display(), e)
                })?;
                let pad_id = tokenizer.token_to_id("[PAD]").unwrap_or(0);
                tokenizer
                    .with_truncation(Some(TruncationParams {
                        max_length: MAX_SEQ_LEN,
                        strategy: TruncationStrategy::LongestFirst,
                        direction: TruncationDirection::Right,
                        stride: 0,
                    }))
                    .map_err(|e| anyhow::anyhow!("set truncation: {}", e))?;
                tokenizer.with_padding(Some(PaddingParams {
                    strategy: PaddingStrategy::BatchLongest,
                    direction: PaddingDirection::Right,
                    pad_id,
                    pad_type_id: 0,
                    pad_token: "[PAD]".to_string(),
                    pad_to_multiple_of: None,
                }));
                let session = Session::builder()?
                    .commit_from_file(&model_path)
                    .with_context(|| format!("load ONNX model {}", model_path.display()))?;
                println!(
                    "[chroma] model loaded fresh in {:?} (files pre-present: {})",
                    t0.elapsed(),
                    files_pre_present
                );
                Ok(Loaded {
                    tokenizer,
                    session: Mutex::new(session),
                })
            })
            .await
    }

    /// Run the model on one padded batch and mean-pool + normalize.
    fn pool_batch(
        &self,
        session: &mut Session,
        encodings: &[tokenizers::Encoding],
    ) -> anyhow::Result<Vec<Vec<f32>>> {
        let batch = encodings.len();
        let seq_len = encodings.first().map(|e| e.get_ids().len()).unwrap_or(0);
        if batch == 0 || seq_len == 0 {
            return Ok(Vec::new());
        }

        let ids: Vec<i64> = encodings
            .iter()
            .flat_map(|e| e.get_ids().iter().map(|&v| v as i64))
            .collect();
        let mask: Vec<i64> = encodings
            .iter()
            .flat_map(|e| e.get_attention_mask().iter().map(|&v| v as i64))
            .collect();
        let types: Vec<i64> = encodings
            .iter()
            .flat_map(|e| e.get_type_ids().iter().map(|&v| v as i64))
            .collect();

        let ids_t =
            Tensor::<i64>::from_array(Array2::from_shape_vec((batch, seq_len), ids)?)?.upcast();
        let mask_arr = Array2::from_shape_vec((batch, seq_len), mask)?;
        let mask_t = Tensor::<i64>::from_array(mask_arr.clone())?.upcast();
        let types_t =
            Tensor::<i64>::from_array(Array2::from_shape_vec((batch, seq_len), types)?)?.upcast();

        // Some exported models drop `token_type_ids` (e.g. DistilBERT) —
        // only pass the inputs the session actually declares.
        let declared: Vec<String> = session
            .inputs()
            .iter()
            .map(|i| i.name().to_string())
            .collect();
        let mut inputs: Vec<(&str, DynTensor)> = vec![("input_ids", ids_t)];
        if declared.iter().any(|n| n == "attention_mask") {
            inputs.push(("attention_mask", mask_t));
        }
        if declared.iter().any(|n| n == "token_type_ids") {
            inputs.push(("token_type_ids", types_t));
        }

        let outputs = session.run(inputs)?;
        // Prefer the named output; fall back to the first output in case a
        // model names it differently. `fallback` lives at this scope so the
        // `&Value` (borrowing the owned ValueRef) outlives the match.
        let fallback = outputs.iter().next();
        let last_hidden: &Value = match outputs.get("last_hidden_state") {
            Some(v) => v,
            None => {
                let (_, v) = fallback
                    .as_ref()
                    .with_context(|| "ONNX model produced no output")?;
                v
            }
        };
        let (_, data) = last_hidden.try_extract_tensor::<f32>()?;

        // Mean-pool over non-padded tokens, then L2-normalize
        // (sentence-transformers default pooling) so the L2 space Chroma
        // uses behaves like cosine similarity.
        let mut result = Vec::with_capacity(batch);
        let mut offset = 0usize;
        for b in 0..batch {
            let mut sum = vec![0.0f32; EMBEDDING_DIM];
            let mut count = 0usize;
            for s in 0..seq_len {
                if mask_arr[[b, s]] != 0 {
                    for d in 0..EMBEDDING_DIM {
                        sum[d] += data[offset + d];
                    }
                    offset += EMBEDDING_DIM;
                    count += 1;
                } else {
                    offset += EMBEDDING_DIM;
                }
            }
            if count == 0 {
                result.push(vec![0.0; EMBEDDING_DIM]);
                continue;
            }
            let norm = sum.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-12);
            result.push(sum.iter().map(|v| v / norm).collect());
        }
        Ok(result)
    }
}

impl Default for OnnxEmbeddingFunction {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EmbeddingFunction for OnnxEmbeddingFunction {
    async fn embed(&self, docs: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let t0 = std::time::Instant::now();
        let loaded = self.loaded().await?;
        let encodings = loaded
            .tokenizer
            .encode_batch(docs.to_vec(), true)
            .map_err(|e| anyhow::anyhow!("tokenize batch: {}", e))?;
        let mut session = loaded
            .session
            .lock()
            .map_err(|_| anyhow::anyhow!("ONNX session lock is poisoned"))?;
        let vectors = self.pool_batch(&mut session, &encodings)?;
        println!("[chroma] embed {} doc(s) in {:?}", docs.len(), t0.elapsed());
        Ok(vectors)
    }
}

/// Validate an existing model file or download the pinned, reviewed bytes.
async fn ensure_model_file(dest: &Path, file: &str, expected_sha256: &str) -> anyhow::Result<()> {
    if dest.exists() {
        if file_sha256(dest).await? == expected_sha256 {
            return Ok(());
        }
        eprintln!(
            "[chroma] checksum mismatch for {}; downloading a clean copy",
            dest.display()
        );
        std::fs::remove_file(dest)
            .with_context(|| format!("remove invalid model file {}", dest.display()))?;
    }

    let mut last_err: Option<anyhow::Error> = None;
    for mirror in model_mirrors() {
        let url = format!(
            "{}/{}/resolve/{}/{}",
            mirror, MODEL_REPO, MODEL_REVISION, file
        );
        match download_file(&url, dest, expected_sha256).await {
            Ok(()) => {
                println!(
                    "[chroma] downloaded {} ({} bytes)",
                    file,
                    dest.metadata().map(|m| m.len()).unwrap_or(0)
                );
                return Ok(());
            }
            Err(error) => {
                eprintln!("[chroma] download failed from {}: {}", mirror, error);
                last_err = Some(error);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no model mirrors to try")))
}

async fn file_sha256(path: &Path) -> anyhow::Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut file = std::fs::File::open(&path)
            .with_context(|| format!("open model file {}", path.display()))?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 1024 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok::<_, anyhow::Error>(format!("{:x}", hasher.finalize()))
    })
    .await
    .map_err(|error| anyhow::anyhow!("model checksum task failed: {}", error))?
}

/// Stream a URL to a temp file, validate its SHA-256, then rename it.
async fn download_file(url: &str, dest: &Path, expected_sha256: &str) -> anyhow::Result<()> {
    let mut response = reqwest::Client::new()
        .get(url)
        .timeout(std::time::Duration::from_secs(600))
        .send()
        .await?
        .error_for_status()?;
    let tmp = dest.with_extension("part");
    let _ = tokio::fs::remove_file(&tmp).await;

    let result = async {
        let mut output = tokio::fs::File::create(&tmp).await?;
        let mut hasher = Sha256::new();
        while let Some(chunk) = response.chunk().await? {
            hasher.update(&chunk);
            output.write_all(&chunk).await?;
        }
        output.sync_all().await?;
        let actual = format!("{:x}", hasher.finalize());
        if actual != expected_sha256 {
            anyhow::bail!(
                "SHA-256 mismatch for {}: expected {}, got {}",
                url,
                expected_sha256,
                actual
            );
        }
        tokio::fs::rename(&tmp, dest).await?;
        Ok(())
    }
    .await;

    if result.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    result
}
