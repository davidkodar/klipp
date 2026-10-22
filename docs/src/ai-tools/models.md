# Models & Manager

Klipp ships a catalogue of AI models that can be downloaded on demand from the **AI Models** dialog. All models are verified by SHA-256 against a manifest bundled with Klipp; bytes that don't match are rejected.

Open the dialog from **Tools → AI Models…**, from the AI Tools panel footer (`Manage AI models`), or via the [Command Palette](../ui/command-palette.md).

## Bundled Catalogue

| Model | Size | License | Runtime | Used for |
|---|---|---|---|---|
| **Whisper Large v3 Turbo (q5)** | 574 MB | MIT | whisper.cpp (Vulkan) | Speech transcription |
| **Whisper Base** | 148 MB | MIT | whisper.cpp (Vulkan) | Transcription on a slow machine |
| **BiRefNet Lite** | 224 MB | MIT | ort | Remove Background |
| **MobileSAM** | 40 MB | Apache-2.0 | candle | Isolate Object (point / box / scribble) |
| **RF-DETR-Seg (Preview)** | 134 MB | Apache-2.0 | ort | Detect Objects, fast |
| **RF-DETR-Seg (Medium)** | 139 MB | Apache-2.0 | ort | Detect Objects and framing analysis |
| **YuNet** | 233 KB | MIT | ort | Face detection for the colour measurements |
| **PP-OCR detector + recogniser** | 2.4 MB + 7.7 MB | Apache-2.0 | ort | On-screen text in screen recordings |
| **Piper voices** (LJSpeech, the fallback where Kokoro cannot run; Kristin, Norman, John, LibriTTS; Cori UK; NST Swedish) | 63–137 MB each | public domain or CC0; LibriTTS CC BY 4.0 | Piper | Scratch narration, chosen in Window → Narrate |
| **Kokoro-82M** (8-bit) and 10 voices, one download (Heart, the default; Bella, Nicole, Michael, Fenrir, Puck; Emma, Isabella, George, Fable) | 86 MB + 0.5 MB a voice | Apache-2.0 | ort, espeak-ng | Natural-sounding narration |
| **Piper en_US Lessac (medium)** | 63 MB | research only, **not for commercial use** | Piper | Kept for older projects that use it |
| **DeepFilterNet3** | 8 MB | MIT OR Apache-2.0 | tract | Voice Isolation (speech enhancement) |
| **OPUS-MT** (Helsinki-NLP; one model a pair: English to and from German, French, Spanish, Swedish, Italian, Dutch, English to Finnish and Danish) | about 300 MB + 5 MB a pair | CC BY 4.0, Tiedemann & Thottingal 2020 | candle | Translate Captions, on a subtitle track's menu |
| **PANNs Cnn10** (Kong et al.) | 25 MB | CC BY 4.0 | candle | Audio roles: whether a clip's sound is speech, music, effects or ambience |
| **CLIP ViT-B/32** (OpenAI) and its tokenizer | 605 MB + 2 MB | MIT | candle | Find by Picture: footage found by a description of what is in it |

## Hosting

Klipp does not redistribute model weights. The bundled manifest points at upstream sources:

- BiRefNet Lite — [huggingface.co/onnx-community/BiRefNet_lite-ONNX](https://huggingface.co/onnx-community/BiRefNet_lite-ONNX)
- MobileSAM — [huggingface.co/lmz/candle-sam](https://huggingface.co/lmz/candle-sam) (mirrored from upstream)
- OPUS-MT — [huggingface.co/Helsinki-NLP](https://huggingface.co/Helsinki-NLP) (weights, e.g. opus-mt-en-de) and [huggingface.co/Xenova](https://huggingface.co/Xenova) (each pair's tokenizer.json)
- RF-DETR-Seg Preview — [huggingface.co/bukuroo/RF-DETR-Seg-ONNX](https://huggingface.co/bukuroo/RF-DETR-Seg-ONNX)
- RF-DETR-Seg Medium — [huggingface.co/davidkodar/rf-detr-seg-medium-onnx](https://huggingface.co/davidkodar/rf-detr-seg-medium-onnx) (Klipp-hosted mirror; upstream Roboflow has not published a Medium ONNX yet)

The trade-off is that upstream URLs can move or break — when they do, Klipp ships a manifest update in a patch release. SHA-256 verification means that if upstream replaces a file with bytes Klipp has not blessed, the download is rejected and the user is told.

## The Dialog

Each row shows:

- **Status dot** — green (installed), amber (downloading), grey (not installed), red (verification failed)
- **Display name** with a `RECOMMENDED` badge if it is the default for any task
- **(i) icon** — opens a model details modal with full attribution, license text, and source URL
- **Three-dot kebab menu** with six actions:
  - Show details
  - Verify integrity (re-hash the on-disk file)
  - Reinstall (re-download)
  - Show in cache folder
  - Copy model info (clipboard)
  - Uninstall…

The list supports filter by task, sort by Recommended / Display name / Size / Last used / License, and a search box.

## On-Disk Layout

```
~/.local/share/klipp/models/
  birefnet_lite_v1.bin
  rf_detr_seg_medium_v1.bin
  whisper_large_v3_turbo_q5.bin
  yunet_face_2023mar.bin
  ppocr_en_det_v3.bin
  ...
```

One file per model, named by its manifest id, each verified by SHA-256
against `assets/models.toml` before it is used. Removing a file is all
an uninstall does; the next request downloads it again.

## Telemetry

Klipp records per-model average inference time and last-used timestamp in `.telemetry.json` next to each model. This data is **never** transmitted — it is used only to drive the AI Tools running-state progress bar and the *Last used* sort key.

To wipe telemetry: delete the `.telemetry.json` files, or use **Uninstall** from the kebab menu (which removes the model and its telemetry together).

## Custom Manifest

Power users can override the bundled manifest by placing `models.toml` at `~/.config/klipp/models.toml`. Klipp loads the user manifest first; missing entries fall back to the bundled one. The schema is documented in `crates/klipp-ml/src/manifest.rs`. Required fields per entry:

```toml
[[model]]
id = "stable_id"
display_name = "User-facing name"
size_bytes = 12345678
sha256 = "64-char-lowercase-hex"
license = "Apache-2.0"
attribution = "Credit shown in consent dialog"
sources = [
    { url = "https://..." },
]
kind = "mask_generator"     # see manifest.rs for all kinds
runtime = "ort"             # or "candle"
prompts = "point | box | scribble"   # or "" for automatic models
```

Downloads exceeding `size_bytes` by more than 5 % are rejected — a guard against malicious or runaway downloads.

## License Audit

Klipp's permissive distribution model (GPL-3.0 with permissive bundled assets) imposes hard licence limits on which models can ship in the catalogue:

**On the GO list** — BiRefNet (MIT), U²-Net (Apache-2.0), MODNet (Apache-2.0), RF-DETR-Seg (Apache-2.0), MobileSAM (Apache-2.0), SAM family (Apache-2.0), Whisper (MIT).

**Off-limits** — RMBG (CC-BY-NC: non-commercial-only), Ultralytics YOLO and YOLO-Seg (AGPL-3.0: viral copyleft would force Klipp to AGPL), YOLO-NAS-Seg (Deci-restricted custom licence).

When proposing new models for inclusion, weights licence — not just code licence — is the gate.
