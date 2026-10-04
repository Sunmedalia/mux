//! Shared tokenizer for bounded blocking workers.
use super::*;

pub(super) fn estimate_tokens(value: &Value) -> Result<usize> {
    let encoded = serde_json::to_string(value)?;
    static BPE: std::sync::OnceLock<std::result::Result<tiktoken_rs::CoreBPE, String>> =
        std::sync::OnceLock::new();
    let bpe = BPE
        .get_or_init(|| {
            tiktoken_rs::cl100k_base().map_err(|_| "failed to initialize token estimator".into())
        })
        .as_ref()
        .map_err(|error| anyhow::anyhow!(error.clone()))?;
    Ok(bpe.encode_with_special_tokens(&encoded).len())
}
