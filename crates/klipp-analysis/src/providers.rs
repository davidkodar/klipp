//! What the analysis models run on (#2250): the ONNX Runtime provider
//! each model's session got, recorded by `klipp_ml_ort::provider`, in
//! this process or handed back by a worker's reply.

/// What each model opened so far runs on, a line each.
pub fn lines() -> Vec<String> {
    #[cfg(feature = "ort-models")]
    {
        klipp_ml_ort::provider::used().iter().map(|u| u.line()).collect()
    }
    #[cfg(not(feature = "ort-models"))]
    {
        Vec::new()
    }
}

/// The lines of the models on the CPU with a GPU present that they
/// don't run on.
pub fn missed_gpu() -> Vec<String> {
    #[cfg(feature = "ort-models")]
    {
        klipp_ml_ort::provider::used().iter().filter(|u| u.misses_a_gpu()).map(|u| u.line()).collect()
    }
    #[cfg(not(feature = "ort-models"))]
    {
        Vec::new()
    }
}

/// This process's record, for a worker's reply.
pub(crate) fn to_json() -> Vec<serde_json::Value> {
    #[cfg(feature = "ort-models")]
    {
        klipp_ml_ort::provider::used().iter().map(|u| serde_json::json!({ "model": u.model, "provider": u.provider, "why_cpu": u.why_cpu })).collect()
    }
    #[cfg(not(feature = "ort-models"))]
    {
        Vec::new()
    }
}

/// A worker's record, kept as this process's own.
pub(crate) fn record_json(used: &serde_json::Value) {
    #[cfg(feature = "ort-models")]
    for u in used.as_array().into_iter().flatten() {
        let (Some(model), Some(provider)) = (u["model"].as_str(), u["provider"].as_str()) else { continue };
        let provider = if provider == "CUDA" { "CUDA" } else { "CPU" };
        let why_cpu = u["why_cpu"].as_str().map(str::to_string);
        klipp_ml_ort::provider::record(klipp_ml_ort::provider::ProviderUse { model: model.to_string(), provider, why_cpu });
    }
    #[cfg(not(feature = "ort-models"))]
    let _ = used;
}

#[cfg(all(test, feature = "ort-models"))]
mod tests {
    /// A worker's record comes back as the editor's own.
    #[test]
    fn a_workers_providers_are_kept_by_the_editor() {
        let sent = serde_json::json!([{ "model": "providers-test", "provider": "CPU", "why_cpu": "an NVIDIA GPU is present, but x" }]);
        super::record_json(&sent);
        let got = klipp_ml_ort::provider::used_by("providers-test").expect("kept");
        assert!(got.misses_a_gpu());
        assert!(super::lines().iter().any(|l| l == "providers-test on CPU: an NVIDIA GPU is present, but x"));
    }
}
