//! Playing sound for the person at the machine to hear (#1891, #1927):
//! an excerpt, a clip, the mix or search previews, through the default
//! output, for an assistant that cannot hear audio itself.
//!
//! The output stream is not `Send`, and a request may come in on any
//! thread, so one player thread owns it and takes orders over a channel;
//! what it is playing is shared for anyone to read.

use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex, OnceLock};

/// One thing to play: a label for the reply, and its file's bytes (WAV or
/// MP3), played at its own rate and channels.
#[derive(Debug, Clone)]
pub struct Piece {
    pub label: String,
    pub bytes: Vec<u8>,
}

/// What the player is doing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Playing {
    /// The labels queued, in order; empty when idle.
    pub queue: Vec<String>,
    /// Whether it is sounding now.
    pub active: bool,
}

enum Order {
    Play(Vec<Piece>, Sender<Result<(), String>>),
    Stop,
}

struct Player {
    orders: Sender<Order>,
    playing: Arc<Mutex<Playing>>,
}

fn player() -> &'static Player {
    static PLAYER: OnceLock<Player> = OnceLock::new();
    PLAYER.get_or_init(|| {
        let (orders, inbox) = channel::<Order>();
        let playing: Arc<Mutex<Playing>> = Default::default();
        let shared = Arc::clone(&playing);
        std::thread::Builder::new()
            .name("klipp-audition".into())
            .spawn(move || {
                // The device is opened on first use and kept.
                let mut out: Option<(rodio::OutputStream, rodio::OutputStreamHandle)> = None;
                let mut sink: Option<rodio::Sink> = None;
                loop {
                    match inbox.recv_timeout(std::time::Duration::from_millis(200)) {
                        Ok(Order::Play(pieces, reply)) => {
                            if let Some(s) = sink.take() {
                                s.stop();
                            }
                            if out.is_none() {
                                out = rodio::OutputStream::try_default().ok();
                            }
                            let result = match &out {
                                None => Err("there is no audio output device to play through".to_string()),
                                Some((_, handle)) => queue(handle, &pieces).map(|s| sink = Some(s)),
                            };
                            let mut p = shared.lock().unwrap_or_else(|e| e.into_inner());
                            *p = match &result {
                                Ok(()) => Playing { queue: pieces.iter().map(|x| x.label.clone()).collect(), active: true },
                                Err(_) => Playing::default(),
                            };
                            let _ = reply.send(result);
                        }
                        Ok(Order::Stop) => {
                            if let Some(s) = sink.take() {
                                s.stop();
                            }
                            *shared.lock().unwrap_or_else(|e| e.into_inner()) = Playing::default();
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            // Played to the end: idle.
                            if sink.as_ref().is_some_and(|s| s.empty()) {
                                sink = None;
                                *shared.lock().unwrap_or_else(|e| e.into_inner()) = Playing::default();
                            }
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
            })
            .expect("the audition thread");
        Player { orders, playing }
    })
}

/// A sink with `pieces` appended in order, playing.
fn queue(handle: &rodio::OutputStreamHandle, pieces: &[Piece]) -> Result<rodio::Sink, String> {
    let sink = rodio::Sink::try_new(handle).map_err(|e| format!("audio sink: {e}"))?;
    for p in pieces {
        let decoder = rodio::Decoder::new(std::io::Cursor::new(p.bytes.clone())).map_err(|e| format!("{} could not be decoded: {e}", p.label))?;
        sink.append(decoder);
    }
    sink.play();
    Ok(sink)
}

/// Play `pieces` one after another, replacing whatever is playing.
pub fn play(pieces: Vec<Piece>) -> Result<(), String> {
    if pieces.is_empty() {
        return Err("nothing to play".into());
    }
    let (reply, answer) = channel();
    player().orders.send(Order::Play(pieces, reply)).map_err(|_| "the audio player has stopped".to_string())?;
    answer.recv_timeout(std::time::Duration::from_secs(10)).map_err(|_| "the audio player did not answer".to_string())?
}

/// Stop whatever is playing.
pub fn stop() {
    let _ = player().orders.send(Order::Stop);
}

/// What is playing now.
pub fn playing() -> Playing {
    player().playing.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// A WAV file's sample rate and channels, from its header; `None` for
/// anything else.
pub fn wav_format(bytes: &[u8]) -> Option<(u32, u16)> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?) as usize;
        if id == b"fmt " && at + 16 <= bytes.len() {
            let channels = u16::from_le_bytes(bytes[at + 10..at + 12].try_into().ok()?);
            let rate = u32::from_le_bytes(bytes[at + 12..at + 16].try_into().ok()?);
            return Some((rate, channels));
        }
        at += 8 + size + (size & 1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WAV header is read for its rate and channels.
    #[test]
    fn a_wav_says_its_rate_and_channels() {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&36u32.to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&44_100u32.to_le_bytes());
        wav.extend_from_slice(&[0; 8]);
        assert_eq!(wav_format(&wav), Some((44_100, 2)));
        assert_eq!(wav_format(b"ID3 not a wav"), None);
    }

    #[test]
    fn nothing_is_refused_before_the_device() {
        assert!(play(Vec::new()).unwrap_err().contains("nothing"));
    }
}
