//! An export of the sound alone (#1813): the mix, as any export makes it,
//! written as MP3, WAV or AAC in M4A, with no video stream.
//!
//! Only safe ffmpeg-next calls: a stream from the encoder's parameters,
//! frames of interleaved f32 resampled into the encoder's own format.

use crate::error::RenderError;
use klipp_core::{ExportAudioCodec, ExportFormat};

fn err(what: &str, e: impl std::fmt::Display) -> RenderError {
    RenderError::Encode(format!("{what}: {e}"))
}

/// The muxer for an audio-only format.
fn muxer(format: ExportFormat) -> Option<&'static str> {
    match format {
        ExportFormat::Mp3 => Some("mp3"),
        ExportFormat::Wav => Some("wav"),
        ExportFormat::M4a => Some("ipod"),
        _ => None,
    }
}

/// The container a trial encode of `codec` is written in (#1930), to
/// measure what the delivery's file will: a lossy codec's ringing round
/// transients, and a lossless one's quantisation. `None` for no audio.
pub fn trial_muxer(codec: ExportAudioCodec) -> Option<&'static str> {
    match codec {
        ExportAudioCodec::Aac => Some("ipod"),
        ExportAudioCodec::Mp3 => Some("mp3"),
        ExportAudioCodec::Opus => Some("ogg"),
        ExportAudioCodec::Flac => Some("flac"),
        ExportAudioCodec::Pcm16 | ExportAudioCodec::Pcm24 => Some("wav"),
        ExportAudioCodec::Copy => None,
    }
}

/// How an export's audio encoder is set up (#1930): the codec's first
/// sample format, stereo, the sequence's rate, the bitrate when given.
/// The video export, the sound-alone writer and the true-peak trials all
/// set theirs up from this, so a trial encodes what the delivery will.
pub(crate) fn configure_audio_encoder(ctx: &mut ffmpeg_next::codec::encoder::audio::Audio, found: ffmpeg_next::Codec, rate: u32, bitrate_kbps: Option<u32>) -> ffmpeg_next::format::Sample {
    use ffmpeg_next::format::{sample::Type, Sample};
    let sample = found.audio().ok().and_then(|a| a.formats()).and_then(|mut f| f.next()).unwrap_or(Sample::F32(Type::Packed));
    ctx.set_rate(rate as i32);
    ctx.set_channel_layout(ffmpeg_next::ChannelLayout::STEREO);
    ctx.set_format(sample);
    ctx.set_time_base(ffmpeg_next::Rational::new(1, rate as i32));
    if let Some(kbps) = bitrate_kbps {
        ctx.set_bit_rate(kbps as usize * 1000);
    }
    sample
}

/// What an opened audio encoder runs as (#1930): compared between an
/// export's encoder and a trial's.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenedAudio {
    pub codec: Option<String>,
    pub format: ffmpeg_next::format::Sample,
    pub rate: u32,
    pub channels: u16,
    pub frame_size: u32,
    pub bit_rate_kbps: Option<u32>,
}

impl OpenedAudio {
    pub(crate) fn of(enc: &ffmpeg_next::codec::encoder::audio::Encoder, bitrate_kbps: Option<u32>) -> Self {
        Self {
            codec: enc.codec().map(|c| c.name().to_string()),
            format: enc.format(),
            rate: enc.rate(),
            channels: enc.channels(),
            frame_size: enc.frame_size(),
            bit_rate_kbps: bitrate_kbps,
        }
    }
}

/// The muxer options a file of `format` is opened with: MP4 and M4A, the
/// web's files, put their index (`moov`) before the media (`mdat`), so a
/// player can start before it has the whole file (#1901). The muxer moves
/// it there when the file is finished, without touching the streams.
pub(crate) fn header_options(format: ExportFormat) -> ffmpeg_next::Dictionary<'static> {
    let mut options = ffmpeg_next::Dictionary::new();
    if matches!(format, ExportFormat::Mp4 | ExportFormat::M4a) {
        options.set("movflags", "+faststart");
    }
    options
}

/// Whether this FFmpeg build has an encoder for `codec`.
pub fn has_encoder(codec: ExportAudioCodec) -> bool {
    ffmpeg_next::encoder::find_by_name(codec.ffmpeg_codec()).is_some()
}

/// What an audio-only export of `format` with `codec` writes in this
/// build: the format and codec themselves, or WAV when MP3 is asked for
/// and no MP3 encoder is built in. The second is the fallback, said.
pub fn resolve(format: ExportFormat, codec: ExportAudioCodec) -> (ExportFormat, ExportAudioCodec, Option<String>) {
    let codec = if format.holds_audio(codec) { codec } else { format.default_audio() };
    if codec == ExportAudioCodec::Mp3 && !has_encoder(codec) {
        return (ExportFormat::Wav, ExportAudioCodec::Pcm16, Some("no MP3 encoder in this FFmpeg: written as WAV".into()));
    }
    (format, codec, None)
}

/// An open audio-only file being written.
pub struct AudioOnly {
    octx: ffmpeg_next::format::context::Output,
    enc: ffmpeg_next::encoder::Audio,
    stream: usize,
    stream_tb: ffmpeg_next::Rational,
    enc_tb: ffmpeg_next::Rational,
    /// Samples per channel a frame takes: the encoder's, or a block for
    /// one that takes any (PCM).
    frame_size: usize,
    /// Interleaved stereo waiting for a whole frame.
    pending: Vec<f32>,
    pts: i64,
    resampler: Option<ffmpeg_next::software::resampling::Context>,
    rate: u32,
    bitrate_kbps: Option<u32>,
}

impl AudioOnly {
    /// Open `path` for `format` with `codec` at `rate` Hz stereo.
    pub fn open(path: &std::path::Path, format: ExportFormat, codec: ExportAudioCodec, bitrate_kbps: Option<u32>, rate: u32) -> Result<Self, RenderError> {
        let mux = muxer(format).ok_or_else(|| RenderError::Encode(format!("{} has a picture; the video export writes it", format.label())))?;
        Self::open_with(path, mux, codec, bitrate_kbps, rate, header_options(format))
    }

    /// [`Self::open`] into the muxer named `mux`: the encoder set up as the
    /// video export sets up its audio (the codec's first sample format,
    /// stereo, `rate`, the bitrate when given), so what it encodes is what
    /// an export would (#1930).
    /// A trial's file is read once and removed, so it isn't laid out for
    /// the web (#1901).
    pub fn open_muxer(path: &std::path::Path, mux: &str, codec: ExportAudioCodec, bitrate_kbps: Option<u32>, rate: u32) -> Result<Self, RenderError> {
        Self::open_with(path, mux, codec, bitrate_kbps, rate, ffmpeg_next::Dictionary::new())
    }

    fn open_with(path: &std::path::Path, mux: &str, codec: ExportAudioCodec, bitrate_kbps: Option<u32>, rate: u32, header: ffmpeg_next::Dictionary<'static>) -> Result<Self, RenderError> {
        use ffmpeg_next::format::{sample::Type, Sample};
        let mut octx = ffmpeg_next::format::output_as(path, mux).map_err(|e| err("audio output", e))?;
        let found = ffmpeg_next::encoder::find_by_name(codec.ffmpeg_codec()).ok_or_else(|| RenderError::Encode(format!("no {} encoder in this FFmpeg", codec.ffmpeg_codec())))?;
        let mut ctx = ffmpeg_next::codec::context::Context::new_with_codec(found).encoder().audio().map_err(|e| err("audio encoder context", e))?;
        let sample = configure_audio_encoder(&mut ctx, found, rate, bitrate_kbps);
        if octx.format().flags().contains(ffmpeg_next::format::flag::Flags::GLOBAL_HEADER) {
            ctx.set_flags(ffmpeg_next::codec::Flags::GLOBAL_HEADER);
        }
        let enc = ctx.open_as(found).map_err(|e| err("open audio encoder", e))?;
        let enc_tb = ffmpeg_next::Rational::new(1, rate as i32);
        let stream = {
            let mut st = octx.add_stream(found).map_err(|e| err("audio stream", e))?;
            st.set_parameters(&enc);
            st.set_time_base(enc_tb);
            st.index()
        };
        octx.write_header_with(header).map_err(|e| err("write audio header", e))?;
        let stream_tb = octx.stream(stream).map(|s| s.time_base()).unwrap_or(enc_tb);
        let resampler = (sample != Sample::F32(Type::Packed))
            .then(|| {
                ffmpeg_next::software::resampling::Context::get(Sample::F32(Type::Packed), ffmpeg_next::ChannelLayout::STEREO, rate, sample, ffmpeg_next::ChannelLayout::STEREO, rate)
                    .map_err(|e| err("audio resampler", e))
            })
            .transpose()?;
        let frame_size = match enc.frame_size() {
            0 => 1024,
            n => n as usize,
        };
        Ok(Self { octx, enc, stream, stream_tb, enc_tb, frame_size, pending: Vec::new(), pts: 0, resampler, rate, bitrate_kbps })
    }

    /// What its encoder opened as.
    pub fn opened(&self) -> OpenedAudio {
        OpenedAudio::of(&self.enc, self.bitrate_kbps)
    }

    /// Interleaved stereo f32, in order.
    pub fn write(&mut self, interleaved: &[f32]) -> Result<(), RenderError> {
        self.pending.extend_from_slice(interleaved);
        while self.pending.len() >= self.frame_size * 2 {
            let rest = self.pending.split_off(self.frame_size * 2);
            let block = std::mem::replace(&mut self.pending, rest);
            self.send(&block)?;
        }
        Ok(())
    }

    fn send(&mut self, block: &[f32]) -> Result<(), RenderError> {
        use ffmpeg_next::format::{sample::Type, Sample};
        let n = block.len() / 2;
        if n == 0 {
            return Ok(());
        }
        let mut frame = ffmpeg_next::frame::Audio::new(Sample::F32(Type::Packed), n, ffmpeg_next::ChannelLayout::STEREO);
        frame.set_rate(self.rate);
        for (d, s) in frame.data_mut(0).chunks_exact_mut(4).zip(block) {
            d.copy_from_slice(&s.to_le_bytes());
        }
        frame.set_pts(Some(self.pts));
        self.pts += n as i64;
        let sent = match self.resampler.as_mut() {
            Some(r) => {
                let mut out = ffmpeg_next::frame::Audio::empty();
                r.run(&frame, &mut out).map_err(|e| err("resample audio", e))?;
                out.set_pts(frame.pts());
                self.enc.send_frame(&out)
            }
            None => self.enc.send_frame(&frame),
        };
        sent.map_err(|e| err("send audio frame", e))?;
        self.drain()
    }

    fn drain(&mut self) -> Result<(), RenderError> {
        let mut packet = ffmpeg_next::Packet::empty();
        while self.enc.receive_packet(&mut packet).is_ok() {
            packet.set_stream(self.stream);
            packet.rescale_ts(self.enc_tb, self.stream_tb);
            packet.write_interleaved(&mut self.octx).map_err(|e| err("write audio packet", e))?;
        }
        Ok(())
    }

    /// The last partial frame, the encoder flushed, the trailer written.
    pub fn finish(mut self) -> Result<(), RenderError> {
        let rest = std::mem::take(&mut self.pending);
        self.send(&rest)?;
        self.enc.send_eof().map_err(|e| err("audio send_eof", e))?;
        self.drain()?;
        self.octx.write_trailer().map_err(|e| err("write audio trailer", e))
    }
}
