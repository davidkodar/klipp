//! CLIP ViT-B/32 (#2212): pictures and words in one space, so frames can
//! be found by describing them. OpenAI's weights (MIT), as safetensors,
//! and the model as candle-transformers has it; its byte-level BPE
//! tokenizer is read here from the model's `tokenizer.json`.

/// The model's and its tokenizer's ids in the manifest.
pub const CLIP_MODEL_ID: &str = "clip_vit_b32";
pub const CLIP_TOKENIZER_ID: &str = "clip_vit_b32_tokenizer";

/// The side of the square picture the model sees.
pub const IMAGE_SIZE: u32 = 224;

/// An embedding's length.
pub const DIM: usize = 512;

const CONTEXT: usize = 77;
const START: u32 = 49406;
const END: u32 = 49407;
const MEAN: [f32; 3] = [0.481_454_66, 0.457_827_5, 0.408_210_73];
const STD: [f32; 3] = [0.268_629_54, 0.261_302_6, 0.275_777_1];

/// CLIP's tokenizer: words split as its pre-tokenizer splits them, each
/// word's UTF-8 bytes as GPT-2's printable characters with `</w>` on the
/// last, merged pair by pair in rank order.
pub struct Tokenizer {
    vocab: std::collections::HashMap<String, u32>,
    ranks: std::collections::HashMap<(String, String), usize>,
    bytes: [char; 256],
}

impl Tokenizer {
    /// From a `tokenizer.json` (a BPE model with string merges).
    pub fn from_json(text: &str) -> Result<Self, String> {
        let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("tokenizer: {e}"))?;
        let model = &v["model"];
        let vocab: std::collections::HashMap<String, u32> = serde_json::from_value(model["vocab"].clone()).map_err(|e| format!("tokenizer vocab: {e}"))?;
        let merges = model["merges"].as_array().ok_or("tokenizer: no merges")?;
        let mut ranks = std::collections::HashMap::with_capacity(merges.len());
        for (rank, m) in merges.iter().enumerate() {
            let pair = match m {
                serde_json::Value::String(s) => s.split_once(' ').map(|(a, b)| (a.to_string(), b.to_string())),
                serde_json::Value::Array(a) => match (a.first().and_then(|x| x.as_str()), a.get(1).and_then(|x| x.as_str())) {
                    (Some(x), Some(y)) => Some((x.to_string(), y.to_string())),
                    _ => None,
                },
                _ => None,
            };
            if let Some(p) = pair {
                ranks.insert(p, rank);
            }
        }
        Ok(Self { vocab, ranks, bytes: byte_chars() })
    }

    /// `text`'s ids, between the start and end tokens, cut to the
    /// model's 77 and padded with 0 (pooling reads the end token, the
    /// highest id, so padding must stay below it).
    pub fn encode(&self, text: &str) -> Vec<u32> {
        let mut ids = vec![START];
        for word in words(&text.to_lowercase()) {
            let mut parts: Vec<String> = word.bytes().map(|b| self.bytes[b as usize].to_string()).collect();
            if let Some(last) = parts.last_mut() {
                last.push_str("</w>");
            }
            loop {
                let best = parts.windows(2).enumerate().filter_map(|(i, w)| self.ranks.get(&(w[0].clone(), w[1].clone())).map(|r| (*r, i))).min();
                let Some((_, i)) = best else { break };
                let joined = format!("{}{}", parts[i], parts[i + 1]);
                parts.splice(i..i + 2, [joined]);
            }
            ids.extend(parts.iter().filter_map(|p| self.vocab.get(p)));
        }
        ids.truncate(CONTEXT - 1);
        ids.push(END);
        ids.resize(CONTEXT, 0);
        ids
    }
}

/// GPT-2's bytes as printable characters: the printable ones as
/// themselves, the rest from U+0100 on.
fn byte_chars() -> [char; 256] {
    let mut out = ['\0'; 256];
    let mut next = 256u32;
    for b in 0..256u32 {
        let printable = (33..=126).contains(&b) || (161..=172).contains(&b) || (174..=255).contains(&b);
        out[b as usize] = if printable {
            char::from_u32(b).unwrap_or('?')
        } else {
            let c = char::from_u32(next).unwrap_or('?');
            next += 1;
            c
        };
    }
    out
}

/// The words CLIP's pre-tokenizer makes of lowercased text: a contraction
/// ('s 't 're 've 'm 'll 'd), a run of letters, a single digit, or a run
/// of anything else that is not a space.
fn words(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '\'' {
            let rest: String = chars[i + 1..chars.len().min(i + 3)].iter().collect();
            if let Some(n) = ["re", "ve", "ll", "s", "t", "m", "d"].iter().find(|s| rest.starts_with(**s)).map(|s| s.len()) {
                out.push(chars[i..=i + n].iter().collect());
                i += n + 1;
                continue;
            }
        }
        let start = i;
        if c.is_alphabetic() {
            while i < chars.len() && chars[i].is_alphabetic() {
                i += 1;
            }
        } else if c.is_numeric() {
            i += 1;
        } else {
            while i < chars.len() && !chars[i].is_whitespace() && !chars[i].is_alphabetic() && !chars[i].is_numeric() {
                i += 1;
            }
        }
        out.push(chars[start..i].iter().collect());
    }
    out
}

#[cfg(feature = "candle")]
mod net {
    use super::*;
    use candle_core::{DType, Device, Tensor};
    use candle_transformers::models::clip::{ClipConfig, ClipModel};

    pub struct Clip {
        device: Device,
        model: ClipModel,
    }

    impl Clip {
        pub fn new(bytes: Vec<u8>, device: Device) -> candle_core::Result<Self> {
            let vb = candle_nn::VarBuilder::from_buffered_safetensors(bytes, DType::F32, &device)?;
            Ok(Self { model: ClipModel::new(vb, &ClipConfig::vit_base_patch32())?, device })
        }

        pub fn images(&self, rgb: &[Vec<u8>]) -> candle_core::Result<Vec<Vec<f32>>> {
            let n = (IMAGE_SIZE * IMAGE_SIZE) as usize;
            let mut chw = Vec::with_capacity(rgb.len() * 3 * n);
            for img in rgb {
                for (c, (m, s)) in MEAN.iter().zip(STD).enumerate() {
                    chw.extend((0..n).map(|p| (img[p * 3 + c] as f32 / 255.0 - m) / s));
                }
            }
            let s = IMAGE_SIZE as usize;
            let x = Tensor::from_vec(chw, (rgb.len(), 3, s, s), &self.device)?;
            unit(&self.model.get_image_features(&x)?)
        }

        pub fn text(&self, ids: &[u32]) -> candle_core::Result<Vec<f32>> {
            let x = Tensor::new(ids, &self.device)?.unsqueeze(0)?;
            Ok(unit(&self.model.get_text_features(&x)?)?.remove(0))
        }
    }

    fn unit(v: &Tensor) -> candle_core::Result<Vec<Vec<f32>>> {
        candle_transformers::models::clip::div_l2_norm(v)?.to_vec2::<f32>()
    }
}

/// The loaded model and its tokenizer.
pub struct Clip {
    tokenizer: Tokenizer,
    #[cfg(feature = "candle")]
    net: net::Clip,
}

impl Clip {
    /// The weights at `weights` and the tokenizer at `tokenizer`, on the
    /// GPU when candle has one, else the CPU.
    pub fn load(weights: &std::path::Path, tokenizer: &std::path::Path) -> Result<Self, String> {
        let tokenizer = Tokenizer::from_json(&std::fs::read_to_string(tokenizer).map_err(|e| format!("{}: {e}", tokenizer.display()))?)?;
        #[cfg(feature = "candle")]
        {
            let bytes = std::fs::read(weights).map_err(|e| format!("{}: {e}", weights.display()))?;
            let device = candle_core::Device::cuda_if_available(0).unwrap_or(candle_core::Device::Cpu);
            let net = net::Clip::new(bytes, device).map_err(|e| format!("loading the picture search model: {e}"))?;
            Ok(Self { tokenizer, net })
        }
        #[cfg(not(feature = "candle"))]
        {
            let _ = (weights, tokenizer);
            Err("this build has no candle backend (build with ai-candle)".into())
        }
    }

    /// Each picture (224×224 RGB, row by row) as a unit-length embedding.
    pub fn embed_images(&self, rgb: &[Vec<u8>]) -> Result<Vec<Vec<f32>>, String> {
        let n = (IMAGE_SIZE * IMAGE_SIZE * 3) as usize;
        if rgb.iter().any(|p| p.len() != n) {
            return Err(format!("pictures must be {IMAGE_SIZE}x{IMAGE_SIZE} RGB"));
        }
        if rgb.is_empty() {
            return Ok(Vec::new());
        }
        #[cfg(feature = "candle")]
        {
            self.net.images(rgb).map_err(|e| format!("embedding pictures: {e}"))
        }
        #[cfg(not(feature = "candle"))]
        Err("this build has no candle backend (build with ai-candle)".into())
    }

    /// `text` as a unit-length embedding in the pictures' space.
    pub fn embed_text(&self, text: &str) -> Result<Vec<f32>, String> {
        let ids = self.tokenizer.encode(text);
        #[cfg(feature = "candle")]
        {
            self.net.text(&ids).map_err(|e| format!("embedding words: {e}"))
        }
        #[cfg(not(feature = "candle"))]
        {
            let _ = ids;
            Err("this build has no candle backend (build with ai-candle)".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_split_as_clip_splits_them() {
        assert_eq!(words("a dog's 2 balls, on a beach!!"), vec!["a", "dog", "'s", "2", "balls", ",", "on", "a", "beach", "!!"]);
        assert_eq!(words("close-up of hands"), vec!["close", "-", "up", "of", "hands"]);
    }

    #[test]
    fn bytes_map_to_printable_characters() {
        let b = byte_chars();
        assert_eq!(b[b'a' as usize], 'a');
        assert_eq!(b[b' ' as usize], '\u{120}');
        let unique: std::collections::HashSet<char> = b.iter().copied().collect();
        assert_eq!(unique.len(), 256);
    }

    /// With CLIP's `tokenizer.json` at `KLIPP_CLIP_TOKENIZER`, words come
    /// out as the reference tokenizer gives them.
    #[test]
    fn words_encode_as_the_reference_tokenizer() {
        let Ok(path) = std::env::var("KLIPP_CLIP_TOKENIZER") else {
            eprintln!("skipping: set KLIPP_CLIP_TOKENIZER to tokenizer.json");
            return;
        };
        let t = Tokenizer::from_json(&std::fs::read_to_string(path).unwrap()).unwrap();
        let ids = t.encode("a photo of a dog");
        // From the reference tokenizer.
        assert_eq!(&ids[..7], &[49406, 320, 1125, 539, 320, 1929, 49407]);
        assert_eq!(ids.len(), 77);
        assert!(ids[7..].iter().all(|i| *i == 0));
    }

    /// With the weights at `KLIPP_CLIP` too, a red picture is nearer
    /// "red" than "blue", and a blue one the other way round.
    #[test]
    fn a_colour_is_nearest_its_name() {
        let (Ok(weights), Ok(tok)) = (std::env::var("KLIPP_CLIP"), std::env::var("KLIPP_CLIP_TOKENIZER")) else {
            eprintln!("skipping: set KLIPP_CLIP and KLIPP_CLIP_TOKENIZER");
            return;
        };
        let clip = Clip::load(std::path::Path::new(&weights), std::path::Path::new(&tok)).unwrap();
        let n = (IMAGE_SIZE * IMAGE_SIZE) as usize;
        let fill = |c: [u8; 3]| (0..n).flat_map(|_| c).collect::<Vec<u8>>();
        let pics = clip.embed_images(&[fill([220, 20, 20]), fill([20, 40, 220])]).unwrap();
        let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        let (red, blue) = (clip.embed_text("a red picture").unwrap(), clip.embed_text("a blue picture").unwrap());
        assert!(dot(&pics[0], &red) > dot(&pics[0], &blue));
        assert!(dot(&pics[1], &blue) > dot(&pics[1], &red));
        assert!((dot(&red, &red) - 1.0).abs() < 1e-4);
    }
}
