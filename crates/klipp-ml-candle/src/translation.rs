//! Text translation for captions (#2213): the OPUS-MT models
//! (Helsinki-NLP, Marian transformers, CC-BY-4.0), one model a language
//! pair, run through candle-transformers' Marian, with the pair's
//! SentencePiece unigram vocabulary read from its `tokenizer.json`.
//!
//! Every base OPUS-MT model has the same shape (512 wide, six encoder
//! and six decoder layers, eight heads, swish); a pair differs only in
//! its vocabulary, so [`PAIRS`] carries that and the rest is
//! `marian_config`. The tokenizer is read here rather than through a
//! tokenizer crate: whitespace split, a `▁` before each word, the best
//! unigram segmentation of each word, and `</s>` after the last; it is
//! what the pair's `tokenizer.json` describes, and nothing more.

use std::collections::HashMap;

/// A language pair Klipp can translate, by ISO 639-1 code, and the size
/// of its vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pair {
    pub source: &'static str,
    pub target: &'static str,
    pub vocab: usize,
}

impl Pair {
    /// The manifest id of the pair's model.
    pub fn model_id(&self) -> String {
        format!("opus_mt_{}_{}", self.source, self.target)
    }

    /// The manifest id of the pair's tokenizer.
    pub fn tokenizer_id(&self) -> String {
        format!("opus_mt_{}_{}_tokenizer", self.source, self.target)
    }
}

/// The pairs in the model manifest.
pub const PAIRS: &[Pair] = &[
    Pair { source: "en", target: "de", vocab: 58101 },
    Pair { source: "en", target: "fr", vocab: 59514 },
    Pair { source: "en", target: "es", vocab: 65001 },
    Pair { source: "en", target: "sv", vocab: 56434 },
    Pair { source: "en", target: "it", vocab: 80035 },
    Pair { source: "en", target: "nl", vocab: 67028 },
    Pair { source: "en", target: "fi", vocab: 65001 },
    Pair { source: "en", target: "da", vocab: 58930 },
    Pair { source: "de", target: "en", vocab: 58101 },
    Pair { source: "fr", target: "en", vocab: 59514 },
    Pair { source: "es", target: "en", vocab: 65001 },
    Pair { source: "sv", target: "en", vocab: 56434 },
    Pair { source: "it", target: "en", vocab: 80379 },
    Pair { source: "nl", target: "en", vocab: 67028 },
];

/// The pair from `source` to `target`, when Klipp has one.
pub fn pair(source: &str, target: &str) -> Option<Pair> {
    PAIRS.iter().copied().find(|p| p.source == source && p.target == target)
}

/// A language's name in English, for menus.
pub fn language_name(code: &str) -> &'static str {
    match code {
        "en" => "English",
        "de" => "German",
        "fr" => "French",
        "es" => "Spanish",
        "sv" => "Swedish",
        "it" => "Italian",
        "nl" => "Dutch",
        "fi" => "Finnish",
        "da" => "Danish",
        _ => "?",
    }
}

/// A pair's SentencePiece unigram vocabulary, as its `tokenizer.json`
/// gives it: each piece's id is its place in the list.
#[derive(Debug, Clone)]
pub struct Unigram {
    pieces: Vec<String>,
    lookup: HashMap<String, (u32, f64)>,
    longest: usize,
    unk: u32,
    eos: u32,
    pad: Option<u32>,
}

/// What a `tokenizer.json` could not be read as.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenizerError(pub String);

impl std::fmt::Display for TokenizerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TokenizerError {}

impl Unigram {
    /// The vocabulary in `json`, a pair's `tokenizer.json`.
    pub fn from_json(json: &str) -> Result<Self, TokenizerError> {
        let v: serde_json::Value = serde_json::from_str(json).map_err(|e| TokenizerError(format!("not JSON: {e}")))?;
        let model = &v["model"];
        if model["type"] != "Unigram" {
            return Err(TokenizerError("not a unigram tokenizer".into()));
        }
        let vocab = model["vocab"].as_array().ok_or_else(|| TokenizerError("no vocabulary".into()))?;
        let mut pieces = Vec::with_capacity(vocab.len());
        let mut lookup = HashMap::with_capacity(vocab.len());
        let mut longest = 1;
        for (id, entry) in vocab.iter().enumerate() {
            let piece = entry[0].as_str().ok_or_else(|| TokenizerError(format!("entry {id} has no piece")))?.to_string();
            let score = entry[1].as_f64().unwrap_or(f64::NEG_INFINITY);
            longest = longest.max(piece.chars().count());
            lookup.entry(piece.clone()).or_insert((id as u32, score));
            pieces.push(piece);
        }
        // By name: the file's own unk_id does not always point at it.
        let id_of = |name: &str| lookup.get(name).map(|(id, _)| *id);
        let unk = id_of("<unk>").ok_or_else(|| TokenizerError("no <unk> piece".into()))?;
        let eos = id_of("</s>").ok_or_else(|| TokenizerError("no </s> piece".into()))?;
        Ok(Self { pad: id_of("<pad>"), pieces, lookup, longest, unk, eos })
    }

    pub fn eos(&self) -> u32 {
        self.eos
    }

    pub fn len(&self) -> usize {
        self.pieces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pieces.is_empty()
    }

    /// `text`'s ids: each word, with `▁` before it, cut into the pieces
    /// whose scores add up to the most, then `</s>`. A character no
    /// piece covers is `<unk>`.
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let mut ids = Vec::new();
        for word in text.split_whitespace() {
            let chars: Vec<char> = std::iter::once('\u{2581}').chain(word.chars()).collect();
            let n = chars.len();
            // best[i]: the most score reaching char i, and the piece that ends there.
            let mut best: Vec<(f64, usize, Option<u32>)> = vec![(f64::NEG_INFINITY, 0, None); n + 1];
            best[0].0 = 0.0;
            for end in 1..=n {
                for start in end.saturating_sub(self.longest)..end {
                    if best[start].0 == f64::NEG_INFINITY {
                        continue;
                    }
                    let piece: String = chars[start..end].iter().collect();
                    let candidate = match self.lookup.get(&piece) {
                        Some(&(id, score)) => Some((best[start].0 + score, id)),
                        // One character nothing covers: unknown, and costly.
                        None if end - start == 1 => Some((best[start].0 - 100.0, self.unk)),
                        None => None,
                    };
                    if let Some((score, id)) = candidate {
                        if score > best[end].0 {
                            best[end] = (score, start, Some(id));
                        }
                    }
                }
            }
            let mut word_ids = Vec::new();
            let mut at = n;
            while at > 0 {
                let (_, start, id) = best[at];
                word_ids.push(id.unwrap_or(self.unk));
                at = start;
            }
            word_ids.reverse();
            ids.extend(word_ids);
        }
        ids.push(self.eos);
        ids
    }

    /// The text `ids` spell, `▁` read as a space and the special pieces
    /// left out.
    pub fn decode(&self, ids: &[u32]) -> String {
        let mut out = String::new();
        for &id in ids {
            if id == self.eos || id == self.unk || Some(id) == self.pad {
                continue;
            }
            if let Some(piece) = self.pieces.get(id as usize) {
                out.push_str(piece);
            }
        }
        out.replace('\u{2581}', " ").trim().to_string()
    }
}

/// The paths of `pair`'s model and tokenizer in the model cache,
/// downloaded first when `download` allows and they are not there yet.
pub fn ensure_pair(pair: &Pair, download: bool) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let manifest = klipp_ml::manifest::load_effective().map_err(|e| format!("model manifest: {e:?}"))?;
    let cache = klipp_ml::cache::ModelCache::open().map_err(|e| format!("model cache: {e:?}"))?;
    let mut paths = Vec::new();
    for id in [pair.model_id(), pair.tokenizer_id()] {
        let entry = manifest.get(&id).ok_or_else(|| format!("no manifest entry {id}"))?;
        let path = cache.path_for(entry);
        if !path.is_file() {
            if !download {
                return Err(format!("the translation model {id} is not downloaded"));
            }
            let cancel = std::sync::atomic::AtomicBool::new(false);
            let mut progress = klipp_ml::download::NoOpProgress;
            klipp_ml::download::download_to_cache(&cache, entry, &mut progress, &cancel).map_err(|e| format!("download of {id}: {e:?}"))?;
        }
        paths.push(path);
    }
    let tokenizer = paths.pop().expect("two paths");
    let model = paths.pop().expect("two paths");
    Ok((model, tokenizer))
}

/// `lines` translated from `source` to `target`, one for one, the model
/// fetched first when it is not in the cache.
pub fn translate_lines(source: &str, target: &str, lines: &[String]) -> Result<Vec<String>, String> {
    let pair = pair(source, target).ok_or_else(|| format!("no translation from {} to {}", language_name(source), language_name(target)))?;
    #[cfg(feature = "candle")]
    {
        let (model, tokenizer) = ensure_pair(&pair, true)?;
        let mut t = Translator::load(&pair, &model, &tokenizer).map_err(|e| e.to_string())?;
        lines.iter().map(|l| t.translate(l).map_err(|e| e.to_string())).collect()
    }
    #[cfg(not(feature = "candle"))]
    {
        let _ = (pair, lines);
        Err("this build has no translation (built without candle)".into())
    }
}

/// The longest translation of a line of `source_tokens` tokens: three
/// times as long and ten more, never past 256. A sentence's translation
/// is seldom twice its length; past that greedy decoding is looping.
pub fn output_limit(source_tokens: usize) -> usize {
    (source_tokens * 3 + 10).min(256)
}

/// Whether the last three tokens of `tokens` came just before as well:
/// greedy decoding caught in a loop.
pub fn repeats_itself(tokens: &[u32]) -> bool {
    let n = tokens.len();
    n >= 6 && tokens[n - 3..] == tokens[n - 6..n - 3]
}

#[cfg(feature = "candle")]
pub use model::{Translator, TranslateError};

#[cfg(feature = "candle")]
mod model {
    use super::{Pair, Unigram};
    use candle_core::{DType, Device, Tensor};
    use candle_transformers::models::marian;

    /// What a translation could not do.
    #[derive(Debug)]
    pub struct TranslateError(pub String);

    impl std::fmt::Display for TranslateError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0)
        }
    }

    impl std::error::Error for TranslateError {}

    fn err(e: impl std::fmt::Display) -> TranslateError {
        TranslateError(e.to_string())
    }

    /// The shape every base OPUS-MT model shares, with `pair`'s vocabulary:
    /// its last id is both the decoder's start and the padding.
    pub fn marian_config(pair: &Pair) -> marian::Config {
        marian::Config {
            activation_function: candle_nn::Activation::Swish,
            d_model: 512,
            decoder_attention_heads: 8,
            decoder_ffn_dim: 2048,
            decoder_layers: 6,
            decoder_start_token_id: (pair.vocab - 1) as u32,
            decoder_vocab_size: Some(pair.vocab),
            encoder_attention_heads: 8,
            encoder_ffn_dim: 2048,
            encoder_layers: 6,
            eos_token_id: 0,
            forced_eos_token_id: 0,
            is_encoder_decoder: true,
            max_position_embeddings: 512,
            pad_token_id: (pair.vocab - 1) as u32,
            scale_embedding: true,
            share_encoder_decoder_embeddings: true,
            use_cache: true,
            vocab_size: pair.vocab,
        }
    }

    /// A loaded pair, ready to translate line by line.
    pub struct Translator {
        model: marian::MTModel,
        tokenizer: Unigram,
        config: marian::Config,
        device: Device,
    }

    impl Translator {
        /// `pair`'s model from its `model.safetensors` and its tokenizer,
        /// on the GPU when candle has one, else the CPU.
        pub fn load(pair: &Pair, model: &std::path::Path, tokenizer: &std::path::Path) -> Result<Self, TranslateError> {
            let device = Device::cuda_if_available(0).unwrap_or(Device::Cpu);
            let tokenizer = Unigram::from_json(&std::fs::read_to_string(tokenizer).map_err(err)?).map_err(err)?;
            if tokenizer.len() != pair.vocab {
                return Err(TranslateError(format!("the tokenizer has {} pieces, the {}→{} model {}", tokenizer.len(), pair.source, pair.target, pair.vocab)));
            }
            let config = marian_config(pair);
            // Read into memory: the safe load, where a mapped file is not.
            let bytes = std::fs::read(model).map_err(err)?;
            let vb = candle_nn::VarBuilder::from_buffered_safetensors(bytes, DType::F32, &device).map_err(err)?;
            let model = marian::MTModel::new(&config, vb).map_err(err)?;
            Ok(Self { model, tokenizer, config, device })
        }

        /// `text` translated, greedily: each next token the likeliest,
        /// the padding never chosen, until `</s>`, a loop, or
        /// [`super::output_limit`] tokens.
        pub fn translate(&mut self, text: &str) -> Result<String, TranslateError> {
            if text.trim().is_empty() {
                return Ok(String::new());
            }
            let ids = self.tokenizer.encode(text);
            let input = Tensor::new(ids.as_slice(), &self.device).and_then(|t| t.unsqueeze(0)).map_err(err)?;
            let encoded = self.model.encoder().forward(&input, 0).map_err(err)?;
            let mut out = vec![self.config.decoder_start_token_id];
            for step in 0..super::output_limit(ids.len()) {
                let context = if step >= 1 { 1 } else { out.len() };
                let start = out.len().saturating_sub(context);
                let next_in = Tensor::new(&out[start..], &self.device).and_then(|t| t.unsqueeze(0)).map_err(err)?;
                let logits = self.model.decode(&next_in, &encoded, start).map_err(err)?;
                let logits = logits.squeeze(0).and_then(|l| { let last = l.dim(0)? - 1; l.get(last) }).map_err(err)?;
                let mut scores: Vec<f32> = logits.to_dtype(DType::F32).and_then(|l| l.to_vec1()).map_err(err)?;
                if let Some(pad) = scores.get_mut(self.config.pad_token_id as usize) {
                    *pad = f32::NEG_INFINITY;
                }
                let next = scores.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map(|(i, _)| i as u32).unwrap_or(self.config.eos_token_id);
                if next == self.config.eos_token_id || next == self.config.forced_eos_token_id {
                    break;
                }
                out.push(next);
                if super::repeats_itself(&out[1..]) {
                    // A loop ("the the the …"): what came before it is
                    // the translation.
                    out.truncate(out.len() - 3);
                    break;
                }
            }
            self.model.reset_kv_cache();
            Ok(self.tokenizer.decode(&out[1..]))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The real en→de pair, when `KLIPP_OPUS_EN_DE` names a folder
        /// holding its `model.safetensors` and `tokenizer.json`.
        #[test]
        fn english_to_german_reads_as_german() {
            let Ok(dir) = std::env::var("KLIPP_OPUS_EN_DE") else {
                eprintln!("skipping: KLIPP_OPUS_EN_DE not set");
                return;
            };
            let dir = std::path::PathBuf::from(dir);
            let pair = super::super::pair("en", "de").unwrap();
            let mut t = Translator::load(&pair, &dir.join("model.safetensors"), &dir.join("tokenizer.json")).expect("the pair loads");
            let out = t.translate("Good morning, how are you today?").unwrap();
            eprintln!("en→de: {out}");
            assert!(out.contains("Guten Morgen") || out.contains("guten Morgen"), "{out}");
            let out = t.translate("The cat sleeps on the table.").unwrap();
            assert!(out.contains("Katze") && out.contains("Tisch"), "{out}");
            // Degenerate input ends, short, rather than looping to the limit.
            let out = t.translate(&"the ".repeat(20)).unwrap();
            eprintln!("degenerate: {out}");
            assert!(out.split_whitespace().count() <= 30, "{out}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny() -> Unigram {
        let json = r#"{"model":{"type":"Unigram","unk_id":2,"vocab":[
            ["</s>",0.0],["<unk>",0.0],["▁",-5.0],["▁the",-1.0],["▁cat",-1.5],["▁ca",-3.0],["t",-2.0],["s",-2.5],["▁sleep",-2.0],["<pad>",0.0]]}}"#;
        Unigram::from_json(json).unwrap()
    }

    #[test]
    fn words_cut_into_their_likeliest_pieces() {
        let t = tiny();
        assert_eq!(t.encode("the cat"), vec![3, 4, 0], "whole pieces win over smaller ones");
        assert_eq!(t.encode("cats"), vec![4, 7, 0]);
        assert_eq!(t.encode("sleeps"), vec![8, 7, 0]);
        assert_eq!(t.encode("the dog")[..1], [3], "the known word is kept");
        assert!(t.encode("dog").contains(&1), "an uncovered letter is unknown");
    }

    #[test]
    fn ids_read_back_as_text_without_the_special_pieces() {
        let t = tiny();
        assert_eq!(t.decode(&[3, 4, 7, 0, 9]), "the cats");
    }

    #[test]
    fn every_pair_is_named_and_found() {
        for p in PAIRS {
            assert_eq!(pair(p.source, p.target), Some(*p));
            assert_ne!(language_name(p.source), "?");
            assert_ne!(language_name(p.target), "?");
            assert!(p.model_id().starts_with("opus_mt_") && p.tokenizer_id().ends_with("_tokenizer"));
        }
    }

    #[test]
    fn every_pair_has_its_model_and_vocabulary_in_the_manifest() {
        let m = klipp_ml::manifest::ModelManifest::parse(klipp_ml::manifest::BUILTIN_MANIFEST).unwrap();
        for p in PAIRS {
            for id in [p.model_id(), p.tokenizer_id()] {
                let e = m.get(&id).unwrap_or_else(|| panic!("{id} in models.toml"));
                assert_eq!(e.license, "CC-BY-4.0", "{id}");
                assert!(!e.attribution.is_empty(), "{id} credits its makers");
            }
        }
    }

    #[test]
    fn a_looping_translation_is_caught() {
        assert!(!repeats_itself(&[5, 6, 7, 8, 9]));
        assert!(!repeats_itself(&[5, 6, 7, 5, 6, 8]));
        assert!(repeats_itself(&[1, 5, 6, 7, 5, 6, 7]), "the same three tokens twice running");
        assert!(repeats_itself(&[4, 4, 4, 4, 4, 4]), "one token over and over");
        assert_eq!(output_limit(4), 22);
        assert_eq!(output_limit(500), 256, "never past the model's reach");
    }
}
