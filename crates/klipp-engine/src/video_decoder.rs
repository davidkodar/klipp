use crate::types::{DecodedFrame, FrameData, FrameQueue, SharedFrameQueue};
use crossbeam_channel::{Receiver, Sender};
use klipp_core::RationalTime;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Commands sent to the decode thread.
pub enum DecodeCommand {
    SeekTo(f64),
    Pause,
    Resume,
    Stop,
}

/// Errors from the video decoder.
#[derive(Debug)]
pub enum DecoderError {
    Open(String),
    NoVideoStream,
    CodecNotFound,
    Codec(String),
    Scaler(String),
}

impl std::fmt::Display for DecoderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open(e) => write!(f, "failed to open: {e}"),
            Self::NoVideoStream => write!(f, "no video stream found"),
            Self::CodecNotFound => write!(f, "codec not found"),
            Self::Codec(e) => write!(f, "codec error: {e}"),
            Self::Scaler(e) => write!(f, "scaler error: {e}"),
        }
    }
}

pub struct VideoDecoder {
    frame_queue: SharedFrameQueue,
    decode_thread: Option<JoinHandle<()>>,
    decode_cmd_tx: Sender<DecodeCommand>,
    /// Set to true by the decode thread if VAAPI surfaces are actually produced
    pub hw_active: Arc<std::sync::atomic::AtomicBool>,
}

impl VideoDecoder {
    /// Open a video file for decoding with optional VAAPI hardware acceleration.
    pub fn open(path: &Path) -> Result<Self, DecoderError> {
        // Validate file can be opened before spawning the thread
        {
            let ctx = ffmpeg_next::format::input(path)
                .map_err(|e| DecoderError::Open(e.to_string()))?;
            let _stream = ctx
                .streams()
                .best(ffmpeg_next::media::Type::Video)
                .ok_or(DecoderError::NoVideoStream)?;
        }

        let frame_queue: SharedFrameQueue = Arc::new(Mutex::new(FrameQueue::new(16)));
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        let queue_clone = frame_queue.clone();
        let path_owned = path.to_path_buf();
        let hw_active = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hw_flag = hw_active.clone();

        let thread = std::thread::Builder::new()
            .name("klipp-vdec".into())
            .spawn(move || {
                decode_thread_main(path_owned, queue_clone, cmd_rx, hw_flag);
            })
            .map_err(|e| DecoderError::Open(format!("spawn thread: {e}")))?;

        Ok(Self {
            frame_queue,
            decode_thread: Some(thread),
            decode_cmd_tx: cmd_tx,
            hw_active,
        })
    }

    pub fn frame_queue(&self) -> &SharedFrameQueue {
        &self.frame_queue
    }

    pub fn seek(&self, secs: f64) {
        let _ = self.decode_cmd_tx.send(DecodeCommand::SeekTo(secs));
    }

    pub fn resume(&self) {
        let _ = self.decode_cmd_tx.send(DecodeCommand::Resume);
    }

    pub fn pause(&self) {
        let _ = self.decode_cmd_tx.send(DecodeCommand::Pause);
    }

    pub fn stop_detached(&mut self) {
        let _ = self.decode_cmd_tx.send(DecodeCommand::Stop);
        self.decode_thread.take();
    }

    pub fn stop(&mut self) {
        let _ = self.decode_cmd_tx.send(DecodeCommand::Stop);
        if let Some(thread) = self.decode_thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------------------------------------------------------------------------
// VAAPI hardware acceleration
// ---------------------------------------------------------------------------

/// Callback for FFmpeg codec negotiation — selects AV_PIX_FMT_VAAPI when available.
/// Without this, FFmpeg falls back to software pixel formats even with hw_device_ctx set.
unsafe extern "C" fn vaapi_get_format(
    _ctx: *mut ffmpeg_next::ffi::AVCodecContext,
    pix_fmts: *const ffmpeg_next::ffi::AVPixelFormat,
) -> ffmpeg_next::ffi::AVPixelFormat {
    use ffmpeg_next::ffi::AVPixelFormat;

    let mut p = pix_fmts;
    while *p != AVPixelFormat::AV_PIX_FMT_NONE {
        if *p == AVPixelFormat::AV_PIX_FMT_VAAPI {
            info!("VAAPI get_format: selected AV_PIX_FMT_VAAPI");
            return AVPixelFormat::AV_PIX_FMT_VAAPI;
        }
        p = p.add(1);
    }

    // VAAPI not offered — fall back to first format in the list
    warn!("VAAPI get_format: AV_PIX_FMT_VAAPI not in candidate list, using software fallback");
    *pix_fmts
}

/// Try to initialize VAAPI on the codec context. Returns true if successful.
/// Must be called BEFORE the decoder is opened (before .decoder().video()).
unsafe fn try_init_vaapi(codec_ctx_ptr: *mut ffmpeg_next::ffi::AVCodecContext) -> bool {
    use ffmpeg_next::ffi::*;

    let device_type = av_hwdevice_find_type_by_name(b"vaapi\0".as_ptr() as *const _);
    if device_type == AVHWDeviceType::AV_HWDEVICE_TYPE_NONE {
        warn!("VAAPI: device type not found");
        return false;
    }

    // Try explicit render nodes first (required for NVIDIA on Wayland),
    // then fall back to auto-detect
    let device_paths: &[&[u8]] = &[
        b"/dev/dri/renderD128\0",
        b"/dev/dri/renderD129\0",
        b"\0", // null = auto-detect
    ];

    let mut hw_device_ctx: *mut AVBufferRef = std::ptr::null_mut();

    for device_path in device_paths {
        let ret = av_hwdevice_ctx_create(
            &mut hw_device_ctx,
            device_type,
            device_path.as_ptr() as *const _,
            std::ptr::null_mut(),
            0,
        );
        if ret >= 0 {
            (*codec_ctx_ptr).hw_device_ctx = av_buffer_ref(hw_device_ctx);
            // Set the get_format callback so FFmpeg requests VAAPI pixel format
            (*codec_ctx_ptr).get_format = Some(vaapi_get_format);
            let path_str = std::str::from_utf8(device_path)
                .unwrap_or("?")
                .trim_end_matches('\0');
            let path_display = if path_str.is_empty() { "auto" } else { path_str };
            info!("Hardware decode: VAAPI active on {path_display}");
            av_buffer_unref(&mut hw_device_ctx);
            return true;
        }
    }

    warn!("Hardware decode: VAAPI init failed on all device paths — falling back to software");
    false
}

/// Download a VAAPI surface frame to CPU as NV12, then convert to RGBA via swscale.
unsafe fn download_vaapi_frame(
    hw_frame: &ffmpeg_next::frame::Video,
    sw_scaler: &mut Option<ffmpeg_next::software::scaling::Context>,
) -> Option<(Vec<u8>, u32, u32)> {
    use ffmpeg_next::ffi::*;

    let sw_frame = av_frame_alloc();
    if sw_frame.is_null() {
        return None;
    }

    // Transfer from GPU (VAAPI surface) to CPU (NV12)
    let ret = av_hwframe_transfer_data(sw_frame, hw_frame.as_ptr(), 0);
    if ret < 0 {
        // Log once at debug level
        debug!(
            "VAAPI transfer failed: ret={ret}, frame format={}, w={}, h={}",
            (*hw_frame.as_ptr()).format,
            (*hw_frame.as_ptr()).width,
            (*hw_frame.as_ptr()).height,
        );
        av_frame_free(&mut (sw_frame as *mut _));
        return None;
    }

    let width = (*sw_frame).width as u32;
    let height = (*sw_frame).height as u32;
    let pix_fmt = std::mem::transmute::<i32, AVPixelFormat>((*sw_frame).format);

    // Create or update scaler: downloaded format → RGBA
    let need_new_scaler = match sw_scaler {
        Some(_) => false,
        None => true,
    };

    if need_new_scaler {
        let src_format = ffmpeg_next::format::Pixel::from(pix_fmt);
        match ffmpeg_next::software::scaling::Context::get(
            src_format,
            width,
            height,
            ffmpeg_next::format::Pixel::RGBA,
            width,
            height,
            ffmpeg_next::software::scaling::Flags::FAST_BILINEAR,
        ) {
            Ok(s) => *sw_scaler = Some(s),
            Err(e) => {
                warn!("VAAPI scaler error: {e}");
                av_frame_free(&mut (sw_frame as *mut _));
                return None;
            }
        }
    }

    let scaler = sw_scaler.as_mut().unwrap();
    let mut rgba_frame = ffmpeg_next::frame::Video::empty();

    // Set up rgba_frame dimensions
    let rgba_ptr = rgba_frame.as_mut_ptr();
    (*rgba_ptr).width = width as i32;
    (*rgba_ptr).height = height as i32;
    (*rgba_ptr).format = AVPixelFormat::AV_PIX_FMT_RGBA as i32;
    let ret = av_frame_get_buffer(rgba_ptr, 0);
    if ret < 0 {
        av_frame_free(&mut (sw_frame as *mut _));
        return None;
    }

    // Use sws_scale directly
    let sws_ctx = scaler.as_mut_ptr();
    sws_scale(
        sws_ctx,
        (*sw_frame).data.as_ptr() as *const *const u8,
        (*sw_frame).linesize.as_ptr(),
        0,
        height as i32,
        (*rgba_ptr).data.as_ptr(),
        (*rgba_ptr).linesize.as_ptr(),
    );

    av_frame_free(&mut (sw_frame as *mut _));

    // Extract RGBA data
    let stride = (*rgba_ptr).linesize[0] as usize;
    let expected_stride = (width * 4) as usize;
    let data_slice = std::slice::from_raw_parts((*rgba_ptr).data[0], stride * height as usize);

    let rgba_data = if stride == expected_stride {
        data_slice.to_vec()
    } else {
        let mut buf = Vec::with_capacity(expected_stride * height as usize);
        for row in 0..height as usize {
            let start = row * stride;
            buf.extend_from_slice(&data_slice[start..start + expected_stride]);
        }
        buf
    };

    Some((rgba_data, width, height))
}

// ---------------------------------------------------------------------------
// Decode thread
// ---------------------------------------------------------------------------

fn decode_thread_main(
    path: PathBuf,
    frame_queue: SharedFrameQueue,
    cmd_rx: Receiver<DecodeCommand>,
    hw_active_flag: Arc<std::sync::atomic::AtomicBool>,
) {
    let mut format_ctx = match ffmpeg_next::format::input(&path) {
        Ok(ctx) => ctx,
        Err(e) => {
            warn!("decode_thread: failed to open {:?}: {e}", path);
            frame_queue.lock().unwrap().push(DecodedFrame::eof());
            return;
        }
    };

    let video_stream = match format_ctx.streams().best(ffmpeg_next::media::Type::Video) {
        Some(s) => s,
        None => {
            warn!("decode_thread: no video stream in {:?}", path);
            frame_queue.lock().unwrap().push(DecodedFrame::eof());
            return;
        }
    };

    let stream_index = video_stream.index();
    let time_base = video_stream.time_base();
    let params = video_stream.parameters();

    let mut codec_ctx = match ffmpeg_next::codec::context::Context::from_parameters(params) {
        Ok(ctx) => ctx,
        Err(e) => {
            warn!("decode_thread: codec context error: {e}");
            frame_queue.lock().unwrap().push(DecodedFrame::eof());
            return;
        }
    };

    // Try VAAPI hardware acceleration
    let use_hw = unsafe { try_init_vaapi(codec_ctx.as_mut_ptr()) };

    let mut decoder = match codec_ctx.decoder().video() {
        Ok(d) => d,
        Err(e) => {
            warn!("decode_thread: video decoder error: {e}");
            frame_queue.lock().unwrap().push(DecodedFrame::eof());
            return;
        }
    };

    // Software scaler — created lazily on first software frame
    let mut sw_scaler: Option<ffmpeg_next::software::scaling::Context> = None;
    // Scaler for VAAPI downloaded frames (created lazily)
    let mut hw_scaler: Option<ffmpeg_next::software::scaling::Context> = None;

    let tb_num = time_base.numerator() as f64;
    let tb_den = time_base.denominator() as f64;
    let tb_f64 = if tb_den != 0.0 { tb_num / tb_den } else { 1.0 / 90000.0 };

    debug!(
        "decode_thread: started, stream={stream_index}, time_base={tb_num}/{tb_den}, hw={use_hw}"
    );

    let mut paused = true;
    let mut eof_sent = false;
    let mut seek_target: Option<f64> = None;
    let mut packets = format_ctx.packets();
    let mut eof_from_demuxer = false;

    loop {
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                DecodeCommand::SeekTo(secs) => {
                    let ts = (secs * 1_000_000.0) as i64;
                    debug!("decode_thread: seek to {secs:.3}s (ts={ts} us)");
                    // Must drop packets iterator before seeking, then recreate
                    drop(packets);
                    let _ = format_ctx.seek(ts, ..ts);
                    packets = format_ctx.packets();
                    decoder.flush();
                    frame_queue.lock().unwrap().clear();
                    seek_target = Some(secs);
                    eof_sent = false;
                    eof_from_demuxer = false;
                }
                DecodeCommand::Pause => paused = true,
                DecodeCommand::Resume => {
                    paused = false;
                    eof_sent = false;
                }
                DecodeCommand::Stop => {
                    debug!("decode_thread: stop");
                    return;
                }
            }
        }

        if paused {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }

        if frame_queue.lock().unwrap().is_full() {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }

        if eof_sent {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }

        // Read next packet (reuses persistent iterator)
        if !eof_from_demuxer {
            match packets.next() {
                Some((stream, packet)) => {
                    if stream.index() != stream_index {
                        continue;
                    }
                    let _ = decoder.send_packet(&packet);
                }
                None => {
                    let _ = decoder.send_eof();
                    eof_from_demuxer = true;
                }
            }
        }

        // Drain decoded frames
        let mut received_frame = ffmpeg_next::frame::Video::empty();
        let mut frames_this_round = 0;

        while decoder.receive_frame(&mut received_frame).is_ok() {
            let pts_secs = received_frame
                .pts()
                .map(|pts| pts as f64 * tb_f64)
                .unwrap_or(0.0);

            // Skip pre-seek frames
            if let Some(target) = seek_target {
                if pts_secs < target - 0.05 {
                    continue;
                }
                seek_target = None;
            }

            // Check if this frame is a VAAPI surface or a regular software frame
            let is_vaapi_frame = unsafe {
                (*received_frame.as_ptr()).format
                    == ffmpeg_next::ffi::AVPixelFormat::AV_PIX_FMT_VAAPI as i32
            };

            let (rgba_data, width, height) = if is_vaapi_frame {
                hw_active_flag.store(true, std::sync::atomic::Ordering::Relaxed);
                // VAAPI path: download from GPU → NV12 → RGBA
                match unsafe { download_vaapi_frame(&received_frame, &mut hw_scaler) } {
                    Some(result) => result,
                    None => {
                        warn!("decode_thread: VAAPI frame download failed, skipping");
                        continue;
                    }
                }
            } else {
                // Software path: scale YUV → RGBA
                // Create scaler lazily on first software frame
                if sw_scaler.is_none() {
                    match ffmpeg_next::software::scaling::Context::get(
                        received_frame.format(),
                        received_frame.width(),
                        received_frame.height(),
                        ffmpeg_next::format::Pixel::RGBA,
                        received_frame.width(),
                        received_frame.height(),
                        ffmpeg_next::software::scaling::Flags::FAST_BILINEAR,
                    ) {
                        Ok(s) => sw_scaler = Some(s),
                        Err(e) => {
                            warn!("decode_thread: scaler creation failed: {e}");
                            continue;
                        }
                    }
                }
                let scaler = sw_scaler.as_mut().unwrap();
                let mut rgba_frame = ffmpeg_next::frame::Video::empty();
                if scaler.run(&received_frame, &mut rgba_frame).is_err() {
                    warn!("decode_thread: scaler failed, skipping frame");
                    continue;
                }

                let width = rgba_frame.width();
                let height = rgba_frame.height();
                let stride = rgba_frame.stride(0);
                let data_ptr = rgba_frame.data(0);

                let expected_stride = (width * 4) as usize;
                let rgba_data = if stride == expected_stride {
                    data_ptr[..expected_stride * height as usize].to_vec()
                } else {
                    let mut buf = Vec::with_capacity(expected_stride * height as usize);
                    for row in 0..height as usize {
                        let start = row * stride;
                        let end = start + expected_stride;
                        buf.extend_from_slice(&data_ptr[start..end]);
                    }
                    buf
                };

                (rgba_data, width, height)
            };

            let decoded = DecodedFrame {
                pts: RationalTime::from_secs_f64(pts_secs, 90000),
                width,
                height,
                data: FrameData::Rgba(rgba_data),
                is_eof: false,
            };

            frame_queue.lock().unwrap().push(decoded);
            frames_this_round += 1;

            if frame_queue.lock().unwrap().is_full() {
                break;
            }
        }

        if frames_this_round == 0 && eof_from_demuxer {
            debug!("decode_thread: EOF reached");
            frame_queue.lock().unwrap().push(DecodedFrame::eof());
            eof_sent = true;
        }
    }
}
