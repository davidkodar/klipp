use klipp_timeline::GradeAccess;
use crate::icons::Icon;
use crate::inspector::video_tab::{find_transform_index, TransformParams};
use crate::state::{AppState, TransformHandle, ViewerTransformDrag};
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, Fps, RationalTime, ViewerMode};
use klipp_engine::PlaybackState;

/// Drop the alpha so the input color renders as a solid outline /
/// label-badge fill. AI Tools' detection palette colors are
/// pre-multiplied at ~50% alpha to look right as mask tints; the
/// bbox outline + label badge want the same hue at full alpha so
/// they read crisply against the viewer.
fn opaque_for_outline(c: egui::Color32) -> egui::Color32 {
    if c.a() == 0 {
        return c;
    }
    // Reverse the pre-multiply: divide each RGB channel by the
    // existing alpha (so we recover the un-premultiplied color),
    // then re-emit at alpha=255.
    let inv_a = 255.0 / (c.a() as f32);
    let r = ((c.r() as f32) * inv_a).round().clamp(0.0, 255.0) as u8;
    let g = ((c.g() as f32) * inv_a).round().clamp(0.0, 255.0) as u8;
    let b = ((c.b() as f32) * inv_a).round().clamp(0.0, 255.0) as u8;
    egui::Color32::from_rgb(r, g, b)
}

/// How far in from a surface's edges a picture dropped on it sits, of its
/// width and height each side: the tracked corners are often a screen's
/// outer edge, and the picture belongs inside the bezel.
const SURFACE_DROP_INSET: f32 = 0.015;

/// The smallest box, in screen points either way, the Text tool takes a
/// drag for (#1713); a smaller one is a click where it started.
const TEXT_TOOL_MIN_BOX: f32 = 8.0;

/// The title text under `p` at the playhead, for the Text tool (#1714):
/// the clip, its text layer and the layer's text, the topmost track's
/// first and a layer's frontmost first. A layer is hit on its box as the
/// in-place editor draws it, hugging the text.
fn title_text_at(state: &AppState, frame: &ViewerFrameState, p: egui::Pos2) -> Option<(klipp_core::ClipId, klipp_timeline::TitleObjectId, String)> {
    let dims = (frame.video_w.max(1.0) as u32, frame.video_h.max(1.0) as u32);
    let at = state.playhead;
    state.sequence().tracks.iter().find_map(|track| {
        let (_, clip) = track.clip_at_timecode(at)?;
        let mut td = clip.title_data().filter(|_| clip.enabled)?.clone();
        tighten_text_editor_boxes(&mut td, dims);
        let clip_time = (state.playhead_smooth_secs - clip.timeline_in.to_secs_f64()).max(0.0);
        td.objects.iter_mut().rev().find_map(|obj| {
            obj.transform.center = obj.transform.evaluated_center(clip_time);
            match &obj.content {
                klipp_timeline::TitleObjectContent::TextBox(tb)
                    if obj.visible && !obj.locked && object_contains_screen_point(obj, frame.image_rect, p) =>
                {
                    Some((clip.id, obj.id, tb.text.clone()))
                }
                _ => None,
            }
        })
    })
}

/// Map a normalised `(0..1, 0..1)` clip-space point to the image
/// rect's pixel coordinates. The viewer paints frame content into
/// `image_rect`; gizmos that store positions in clip-space (focal
/// points, anchors, hit-targets) call this to project to screen.
///
/// Callers that want a clamped-into-frame result clamp the input
/// before passing in — this helper preserves `(nx, ny)` outside
/// `[0, 1]` so gizmos that legitimately overflow the frame (a title
/// object dragged past the canvas) still draw at the right offset.
pub(crate) fn viewer_norm_to_screen(image_rect: egui::Rect, nx: f32, ny: f32) -> egui::Pos2 {
    egui::pos2(
        image_rect.min.x + nx * image_rect.width(),
        image_rect.min.y + ny * image_rect.height(),
    )
}

/// A title's canvas as it is drawn on the viewer (#2014): the frame-sized
/// canvas its layers are laid out on, placed by the clip's own transform
/// (and the camera at its depth) as the renderer places it. Points are
/// worked out on the unplaced canvas, `image_rect`'s own coordinates, as
/// the editor always has, then `place`d onto the screen; the pointer is
/// `unplace`d back. With no placement both are the identity.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TitleView {
    pub(crate) image_rect: egui::Rect,
    /// `(px, py, sx, sy, rotation°, anchor_x, anchor_y)`, positions in the
    /// sequence's pixels, of a frame `frame` big.
    pub(crate) placement: Option<klipp_compose::eval::ResolvedTransform>,
    pub(crate) frame: (f32, f32),
}

impl TitleView {
    /// The canvas at `image_rect`, unplaced.
    pub(crate) fn flat(image_rect: egui::Rect) -> Self {
        Self { image_rect, placement: None, frame: (1.0, 1.0) }
    }

    /// A point of the unplaced canvas (screen coordinates over
    /// `image_rect`) where the placed canvas draws it: about the anchor,
    /// scaled, turned, then moved, as the Transform maps forward.
    pub(crate) fn place(&self, p: egui::Pos2) -> egui::Pos2 {
        let Some((px, py, sx, sy, rot, ax, ay)) = self.placement else { return p };
        let (fw, fh) = (self.frame.0.max(1.0), self.frame.1.max(1.0));
        let (nx, ny) = viewer_screen_to_norm(self.image_rect, p);
        let (x, y) = (nx * fw, ny * fh);
        let (ax, ay) = (ax * fw, ay * fh);
        let (dx, dy) = ((x - ax) * sx, (y - ay) * sy);
        let (sin, cos) = rot.to_radians().sin_cos();
        let (x, y) = (ax + px + dx * cos - dy * sin, ay + py + dx * sin + dy * cos);
        viewer_norm_to_screen(self.image_rect, x / fw, y / fh)
    }

    /// The inverse of [`Self::place`]: a screen point where the unplaced
    /// canvas has it. The caller hides the editor where the scale is near
    /// nothing (`placement_for_editing`), so the inverse is well defined.
    pub(crate) fn unplace(&self, p: egui::Pos2) -> egui::Pos2 {
        let Some((px, py, sx, sy, rot, ax, ay)) = self.placement else { return p };
        let (fw, fh) = (self.frame.0.max(1.0), self.frame.1.max(1.0));
        let (nx, ny) = viewer_screen_to_norm(self.image_rect, p);
        let (x, y) = (nx * fw, ny * fh);
        let (ax, ay) = (ax * fw, ay * fh);
        let (dx, dy) = (x - ax - px, y - ay - py);
        let (sin, cos) = rot.to_radians().sin_cos();
        let (rx, ry) = (dx * cos + dy * sin, -dx * sin + dy * cos);
        let (x, y) = (ax + rx / sx, ay + ry / sy);
        viewer_norm_to_screen(self.image_rect, x / fw, y / fh)
    }

    /// A canvas point, normalised, on the screen.
    pub(crate) fn to_screen(self, nx: f32, ny: f32) -> egui::Pos2 {
        self.place(viewer_norm_to_screen(self.image_rect, nx, ny))
    }

    /// A screen point as a normalised canvas point.
    pub(crate) fn to_norm(self, p: egui::Pos2) -> (f32, f32) {
        viewer_screen_to_norm(self.image_rect, self.unplace(p))
    }

    /// How much bigger the canvas is drawn than unplaced: the geometric
    /// mean of its scales, for sizes (a snap distance, a font) that
    /// follow it.
    pub(crate) fn scale(&self) -> f32 {
        self.placement.map_or(1.0, |(_, _, sx, sy, ..)| (sx.abs() * sy.abs()).sqrt().max(1e-3))
    }
}

/// The time a title's canvas is edited at: the smoothed playhead the
/// editor reads its layers at, so the placement and the layers it carries
/// don't drift apart while scrubbing or playing.
fn title_edit_time(state: &AppState) -> klipp_core::Timecode {
    klipp_core::RationalTime::from_secs_f64(state.playhead_smooth_secs.max(0.0), state.playhead.rate)
}

/// The canvas of title clip `clip` drawn at `image_rect` at the playhead
/// (#2014), or why its contents can't be edited there.
pub(crate) fn title_view_of(state: &AppState, clip: &klipp_timeline::Clip, image_rect: egui::Rect) -> Result<TitleView, klipp_compose::eval::NotFollowed> {
    let res = state.sequence().resolution;
    klipp_compose::eval::placement_for_editing(clip, state.sequence(), title_edit_time(state))
        .map(|placement| TitleView { image_rect, placement, frame: (res.width as f32, res.height as f32) })
}

/// Whether `p` is inside the outline `poly` on screen (even-odd, so any
/// simple polygon, convex or not).
fn in_screen_polygon(poly: &[egui::Pos2], p: egui::Pos2) -> bool {
    let mut inside = false;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x) {
            inside = !inside;
        }
    }
    inside
}

/// Inverse of [`viewer_norm_to_screen`]. Returns the clip-space
/// `(nx, ny)` for a screen-space pointer position. Result is **not**
/// clamped — callers that want `[0, 1]` (e.g. focal-point storage)
/// `.clamp(0.0, 1.0)` after reading. The drag handler for the
/// title-object editor needs unclamped results so drag-deltas stay
/// accurate when the cursor leaves the canvas.
///
/// Width / height each clamp to `>= 1.0` to avoid divide-by-zero on
/// a degenerate image rect (briefly possible during layout settle).
pub(crate) fn viewer_screen_to_norm(image_rect: egui::Rect, p: egui::Pos2) -> (f32, f32) {
    let w = image_rect.width().max(1.0);
    let h = image_rect.height().max(1.0);
    (
        (p.x - image_rect.min.x) / w,
        (p.y - image_rect.min.y) / h,
    )
}

/// Map a frame-pixel position (e.g. a glyph bounding-box from
/// `klipp_title::compute_char_positions`, which returns coords in
/// frame pixels at the title's render resolution) to the viewer
/// image-rect's screen pixels. Composes [`viewer_norm_to_screen`]
/// with the frame_w/h normalisation; pulls a degenerate-frame guard
/// (`>= 1.0`) so a zero-sized frame doesn't divide-by-zero.
///
/// Used by the visual letter picker
/// (`crate::inspector::letter_picker`).
pub(crate) fn viewer_frame_px_to_screen(
    image_rect: egui::Rect,
    frame_w: f32, frame_h: f32,
    px: f32, py: f32,
) -> egui::Pos2 {
    let fw = frame_w.max(1.0);
    let fh = frame_h.max(1.0);
    viewer_norm_to_screen(image_rect, px / fw, py / fh)
}

/// Per-frame state computed once at the top of `show_program_viewer`.
/// Passed by reference to extracted helper functions. All fields are `Copy`.
#[derive(Debug, Clone, Copy)]
struct ViewerFrameState {
    image_rect: egui::Rect,
    viewer_rect: egui::Rect,
    video_w: f32,
    video_h: f32,
    grade_still_active: bool,
    /// The graded frame of the clip the viewer wipes against (#899), when
    /// it is drawn: the split's left side.
    wipe_frame: Option<egui::TextureId>,
    /// `(width, height, proxy_ready)` of the media item under the playhead, if any.
    current_media_info: Option<(u32, u32, bool)>,
}

/// Resting fitted rectangles; changing playhead position only translates
/// these, so drawing editor handles does not rasterize text every frame.
struct TitleFitGeometry {
    title: klipp_timeline::TitleData,
    dimensions: (u32, u32),
    transforms: Vec<(klipp_timeline::TitleObjectId, Option<klipp_timeline::ObjectTransform>)>,
}

#[derive(Default)]
pub struct ViewerPanel {
    title_fit_geometry: std::cell::RefCell<Option<TitleFitGeometry>>,
    /// Set when the window could not be started. The output is still on
    /// and still publishing — only the convenience of opening a viewer for
    /// it failed — so the button says so rather than claiming success.
    pub hdr_output_unopened: bool,
    /// The HDR reference-output window, when this panel launched it.
    ///
    /// Owned here so toggling the output off closes the window it opened,
    /// rather than leaving one behind that a user has to hunt down. A
    /// presenter the user started themselves is not in here and is left
    /// alone — it attaches and detaches on its own.
    pub hdr_output_child: Option<std::process::Child>,
    pub video_texture: Option<egui::TextureHandle>,
    /// A 3D qualifier stroke's points while it is drawn (#895), in frame
    /// fractions.
    stroke_points: Vec<[f32; 2]>,
    /// Separate texture for title overlay — rendered on UI thread for instant preview.
    /// Drawn on top of video_texture with alpha blending. Cleared when title is deselected.
    pub title_overlay_texture: Option<egui::TextureHandle>,
    pub title_overlay_size: (u32, u32),
    pub logo_texture: Option<egui::TextureHandle>,

    // ── GPU-resident viewer path ──
    // Registered egui `TextureId` for `FrameBuffer.gpu_output`. The
    // engine encodes the graded frame directly into that texture on
    // the shared wgpu device; registering a native texture here gives
    // us an id the viewer can sample without ever touching
    // `fb.data`. `gpu_texture_gen` tracks the generation we last
    // registered so we re-register only on actual texture swap
    // (first use, resolution change).
    pub gpu_texture_id: Option<egui::TextureId>,
    pub gpu_texture_gen: u64,
    /// Whether the newest frame is in that texture rather than in
    /// `video_texture` (`FrameBuffer::gpu_output_live`), and its size.
    /// A frame of bytes that follows one on the GPU paints from
    /// `video_texture` again.
    pub gpu_frame_live: bool,
    pub gpu_frame_size: (u32, u32),
    /// Owned wgpu `TextureView` backing the registered `gpu_texture_id`.
    /// MUST stay alive for the lifetime of `gpu_texture_id`: wgpu 22's
    /// `BindGroup` records the view's storage Id without taking a
    /// reference-counted clone, so once this `TextureView` drops, the
    /// next egui paint that samples `gpu_texture_id` panics with
    /// "TextureView[Id(N,vk)] does not exist". Replaced atomically with
    /// the new view inside `refresh_gpu_viewer_texture` whenever a new
    /// texture generation appears; the previous view drops one step
    /// after `renderer.free_texture(old_id)` releases egui-wgpu's
    /// internal BindGroup so storage cleanup is well-ordered.
    pub gpu_texture_view: Option<wgpu::TextureView>,
    /// The monitor LUT from Preferences (#465), applied to the picture on
    /// screen only: see [`crate::viewer_lut`].
    pub lut: crate::viewer_lut::ViewerLut,
    pub lut_pass: Option<crate::viewer_lut::ViewerLutPass>,
    /// The parked frame's bytes, kept for the pass when a LUT is set.
    pub lut_source_bytes: Option<(Vec<u8>, u32, u32)>,
    /// The LUT'd picture, registered with egui, and the view behind it
    /// (kept alive for the id, as `gpu_texture_view` is).
    pub lut_texture_id: Option<egui::TextureId>,
    pub lut_texture_view: Option<wgpu::TextureView>,
    /// Whether `lut_texture_id` holds the current picture: shown in
    /// place of the plain one only then.
    pub lut_live: bool,
}

impl Drop for ViewerPanel {
    /// Take the reference-output window with us.
    ///
    /// Best effort: a process that exits through `std::process::exit` runs
    /// no destructors, and the presenter is built to cope with that — it
    /// sees the shared frame's magic gone and says the viewer stopped
    /// publishing rather than freezing on the last picture. This is so the
    /// ordinary case does not leave a window behind for the user to find.
    fn drop(&mut self) {
        self.close_hdr_output_window();
    }
}

/// The master meter's size in the transport row. Wide and short: the
/// row has horizontal slack and no vertical room at all.
const MASTER_METER_W: f32 = 96.0;
const MASTER_METER_H: f32 = 12.0;

/// The quietest level the meter draws. Below this a signal is not
/// worth a pixel, and the scale would spend half its width on it.
const MASTER_METER_FLOOR_DB: f32 = -60.0;

/// Where the bar stops being green. -18 dBFS is the usual dialogue
/// target and -6 is where a mix is asking to be turned down.
const MASTER_METER_WARN_DB: f32 = -6.0;

/// A peak level as a fraction across the meter, 0 at the floor and 1
/// at 0 dBFS.
///
/// Linear in decibels, not in amplitude. An amplitude scale spends
/// three-quarters of its width on the top 12 dB and squashes
/// everything a dialogue editor actually watches into the left end.
pub fn master_meter_fraction(peak_linear: f32) -> f32 {
    if peak_linear <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * peak_linear.log10();
    ((db - MASTER_METER_FLOOR_DB) / -MASTER_METER_FLOOR_DB).clamp(0.0, 1.0)
}

/// Whether a peak is at or over full scale, and so being clipped by
/// whatever it is written to.
pub fn master_meter_is_clipping(peak_linear: f32) -> bool {
    peak_linear >= 1.0
}

/// A two-bar stereo peak meter: left above, right below.
///
/// Drawn rather than built from widgets because it is read, never
/// clicked — a widget here would take focus and a tooltip slot for
/// something nobody interacts with.
fn draw_master_meter(
    painter: &egui::Painter,
    rect: egui::Rect,
    peak_l: f32,
    peak_r: f32,
    theme: &KlippTheme,
) {
    let bar_h = ((rect.height() - 2.0) * 0.5).max(1.0);
    painter.rect_filled(rect, 1.0, theme.bg_deep);
    // An outline, because the trough alone is the same value as the
    // panel behind it: with nothing playing the meter was invisible
    // apart from its -6 mark, so there was no sign it existed until
    // something made a sound. A meter you cannot find at rest is not a
    // meter an editor learns to glance at.
    painter.rect_stroke(rect, 1.0, egui::Stroke::new(1.0_f32, theme.border));

    // The -6 dB mark, so "near the top" has somewhere to be near.
    let warn_x = rect.left()
        + rect.width() * ((MASTER_METER_WARN_DB - MASTER_METER_FLOOR_DB) / -MASTER_METER_FLOOR_DB);
    painter.vline(
        warn_x,
        rect.y_range(),
        egui::Stroke::new(1.0_f32, theme.border),
    );

    for (i, peak) in [peak_l, peak_r].into_iter().enumerate() {
        let top = rect.top() + 1.0 + i as f32 * (bar_h + 1.0) - if i == 0 { 1.0 } else { 0.0 };
        let top = top.max(rect.top());
        let w = rect.width() * master_meter_fraction(peak);
        if w <= 0.0 {
            continue;
        }
        let colour = if master_meter_is_clipping(peak) {
            theme.marker_error
        } else if 20.0 * peak.max(1e-6).log10() >= MASTER_METER_WARN_DB {
            theme.marker_warning
        } else {
            theme.marker_success
        };
        painter.rect_filled(
            egui::Rect::from_min_size(egui::pos2(rect.left(), top), egui::vec2(w, bar_h)),
            0.0,
            colour,
        );
    }
}

impl ViewerPanel {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        kb: &crate::keybindings::KeyBindings,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        // Use max_rect to get the FULL allocated rect — no insets from cursor/spacing
        let panel_rect = ui.max_rect();

        // Zero out spacing so nothing insets the content
        ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);

        // Background: pure black, painted to the full rect
        ui.painter().rect_filled(panel_rect, 0.0, egui::Color32::BLACK);
        ui.advance_cursor_after_rect(panel_rect);

        // Reserve space: scrub bar lane (16px — gives the playhead ball
        // 14px of vertical room without being clipped by the picture above
        // or the playbar below) + playbar (40px). The visible track inside
        // the lane is still a thin 4px strip; the rest is breathing room.
        let scrub_h = 16.0;
        let playbar_h = 40.0;
        let bottom_h = scrub_h + playbar_h;
        let viewer_rect = egui::Rect::from_min_max(
            panel_rect.min,
            egui::pos2(panel_rect.max.x, panel_rect.max.y - bottom_h),
        );

        // Side-by-side dual viewer: this panel is always the Program
        // Viewer. Source content lives in its own panel
        // (`render_source_viewer_placeholder` in app.rs). The legacy
        // Program/Source toggle is gone — `viewer_mode` is retained as a
        // focus/intent hint for keyboard handlers (e.g. I/O marks
        // targeting source vs sequence) but no longer drives visibility
        // here.
        self.show_program_viewer(ui, state, theme, viewer_rect, &mut cmds);
        // Trim mode (#914): with an edit point selected, the two frames
        // either side of the cut, over the picture.
        crate::trim_view::draw(ui, state, theme, viewer_rect);

        // Scroll-zoom + middle-drag-pan over the video area. Allocated
        // AFTER `show_program_viewer` so transform handles / on-canvas
        // gizmos painted inside the viewer keep priority on LMB events.
        let tex_size = self.video_texture.as_ref()
            .map(|t| t.size_vec2())
            .unwrap_or_else(|| {
                state.media_pool.all().next()
                    .map(|m| egui::vec2(m.resolution.width as f32, m.resolution.height as f32))
                    .unwrap_or(egui::vec2(1920.0, 1080.0))
            });
        self.handle_viewer_zoom_pan_input(
            ui,
            state,
            viewer_rect,
            tex_size,
            ViewerMode::Program,
        );

        // Floating "..." overflow menu in the video area's top-right
        // corner — mirrors the Source side. Holds whatever the responsive
        // playbar dropped at the current width plus zoom presets and the
        // viewer overlay toggles.
        self.draw_viewer_overflow_menu(
            ui,
            state,
            theme,
            viewer_rect,
            ViewerMode::Program,
            kb,
            &mut cmds,
        );
        // The nodes this frame didn't draw as asked (M4 step 10): after
        // the zoom and pan, so a click on a chip is the chip's.
        crate::panels::viewer_declines::draw_chips(ui, state, theme, viewer_rect, &mut cmds);

        // ── Scrub bar + Play bar (shared transport chrome) ──
        let chrome_rect = egui::Rect::from_min_size(
            egui::pos2(panel_rect.min.x, viewer_rect.max.y),
            egui::vec2(panel_rect.width(), bottom_h),
        );
        let chrome_cmds = self.draw_transport_chrome(ui, state, theme, chrome_rect, ViewerMode::Program);
        cmds.extend(chrome_cmds);

        cmds
    }

    /// Draw the scrub bar + play bar (transport chrome) within the given
    /// rect. The scrub bar gets the top 16 px, the play bar the remaining
    /// 40 px below it — matching the fixed heights used inside
    /// `ViewerPanel::show`. Exposed so side-by-side layouts (Source Viewer)
    /// can render matching chrome under their own video rects.
    ///
    /// `viewer_kind` selects which playhead the scrub bar and play-bar
    /// timecode reflect (Program → timeline; Source → source clip), and
    /// which engine the buttons drive: every command from this bar goes
    /// through `route_command_for_viewer`, so the source playbar plays
    /// the source and the program playbar plays the timeline.
    ///
    /// This paragraph used to say the opposite — that the buttons all
    /// targeted the timeline regardless — which was true before source
    /// routing landed and has been wrong since. Left uncorrected it
    /// argues for merging the two bars as duplicates, which they are
    /// not: two viewers with independent playback need two transports,
    /// the same as every other dual-viewer NLE.
    pub fn draw_transport_chrome(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        rect: egui::Rect,
        viewer_kind: ViewerMode,
    ) -> Vec<AppCommand> {
        const SCRUB_H: f32 = 16.0;
        const PLAYBAR_H: f32 = 40.0;
        let mut cmds = Vec::new();
        let scrub_rect = egui::Rect::from_min_size(
            rect.min,
            egui::vec2(rect.width(), SCRUB_H),
        );
        self.draw_scrub_bar(ui, state, theme, scrub_rect, viewer_kind, &mut cmds);
        // (state mut-borrow flows through to scrub_bar so the
        // playhead can update visually on the same frame the user
        // drags — without that the bar lags engine seek latency.)
        let playbar_rect = egui::Rect::from_min_size(
            egui::pos2(rect.min.x, scrub_rect.max.y),
            egui::vec2(rect.width(), PLAYBAR_H),
        );
        self.draw_playbar(ui, state, theme, playbar_rect, viewer_kind, &mut cmds);
        // Viewer focus belongs to the transport strip, separate from the
        // scrub track and its playback-position indicator.
        let focus_color = if state.viewer_mode == viewer_kind {
            theme.accent
        } else {
            theme.border
        };
        ui.painter().rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(playbar_rect.left(), playbar_rect.bottom() - 2.0),
                playbar_rect.right_bottom(),
            ),
            0.0,
            focus_color,
        );
        cmds
    }

    pub fn show_fullscreen(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        let available = ui.available_size();

        let panel_rect = egui::Rect::from_min_size(ui.cursor().min, available);
        let resp = ui.allocate_rect(panel_rect, egui::Sense::hover());
        ui.painter().rect_filled(panel_rect, 0.0, egui::Color32::BLACK);

        // Prefer the GPU-resident output when live; fall back to the
        // CPU-uploaded `video_texture`. The video_texture is used for
        // aspect-ratio detection either way — it's populated in
        // parallel by `upload_frame_to_egui`, so `size_vec2()` is
        // still the correct dimensions even when we paint via
        // `gpu_texture_id`.
        let gpu_id = self.gpu_frame_live.then_some(self.gpu_texture_id).flatten();
        if let Some(tex) = &self.video_texture {
            let tex_size = match gpu_id {
                Some(_) if self.gpu_frame_size.0 > 0 && self.gpu_frame_size.1 > 0 => {
                    egui::vec2(self.gpu_frame_size.0 as f32, self.gpu_frame_size.1 as f32)
                }
                _ => tex.size_vec2(),
            };
            if tex_size.x > 0.0 && tex_size.y > 0.0 {
                let video_aspect = tex_size.x / tex_size.y;
                let panel_aspect = available.x / available.y;
                let (dw, dh) = if video_aspect > panel_aspect {
                    (available.x, available.x / video_aspect)
                } else {
                    (available.y * video_aspect, available.y)
                };
                let image_rect = egui::Rect::from_center_size(
                    panel_rect.center(),
                    egui::vec2(dw, dh),
                );
                let image_id = self.lut_live.then_some(self.lut_texture_id).flatten().or(gpu_id).unwrap_or_else(|| tex.id());
                ui.painter().image(
                    image_id,
                    image_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        }

        // Mouse movement shows overlay
        let dt = ui.input(|i| i.unstable_dt) as f64;
        if resp.hovered() && ui.input(|i| i.pointer.velocity().length() > 1.0) {
            state.fullscreen_overlay_visible = true;
            state.fullscreen_overlay_timer = state.viewer_prefs.fullscreen_overlay_timeout_secs as f64;
        }
        if state.fullscreen_overlay_timer > 0.0 {
            state.fullscreen_overlay_timer -= dt;
            if state.fullscreen_overlay_timer <= 0.0 {
                state.fullscreen_overlay_visible = false;
                state.fullscreen_overlay_timer = 0.0;
            }
        }

        if state.fullscreen_overlay_visible {
            let bar_h = 48.0;
            let bar_rect = egui::Rect::from_min_size(
                egui::pos2(panel_rect.left(), panel_rect.bottom() - bar_h),
                egui::vec2(panel_rect.width(), bar_h),
            );
            ui.painter().rect_filled(
                bar_rect, 0.0,
                egui::Color32::from_rgba_premultiplied(0, 0, 0, 180),
            );
            // In frames at the sequence's rate: the raw value is ticks
            // at an NTSC rate (#2665).
            let tc_text = state.timecode_display().position(state.playhead);
            ui.painter().text(
                egui::pos2(bar_rect.left() + 16.0, bar_rect.center().y),
                egui::Align2::LEFT_CENTER,
                &tc_text,
                egui::FontId::monospace(16.0),
                // Text sits on top of a black-translucent perf
                // overlay bar — stays light regardless of theme
                // polarity (the bar is the contrast partner, not
                // the panel underneath).
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 220),
            );
            let playback_text = match state.playback {
                PlaybackState::Playing => "\u{25B6} Playing",
                PlaybackState::Paused => "\u{23F8} Paused",
                PlaybackState::Idle | PlaybackState::Stopped => "\u{23F9} Idle",
            };
            ui.painter().text(
                egui::pos2(bar_rect.right() - 16.0, bar_rect.center().y),
                egui::Align2::RIGHT_CENTER,
                playback_text,
                egui::FontId::proportional(13.0),
                egui::Color32::from_rgba_premultiplied(200, 200, 210, 200),
            );
            ui.ctx().request_repaint();
        }

        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            cmds.push(AppCommand::ToggleFullscreen);
        }
        if state.playback == PlaybackState::Playing {
            ui.ctx().request_repaint();
        }

        cmds
    }

    /// Capture clicks/drags onto the viewer image when the AI Tools
    /// panel is open with an active task. Routes pointer events into
    /// `state.ai_tools_state.prompts` in normalised image coords.
    fn handle_ai_tools_capture(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        image_rect: egui::Rect,
    ) {
        if !state.ai_tools_state.panel_visible
            || state.ai_tools_state.active_task.is_none()
            || state.eyedropper_mode != crate::state::EyedropperMode::None
        {
            return;
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        let resp = ui.allocate_rect(image_rect, egui::Sense::click_and_drag());
        let normalise = |p: egui::Pos2| -> (f32, f32) {
            let nx = ((p.x - image_rect.min.x) / image_rect.width()).clamp(0.0, 1.0);
            let ny = ((p.y - image_rect.min.y) / image_rect.height()).clamp(0.0, 1.0);
            (nx, ny)
        };

        if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let (nx, ny) = normalise(p);
                state
                    .ai_tools_state
                    .prompts
                    .record_click(nx, ny, crate::ai_tools::ClickButton::Left);
            }
        }
        if resp.secondary_clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let (nx, ny) = normalise(p);
                state
                    .ai_tools_state
                    .prompts
                    .record_click(nx, ny, crate::ai_tools::ClickButton::Right);
            }
        }
        if resp.drag_started() {
            if let Some(p) = resp.interact_pointer_pos() {
                let (nx, ny) = normalise(p);
                let button = if ui.input(|i| i.pointer.secondary_down()) {
                    crate::ai_tools::ClickButton::Right
                } else {
                    crate::ai_tools::ClickButton::Left
                };
                state.ai_tools_state.prompts.start_drag(nx, ny, button);
            }
        } else if resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                let (nx, ny) = normalise(p);
                state.ai_tools_state.prompts.extend_drag(nx, ny);
            }
        }
        if resp.drag_stopped() {
            if let Some(p) = resp.interact_pointer_pos() {
                let (nx, ny) = normalise(p);
                state.ai_tools_state.prompts.end_drag(nx, ny);
            }
        }
    }

    /// Paint the AI Tools mask overlay + prompt markers onto the
    /// viewer when the panel is open and there's something to show.
    /// No-op when the panel is closed.
    fn draw_ai_tools_overlay(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        image_rect: egui::Rect,
    ) {
        if !state.ai_tools_state.panel_visible {
            return;
        }

        let painter = ui.painter_at(image_rect);

        if let Some(tex) = &state.ai_tools_state.mask_overlay_texture {
            crate::ai_tools::paint_mask_texture(&painter, image_rect, tex);
        }

        // Per-candidate bbox + class label badge. RF-DETR-Seg
        // emits up to 64 candidates each with bbox + label +
        // confidence; we draw all of them so the user sees
        // multi-object detections rather than just the
        // highest-confidence one. BiRefNet / MobileSAM still
        // produce a single candidate with `bbox = None` so the
        // bbox-drawing branch falls through cleanly for them.
        let candidates = state.ai_tools_state.inference.candidates();
        for (idx, cand) in candidates.iter().enumerate() {
            let Some(bbox) = cand.bbox else {
                continue;
            };
            let bbox_color = crate::ai_tools::detection_color(idx);
            let stroke = egui::Stroke::new(2.0_f32, opaque_for_outline(bbox_color));
            let bx0 = image_rect.min.x + bbox.x * image_rect.width();
            let by0 = image_rect.min.y + bbox.y * image_rect.height();
            let bx1 = bx0 + bbox.w * image_rect.width();
            let by1 = by0 + bbox.h * image_rect.height();
            let rect = egui::Rect::from_min_max(
                egui::pos2(bx0, by0),
                egui::pos2(bx1, by1),
            );
            painter.rect_stroke(rect, 2.0, stroke);

            // Label badge — class name + confidence at the top-left
            // of the bbox. White text on the bbox color so it stays
            // readable on any background.
            let label_text = format!(
                "{} {:.0}%",
                cand.label.as_deref().unwrap_or("?"),
                cand.confidence * 100.0,
            );
            let font_id = egui::FontId::proportional(11.0);
            let galley = painter.layout_no_wrap(
                label_text,
                font_id,
                egui::Color32::WHITE,
            );
            let badge_rect = egui::Rect::from_min_size(
                egui::pos2(bx0, by0 - galley.rect.height() - 4.0),
                egui::vec2(galley.rect.width() + 8.0, galley.rect.height() + 4.0),
            );
            painter.rect_filled(badge_rect, 2.0, opaque_for_outline(bbox_color));
            painter.galley(
                badge_rect.min + egui::vec2(4.0, 2.0),
                galley,
                egui::Color32::WHITE,
            );
        }

        // Click / drag markers — small dots for clicks, rectangle for
        // boxes, polyline for scribbles. Visualises what the model is
        // about to receive so the user can preview their prompt set.
        let to_screen = |x: f32, y: f32| -> egui::Pos2 {
            egui::pos2(
                image_rect.min.x + x * image_rect.width(),
                image_rect.min.y + y * image_rect.height(),
            )
        };
        for click in &state.ai_tools_state.prompts.clicks {
            let color = if click.positive {
                egui::Color32::from_rgb(80, 220, 100)
            } else {
                egui::Color32::from_rgb(220, 80, 80)
            };
            painter.circle_filled(to_screen(click.x, click.y), 5.0, color);
            painter.circle_stroke(
                to_screen(click.x, click.y),
                5.0,
                egui::Stroke::new(1.5_f32, egui::Color32::WHITE),
            );
        }
        for b in &state.ai_tools_state.prompts.boxes {
            let r = egui::Rect::from_two_pos(to_screen(b.x1, b.y1), to_screen(b.x2, b.y2));
            painter.rect_stroke(
                r,
                0.0,
                egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(80, 200, 240)),
            );
        }
        for s in &state.ai_tools_state.prompts.finalized_scribbles {
            if s.points.len() < 2 {
                continue;
            }
            let stroke = egui::Stroke::new(
                2.0_f32,
                if s.positive {
                    egui::Color32::from_rgb(80, 220, 100)
                } else {
                    egui::Color32::from_rgb(220, 80, 80)
                },
            );
            for window in s.points.windows(2) {
                let a = to_screen(window[0].0, window[0].1);
                let b = to_screen(window[1].0, window[1].1);
                painter.line_segment([a, b], stroke);
            }
        }
    }

    fn handle_eyedropper_click(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        image_rect: egui::Rect,
        cmds: &mut Vec<AppCommand>,
    ) {
        // ── A 3D qualifier stroke (#895): dragged across the colours ──
        if state.eyedropper_mode == crate::state::EyedropperMode::QualifierStroke {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            let resp = ui.allocate_rect(image_rect, egui::Sense::click_and_drag());
            let to_frame = |p: egui::Pos2| [((p.x - image_rect.min.x) / image_rect.width()).clamp(0.0, 1.0), ((p.y - image_rect.min.y) / image_rect.height()).clamp(0.0, 1.0)];
            if resp.drag_started() {
                self.stroke_points.clear();
            }
            if resp.dragged() || resp.drag_started() {
                if let Some(p) = resp.interact_pointer_pos() {
                    self.stroke_points.push(to_frame(p));
                }
            }
            // The stroke drawn as it goes.
            let line: Vec<egui::Pos2> = self.stroke_points.iter().map(|p| egui::pos2(image_rect.min.x + p[0] * image_rect.width(), image_rect.min.y + p[1] * image_rect.height())).collect();
            let remove = state.qualifier_stroke_pick.is_some_and(|(_, _, r)| r);
            let colour = if remove { egui::Color32::from_rgb(230, 80, 80) } else { egui::Color32::from_rgb(90, 200, 120) };
            if line.len() > 1 {
                ui.painter().add(egui::Shape::line(line, egui::Stroke::new(3.0_f32, colour)));
            }
            let done = if resp.drag_stopped() {
                Some(std::mem::take(&mut self.stroke_points))
            } else if resp.clicked() {
                resp.interact_pointer_pos().map(|p| vec![to_frame(p)])
            } else {
                None
            };
            if let Some(points) = done.filter(|p| !p.is_empty()) {
                cmds.push(AppCommand::QualifierStroke { points });
                state.eyedropper_mode = crate::state::EyedropperMode::None;
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                state.eyedropper_mode = crate::state::EyedropperMode::None;
                state.qualifier_stroke_pick = None;
                self.stroke_points.clear();
            }
            return;
        }
        // ── Eyedropper interaction (White Balance or Qualifier) ──
        if state.eyedropper_mode != crate::state::EyedropperMode::None {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            let eyedropper_resp = ui.allocate_rect(image_rect, egui::Sense::click());
            if eyedropper_resp.clicked() {
                if let Some(pos) = eyedropper_resp.interact_pointer_pos() {
                    let nx = ((pos.x - image_rect.min.x) / image_rect.width()).clamp(0.0, 1.0);
                    let ny = ((pos.y - image_rect.min.y) / image_rect.height()).clamp(0.0, 1.0);
                    match state.eyedropper_mode {
                        crate::state::EyedropperMode::WhiteBalance => {
                            cmds.push(AppCommand::RequestWhiteBalanceSample { nx, ny });
                        }
                        crate::state::EyedropperMode::Qualifier => {
                            cmds.push(AppCommand::RequestQualifierSample { nx, ny });
                        }
                        crate::state::EyedropperMode::HueCurve => {
                            cmds.push(AppCommand::RequestHueCurveSample { nx, ny });
                        }
                        crate::state::EyedropperMode::None | crate::state::EyedropperMode::QualifierStroke => {}
                    }
                    state.eyedropper_mode = crate::state::EyedropperMode::None;
                }
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                state.eyedropper_mode = crate::state::EyedropperMode::None;
                state.wb_pick_clip = None;
                state.qualifier_pick = None;
                state.hue_curve_pick = None;
            }
        }
    }

    fn draw_empty_state(&mut self, ui: &mut egui::Ui, viewer_rect: egui::Rect, theme: &KlippTheme) {
        // Empty state: Klipp logo at 40% opacity
        let center = viewer_rect.center();
        let logo_tex = self.logo_texture.get_or_insert_with(|| {
            let png_bytes = include_bytes!("../../../../assets/branding/klipp-refined/klipp-icon-128.png");
            let image = crate::ui_util::decode_png_or_fallback(png_bytes);
            crate::app::alloc_texture_logged(ui.ctx(), "klipp_viewer_logo", image, egui::TextureOptions::LINEAR)
        });
        let logo_size = 48.0;
        let logo_rect = egui::Rect::from_center_size(
            egui::pos2(center.x, center.y - 8.0),
            egui::vec2(logo_size, logo_size),
        );
        ui.painter().image(
            logo_tex.id(),
            logo_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::from_white_alpha(102),
        );
        let logo_color = egui::Color32::from_rgba_unmultiplied(
            theme.accent.r(), theme.accent.g(), theme.accent.b(), 102,
        );
        ui.painter().text(
            egui::pos2(center.x, center.y + logo_size * 0.5),
            egui::Align2::CENTER_CENTER,
            "Klipp",
            egui::FontId::proportional(16.0),
            logo_color,
        );
    }

    /// Compute per-frame layout state for the program viewer.
    /// Returns `None` if there's no content to draw (caller should early-return).
    /// Also draws the empty state and stores bookkeeping on `state` as a side effect.
    fn compute_video_layout(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        viewer_rect: egui::Rect,
        theme: &KlippTheme,
    ) -> Option<ViewerFrameState> {
        // Extract media info up-front so the borrow is short-lived
        let current_media_info: Option<(u32, u32, bool)> = state.sequence().tracks.iter()
            .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
            .find_map(|t| t.clip_at_timecode(state.playhead))
            .and_then(|(_, clip)| clip.media_id().and_then(|mid| state.media_pool.get(&mid)))
            .map(|m| (m.resolution.width, m.resolution.height, m.proxy_ready));

        // Anything pictured at the playhead: a project just opened is
        // Idle, and its first shot may be a title, a still or a compound
        // clip, not media (#2664).
        let has_content = state.playback != PlaybackState::Idle || pictured_at(state.sequence(), state.playhead);

        if !has_content {
            self.draw_empty_state(ui, viewer_rect, theme);
            return None;
        }

        // Fit the Program image to the stable sequence/output aspect. The
        // texture can be recreated at a different backing size when the
        // viewer layout changes; using that transient size here makes the
        // same portrait frame become wider or narrower between dual and
        // single layouts, which also moves and clips title overlays.
        // Texture pixels are sampled into this sequence frame below.
        let sequence_resolution = state.sequence().resolution;
        let (video_w, video_h) = if sequence_resolution.width > 0 && sequence_resolution.height > 0 {
            (sequence_resolution.width as f32, sequence_resolution.height as f32)
        } else {
            current_media_info
                .map(|(w, h, _)| (w as f32, h as f32))
                .or_else(|| state.media_pool.all().next().map(|m| (m.resolution.width as f32, m.resolution.height as f32)))
                .unwrap_or((1920.0, 1080.0))
        };

        if video_w == 0.0 || video_h == 0.0 { return None; }

        let video_aspect = video_w / video_h;
        let panel_aspect = viewer_rect.width() / viewer_rect.height();

        // Honor the user's per-viewer zoom selection. `zoom == 0.0`
        // means "fit to viewer" (the default and pro-NLE letterbox
        // behavior). Any other value is a multiplier on the source's
        // native pixel dimensions. Pan only applies in zoom-in mode;
        // Fit always centers. The painter is clipped to `viewer_rect`
        // below so high-zoom output doesn't bleed into chrome.
        let view = state.program_view;
        let (display_w, display_h) = if view.zoom <= 0.0 {
            if video_aspect > panel_aspect {
                (viewer_rect.width(), viewer_rect.width() / video_aspect)
            } else {
                (viewer_rect.height() * video_aspect, viewer_rect.height())
            }
        } else {
            (video_w * view.zoom, video_h * view.zoom)
        };

        let pan = if view.zoom <= 0.0 { egui::Vec2::ZERO } else { view.pan };
        let image_rect = egui::Rect::from_center_size(
            viewer_rect.center() + pan,
            egui::vec2(display_w, display_h),
        );

        // Store viewer image rect for scope cursor sampling
        state.viewer_image_rect = Some(image_rect);

        // Scope cursor: will be set at end of function
        state.viewer_pointer_pos = None;

        // Grade still split-view comparison (takes priority over before/after)
        let grade_still_active = state.grade_still_compare
            && state.active_grade_still.is_some()
            && state.active_grade_still.unwrap() < state.grade_stills.len();

        // A Colour page tool: the Edit page's viewer shows the frame.
        let wipe_frame = state.wipe_clip.filter(|_| state.show_color_panel).and_then(|c| state.graded_thumbs.wipe_frame(c)).map(|t| t.id());
        Some(ViewerFrameState {
            image_rect,
            viewer_rect,
            video_w,
            video_h,
            grade_still_active: grade_still_active && wipe_frame.is_none(),
            wipe_frame,
            current_media_info,
        })
    }

    fn draw_grade_still_split(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        frame: &ViewerFrameState,
    ) {
        let image_rect = frame.image_rect;
        let split_frac = state.split_position;
        let split_x = image_rect.min.x + split_frac * image_rect.width();

        // Left side: the wiped clip's graded frame (#899), or the grade
        // still.
        if let Some(id) = frame.wipe_frame {
            let left_rect = egui::Rect::from_min_max(image_rect.min, egui::pos2(split_x, image_rect.max.y));
            ui.painter().image(id, left_rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(split_frac, 1.0)), egui::Color32::WHITE);
            self.draw_live_right(ui, image_rect, split_frac, split_x);
            return;
        }
        let still_idx = state.active_grade_still.unwrap();
        let still = &mut state.grade_stills[still_idx];
        // Lazily upload texture
        if still.texture.is_none() && !still.rgba.is_empty() {
            let sw = still.width as usize;
            let sh = still.height as usize;
            let pixels: Vec<egui::Color32> = still.rgba.chunks_exact(4)
                .map(|px| egui::Color32::from_rgba_premultiplied(px[0], px[1], px[2], px[3]))
                .collect();
            let image = egui::ColorImage { size: [sw, sh], pixels };
            let name = format!("grade_still_{}", still_idx);
            still.texture = Some(crate::app::alloc_texture_logged(
                ui.ctx(),
                &name,
                image,
                egui::TextureOptions::LINEAR,
            ));
        }
        if let Some(tex) = &still.texture {
            let left_rect = egui::Rect::from_min_max(image_rect.min, egui::pos2(split_x, image_rect.max.y));
            let uv_right = split_frac;
            ui.painter().image(
                tex.id(),
                left_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(uv_right, 1.0)),
                egui::Color32::WHITE,
            );
        }

        self.draw_live_right(ui, image_rect, split_frac, split_x);
    }

    /// A split's right side: the current graded frame.
    fn draw_live_right(&self, ui: &mut egui::Ui, image_rect: egui::Rect, split_frac: f32, split_x: f32) {
        if let Some(tex) = &self.video_texture {
            let right_rect = egui::Rect::from_min_max(egui::pos2(split_x, image_rect.min.y), image_rect.max);
            let uv_left = split_frac;
            ui.painter().image(
                tex.id(),
                right_rect,
                egui::Rect::from_min_max(egui::pos2(uv_left, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
    }

    fn draw_split_divider(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        frame: &ViewerFrameState,
    ) {
        let image_rect = frame.image_rect;
        let grade_still_active = frame.grade_still_active || frame.wipe_frame.is_some();
        let stab_compare_active = state.stabilization_compare_active;
        let effect_compare_active = state.split_compare_node.is_some();
        let split_x = image_rect.min.x + state.split_position * image_rect.width();
        let divider_color = if grade_still_active {
            theme.marker_info // cool blue for grade still compare
        } else if stab_compare_active {
            theme.marker_success // green for stabilization compare
        } else if effect_compare_active {
            theme.accent_alt // per-effect compare — distinct from primary accent
        } else {
            theme.accent
        };

        // Vertical divider line
        ui.painter().vline(
            split_x,
            image_rect.y_range(),
            egui::Stroke::new(2.0_f32, divider_color),
        );

        // Labels
        let label_font = egui::FontId::proportional(11.0);
        let label_y = image_rect.min.y + 14.0;
        let (left_label, right_label) = if frame.wipe_frame.is_some() {
            let seq = &state.sequences[state.active_sequence_idx];
            let name = state.wipe_clip.and_then(|id| seq.all_clips().find(|c| c.id == id)).map(|c| {
                let media = c.media_id().and_then(|m| state.media_pool.get(&m)).map(|m| m.filename.clone()).unwrap_or_default();
                c.label(&media).to_string()
            }).unwrap_or_default();
            (format!("Clip: {name}"), "Now".to_string())
        } else if grade_still_active {
            let idx = state.active_grade_still.unwrap();
            (format!("Still: {}", state.grade_stills[idx].name), "Now".to_string())
        } else if stab_compare_active {
            ("Raw".to_string(), "Stabilized".to_string())
        } else if effect_compare_active {
            ("Without".to_string(), "With".to_string())
        } else {
            ("Before".to_string(), "After".to_string())
        };
        // On a dark pill each, so they read over any picture: bare white
        // text vanished over a bright frame, and a split nobody can tell
        // is on reads as a half-ungraded picture.
        let pill = |ui: &egui::Ui, pos: egui::Pos2, align: egui::Align2, text: &str| {
            let galley = ui.painter().layout_no_wrap(text.to_string(), label_font.clone(), egui::Color32::WHITE);
            let rect = align.anchor_size(pos, galley.size() + egui::vec2(10.0, 4.0));
            ui.painter().rect_filled(rect, 3.0, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 170));
            ui.painter().galley(rect.min + egui::vec2(5.0, 2.0), galley, egui::Color32::WHITE);
        };
        pill(ui, egui::pos2(split_x - 8.0, label_y), egui::Align2::RIGHT_CENTER, &left_label);
        pill(ui, egui::pos2(split_x + 8.0, label_y), egui::Align2::LEFT_CENTER, &right_label);

        // Small triangle arrows on the divider
        let arrow_y = image_rect.center().y;
        let tri_size = 6.0;
        // Left arrow
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(split_x - 2.0, arrow_y - tri_size),
                egui::pos2(split_x - 2.0 - tri_size, arrow_y),
                egui::pos2(split_x - 2.0, arrow_y + tri_size),
            ],
            divider_color,
            egui::Stroke::NONE,
        ));
        // Right arrow
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(split_x + 2.0, arrow_y - tri_size),
                egui::pos2(split_x + 2.0 + tri_size, arrow_y),
                egui::pos2(split_x + 2.0, arrow_y + tri_size),
            ],
            divider_color,
            egui::Stroke::NONE,
        ));

        // Drag interaction on divider
        let drag_rect = egui::Rect::from_center_size(
            egui::pos2(split_x, image_rect.center().y),
            egui::vec2(16.0, image_rect.height()),
        );
        let drag_resp = ui.allocate_rect(drag_rect, egui::Sense::drag());
        if drag_resp.dragged() {
            if let Some(pos) = drag_resp.interact_pointer_pos() {
                state.split_position = ((pos.x - image_rect.min.x) / image_rect.width())
                    .clamp(0.05, 0.95);
            }
        }
        if drag_resp.hovered() || drag_resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
    }

    fn draw_overlay_badges(
        &self,
        painter: &egui::Painter,
        state: &AppState,
        frame: &ViewerFrameState,
    ) {
        let image_rect = frame.image_rect;
        // Collect labels for all active overlays (multiple can be active)
        let mut labels: Vec<&str> = Vec::new();
        let ov = &state.viewer_overlays;
        if ov.zebras { labels.push("ZEBRA"); }
        if ov.false_color { labels.push("FALSE COLOUR"); }
        if ov.focus_peaking { labels.push("FOCUS PEAKING"); }
        if ov.safe_areas { labels.push("SAFE AREAS"); }
        if ov.grid { labels.push("GRID"); }
        // no badge for checkerboard
        if !labels.is_empty() {
            let label = labels.join(" + ");
            let badge_pos = egui::pos2(image_rect.max.x - 8.0, image_rect.min.y + 12.0);
            let badge_color = egui::Color32::from_rgb(0xff, 0x40, 0x40);
            // Shadow
            painter.text(
                egui::pos2(badge_pos.x + 0.5, badge_pos.y + 0.5),
                egui::Align2::RIGHT_CENTER,
                &label,
                egui::FontId::proportional(10.0),
                egui::Color32::from_rgb(0x00, 0x00, 0x00),
            );
            painter.text(
                badge_pos,
                egui::Align2::RIGHT_CENTER,
                &label,
                egui::FontId::proportional(10.0),
                badge_color,
            );
        }
    }

    fn draw_safe_area_overlay(
        &self,
        painter: &egui::Painter,
        state: &AppState,
        frame: &ViewerFrameState,
    ) {
        let image_rect = frame.image_rect;
        let center = image_rect.center();
        let w = image_rect.width();
        let h = image_rect.height();
        // 90% action safe
        let action_rect = egui::Rect::from_center_size(center, egui::vec2(w * state.viewer_prefs.action_safe_pct, h * state.viewer_prefs.action_safe_pct));
        painter.rect_stroke(action_rect, 0.0, egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(255, 200, 0, 120)));
        painter.text(
            egui::pos2(action_rect.left() + 4.0, action_rect.top() + 2.0),
            egui::Align2::LEFT_TOP, "Action Safe",
            egui::FontId::proportional(10.0),
            egui::Color32::from_rgba_premultiplied(255, 200, 0, 140),
        );
        // 80% title safe
        let title_rect = egui::Rect::from_center_size(center, egui::vec2(w * state.viewer_prefs.title_safe_pct, h * state.viewer_prefs.title_safe_pct));
        painter.rect_stroke(title_rect, 0.0, egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(0, 180, 255, 120)));
        painter.text(
            egui::pos2(title_rect.left() + 4.0, title_rect.top() + 2.0),
            egui::Align2::LEFT_TOP, "Title Safe",
            egui::FontId::proportional(10.0),
            egui::Color32::from_rgba_premultiplied(0, 180, 255, 140),
        );
    }

    fn draw_grid_overlay(
        &self,
        painter: &egui::Painter,
        state: &AppState,
        frame: &ViewerFrameState,
    ) {
        let image_rect = frame.image_rect;
        let w = image_rect.width();
        let h = image_rect.height();
        let gc = state.viewer_prefs.grid_color;
        let stroke = egui::Stroke::new(0.5_f32, egui::Color32::from_rgba_premultiplied(gc[0], gc[1], gc[2], gc[3]));
        // Rule of thirds: vertical lines
        for i in 1..=2 {
            let x = image_rect.left() + w * i as f32 / 3.0;
            painter.line_segment([egui::pos2(x, image_rect.top()), egui::pos2(x, image_rect.bottom())], stroke);
        }
        // Rule of thirds: horizontal lines
        for i in 1..=2 {
            let y = image_rect.top() + h * i as f32 / 3.0;
            painter.line_segment([egui::pos2(image_rect.left(), y), egui::pos2(image_rect.right(), y)], stroke);
        }
        // Center crosshair — reuses the grid_color pref with boosted alpha
        // so it remains visible against bright frames where the faint grid
        // would normally disappear. Clamped to u8.
        let cx = image_rect.center().x;
        let cy = image_rect.center().y;
        let cross_alpha = ((gc[3] as u16) * 2).min(255) as u8;
        let cross_color = egui::Color32::from_rgba_premultiplied(gc[0], gc[1], gc[2], cross_alpha);
        let cross = egui::Stroke::new(0.5_f32, cross_color);
        painter.line_segment([egui::pos2(cx - 12.0, cy), egui::pos2(cx + 12.0, cy)], cross);
        painter.line_segment([egui::pos2(cx, cy - 12.0), egui::pos2(cx, cy + 12.0)], cross);
    }

    fn draw_checkerboard_overlay(
        &self,
        painter: &egui::Painter,
        state: &AppState,
        frame: &ViewerFrameState,
    ) {
        let image_rect = frame.image_rect;
        let tile: f32 = state.viewer_prefs.checkerboard_tile_size;
        let c1 = egui::Color32::from_rgb(80, 80, 80);
        let c2 = egui::Color32::from_rgb(120, 120, 120);
        let x0 = image_rect.left();
        let y0 = image_rect.top();
        let mut y = y0;
        let mut row = 0;
        while y < image_rect.bottom() {
            let mut x = x0;
            let mut col = 0;
            while x < image_rect.right() {
                let color = if (row + col) % 2 == 0 { c1 } else { c2 };
                let r = egui::Rect::from_min_size(
                    egui::pos2(x, y),
                    egui::vec2(tile.min(image_rect.right() - x), tile.min(image_rect.bottom() - y)),
                );
                painter.rect_filled(r, 0.0, color);
                x += tile;
                col += 1;
            }
            y += tile;
            row += 1;
        }
    }

    fn draw_proxy_badge(
        &self,
        painter: &egui::Painter,
        state: &AppState,
        theme: &KlippTheme,
        frame: &ViewerFrameState,
    ) {
        let image_rect = frame.image_rect;
        if state.showing_thumbnail_preview {
            let badge_pos = egui::pos2(image_rect.right() - 8.0, image_rect.top() + 8.0);
            painter.text(
                badge_pos,
                egui::Align2::RIGHT_TOP,
                "PREVIEW",
                egui::FontId::monospace(10.0),
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 140),
            );
        } else if state.use_proxies && frame.current_media_info.is_some_and(|(_, _, proxy_ready)| proxy_ready) {
            let badge_pos = egui::pos2(image_rect.right() - 8.0, image_rect.top() + 8.0);
            painter.text(
                badge_pos,
                egui::Align2::RIGHT_TOP,
                "PROXY",
                egui::FontId::monospace(10.0),
                theme.accent,
            );
        }
    }

    fn draw_multicam_angle_bar(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        cmds: &mut Vec<AppCommand>,
        frame: &ViewerFrameState,
    ) {
        let image_rect = frame.image_rect;
        let bar_h = 32.0;
        let bar_rect = egui::Rect::from_min_size(
            egui::pos2(image_rect.left(), image_rect.bottom() + 4.0),
            egui::vec2(image_rect.width(), bar_h),
        );

        let btn_w = (bar_rect.width() / state.multicam_angles.len().max(1) as f32).min(80.0);
        for (i, &angle) in state.multicam_angles.iter().enumerate() {
            let is_active = angle == state.multicam_active_angle;
            let btn_rect = egui::Rect::from_min_size(
                egui::pos2(bar_rect.left() + i as f32 * btn_w, bar_rect.top()),
                egui::vec2(btn_w - 2.0, bar_h),
            );
            let resp = ui.allocate_rect(btn_rect, egui::Sense::click());
            let bg = if is_active {
                theme.accent
            } else if resp.hovered() {
                theme.bg_hover
            } else {
                theme.bg_panel
            };
            ui.painter().rect_filled(btn_rect, 4.0, bg);
            // CONTRAST RULE: dark text on amber background
            let text_color = if is_active {
                theme.text_on_accent
            } else {
                theme.text_primary
            };
            ui.painter().text(
                btn_rect.center(),
                egui::Align2::CENTER_CENTER,
                format!("Angle {}", angle as usize + 1),
                egui::FontId::proportional(12.0),
                text_color,
            );

            if resp.clicked() {
                cmds.push(AppCommand::SwitchMulticamAngle { angle: angle as usize + 1 });
            }
        }
    }

    fn handle_scope_cursor_input(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        frame: &ViewerFrameState,
    ) {
        let image_rect = frame.image_rect;
        // Scope cursor: read pointer position from raw input events (most reliable on Wayland)
        for event in &ui.ctx().input(|i| i.events.clone()) {
            if let egui::Event::PointerMoved(pos) = event {
                if image_rect.contains(*pos) {
                    state.viewer_pointer_pos = Some(*pos);
                }
            }
        }
        // Fallback: also check egui's pointer state
        if state.viewer_pointer_pos.is_none() {
            if let Some(pos) = ui.input(|i| i.pointer.hover_pos().or_else(|| i.pointer.latest_pos())) {
                if image_rect.contains(pos) {
                    state.viewer_pointer_pos = Some(pos);
                }
            }
        }

        // Click-to-lock scope cursor: Ctrl+Click on viewer image
        if state.scope_cursor_show || (state.scope_window.open
            && state.scope_window.panes.iter().any(|p| p.settings.show_scope_cursor))
        {
            let clicked = ui.input(|i| i.pointer.primary_clicked() && i.modifiers.ctrl);
            if clicked {
                if let Some(pos) = state.viewer_pointer_pos {
                    // Toggle lock: if already locked at this position, unlock
                    if state.scope_cursor_locked {
                        state.scope_cursor_locked = false;
                        state.scope_cursor_locked_pos = None;
                        state.scope_cursor_locked_data = None;
                    } else {
                        state.scope_cursor_locked = true;
                        state.scope_cursor_locked_pos = Some(pos);
                        // Data will be computed by app.rs on next frame
                    }
                }
            }
        }
    }

    fn draw_scope_cursor(
        &self,
        painter: &egui::Painter,
        state: &AppState,
        frame: &ViewerFrameState,
    ) {
        let image_rect = frame.image_rect;
        let video_w = frame.video_w;
        let video_h = frame.video_h;

        // Draw sample box overlay on viewer when any scope cursor is active
        let floating_cursor = state.scope_window.open
            && state.scope_window.panes.iter().any(|p| p.settings.show_scope_cursor);
        let bottom_cursor = state.scope_cursor_show && state.show_scopes;
        let any_cursor = floating_cursor || bottom_cursor;

        // Determine the effective sample position (live or locked)
        let effective_pos = if state.scope_cursor_locked {
            state.scope_cursor_locked_pos
        } else {
            state.viewer_pointer_pos
        };

        if let Some(pos) = effective_pos {
            if any_cursor {
                // Pick the largest sample radius across all active scope cursors
                let mut sample_radius = if bottom_cursor { state.scope_cursor_sample_radius } else { 0 };
                if floating_cursor {
                    let win_r = state.scope_window.panes.iter()
                        .filter(|p| p.settings.show_scope_cursor)
                        .map(|p| p.settings.scope_cursor_sample_radius)
                        .max()
                        .unwrap_or(0);
                    sample_radius = sample_radius.max(win_r);
                }
                let scale_x = image_rect.width() / video_w;
                let scale_y = image_rect.height() / video_h;
                let box_w = (sample_radius as f32 * 2.0 + 1.0) * scale_x;
                let box_h = (sample_radius as f32 * 2.0 + 1.0) * scale_y;
                let cursor_color = if state.scope_cursor_locked {
                    egui::Color32::from_rgba_unmultiplied(232, 160, 32, 220)
                } else {
                    egui::Color32::from_rgba_unmultiplied(255, 200, 50, 200)
                };
                let box_rect = egui::Rect::from_center_size(pos, egui::vec2(box_w, box_h));
                painter.rect_stroke(box_rect, 0.0, egui::Stroke::new(1.0_f32, cursor_color));
                // Center crosshair
                painter.line_segment(
                    [egui::pos2(pos.x - 4.0, pos.y), egui::pos2(pos.x + 4.0, pos.y)],
                    egui::Stroke::new(1.0_f32, cursor_color),
                );
                painter.line_segment(
                    [egui::pos2(pos.x, pos.y - 4.0), egui::pos2(pos.x, pos.y + 4.0)],
                    egui::Stroke::new(1.0_f32, cursor_color),
                );
                // "LOCKED" label
                if state.scope_cursor_locked {
                    painter.text(
                        egui::pos2(pos.x, pos.y - 10.0),
                        egui::Align2::CENTER_BOTTOM,
                        "LOCKED",
                        egui::FontId::proportional(10.0),
                        cursor_color,
                    );
                }
            }
        }
    }

    fn handle_text_edit_entry(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        frame: &ViewerFrameState,
        cmds: &mut Vec<AppCommand>,
    ) {
        let image_rect = frame.image_rect;
        if state.viewer_text_edit {
            return;
        }
        let clip_id = match state.selected_clip {
            Some(id) => id,
            None => return,
        };
        if !ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary)) {
            return;
        }
        let pos = match ui.input(|i| i.pointer.interact_pos()) {
            Some(p) => p,
            None => return,
        };
        if !image_rect.contains(pos) {
            return;
        }
        // Pull the title's data once. If the selected clip isn't a
        // Title, bail out.
        let td_opt = state.sequence().tracks.iter()
            .find_map(|t| t.find_clip(clip_id))
            .and_then(|c| c.title_data())
            .cloned();
        let mut td = match td_opt {
            Some(td) => td,
            None => return,
        };
        // Where the title is drawn (#2014): the press, back on its canvas.
        // A title placed so the canvas can't follow isn't edited here.
        let Some(view) = state.sequence().tracks.iter().find_map(|t| t.find_clip(clip_id)).and_then(|c| title_view_of(state, c, image_rect).ok()) else { return };
        let pos = view.unplace(pos);
        tighten_text_editor_boxes(&mut td, (frame.video_w.max(1.0) as u32, frame.video_h.max(1.0) as u32));
        let clip_time = state.sequence().tracks.iter()
            .find_map(|t| t.find_clip(clip_id))
            .map(|c| (state.playhead_smooth_secs - c.timeline_in.to_secs_f64()).max(0.0))
            .unwrap_or(0.0);
        for obj in &mut td.objects {
            obj.transform.center = obj.transform.evaluated_center(clip_time);
        }

        // Double-click on a TextBox object's bounding box → enter
        // inline edit for THAT object. Iterate top-down so the
        // front-most layer wins when stacks overlap. Hidden /
        // locked layers are excluded.
        let mut target_object: Option<(klipp_timeline::TitleObjectId, String)> = None;
        for obj in td.objects.iter().rev() {
            if !obj.visible || obj.locked {
                continue;
            }
            if let klipp_timeline::TitleObjectContent::TextBox(tb) = &obj.content {
                if object_screen_bb(obj, image_rect).contains(pos) {
                    target_object = Some((obj.id, tb.text.clone()));
                    break;
                }
            }
        }

        if let Some((oid, text)) = target_object {
            // Inline-edit path: enter edit mode anchored to a
            // specific object, and seed the buffer from that
            // object's text rather than `td.text()` (which is the
            // primary TextBox — fine for single-text titles, wrong
            // for multi-object).
            cmds.push(AppCommand::BeginTitleEdit { clip_id });
            cmds.push(AppCommand::SelectTitleObject {
                clip_id,
                object_id: Some(oid.0),
            });
            state.viewer_text_edit_object = Some(oid);
            state.viewer_text_edit_request_focus = true;
            state.viewer_char_positions.clear();
            state.viewer_text_cursor = 0;
            state.viewer_text_select_anchor = None;
            state.viewer_text_edit_buffer = text;
        } else if let Some((oid, text)) = td.objects.iter().find_map(|o| match &o.content {
            klipp_timeline::TitleObjectContent::TextBox(tb) if o.visible && !o.locked => Some((o.id, tb.text.clone())),
            _ => None,
        }) {
            // A double-click beside the text still edits: the first
            // text layer, in place.
            cmds.push(AppCommand::BeginTitleEdit { clip_id });
            cmds.push(AppCommand::SelectTitleObject { clip_id, object_id: Some(oid.0) });
            state.viewer_text_edit_object = Some(oid);
            state.viewer_text_edit_request_focus = true;
            state.viewer_char_positions.clear();
            state.viewer_text_cursor = text.chars().count();
            state.viewer_text_select_anchor = None;
            state.viewer_text_edit_buffer = text;
        }
    }

    fn handle_text_edit_exit(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        cmds: &mut Vec<AppCommand>,
    ) {
        let _ = ui;
        // Auto-exit text edit if the clip was deselected or changed
        if state.viewer_text_edit {
            let still_selected = state.viewer_text_edit_clip
                .is_some_and(|cid| state.selected_clip == Some(cid));
            if !still_selected {
                cmds.push(AppCommand::EndTitleEdit);
                state.viewer_text_edit_buffer.clear();
                state.viewer_text_selection = None;
                state.viewer_text_selection_locked = None;
                state.viewer_text_cursor = 0;
                state.viewer_char_positions.clear();
                state.viewer_text_select_anchor = None;
                state.viewer_text_edit_object = None;
            }
        }
    }

    /// The Text tool (#1713): a click on the picture starts a title
    /// there, a drag draws its text box. The points go to the frame
    /// through [`viewer_screen_to_norm`] on the image rect, as the
    /// title-object editor reads its drags, so the box lands where it was
    /// drawn at any zoom or pan. A drag smaller than
    /// [`TEXT_TOOL_MIN_BOX`] either way is taken as a click where it
    /// started. A click on a title's text at the playhead edits that
    /// title in place instead (#1714).
    fn handle_text_tool(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        frame: &ViewerFrameState,
        cmds: &mut Vec<AppCommand>,
    ) {
        let image_rect = frame.image_rect;
        let area = image_rect.intersect(frame.viewer_rect);
        if !area.is_positive() {
            return;
        }
        let id = ui.id().with("text_tool");
        let resp = ui.interact(area, id, egui::Sense::click_and_drag());
        if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
        }
        // Where the drag began: egui forgets the press as the button
        // comes up, and the box is wanted on that frame.
        if resp.drag_started() {
            if let Some(origin) = ui.input(|i| i.pointer.press_origin()) {
                ui.data_mut(|d| d.insert_temp(id, origin));
            }
        }
        let origin: Option<egui::Pos2> = ui.data(|d| d.get_temp(id));
        let pointer = ui.input(|i| i.pointer.latest_pos());
        let point = |p: egui::Pos2| {
            let (x, y) = viewer_screen_to_norm(image_rect, p);
            AppCommand::CreateTitleInViewer { x, y, box_size: None }
        };
        if resp.drag_stopped() {
            ui.data_mut(|d| d.remove::<egui::Pos2>(id));
            if let (Some(a), Some(b)) = (origin, pointer) {
                let drawn = egui::Rect::from_two_pos(a, b);
                if drawn.width() >= TEXT_TOOL_MIN_BOX && drawn.height() >= TEXT_TOOL_MIN_BOX {
                    let (x0, y0) = viewer_screen_to_norm(image_rect, drawn.min);
                    let (x1, y1) = viewer_screen_to_norm(image_rect, drawn.max);
                    cmds.push(AppCommand::CreateTitleInViewer {
                        x: (x0 + x1) * 0.5,
                        y: (y0 + y1) * 0.5,
                        box_size: Some((x1 - x0, y1 - y0)),
                    });
                } else {
                    cmds.push(point(a));
                }
            }
        } else if resp.dragged() {
            if let (Some(a), Some(b)) = (origin, pointer) {
                ui.painter_at(frame.viewer_rect).rect_stroke(
                    egui::Rect::from_two_pos(a, b),
                    0.0,
                    egui::Stroke::new(1.5_f32, theme.accent),
                );
            }
        } else if resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                if let Some((clip_id, object_id, text)) = title_text_at(state, frame, p) {
                    cmds.push(AppCommand::SelectClip(Some(clip_id)));
                    cmds.push(AppCommand::BeginTitleEdit { clip_id });
                    cmds.push(AppCommand::SelectTitleObject { clip_id, object_id: Some(object_id.0) });
                    state.selected_clip = Some(clip_id);
                    state.selected_clips = vec![clip_id];
                    state.viewer_text_edit_object = Some(object_id);
                    state.viewer_text_edit_request_focus = true;
                    state.viewer_char_positions.clear();
                    state.viewer_text_cursor = text.chars().count();
                    state.viewer_text_select_anchor = None;
                    state.viewer_text_edit_buffer = text;
                } else {
                    cmds.push(point(p));
                }
            }
        }
    }

    /// Editing text on the render itself. The engine's overlay is the
    /// text; an invisible `TextEdit` handles the keys, the clipboard
    /// and undo; the caret and the selection are drawn from the
    /// engine's own glyph boxes, and a click or a drag on the text
    /// places them by those boxes too.
    fn draw_text_edit_widget(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        frame: &ViewerFrameState,
        cmds: &mut Vec<AppCommand>,
    ) {
        if !state.viewer_text_edit {
            return;
        }
        let image_rect = frame.image_rect;
        let viewer_rect = frame.viewer_rect;
        let (video_w, video_h) = (frame.video_w.max(1.0), frame.video_h.max(1.0));
        let edit_id = ui.id().with("viewer_text_edit");
        let prev_buf = state.viewer_text_edit_buffer.clone();
        let Some(clip_id) = state.viewer_text_edit_clip else { return };
        let Some(clip) = state.sequence().tracks.iter().find_map(|t| t.find_clip(clip_id)) else { return };
        let Some(td) = clip.title_data().cloned() else {
            return;
        };
        // Where the title is drawn (#2014). Glyphs are found under the
        // pointer on the unplaced canvas, where they stand upright, and
        // drawn where the placed title draws them. Placed so the canvas
        // can't follow, the keys still edit the text; nothing is drawn.
        let view = title_view_of(state, clip, image_rect);
        let mut td = td;
        tighten_text_editor_boxes(&mut td, (video_w.max(1.0) as u32, video_h.max(1.0) as u32));
        let clip_time = (state.playhead_smooth_secs - clip.timeline_in.to_secs_f64()).max(0.0);
        // The layer being edited: the targeted one, else the first
        // text layer.
        let obj = state
            .viewer_text_edit_object
            .and_then(|oid| td.object(oid).cloned())
            .or_else(|| td.objects.iter().find(|o| matches!(o.content, klipp_timeline::TitleObjectContent::TextBox(_))).cloned());
        let Some(mut obj) = obj else { return };
        obj.transform.center = obj.transform.evaluated_center(clip_time);
        if state.viewer_text_edit_object != Some(obj.id) {
            state.viewer_text_edit_object = Some(obj.id);
            state.viewer_char_positions.clear();
        }
        let drawn = view.is_ok();
        let view = view.unwrap_or_else(|_| TitleView::flat(image_rect));
        let bb = points_bb(&object_screen_points(&obj, image_rect).map(|p| view.place(p)));
        let on_canvas = |x: f32, y: f32| {
            egui::pos2(image_rect.min.x + x / video_w * image_rect.width(), image_rect.min.y + y / video_h * image_rect.height())
        };
        let to_screen = |x: f32, y: f32| view.place(on_canvas(x, y));

        // ── The pointer on the text: caret and selection by glyph box ──
        let hit = ui.interact(bb.expand(8.0), edit_id.with("hit"), egui::Sense::click_and_drag());
        let index_at = |positions: &[klipp_title::CharPosition], p: egui::Pos2| -> Option<usize> {
            if positions.is_empty() {
                return None;
            }
            // Vertical text (#1704): the column under the pointer, then
            // the nearest cell edge down it.
            if positions[0].vertical {
                let column_of = |cp: &klipp_title::CharPosition| (on_canvas(cp.x, cp.y).x, on_canvas(cp.x + cp.height, cp.y).x);
                let mut best_column: Option<(f32, f32)> = None;
                for cp in positions.iter() {
                    let (left, right) = column_of(cp);
                    let d = if p.x < left { left - p.x } else if p.x > right { p.x - right } else { 0.0 };
                    if best_column.is_none_or(|(bd, _)| d < bd) {
                        best_column = Some((d, left));
                    }
                }
                let column_left = best_column?.1;
                let mut best: Option<(f32, usize)> = None;
                for cp in positions.iter().filter(|cp| (column_of(cp).0 - column_left).abs() <= 0.5) {
                    let (top, foot) = (on_canvas(cp.x, cp.y).y, on_canvas(cp.x, cp.y + cp.advance).y);
                    for (edge, idx) in [(top, cp.char_index), (foot, cp.char_index + 1)] {
                        let d = (p.y - edge).abs();
                        if best.is_none_or(|(bd, _)| d < bd) {
                            best = Some((d, idx));
                        }
                    }
                }
                return best.map(|(_, i)| i);
            }
            // The row under the pointer (nearest row when between
            // lines), then the nearest glyph edge within it.
            let row_of = |cp: &klipp_title::CharPosition| (on_canvas(cp.x, cp.y).y, on_canvas(cp.x, cp.y + cp.height).y);
            let mut best_row: Option<(f32, f32)> = None;
            for cp in positions.iter() {
                let (top, bottom) = row_of(cp);
                let d = if p.y < top { top - p.y } else if p.y > bottom { p.y - bottom } else { 0.0 };
                if best_row.is_none_or(|(bd, _)| d < bd) {
                    best_row = Some((d, top));
                }
            }
            let row_top = best_row?.1;
            let mut best: Option<(f32, usize)> = None;
            for cp in positions.iter() {
                if (row_of(cp).0 - row_top).abs() > 0.5 {
                    continue;
                }
                let left = on_canvas(cp.x, cp.y).x;
                let right = on_canvas(cp.x + cp.advance, cp.y).x;
                // A right-to-left character's start is its right edge (#1693).
                let (before, after) = if cp.rtl { (right, left) } else { (left, right) };
                for (edge, idx) in [(before, cp.char_index), (after, cp.char_index + 1)] {
                    let d = (p.x - edge).abs();
                    if best.is_none_or(|(bd, _)| d < bd) {
                        best = Some((d, idx));
                    }
                }
            }
            best.map(|(_, i)| i)
        };
        let mut set_range: Option<(usize, usize)> = None;
        if let Some(p) = hit.interact_pointer_pos() {
            if hit.drag_started() || hit.clicked() {
                if let Some(i) = index_at(&state.viewer_char_positions, view.unplace(p)) {
                    state.viewer_text_select_anchor = Some(i);
                    set_range = Some((i, i));
                }
            } else if hit.dragged() {
                if let (Some(a), Some(i)) = (state.viewer_text_select_anchor, index_at(&state.viewer_char_positions, view.unplace(p))) {
                    set_range = Some((a, i));
                }
            }
        }
        if hit.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
        }
        // A title the Text tool has just made: all of its text, so the
        // first key typed replaces the placeholder (#1713).
        if std::mem::take(&mut state.viewer_text_edit_select_all) {
            set_range = Some((0, state.viewer_text_edit_buffer.chars().count()));
        } else if state.viewer_text_edit_request_focus && set_range.is_none() {
            // A new editing session starts at the caret it was given:
            // the editor's own state still holds the last session's,
            // whose selection would swallow the first key (#1714).
            set_range = Some((state.viewer_text_cursor, state.viewer_text_cursor));
        }
        if let Some((a, b)) = set_range {
            let len = state.viewer_text_edit_buffer.chars().count();
            let (a, b) = (a.min(len), b.min(len));
            let mut st = egui::TextEdit::load_state(ui.ctx(), edit_id).unwrap_or_default();
            st.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(a), egui::text::CCursor::new(b))));
            egui::TextEdit::store_state(ui.ctx(), edit_id, st);
            state.viewer_text_cursor_blink_time = ui.input(|i| i.time);
        }

        // ── The invisible editor: keys, clipboard, undo ──
        let mut te_ui = ui.new_child(egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(bb.min, egui::vec2(1.0, 1.0))));
        te_ui.style_mut().visuals.selection.bg_fill = egui::Color32::TRANSPARENT;
        te_ui.style_mut().visuals.selection.stroke = egui::Stroke::NONE;
        let te_output = egui::TextEdit::multiline(&mut state.viewer_text_edit_buffer)
            .id(edit_id)
            .desired_width(1.0)
            .desired_rows(1)
            .frame(false)
            .margin(egui::Margin::ZERO)
            .text_color(egui::Color32::TRANSPARENT)
            // Tab types a tab, for the box's tab stops (#1721), rather than
            // moving focus out of the text.
            .lock_focus(true)
            .show(&mut te_ui);
        if state.viewer_text_edit_request_focus || !te_output.response.has_focus() {
            state.viewer_text_edit_request_focus = false;
            te_output.response.request_focus();
        }
        if let Some(cursor_range) = te_output.cursor_range {
            let primary = cursor_range.primary.ccursor.index;
            let secondary = cursor_range.secondary.ccursor.index;
            if primary != state.viewer_text_cursor {
                state.viewer_text_cursor_blink_time = ui.input(|i| i.time);
            }
            state.viewer_text_cursor = primary;
            if primary != secondary {
                let (s, e) = if primary < secondary { (primary, secondary) } else { (secondary, primary) };
                state.viewer_text_selection = Some((s, e));
                state.viewer_text_selection_locked = Some((s, e));
            } else if !ui.input(|i| i.pointer.primary_down()) {
                state.viewer_text_selection = None;
            }
        }

        // ── Style under the caret, for the inspector's selection-aware tools ──
        if let klipp_timeline::TitleObjectContent::TextBox(tb) = &obj.content {
            let cursor_byte = char_to_byte(&state.viewer_text_edit_buffer, state.viewer_text_cursor);
            let check_byte = cursor_byte.saturating_sub(1);
            let run = tb.style_runs.iter().find(|r| check_byte >= r.start && check_byte < r.end);
            state.viewer_text_cursor_font_size = Some(run.and_then(|r| r.font_size).unwrap_or(tb.font_size));
            state.viewer_text_cursor_color = Some(run.and_then(|r| r.color).unwrap_or(tb.color));
        }

        // ── Paint: box, selection, caret, badge ──
        // Nothing drawn where the canvas can't follow (#2014); the typing
        // and the way out below still run.
        let painter = ui.painter_at(viewer_rect);
        let painter = if drawn { painter } else { painter.with_clip_rect(egui::Rect::NOTHING) };
        painter.rect_stroke(bb.expand(4.0), crate::theme::radius::CONTROL, egui::Stroke::new(1.5_f32, theme.accent));
        let positions = &state.viewer_char_positions;
        if let Some((s, e)) = state.viewer_text_selection {
            let fill = egui::Color32::from_rgba_unmultiplied(theme.accent.r(), theme.accent.g(), theme.accent.b(), 90);
            for cp in positions.iter().filter(|cp| cp.char_index >= s && cp.char_index < e) {
                // A vertical cell runs down its column (#1704).
                let (across, down) = if cp.vertical { (cp.height, cp.advance) } else { (cp.advance, cp.height) };
                let r = egui::Rect::from_min_max(to_screen(cp.x, cp.y), to_screen(cp.x + across, cp.y + down));
                painter.rect_filled(r, 0.0, fill);
            }
        }
        if let Some([a, b]) = caret_ends(positions, state.viewer_text_cursor) {
            let t = ui.input(|i| i.time);
            let since = t - state.viewer_text_cursor_blink_time;
            let on = since < 0.05 || (since * 2.0).fract() < 0.5;
            if on {
                painter.line_segment([to_screen(a.0, a.1), to_screen(b.0, b.1)], egui::Stroke::new(2.0_f32, theme.text_primary));
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(80));
        } else if positions.is_empty() {
            // Nothing laid out yet (empty text, or the boxes still
            // coming): a caret at the box centre so the editor still
            // reads as live.
            painter.line_segment(
                [egui::pos2(bb.center().x, bb.min.y + 4.0), egui::pos2(bb.center().x, bb.max.y - 4.0)],
                egui::Stroke::new(2.0_f32, theme.text_primary),
            );
        }
        {
            let font = egui::FontId::proportional(crate::theme::type_scale::CAPTION);
            let layer_name = if obj.name.is_empty() { "Text layer" } else { obj.name.as_str() };
            let galley = painter.layout_no_wrap(format!("Editing: {layer_name} \u{2014} Esc to finish"), font, theme.text_primary);
            let pad = egui::vec2(8.0, 3.0);
            let size = galley.size() + pad * 2.0;
            let x = bb.min.x.max(image_rect.min.x + 2.0);
            let y = (bb.min.y - 8.0 - size.y).max(image_rect.min.y + 2.0);
            let r = egui::Rect::from_min_size(egui::pos2(x, y), size);
            painter.rect_filled(r, crate::theme::radius::CONTROL, theme.bg_panel);
            painter.rect_stroke(r, crate::theme::radius::CONTROL, egui::Stroke::new(1.0_f32, theme.border));
            painter.galley(r.min + pad, galley, theme.text_primary);
        }

        // ── Commit typing ──
        if state.viewer_text_edit_buffer != prev_buf {
            state.viewer_char_positions.clear();
            cmds.push(AppCommand::SetTitleObjectText { clip_id, object_id: obj.id.0, text: state.viewer_text_edit_buffer.clone() });
        }

        // ── Leave: Esc, or a click elsewhere in the viewer ──
        let exit_escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let click_outside = ui.input(|i| i.pointer.primary_clicked())
            && ui.input(|i| i.pointer.interact_pos()).is_some_and(|pos| !bb.expand(8.0).contains(pos) && viewer_rect.contains(pos));
        if exit_escape || click_outside {
            cmds.push(AppCommand::EndTitleEdit);
            state.viewer_text_edit_buffer.clear();
            state.viewer_text_selection = None;
            state.viewer_text_selection_locked = None;
            state.viewer_text_cursor = 0;
            state.viewer_char_positions.clear();
            state.viewer_text_select_anchor = None;
            state.viewer_text_edit_object = None;
        }
    }

    fn show_program_viewer(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        viewer_rect: egui::Rect,
        cmds: &mut Vec<AppCommand>,
    ) {
        let _ = cmds;

        let frame = match self.compute_video_layout(ui, state, viewer_rect, theme) {
            Some(f) => f,
            None => return,
        };
        let image_rect = frame.image_rect;
        let video_w = frame.video_w;
        let video_h = frame.video_h;

        // Program viewer paints only its own output. The `browser_skim_texture`
        // is a hover-preview surface that belongs to the Source Viewer in
        // the dual-viewer layout — showing it here would duplicate the
        // preview onto the Program side, which was the single-viewer-era
        // behaviour.
        if frame.grade_still_active || frame.wipe_frame.is_some() {
            self.draw_grade_still_split(ui, state, &frame);
        } else {
            // Prefer the engine's GPU-resident output (`gpu_texture_id`)
            // when registered — it's already on the same wgpu device
            // as egui-wgpu, so sampling is a single texture read with
            // no PCIe download. Fall back to the legacy CPU-uploaded
            // `video_texture` when the GPU path isn't live (env gate
            // off, eframe running glow, shared device not installed,
            // or this frame's grade hasn't finished yet).
            // Through the monitor LUT when one is set: on screen only,
            // the frame itself stays as graded (#465).
            let image_id = self.lut_live
                .then_some(self.lut_texture_id)
                .flatten()
                .or_else(|| self.gpu_frame_live.then_some(self.gpu_texture_id).flatten())
                .or_else(|| self.video_texture.as_ref().map(|t| t.id()));
            if let Some(id) = image_id {
                // `painter_at(viewer_rect)` clips to the viewer's panel
                // so high-zoom output stays inside it instead of spilling
                // into the chrome row / neighbouring panels.
                ui.painter_at(viewer_rect).image(
                    id,
                    image_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        }

        // No title overlay is drawn here: the engine composites titles
        // into the frame above, paused as well as playing. Laying the
        // UI's own raster of the selected title over it printed the text
        // twice, the second copy at the raster's layout rather than the
        // engine's. The raster is still made for the in-place editor's
        // glyph positions.

        self.handle_text_edit_entry(ui, state, &frame, cmds);
        self.handle_text_edit_exit(ui, state, cmds);
        self.draw_text_edit_widget(ui, state, theme, &frame, cmds);

        // ── The Text tool (#1713) ──
        // Armed, it has the picture to itself: the transform and
        // title-object handles below stand down, so a click or a drag on
        // the picture is the tool's. While a title is being typed into,
        // the in-place editor has the clicks: one outside it ends the
        // editing, and the next makes another title.
        let text_tool = state.active_tool == klipp_core::Tool::Text && !state.show_color_panel;
        if text_tool
            && !state.viewer_text_edit
            && state.surface_tool.is_none()
            && state.eyedropper_mode == crate::state::EyedropperMode::None
            && !state.has_modal_open()
        {
            self.handle_text_tool(ui, state, theme, &frame, cmds);
        }

        // ── On-screen transform controls ──
        // Suppress transform controls during text edit mode, and in the
        // Colour workspace.
        //
        // Colour is where the picture is judged, and these draw a white
        // box, eight handles and a centre crosshair across it — the
        // crosshair lands on whatever is framed centre, which in
        // practice is the face whose skin tone is being matched. White
        // geometry over an image changes how the eye reads the tones
        // underneath it, which is the whole reason grading suites dim
        // the surround. Sizing is an Edit-side task and there is no
        // sizing UI surfaced in Colour to go with the handles anyway.
        //
        // The toggle itself is untouched: leaving Colour brings them
        // straight back, and nothing is written to the user's prefs.
        let title_object_active = state
            .selected_title_object
            .is_some_and(|(selected_clip, _)| Some(selected_clip) == state.selected_clip);
        // Frame Glide, while a clip is being framed, has the viewer to
        // itself (#611); so does the surface tool while it is up.
        if state.frame_glide_clip.is_some() && !state.has_modal_open() {
            crate::panels::frame_glide_tool::show(ui, state, image_rect, cmds);
        } else if state.surface_tool.is_some() && !state.has_modal_open() {
            self.draw_surface_tool(ui, state, image_rect, cmds);
        } else if state.show_transform_controls
            && !state.show_color_panel
            && !state.viewer_text_edit
            && !text_tool
            && !title_object_active
            && state.eyedropper_mode == crate::state::EyedropperMode::None
            && !state.has_modal_open()
        {
            // The selected clip's motion path (#2276), taking a press on
            // its dots and handles before the gizmo can.
            super::motion_path::show(ui, state, image_rect, video_w, video_h, cmds);
            let start = cmds.len();
            self.draw_transform_controls(ui, state, image_rect, frame.viewer_rect, video_w, video_h, cmds);
            // The gizmo moves the clip's placement by path, and adds no
            // Transform (M3 step 13).
            if let Some(clip) = state.selected_clip.and_then(|id| state.sequence().all_clips().find(|c| c.id == id).cloned()) {
                let mine = cmds.split_off(start);
                cmds.extend(crate::inspector::placement_by_path::by_path(mine, &clip));
            }
            self.draw_corner_pin_handles(ui, state, image_rect, frame.viewer_rect, cmds);
            self.draw_waterline(ui, state, image_rect, frame.viewer_rect, cmds);
            self.draw_surface_outlines(ui, state, image_rect);
        }
        self.draw_surface_tracking_badge(ui, state, image_rect, cmds);
        self.handle_surface_drop(ui, state, image_rect);

        // ── On-canvas Title-object editor ──
        // Active for any selected Title clip, regardless of
        // `show_transform_controls`: title objects have their own
        // editing affordance (bounding boxes + handles) that's
        // visually distinct from the clip-level transform handles.
        // Suppressed during text-edit mode and other modal flows.
        // A handle drag ends with the button, whether or not the editor
        // below is drawn to see it: one that stood down mid-drag would
        // leave the drag's undo step open, and later edits folded in.
        if let Some(drag) = &state.title_object_drag {
            if !ui.input(|i| i.pointer.primary_down()) {
                cmds.push(AppCommand::EndTitleObjectDrag { clip_id: drag.clip_id });
                state.title_object_drag = None;
            }
        }
        if !state.viewer_text_edit
            && !text_tool
            && state.eyedropper_mode == crate::state::EyedropperMode::None
            && !state.has_modal_open()
        {
            self.draw_title_object_editor(ui, state, image_rect, cmds);
            // Hyperzoom focal-point handle. Reads/writes the same
            // egui-memory scratch that the inspector's HYPERZOOM
            // section spinboxes use, so dragging here updates the
            // spinboxes live and editing the spinboxes moves the
            // crosshair on next paint. Skipped on title-text-edit /
            // eyedropper / modal flows for the same reasons as the
            // title-object editor above.
            self.draw_hyperzoom_focal_handle(ui, state, image_rect);
        }

        // ── Before/After or Grade Still split divider ──
        let show_split_divider = state.before_after_active
            || frame.grade_still_active
            || frame.wipe_frame.is_some()
            || state.stabilization_compare_active
            || state.split_compare_node.is_some();
        if show_split_divider {
            self.draw_split_divider(ui, state, theme, &frame);
            // Keyboard nudge: ←/→ move the split by 0.02. Only consumed
            // while a compare mode is active so the same keys remain free
            // for transport / track navigation in normal viewer mode.
            ui.input(|i| {
                if i.key_pressed(egui::Key::ArrowRight) {
                    state.split_position = (state.split_position + 0.02).clamp(0.05, 0.95);
                }
                if i.key_pressed(egui::Key::ArrowLeft) {
                    state.split_position = (state.split_position - 0.02).clamp(0.05, 0.95);
                }
            });
        }

        self.handle_eyedropper_click(ui, state, image_rect, cmds);
        self.handle_ai_tools_capture(ui, state, image_rect);
        self.draw_ai_tools_overlay(ui, state, image_rect);

        // ── Interactive mask handles on viewer ──
        // Draw and interact with masks from the selected node in the clip's node graph.
        self.draw_mask_handles(ui, state, image_rect, cmds, theme);

        // ── Point tracking overlay ──
        self.draw_track_points(ui, state, image_rect, cmds, theme);

        // ── Overlay mode badge (top-right of video) ──
        self.draw_overlay_badges(ui.painter(), state, &frame);

        // Safe area overlay: title safe (80%) and action safe (90%)
        if state.viewer_overlays.safe_areas {
            self.draw_safe_area_overlay(ui.painter(), state, &frame);
        }

        // Grid overlay: rule of thirds + center crosshair
        if state.viewer_overlays.grid {
            self.draw_grid_overlay(ui.painter(), state, &frame);
        }

        // Checkerboard transparency overlay (drawn behind video, replaces black background)
        if state.viewer_overlays.checkerboard {
            self.draw_checkerboard_overlay(ui.painter(), state, &frame);
        }

        // Timecode + zoom now shown in playbar below scrub bar

        // Proxy badge
        self.draw_proxy_badge(ui.painter(), state, theme, &frame);

        // ── Multicam angle bar (shown below the viewer when multicam is active) ──
        if state.multicam_at_playhead && !state.multicam_angles.is_empty() {
            self.draw_multicam_angle_bar(ui, state, theme, cmds, &frame);
        }

        // Performance overlay
        if state.show_perf_overlay {
            self.draw_perf_overlay(ui, state, image_rect, theme);
        }

        self.handle_scope_cursor_input(ui, state, &frame);
        self.draw_scope_cursor(ui.painter(), state, &frame);
        self.draw_hdr_monitor(ui, state, theme, image_rect, cmds);
        draw_viewed_node(ui, state, image_rect, cmds);
    }

    /// HDR monitoring: what the viewer shows for an HDR delivery, and the
    /// light under the pointer.
    ///
    /// An SDR screen cannot show an HDR picture, so the numbers are the
    /// instrument: the readout says what the picture is in cd/m² at the
    /// pointer, and false colour says it for the whole frame at once. The
    /// levels to judge against are 26 (an 18% grey card), 203 (diffuse
    /// white) and 1000 (the master's peak).
    /// Open the HDR reference-output window.
    ///
    /// Looked for beside the running executable first, because that is
    /// where a built or installed Klipp keeps its own binaries, then on
    /// `PATH`. If neither has it the output still turns *on* — publishing
    /// is the engine's job and works regardless — so someone running the
    /// presenter themselves is not blocked by this convenience failing.
    fn open_hdr_output_window(&mut self) {
        self.hdr_output_unopened = false;
        if self.hdr_output_child.is_some() {
            return;
        }
        const BIN: &str = "klipp-hdr-out";
        let beside = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join(BIN)))
            .filter(|p| p.is_file());
        let program = beside.unwrap_or_else(|| std::path::PathBuf::from(BIN));
        match std::process::Command::new(&program).stdin(std::process::Stdio::null()).spawn() {
            Ok(child) => {
                tracing::info!("HDR reference output: opened {}", program.display());
                self.hdr_output_child = Some(child);
            }
            Err(e) => {
                // Naming the build command, because the likeliest reason
                // is that only `klipp-app` was built: the presenter is a
                // separate binary and `cargo build -p klipp-app` does not
                // produce it.
                tracing::warn!(
                    "could not open {} ({e}). The picture is still being published — build \
                     the window with `cargo build --release -p klipp-hdr-out` and run it, or \
                     run an installed `klipp-hdr-out`.",
                    program.display(),
                );
                // Said on the button, not only logged: the output *is* on
                // and publishing, and the only thing missing is the window.
                self.hdr_output_unopened = true;
            }
        }
    }

    /// Close the window this panel opened, if it opened one.
    fn close_hdr_output_window(&mut self) {
        if let Some(mut child) = self.hdr_output_child.take() {
            // Asked to go, then reaped: without the wait it becomes a
            // zombie for as long as Klipp runs.
            let _ = child.kill();
            let _ = child.wait();
            tracing::info!("HDR reference output: closed the window");
        }
    }

    fn draw_hdr_monitor(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        image_rect: egui::Rect,
        cmds: &mut Vec<AppCommand>,
    ) {
        use klipp_core::HdrMonitor;
        const H: f32 = 22.0;
        let label = match state.hdr_monitor {
            HdrMonitor::Off => "HDR: off",
            HdrMonitor::Preview => "HDR: preview",
            HdrMonitor::FalseColour => "HDR: false colour",
        };
        let width = if state.hdr_monitor == HdrMonitor::Off { 78.0 } else { 118.0 };
        let btn = egui::Rect::from_min_size(
            egui::pos2(image_rect.max.x - width - 8.0, image_rect.min.y + 8.0),
            egui::vec2(width, H),
        );
        let resp = ui.interact(btn, ui.id().with("hdr_monitor"), egui::Sense::click());
        let on = state.hdr_monitor.renders_hdr();
        let bg = if on { theme.accent } else { theme.bg_raised.gamma_multiply(0.9) };
        ui.painter().rect_filled(btn, 4.0, bg);
        ui.painter().text(
            btn.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(11.0),
            if on { theme.text_on_accent } else { theme.text_secondary },
        );
        if resp.clicked() {
            // Off → preview → false colour → off.
            state.hdr_monitor = match state.hdr_monitor {
                HdrMonitor::Off => HdrMonitor::Preview,
                HdrMonitor::Preview => HdrMonitor::FalseColour,
                HdrMonitor::FalseColour => HdrMonitor::Off,
            };
            cmds.push(AppCommand::SetHdrMonitor(state.hdr_monitor));
        }
        resp.on_hover_text(
            "What the viewer shows for an HDR delivery. Preview: the HDR picture as SDR, \
             highlights rolled off. False colour: the levels as flat colours — grey 26, \
             white 203, peak 1000 cd/m². The readout below gives the light under the pointer.",
        );

        // The reference output, and only while monitoring is on: the engine
        // publishes the display light the render produces, and it only
        // produces display light for monitoring. Offering the toggle when
        // nothing would be published would be a switch that does nothing.
        if on {
            let out = egui::Rect::from_min_size(
                egui::pos2(btn.min.x, btn.max.y + 4.0),
                egui::vec2(width, H),
            );
            let resp = ui.interact(out, ui.id().with("hdr_reference_output"), egui::Sense::click());
            let live = state.hdr_reference_output;
            let bg = if live { theme.accent } else { theme.bg_raised.gamma_multiply(0.9) };
            ui.painter().rect_filled(out, 4.0, bg);
            ui.painter().text(
                out.center(),
                egui::Align2::CENTER_CENTER,
                match (live, self.hdr_output_unopened) {
                    (true, true) => "Ref out: no window",
                    (true, false) => "Ref out: on",
                    (false, _) => "Ref out: off",
                },
                egui::FontId::proportional(11.0),
                if live { theme.text_on_accent } else { theme.text_secondary },
            );
            if resp.clicked() {
                state.hdr_reference_output = !state.hdr_reference_output;
                cmds.push(AppCommand::SetHdrReferenceOutput(state.hdr_reference_output));
                if state.hdr_reference_output {
                    self.open_hdr_output_window();
                } else {
                    self.close_hdr_output_window();
                    self.hdr_output_unopened = false;
                }
            }
            resp.on_hover_text(
                "Send the HDR picture to a separate window whose surface can \
                 carry it: linear, sixteen bits, highlights above reference \
                 white intact. That window is where an HDR reference monitor \
                 should look — this one is showing you an SDR stand-in for \
                 it.\n\nThe display has to be in HDR too for the picture to \
                 arrive as HDR; on Hyprland that is the monitor's `cm` and \
                 `bitdepth, 10`.\n\nCosts a frame readback, so playback is \
                 slower while it is on.",
            );
        }

        // The readout: the picture's light where the pointer is.
        if on {
            let text = match state.hdr_nits_under_pointer {
                Some(nits) if nits >= 100.0 => format!("{nits:.0} cd/m²"),
                Some(nits) if nits >= 10.0 => format!("{nits:.1} cd/m²"),
                Some(nits) => format!("{nits:.2} cd/m²"),
                None => "— cd/m²".to_string(),
            };
            let row = egui::Rect::from_min_size(
                egui::pos2(btn.min.x, btn.max.y + 8.0 + H),
                egui::vec2(width, H),
            );
            ui.painter().rect_filled(row, 4.0, theme.bg_raised.gamma_multiply(0.9));
            ui.painter().text(
                row.center(),
                egui::Align2::CENTER_CENTER,
                text,
                egui::FontId::monospace(11.0),
                theme.text_primary,
            );
        }
    }

    fn draw_scrub_bar(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        rect: egui::Rect,
        viewer_kind: ViewerMode,
        cmds: &mut Vec<AppCommand>,
    ) {
        let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());

        // Visible track is a thin strip centred vertically inside the wider
        // scrub lane. The lane's extra height exists purely so the playhead
        // ball can sit on top of the track without being clipped above/below.
        let track_h = 4.0_f32;
        let track_rect = egui::Rect::from_min_size(
            egui::pos2(rect.min.x, rect.center().y - track_h * 0.5),
            egui::vec2(rect.width(), track_h),
        );

        // Track background — full width.
        ui.painter().rect_filled(track_rect, 0.0, theme.bg_raised);

        // The source clip's marks: favourite stretches in green and
        // rejected ones in red along the track, notes as ticks above it.
        if viewer_kind == ViewerMode::Source {
            if let Some(media) = state.source_media_id.and_then(|mid| state.media_pool.get(&mid)) {
                let dur = media.duration.to_secs_f64();
                if dur > 0.0 {
                    let x_of = |secs: f64| track_rect.min.x + (secs / dur).clamp(0.0, 1.0) as f32 * track_rect.width();
                    for m in &media.markers {
                        match m.kind {
                            klipp_core::MediaMarkerKind::Note => {
                                let x = x_of(m.secs);
                                ui.painter().rect_filled(
                                    egui::Rect::from_min_max(egui::pos2(x - 1.0, track_rect.min.y - 6.0), egui::pos2(x + 1.0, track_rect.max.y)),
                                    0.0,
                                    egui::Color32::from_rgb(90, 160, 255),
                                );
                            }
                            klipp_core::MediaMarkerKind::Analysis => {
                                // Analysis marks are quieter than the editor's: a
                                // thin grey line for a cut, a faint band for a silence.
                                let colour = egui::Color32::from_rgba_unmultiplied(170, 180, 200, 110);
                                if m.is_range() {
                                    ui.painter().rect_filled(
                                        egui::Rect::from_min_max(egui::pos2(x_of(m.secs), track_rect.min.y + 1.0), egui::pos2(x_of(m.end_secs()), track_rect.max.y - 1.0)),
                                        0.0,
                                        colour,
                                    );
                                } else {
                                    let x = x_of(m.secs);
                                    ui.painter().rect_filled(
                                        egui::Rect::from_min_max(egui::pos2(x - 0.5, track_rect.min.y - 4.0), egui::pos2(x + 0.5, track_rect.max.y)),
                                        0.0,
                                        colour,
                                    );
                                }
                            }
                            kind => {
                                let colour = if kind == klipp_core::MediaMarkerKind::Favourite {
                                    egui::Color32::from_rgba_unmultiplied(80, 200, 120, 170)
                                } else {
                                    egui::Color32::from_rgba_unmultiplied(220, 70, 70, 170)
                                };
                                ui.painter().rect_filled(
                                    egui::Rect::from_min_max(egui::pos2(x_of(m.secs), track_rect.min.y - 2.0), egui::pos2(x_of(m.end_secs()), track_rect.max.y + 2.0)),
                                    1.0,
                                    colour,
                                );
                            }
                        }
                    }
                }
            }
        }

        // Per-viewer playhead/duration. Source uses the loaded media's
        // total duration; Program uses the sequence duration.
        let (dur_secs, current_secs, rate): (f64, f64, u32) = match viewer_kind {
            ViewerMode::Program => (
                state.sequence_duration().to_secs_f64(),
                state.playhead_smooth_secs,
                state.fps.rate(),
            ),
            ViewerMode::Source => {
                let media = state
                    .source_media_id
                    .and_then(|mid| state.media_pool.get(&mid));
                let dur = media.map(|m| m.duration.to_secs_f64()).unwrap_or(0.0);
                let cur = state.source_playhead.to_secs_f64();
                let rate = media.map(|m| m.fps.rate()).unwrap_or(state.fps.rate());
                (dur, cur, rate)
            }
        };
        let progress = if dur_secs > 0.0 {
            (current_secs / dur_secs).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };

        let fill_w = progress * track_rect.width();
        if fill_w > 0.0 {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(track_rect.min, egui::vec2(fill_w, track_rect.height())),
                0.0,
                theme.accent,
            );
        }

        // Empty viewers retain the track and focus affordance, but have
        // no playback position to mark. Loaded stills count as content.
        let has_content = match viewer_kind {
            ViewerMode::Source => state.source_media_id
                .and_then(|mid| state.media_pool.get(&mid))
                .is_some(),
            ViewerMode::Program => dur_secs > 0.0,
        };
        if has_content {
            let head_x = track_rect.min.x + fill_w;
            let head_center = egui::pos2(head_x, rect.center().y);
            let ph = theme.playhead;
            ui.painter().circle_filled(head_center, 7.0, egui::Color32::from_rgba_unmultiplied(ph.r(), ph.g(), ph.b(), 60));
            ui.painter().circle_filled(head_center, 5.0, ph);
        }

        // Scrub interaction — clicks/drags both focus the viewer and
        // (if there's a clip to seek in) emit a per-viewer seek. The
        // focus emit runs unconditionally so an empty source scrub bar
        // still accepts focus on click.
        // egui's `resp.dragged()` for a `Sense::click_and_drag`
        // widget can fire spuriously while a drag started elsewhere
        // (e.g. a mask handle on the image) is in progress — the
        // scrub bar would then dispatch SeekTo on every drag tick
        // from the cursor's position over the image, yanking the
        // playhead wildly. The rect-contains gate makes the scrub
        // bar respond ONLY to gestures whose cursor is actually
        // over it.
        let pointer_in_rect = resp
            .interact_pointer_pos()
            .map(|p| rect.contains(p))
            .unwrap_or(false);
        if (resp.dragged() || resp.clicked()) && pointer_in_rect {
            if state.viewer_mode != viewer_kind {
                cmds.push(AppCommand::SetViewerMode(viewer_kind));
            }
            if dur_secs > 0.0 {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let t = ((pos.x - track_rect.min.x) / track_rect.width().max(1.0))
                        .clamp(0.0, 1.0) as f64;
                    let target_secs = t * dur_secs;
                    let target_tc = klipp_core::Timecode::from_secs_f64(target_secs, rate);
                    // Instant visual feedback: paint the playhead at
                    // the cursor position right now, on this frame,
                    // instead of waiting for the engine seek to ack.
                    // Without this, the yellow blob lags the cursor
                    // by the seek latency and scrubbing feels
                    // disconnected from the input.
                    match viewer_kind {
                        ViewerMode::Source => {
                            state.source_playhead = target_tc;
                        }
                        ViewerMode::Program => {
                            state.playhead = target_tc;
                            state.playhead_smooth_secs = target_secs;
                            state.playhead_anchor = target_tc;
                            state.playhead_anchor_wall = std::time::Instant::now();
                        }
                    }
                    // Throttle engine dispatch to ~30 Hz. UI repaints
                    // at 60+ Hz on drag — sending a SeekSource per
                    // frame piles 30+ events on the engine in a
                    // single second; even with command coalescing
                    // each one keeps the decoder thread spinning on
                    // demux + BSF work and the fan winds up. 33 ms
                    // is below the human-perception floor for scrub
                    // smoothness so we lose nothing visually. The
                    // final position on drag-release always lands
                    // (gated below).
                    let now = std::time::Instant::now();
                    let throttled = match viewer_kind {
                        ViewerMode::Source => state
                            .source_seek_dispatch_at
                            .map(|t| now.duration_since(t) < std::time::Duration::from_millis(33))
                            .unwrap_or(false),
                        ViewerMode::Program => false,
                    };
                    let drag_release = resp.drag_stopped();
                    if !throttled || drag_release || resp.clicked() {
                        let cmd = match viewer_kind {
                            ViewerMode::Program => AppCommand::SeekTo(target_tc),
                            ViewerMode::Source => {
                                state.source_seek_dispatch_at = Some(now);
                                AppCommand::SeekSource(target_tc)
                            }
                        };
                        cmds.push(cmd);
                    }
                }
            }
        }
    }

    fn draw_playbar(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        rect: egui::Rect,
        viewer_kind: ViewerMode,
        cmds: &mut Vec<AppCommand>,
    ) {
        // Per-viewer enable. The Source playbar gates on "is a
        // clip loaded into the source viewer?" because
        // `state.source_playback` only transitions to Playing /
        // Paused — it stays Idle until the source engine has
        // played at least once, which the user can't trigger if
        // the button itself is disabled (chicken-and-egg). The
        // Program playbar keeps the existing "engine non-Idle"
        // gate, which the timeline engine sets to Stopped when a
        // project loads.
        let enabled = match viewer_kind {
            ViewerMode::Source => state.source_media_id.is_some(),
            ViewerMode::Program => state.playback != PlaybackState::Idle,
        };
        let fps_val = state.fps.as_f64();
        let cy = rect.center().y;

        // Background
        ui.painter().rect_filled(rect, 0.0, theme.bg_deep);

        // ── Left: Timecode ── (per-viewer: source clip time vs. timeline time)
        let (tc_secs, tc_display) = match viewer_kind {
            // The programme's clock is the sequence's, drop-frame and all.
            ViewerMode::Program => (state.playhead_smooth_secs, state.timecode_display()),
            ViewerMode::Source => {
                // A source is read on its *own* clock: the camera's rate
                // decides, not the sequence it happens to be cut into.
                // Klipp does not record whether a file's own timecode was
                // drop-frame, so the rate's convention stands in.
                let media = state
                    .source_media_id
                    .and_then(|mid| state.media_pool.get(&mid));
                let fps = media.map(|m| m.fps).unwrap_or(state.fps);
                let display = klipp_core::TimecodeDisplay {
                    fps,
                    drop: klipp_timeline::Sequence::default_drop_frame(fps),
                };
                (state.source_playhead.to_secs_f64(), display)
            }
        };
        let tc_str = format_timecode_hmsf(tc_secs, tc_display);
        if state.editing_timecode {
            let tc_rect = egui::Rect::from_min_size(
                egui::pos2(rect.min.x + 8.0, cy - 12.0),
                egui::vec2(110.0, 24.0),
            );
            let mut tc_ui = ui.new_child(egui::UiBuilder::new().max_rect(tc_rect));
            let response = tc_ui.add(
                egui::TextEdit::singleline(&mut state.timecode_input)
                    .desired_width(100.0)
                    .font(egui::TextStyle::Monospace),
            );
            if !response.has_focus() { response.request_focus(); }
            let lost = response.lost_focus();
            let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            if lost || esc {
                if enter {
                    if let Some(tc) = parse_timecode_input(&state.timecode_input, state.fps) {
                        cmds.push(AppCommand::SeekTo(tc));
                    }
                }
                state.editing_timecode = false;
            }
        } else {
            // Click-to-edit is Program-only: typing a timecode emits
            // `SeekTo` which targets the timeline, so allowing it on the
            // source side would silently seek the wrong viewer.
            let sense = if viewer_kind == ViewerMode::Program {
                egui::Sense::click()
            } else {
                egui::Sense::hover()
            };
            let tc_resp = ui.allocate_rect(
                egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + 8.0, rect.min.y),
                    egui::vec2(110.0, rect.height()),
                ),
                sense,
            );
            ui.painter().text(
                egui::pos2(rect.min.x + 12.0, cy),
                egui::Align2::LEFT_CENTER,
                &tc_str,
                egui::FontId::monospace(13.0),
                theme.text_primary,
            );
            if tc_resp.clicked() {
                state.editing_timecode = true;
                state.timecode_input = tc_str;
            }
        }

        // ── Single viewer: which one this is, and the way to the other ──
        let show_viewer_switcher = !state.viewers_side_by_side() && !state.single_viewer_is_program();
        // Keep the timecode and core transport clear in narrow layouts.
        // The bottom focus edge remains visible when the label cannot fit.
        let show_viewer_label = !show_viewer_switcher && rect.width() >= 560.0;
        if show_viewer_switcher {
            let font = egui::FontId::proportional(crate::theme::type_scale::CAPTION);
            let labels = ["Source", "Program"];
            let h = crate::theme::control::COMPACT;
            let size = crate::widgets::segmented::segmented_size(ui, &labels, &font, h);
            let sw = egui::Rect::from_min_size(egui::pos2(rect.min.x + 126.0, cy - h / 2.0), size);
            let active = if viewer_kind == ViewerMode::Source { 0 } else { 1 };
            if let Some(i) = crate::widgets::segmented::segmented_in(ui, sw, &labels, active, theme, font.clone()) {
                if i != active {
                    cmds.push(AppCommand::SetViewerMode(if i == 0 { ViewerMode::Source } else { ViewerMode::Program }));
                }
            }
            // With the selects stacked, the sequence the program plays, the
            // one of the two worked in, named beside the switch (#2324).
            if viewer_kind == ViewerMode::Program && crate::timeline::stacked::stacked_edit(state) {
                ui.painter().text(egui::pos2(sw.max.x + 8.0, cy), egui::Align2::LEFT_CENTER, &state.sequence().name, font, theme.text_secondary);
            }
        }
        if show_viewer_label {
            // With the selects stacked, the program names the sequence it
            // plays: the one of the two worked in (#2324).
            let label = match viewer_kind {
                ViewerMode::Source => "Source".to_string(),
                ViewerMode::Program if crate::timeline::stacked::stacked_edit(state) => format!("Program \u{00B7} {}", state.sequence().name),
                ViewerMode::Program => "Program".to_string(),
            };
            ui.painter().text(
                egui::pos2(rect.min.x + 126.0, cy),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(crate::theme::type_scale::CAPTION),
                if state.viewer_mode == viewer_kind { theme.text_primary } else { theme.text_dim },
            );
        }

        // ── Center: Transport buttons ──
        //
        // The group is centred in the rect, which on a narrow viewer put
        // it straight on top of the timecode: at ~330 px the leading
        // edge lands about 50 px in, while the timecode runs to about
        // 98 px, so the buttons painted over it and it read as "00:0".
        //
        // The right-hand controls already solve this by dropping in a
        // priority order. The same idea, mirrored: the timecode is the
        // one readout in this bar that must never be half-drawn, so it
        // keeps its space and the transport sheds its outermost pairs —
        // skip-to-end first, then the 10-second jumps. Step and play are
        // the core and always survive.
        let btn_size = 30.0;
        let play_size = 38.0;
        let btn_spacing = 4.0;
        // Everything left of the centre group: the timecode's 110 px
        // slot at +8, or the viewer switcher at +126 when it is shown.
        let left_reserved = if show_viewer_switcher || show_viewer_label {
            196.0
        } else {
            126.0
        };
        let group_w = |pairs: f32| pairs * 2.0 * (btn_size + btn_spacing) + play_size;
        // 3 pairs is the full set, then 2, then 1.
        let mut pairs = 3.0_f32;
        while pairs > 1.0 && rect.center().x - group_w(pairs) / 2.0 < rect.min.x + left_reserved {
            pairs -= 1.0;
        }
        let show_skip = pairs >= 3.0;
        let show_jump = pairs >= 2.0;
        let total_w = group_w(pairs);
        let mut x = (rect.center().x - total_w / 2.0).max(rect.min.x + left_reserved);

        let nav_fg = theme.text_primary;
        let nav_bg = theme.bg_panel;
        let nav_hover = theme.bg_hover;
        let disabled_fg = theme.text_dim;

        // Helper closure: every push from this playbar goes through
        // viewer-routing so the source-side playbar drives the source
        // engine instead of the timeline. Without this, clicking Play
        // in the source viewer's chrome would emit `AppCommand::Play`
        // and start timeline playback — exactly the bug we hit.
        use crate::panels::transport::route_command_for_viewer;
        let route = |c: AppCommand| route_command_for_viewer(c, viewer_kind);

        // Skip back — outermost pair, first to go when space is tight.
        if show_skip {
        x += self.playbar_nav_btn(ui, Icon::SkipBack, x, cy, btn_size, enabled, nav_fg, nav_bg, nav_hover, disabled_fg, "Go to start (Home)", cmds, Some(route(AppCommand::SeekTo(klipp_core::RationalTime::from_frames(0, state.fps.rate())))));
        }
        // Rewind — the 10-second jump, second to go.
        if show_jump {
        x += self.playbar_nav_btn(ui, Icon::Rewind, x, cy, btn_size, enabled, nav_fg, nav_bg, nav_hover, disabled_fg, "Rewind (J)", cmds, Some(route(AppCommand::SeekRelative(-(fps_val * 10.0) as i64))));
        }
        // Step back — core, always shown.
        x += self.playbar_nav_btn(ui, Icon::CaretLeft, x, cy, btn_size, enabled, nav_fg, nav_bg, nav_hover, disabled_fg, "Step back 1 frame", cmds, Some(route(AppCommand::SeekRelative(-1))));

        // Play / Pause (larger, amber)
        {
            let play_rect = egui::Rect::from_center_size(
                egui::pos2(x + play_size / 2.0, cy),
                egui::vec2(play_size, play_size),
            );
            let resp = ui.allocate_rect(play_rect, if enabled { egui::Sense::click() } else { egui::Sense::hover() });
            // Read the playing state of *this viewer* so the icon
            // accurately reflects what clicking will do. Without this
            // the source playbar would always show the timeline's
            // play state.
            let is_playing = match viewer_kind {
                ViewerMode::Source => state.source_playback == PlaybackState::Playing,
                ViewerMode::Program => state.playback == PlaybackState::Playing,
            };
            // No block of colour under the play glyph: the glyph is the
            // control, lit when it can be pressed, the field only under
            // the pointer.
            if enabled && resp.hovered() {
                ui.painter().rect_filled(play_rect, crate::theme::radius::CONTROL, theme.bg_hover);
            }
            let play_icon = if is_playing { Icon::Pause } else { Icon::Play };
            let icon_rect = egui::Rect::from_center_size(play_rect.center(), egui::vec2(18.0, 18.0));
            let tint = if enabled { theme.text_primary } else { theme.text_dim };
            play_icon.image(18.0).tint(tint).paint_at(ui, icon_rect);
            let clicked = resp.clicked();
            resp.on_hover_text("Play / Pause (Space)");
            if clicked && enabled {
                if is_playing { cmds.push(route(AppCommand::Pause)); } else { cmds.push(route(AppCommand::Play)); }
            }
        }
        x += play_size + btn_spacing;

        // Step forward
        x += self.playbar_nav_btn(ui, Icon::CaretRight, x, cy, btn_size, enabled, nav_fg, nav_bg, nav_hover, disabled_fg, "Step forward 1 frame", cmds, Some(route(AppCommand::SeekRelative(1))));
        // Fast forward
        if show_jump {
        x += self.playbar_nav_btn(ui, Icon::FastForward, x, cy, btn_size, enabled, nav_fg, nav_bg, nav_hover, disabled_fg, "Fast forward (L)", cmds, Some(route(AppCommand::SeekRelative((fps_val * 10.0) as i64))));
        }
        // Skip forward
        if show_skip {
            let dur = state.sequence_duration();
            self.playbar_nav_btn(ui, Icon::SkipForward, x, cy, btn_size, enabled, nav_fg, nav_bg, nav_hover, disabled_fg, "Go to end (End)", cmds, Some(route(AppCommand::SeekTo(dur))));
        }

        // ── Right side controls ── (responsive)
        //
        // Layout right-to-left from `rect.max.x - 12`. Each control consumes
        // its slot width plus a 6-px gap before the next leftward control.
        // When the chrome rect is narrow (e.g. portrait-project viewers,
        // ~470 px wide on a 1080p monitor) we'd otherwise march straight
        // into the center transport group and overlap. So we keep controls
        // in this priority order (most-essential first; the rest drop when
        // the budget runs out):
        //   1. Mute toggle               — audio is global, always reachable
        //   2. Match-frame button        — pro NLE workflow control, kept
        //                                  on both Source and Program sides
        //   3. Volume slider             — fine adjust
        //   4. Volume percentage         — cosmetic readout
        //   5. Transform toggle (Program)
        //   6. Camera/extract (Program)
        let center_half_w = total_w / 2.0;
        let center_pad = 8.0; // breathing room between center group + side group
        let right_margin = 12.0;
        let budget_right = (rect.width() / 2.0 - center_half_w - center_pad - right_margin).max(0.0);

        let btn_w = 20.0;
        let gap = 6.0;
        let mute_slot = btn_w + gap;
        let mf_slot = btn_w + gap;
        let slider_slot = 60.0 + gap;
        let vol_pct_slot = 36.0 + gap;
        let xform_slot = btn_w + gap;
        let cam_slot = btn_w + gap;
        let scopes_slot = btn_w + gap;
        let layout_slot = btn_w + gap;
        // The master meter. Wide enough to read a level off rather
        // than only notice one: at 96 px the span from -60 to 0 dB is
        // about 1.6 px per dB, so the difference between -6 and -3 is
        // visible rather than implied.
        let meter_slot = MASTER_METER_W + gap;

        let mut remaining = budget_right;
        let mut show_mute = false;
        let mut show_mf = false;
        let mut show_slider = false;
        let mut show_vol_pct = false;
        let mut show_xform = false;
        let mut show_camera = false;
        let mut show_scopes = false;
        let mut show_layout = false;
        if remaining >= mute_slot { remaining -= mute_slot; show_mute = true; }
        if remaining >= mf_slot { remaining -= mf_slot; show_mf = true; }
        if viewer_kind == ViewerMode::Program {
            // Scopes and the viewer layout are the picture's own
            // controls; they outrank the volume readout for room.
            if remaining >= scopes_slot { remaining -= scopes_slot; show_scopes = true; }
            if remaining >= layout_slot { remaining -= layout_slot; show_layout = true; }
        }
        if remaining >= slider_slot { remaining -= slider_slot; show_slider = true; }
        if remaining >= vol_pct_slot { remaining -= vol_pct_slot; show_vol_pct = true; }
        // The Program viewer is the timeline's output, so it is the
        // only one with a master to meter. Asked for after the volume
        // controls it sits beside: a meter with no volume control next
        // to it reads as decoration.
        let mut show_meter = false;
        if viewer_kind == ViewerMode::Program && show_slider && remaining >= meter_slot {
            remaining -= meter_slot;
            show_meter = true;
        }
        if viewer_kind == ViewerMode::Program {
            if remaining >= xform_slot { remaining -= xform_slot; show_xform = true; }
            if remaining >= cam_slot { remaining -= cam_slot; show_camera = true; }
        }
        let _ = remaining; // suppress unused warning past the last conditional

        // Walking cursor: the right edge of the next control we'll place
        // (each placement also consumes a 6-px gap before the one to its
        // left). Starts at the panel's right margin.
        let mut cursor_right = rect.max.x - right_margin;

        // Volume percentage — fixed 36px slot at the far right.
        if show_vol_pct {
            let vol_pct = (state.master_volume * 100.0) as u32;
            let vol_text = format!("{}%", vol_pct);
            ui.painter().text(
                egui::pos2(cursor_right, cy),
                egui::Align2::RIGHT_CENTER,
                &vol_text,
                egui::FontId::monospace(10.0),
                theme.text_dim,
            );
            cursor_right -= 36.0 + gap;
        }

        // Volume slider — 60px wide. When vol_pct is hidden, the slider
        // shifts right to meet the panel edge.
        //
        // CRITICAL: egui::Slider's track width is governed by `spacing.slider_width`,
        // NOT by the parent UI's max_rect or by `add_sized`. The slider widget
        // internally calls `ui.allocate_response(vec2(spacing.slider_width, ...))`
        // which ignores both the child UI's bounds and any size hint passed to
        // add_sized. The ONLY way to make the slider narrower is to mutate
        // `spacing_mut().slider_width` on the UI before adding the slider.
        if show_slider {
            let slider_w = 60.0_f32;
            let slider_h = 16.0_f32;
            let slider_rect = egui::Rect::from_min_size(
                egui::pos2(cursor_right - slider_w, cy - slider_h * 0.5),
                egui::vec2(slider_w, slider_h),
            );
            let mut vol = state.master_volume;
            let mut slider_ui = ui.new_child(egui::UiBuilder::new().max_rect(slider_rect));
            slider_ui.spacing_mut().slider_width = slider_w;
            let slider = egui::Slider::new(&mut vol, 0.0..=1.3).show_value(false);
            if slider_ui.add(slider).changed() {
                cmds.push(AppCommand::SetMasterVolume(vol));
            }
            cursor_right -= slider_w + gap;
        }

        // Mute button — always shown when any right-side chrome fits.
        if show_mute {
            let mute_x = cursor_right - btn_w / 2.0;
            let mute_rect = egui::Rect::from_center_size(
                egui::pos2(mute_x, cy),
                egui::vec2(btn_w, btn_w),
            );
            let mute_resp = ui.allocate_rect(mute_rect, egui::Sense::click())
                .on_hover_text(if state.master_muted { "Unmute master output" } else { "Mute master output" });
            let mute_icon = if state.master_muted { Icon::SpeakerSlash } else { Icon::SpeakerHigh };
            let mute_color = if state.master_muted {
                theme.marker_error
            } else {
                theme.text_secondary
            };
            let mute_icon_rect = egui::Rect::from_center_size(mute_rect.center(), egui::vec2(14.0, 14.0));
            mute_icon.image(14.0).tint(mute_color).paint_at(ui, mute_icon_rect);
            if mute_resp.clicked() {
                let old = state.master_muted;
                cmds.push(AppCommand::SetMasterMute(!old));
            }
            cursor_right -= btn_w + gap;
        }

        // Master output meter, left of the mute button so the audio
        // group reads as one: level, mute, volume, percentage.
        //
        // The output, not the selection. The inspector's AUDIO tab
        // already meters whichever clip is selected, which is a
        // different question from "is the programme clipping" — and
        // the answer to that one has to be on screen while cutting,
        // with nothing selected at all.
        if show_meter {
            let meter_rect = egui::Rect::from_min_size(
                egui::pos2(cursor_right - MASTER_METER_W, cy - MASTER_METER_H * 0.5),
                egui::vec2(MASTER_METER_W, MASTER_METER_H),
            );
            draw_master_meter(
                ui.painter(),
                meter_rect,
                state.master_peak_left,
                state.master_peak_right,
                theme,
            );
            cursor_right -= MASTER_METER_W + gap;
        }

        // Match Frame — appears on both viewers in the same slot;
        // direction is determined by which viewer hosts the button
        // (Resolve / Premiere convention). The Program viewer's
        // button finds the source frame for the current program
        // playhead and loads it in the source viewer. The Source
        // viewer's button jumps the program playhead to the next
        // timeline occurrence of the source frame.
        if show_mf {
            let mf_x = cursor_right - btn_w / 2.0;
            let (mf_cmd, mf_enabled, mf_tooltip) = match viewer_kind {
                ViewerMode::Program => (
                    AppCommand::MatchFrameProgramToSource,
                    true, // playhead-resolution happens at handler time; toast on miss
                    "Match Frame — load source for program frame (F)",
                ),
                ViewerMode::Source => (
                    AppCommand::MatchFrameSourceToProgram,
                    state.source_media_id.is_some(),
                    "Match Frame — jump timeline to source frame (Shift+F)",
                ),
            };
            let mf_rect = egui::Rect::from_center_size(
                egui::pos2(mf_x, cy),
                egui::vec2(btn_w, btn_w),
            );
            let mf_resp = ui.allocate_rect(
                mf_rect,
                if mf_enabled { egui::Sense::click() } else { egui::Sense::hover() },
            );
            let mf_color = if !mf_enabled {
                theme.text_dim
            } else if mf_resp.hovered() {
                theme.accent
            } else {
                theme.text_secondary
            };
            let mf_icon_rect = egui::Rect::from_center_size(mf_rect.center(), egui::vec2(14.0, 14.0));
            Icon::FrameCorners.image(14.0).tint(mf_color).paint_at(ui, mf_icon_rect);
            let mf_clicked = mf_resp.clicked();
            mf_resp.on_hover_text(mf_tooltip);
            if mf_clicked && mf_enabled {
                cmds.push(mf_cmd);
            }
            cursor_right -= btn_w + gap;
        }

        // Transform-controls toggle and camera/extract-frame button are
        // Program-only — they operate on the timeline output (transform
        // handles + frame capture). On the Source side they'd both be
        // misleading: clicking the xform toggle would highlight on
        // both viewers since it writes shared state, and the camera
        // would always grab the program frame anyway.
        if viewer_kind == ViewerMode::Program {
            // Transform handles only make sense for an actual selected,
            // non-Gap clip on the timeline — `draw_transform_controls`
            // early-returns otherwise. Reflect that in the toggle so the
            // button is visibly disabled when there's nothing to act on
            // and a click surfaces a toast instead of silently doing
            // nothing.
            let xform_enabled = state
                .selected_clip
                .and_then(|cid| {
                    state.sequence().tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .find(|c| c.id == cid)
                })
                .is_some_and(|c| !matches!(c.kind, klipp_timeline::ClipKind::Gap));
            if show_xform {
                let xform_x = cursor_right - btn_w / 2.0;
                let xform_rect = egui::Rect::from_center_size(
                    egui::pos2(xform_x, cy),
                    egui::vec2(btn_w, btn_w),
                );
                let xform_resp = ui.allocate_rect(xform_rect, egui::Sense::click());
                let xform_color = if !xform_enabled {
                    theme.text_dim
                } else if state.show_transform_controls {
                    theme.accent
                } else {
                    theme.text_secondary
                };
                let stroke_w = if xform_enabled { 1.5_f32 } else { 1.2_f32 };
                let ir = xform_rect.shrink(4.0);
                ui.painter().rect_stroke(ir, 0.0, egui::Stroke::new(stroke_w, xform_color));
                let cs = 2.0;
                for corner in [ir.left_top(), ir.right_top(), ir.left_bottom(), ir.right_bottom()] {
                    ui.painter().rect_filled(
                        egui::Rect::from_center_size(corner, egui::vec2(cs * 2.0, cs * 2.0)),
                        0.0,
                        xform_color,
                    );
                }
                if xform_resp.clicked() {
                    if xform_enabled {
                        state.show_transform_controls = !state.show_transform_controls;
                    } else {
                        state.toast.show("Select a video clip to use transform controls");
                    }
                }
                xform_resp.on_hover_text(if xform_enabled {
                    "Toggle transform controls"
                } else {
                    "Select a clip to enable transform"
                });
                cursor_right -= btn_w + gap;
            } else {
                let _ = xform_enabled;
            }

            if show_camera {
                let cam_x = cursor_right - btn_w / 2.0;
                let cam_rect = egui::Rect::from_center_size(
                    egui::pos2(cam_x, cy),
                    egui::vec2(btn_w, btn_w),
                );
                let cam_resp = ui.allocate_rect(cam_rect, egui::Sense::click());
                let cam_color = if cam_resp.hovered() {
                    theme.accent
                } else {
                    theme.text_secondary
                };
                let body = cam_rect.shrink2(egui::vec2(3.0, 5.0));
                ui.painter().rect_stroke(body, 2.0, egui::Stroke::new(1.2_f32, cam_color));
                let bump = egui::Rect::from_center_size(
                    egui::pos2(cam_rect.center().x - 3.0, body.min.y - 1.5),
                    egui::vec2(5.0, 3.0),
                );
                ui.painter().rect_stroke(bump, 1.0, egui::Stroke::new(1.2_f32, cam_color));
                ui.painter().circle_stroke(body.center(), 3.2, egui::Stroke::new(1.2_f32, cam_color));
                if cam_resp.clicked() {
                    cmds.push(AppCommand::ExtractFrameToBin);
                }
                cam_resp.on_hover_text("Extract frame to bin (Ctrl+Alt+B)");
                cursor_right -= btn_w + gap;
            }

            // Scopes: the dock in Color, the floating window elsewhere.
            if show_scopes {
                let r = egui::Rect::from_center_size(egui::pos2(cursor_right - btn_w / 2.0, cy), egui::vec2(btn_w, btn_w));
                let resp = ui.allocate_rect(r, egui::Sense::click());
                let on = state.scopes_shown();
                let colour = if on { theme.accent } else if resp.hovered() { theme.text_primary } else { theme.text_secondary };
                if on {
                    ui.painter().rect_filled(r, crate::theme::radius::CONTROL, theme.accent_subtle);
                }
                crate::panels::toolbar::draw_scopes_icon(ui.painter(), r, colour);
                if resp.clicked() {
                    state.toggle_scopes();
                }
                resp.on_hover_text("Scopes (Alt+V)");
                cursor_right -= btn_w + gap;
            }

            // One viewer or two.
            if show_layout {
                let r = egui::Rect::from_center_size(egui::pos2(cursor_right - btn_w / 2.0, cy), egui::vec2(btn_w, btn_w));
                let resp = ui.allocate_rect(r, egui::Sense::click());
                let colour = if resp.hovered() { theme.text_primary } else { theme.text_secondary };
                let dual = state.viewers_side_by_side();
                draw_viewer_layout_icon(ui.painter(), r, dual, colour);
                if resp.clicked() {
                    state.toggle_viewer_layout();
                }
                resp.on_hover_text(if dual { "Single viewer" } else { "Dual viewers" });
                cursor_right -= btn_w + gap;
            }
        }
        let _ = cursor_right;
    }

    /// Helper for playbar navigation buttons. Returns advance width (btn_size + spacing).
    #[allow(clippy::too_many_arguments)]
    fn playbar_nav_btn(
        &self,
        ui: &mut egui::Ui,
        icon: Icon,
        x: f32,
        cy: f32,
        size: f32,
        enabled: bool,
        fg: egui::Color32,
        bg: egui::Color32,
        hover_bg: egui::Color32,
        disabled_fg: egui::Color32,
        tooltip: &str,
        cmds: &mut Vec<AppCommand>,
        cmd: Option<AppCommand>,
    ) -> f32 {
        let btn_rect = egui::Rect::from_center_size(
            egui::pos2(x + size / 2.0, cy),
            egui::vec2(size, size),
        );
        let resp = ui.allocate_rect(btn_rect, if enabled { egui::Sense::click() } else { egui::Sense::hover() });
        let fill = if resp.hovered() && enabled { hover_bg } else { bg };
        ui.painter().rect_filled(btn_rect, size / 2.0, fill);
        let icon_rect = egui::Rect::from_center_size(btn_rect.center(), egui::vec2(14.0, 14.0));
        icon.image(14.0).tint(if enabled { fg } else { disabled_fg }).paint_at(ui, icon_rect);
        let clicked = resp.clicked();
        resp.on_hover_text(tooltip);
        if clicked && enabled {
            if let Some(c) = cmd { cmds.push(c); }
        }
        size + 4.0 // btn_size + spacing
    }

    fn draw_perf_overlay(
        &self,
        ui: &mut egui::Ui,
        state: &AppState,
        image_rect: egui::Rect,
        theme: &KlippTheme,
    ) {
        let font = egui::FontId::monospace(11.0);
        let line_h = 15.0;
        let pad = 6.0;

        let hw_label = match state.perf_hw_decode {
            Some(true) => "NVDEC",
            Some(false) => "SW",
            None => "—", // no decoder open right now
        };
        let playback_label = match state.playback {
            PlaybackState::Playing => "PLAY",
            PlaybackState::Paused => "PAUSE",
            PlaybackState::Stopped => "STOP",
            PlaybackState::Idle => "IDLE",
        };

        // Engine-side per-frame telemetry. Painted between the UI
        // counters and the OS counters so the user reads "what's my
        // UI doing / what's the engine doing / what's the process
        // doing" top-to-bottom. Values come from the
        // `klipp_engine::perf::PerfRing` snapshotted in
        // `update_perf_stats`.
        let avg = &state.perf_averages;
        // Decode and post-process run on separate threads (pipelined),
        // so frame pacing is bound by the slowest stage, not the sum.
        // Decode and post-process run on separate threads (pipelined), so
        // pacing is bound by the slowest stage. The readback is engine-side
        // work that the post-process stage table does not cover, so it adds
        // to that side rather than being max'd against it — leaving it out
        // is what let this line report 5.2 ms while frames took 35 ms.
        let engine_ms = avg.total_ms + state.perf_readback_ms as f32;
        let actual_ms = engine_ms.max(avg.decode_ms);
        let (res_line, fps_line, budget_line) = match state.perf_latest_sample {
            Some(ref s) => (
                format!("Frame:  {}x{}", s.width, s.height),
                {
                    // The number that was missing. A target with no
                    // achieved figure beside it reads as a promise.
                    let got = state.perf_video_fps as f32;
                    let short = s.target_fps > 0.0 && got > 0.0 && got < s.target_fps * 0.9;
                    format!(
                        "Target: {:.1} fps   actual {:.1} fps{}",
                        s.target_fps,
                        got,
                        if short { "  ⚠ BELOW TARGET" } else { "" },
                    )
                },
                match s.frame_budget_ms() {
                    Some(budget) => {
                        format!(
                            "Budget: {:.1} ms  actual {:.1} ms",
                            budget, actual_ms,
                        )
                    }
                    None => format!("Budget: — ms  actual {:.1} ms", actual_ms),
                },
            ),
            None => (
                "Frame:  — (engine idle)".to_string(),
                "Target: — fps".to_string(),
                "Budget: — ms  actual —".to_string(),
            ),
        };

        let rec_line = if state.perf_recording_active {
            Some("● REC  (Ctrl+Alt+P to stop)".to_string())
        } else {
            None
        };

        let mut lines = Vec::new();
        if let Some(r) = rec_line { lines.push(r); }
        lines.extend([
            format!("UI FPS: {:.0}", state.perf_fps),
            format!("Decode: {hw_label}"),
            format!("State:  {playback_label}"),
            res_line,
            fps_line,
            budget_line,
            format!(
                "  decode      {:.2} ms",
                avg.decode_ms,
            ),
            format!(
                "  engine readback {:.2} ms",
                state.perf_readback_ms,
            ),
            {
                let r = klipp_effects::gpu::readback_stats();
                format!("  effects downloads: {} / {:.1} MiB / {:.1} ms total ({} failed)",
                    r.count, r.bytes as f64 / 1_048_576.0, r.total_us as f64 / 1000.0, r.failures)
            },
            "    process totals; blocking title/effects only; overlaps render time".to_string(),
            {
                let s = klipp_effects::residency::snapshot();
                match s.latest_viewer_frame {
                    Some(f) => format!("  CPU picture fallbacks: {} this frame ({:.2}s): {}", f.fallback_count(), f.seconds,
                        f.events.iter().filter(|e| e.work == klipp_effects::residency::Work::Fallback).take(3).map(|e|format!("{}: {}", e.feature,e.reason)).collect::<Vec<_>>().join("; ")),
                    None => "  CPU picture fallbacks: no viewer sample".to_string(),
                }
            },
            format!(
                "  seq read    {:.2} ms",
                avg.seq_read_ms,
            ),
            format!(
                "  grade       {:.2} ms",
                avg.grade_ms,
            ),
            format!(
                "  builtin fx  {:.2} ms",
                avg.builtin_ms,
            ),
            format!(
                "  ofx         {:.2} ms",
                avg.ofx_ms,
            ),
            format!(
                "  lut         {:.2} ms",
                avg.lut_ms,
            ),
            // Wrap-stage timings — populated by `update_wrap_stages`.
            // bridge u8 / reencode only run when paused, scrubbing, or
            // comparing (the pregrade encode is skipped during forward
            // playback), and scope only when a scope is open — so 0.00
            // during plain playback is expected, not a bug.
            format!(
                "  bridge u8   {:.2} ms",
                avg.bridge_u8_ms,
            ),
            format!(
                "  reencode    {:.2} ms",
                avg.reencode_ms,
            ),
            format!(
                "  scope       {:.2} ms",
                avg.scope_ms,
            ),
            format!(
                "Drops:  {} in last 1s · {} total (frames never produced are not drops)",
                state.perf_drops_last_sec, state.perf_dropped_frames,
            ),
            format!("Frames: {}", state.perf_frame_count),
            {
                // top-style convention: 100% = one core fully busy.
                // Spell it out so 858% doesn't read as nonsense.
                static CORES: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
                let cores = *CORES.get_or_init(|| {
                    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
                });
                format!(
                    "CPU:    {:.0}% ({:.1} of {} cores)",
                    state.perf_cpu_percent,
                    state.perf_cpu_percent / 100.0,
                    cores,
                )
            },
            format!("Mem:    {:.0} MB", state.perf_memory_mb),
        ]);

        let max_w = lines.iter()
            .map(|l| {
                ui.painter()
                    .layout_no_wrap(l.clone(), font.clone(), egui::Color32::WHITE)
                    .size().x
            })
            .fold(0.0f32, f32::max);

        let box_w = max_w + pad * 2.0;
        let box_h = lines.len() as f32 * line_h + pad * 2.0;
        let box_pos = egui::pos2(image_rect.left() + 4.0, image_rect.top() + 4.0);
        let box_rect = egui::Rect::from_min_size(box_pos, egui::vec2(box_w, box_h));

        ui.painter().rect_filled(
            box_rect, 4.0,
            egui::Color32::from_rgba_premultiplied(0, 0, 0, 200),
        );

        // Pick per-line colours by content instead of by hard-coded
        // index — lines have shifted as we added the engine telemetry.
        // Highlights: red for active drops, amber for high CPU, green
        // for HW decode, coral for "actual > budget" frame pacing.
        let budget_exceeded = state
            .perf_latest_sample
            .as_ref()
            .and_then(|s| s.frame_budget_ms())
            .map(|b| actual_ms > b)
            .unwrap_or(false);
        for (i, line) in lines.iter().enumerate() {
            let _ = i;
            // One arm per status line. Two arms share a colour, which
            // is coincidence, not a case to merge.
            #[allow(clippy::if_same_then_else)]
            let color = if line.starts_with("● REC") {
                theme.marker_error
            } else if line.starts_with("Decode:") && state.perf_hw_decode == Some(true) {
                theme.marker_success
            } else if line.starts_with("Drops:") && state.perf_drops_last_sec > 0 {
                theme.marker_error
            } else if line.starts_with("Budget:") && budget_exceeded {
                theme.marker_warning
            } else if line.starts_with("CPU:") && state.perf_cpu_percent > 80.0 {
                theme.marker_warning
            } else {
                theme.text_primary
            };
            ui.painter().text(
                egui::pos2(box_pos.x + pad, box_pos.y + pad + i as f32 * line_h),
                egui::Align2::LEFT_TOP,
                line,
                font.clone(),
                color,
            );
        }
    }

    /// Draw track point crosshairs and handle tracking placement mode.
    fn draw_track_points(
        &self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        image_rect: egui::Rect,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
    ) {
        // Tracking placement mode: badge + click to place. The mode badge
        // alone signals tracking — no inner border that would compete with
        // the outer panel chrome.
        if state.tracking_mode {
            // "TRACKING" label top-left
            let label_pos = egui::pos2(image_rect.left() + 8.0, image_rect.top() + 8.0);
            let label_bg = egui::Rect::from_min_size(label_pos, egui::vec2(80.0, 18.0));
            ui.painter().rect_filled(label_bg, 2.0, egui::Color32::from_rgba_unmultiplied(
                theme.accent.r(), theme.accent.g(), theme.accent.b(), 200,
            ));
            ui.painter().text(
                egui::pos2(label_bg.center().x, label_bg.center().y),
                egui::Align2::CENTER_CENTER,
                "TRACKING",
                egui::FontId::proportional(10.0),
                theme.text_on_accent,
            );

            // Click to place tracker
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            let place_resp = ui.allocate_rect(image_rect, egui::Sense::click());
            if place_resp.clicked() {
                if let Some(pos) = place_resp.interact_pointer_pos() {
                    let nx = ((pos.x - image_rect.min.x) / image_rect.width()).clamp(0.0, 1.0);
                    let ny = ((pos.y - image_rect.min.y) / image_rect.height()).clamp(0.0, 1.0);
                    if let Some(clip_id) = state.selected_clip {
                        cmds.push(AppCommand::AddTrackPoint { clip_id, x: nx as f64, y: ny as f64 });
                        state.tracking_mode = false;
                    }
                }
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                state.tracking_mode = false;
            }
        }

        // Draw existing track points
        if let Some(clip_id) = state.selected_clip {
            let clip_opt = state.sequence().tracks.iter()
                .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                .find_map(|t| t.find_clip(clip_id));
            if let Some(clip) = clip_opt {
                if clip.track_points.is_empty() {
                    return;
                }

                // Compute current frame index relative to clip source_in
                let playhead_secs = state.playhead_smooth_secs;
                let clip_in_secs = clip.timeline_in.to_secs_f64();
                let source_in_secs = clip.source_in.to_secs_f64();
                let clip_time = playhead_secs - clip_in_secs + source_in_secs;
                let fps = state.fps.as_f64();
                let current_frame = (clip_time * fps).round() as usize;

                for (idx, tp) in clip.track_points.iter().enumerate() {
                    if !tp.enabled {
                        continue;
                    }

                    // Find position for current frame
                    let frame_offset = current_frame.saturating_sub(tp.start_frame);
                    let pos = if frame_offset < tp.positions.len() {
                        Some(tp.positions[frame_offset])
                    } else {
                        None
                    };

                    if let Some((nx, ny)) = pos {
                        let color = egui::Color32::from_rgba_unmultiplied(
                            tp.color[0], tp.color[1], tp.color[2], tp.color[3],
                        );
                        let dim_color = egui::Color32::from_rgba_unmultiplied(
                            tp.color[0], tp.color[1], tp.color[2], 50,
                        );

                        // Convert normalized to screen coordinates
                        let sx = nx as f32 * image_rect.width() + image_rect.left();
                        let sy = ny as f32 * image_rect.height() + image_rect.top();
                        let center = egui::pos2(sx, sy);
                        let arm = 8.0;

                        // Crosshair lines
                        ui.painter().line_segment(
                            [egui::pos2(sx - arm, sy), egui::pos2(sx + arm, sy)],
                            egui::Stroke::new(1.5_f32, color),
                        );
                        ui.painter().line_segment(
                            [egui::pos2(sx, sy - arm), egui::pos2(sx, sy + arm)],
                            egui::Stroke::new(1.5_f32, color),
                        );

                        // Center circle
                        ui.painter().circle_stroke(center, 3.0, egui::Stroke::new(1.5_f32, color));

                        // Search region rectangle (dim)
                        let sr_px = tp.search_radius as f32 * 2.0;
                        // Scale search region relative to image size (approximate)
                        let sr_screen_w = sr_px / 960.0 * image_rect.width(); // assume half-res ~960
                        let sr_screen_h = sr_px / 540.0 * image_rect.height();
                        let sr_rect = egui::Rect::from_center_size(
                            center,
                            egui::vec2(sr_screen_w, sr_screen_h),
                        );
                        ui.painter().rect_stroke(sr_rect, 0.0, egui::Stroke::new(0.5_f32, dim_color));

                        // Name label
                        let label = if tp.name.is_empty() {
                            format!("Track {}", idx + 1)
                        } else {
                            tp.name.clone()
                        };
                        ui.painter().text(
                            egui::pos2(sx + arm + 4.0, sy - arm),
                            egui::Align2::LEFT_CENTER,
                            &label,
                            egui::FontId::proportional(10.0),
                            color,
                        );

                        // Highlight active track point
                        if state.active_track_point == Some(idx) {
                            ui.painter().circle_stroke(center, 6.0, egui::Stroke::new(1.0_f32, color));
                        }
                    }
                }
            }
        }
    }

    /// Draw interactive mask shape outlines and drag handles in the viewer.
    fn draw_mask_handles(
        &self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        image_rect: egui::Rect,
        cmds: &mut Vec<AppCommand>,
        theme: &KlippTheme,
    ) {
        use crate::state::{MaskHandle, ViewerMaskDrag};
        // The Mask tool draws and edits the freeform masks itself (#74),
        // with or without a mask yet; the other shapes keep their handles
        // below.
        let mask_tool = state.active_tool == klipp_core::Tool::Mask;
        if mask_tool {
            crate::mask_edit::mask_tool(ui, state, image_rect, cmds, theme);
        }

        // Show masks whenever the clip has at least one Mask node
        // with shapes — selection is not required. Prefer the
        // user's selected Mask node if there is one (so multiple
        // Mask nodes on the same clip can be disambiguated), else
        // fall back to the first Mask node found. The Color-tab
        // user flow never selects a node, so the fallback is what
        // makes the overlay visible for them.
        let clip_id = match state.selected_clip {
            Some(id) => id,
            None => {
                state.viewer_mask_drag = None;
                return;
            }
        };
        let selected_id = state.selected_node_id;
        let mask_node: Option<(klipp_nodes::NodeId, klipp_core::MaskStack)> = state
            .sequence()
            .tracks
            .iter()
            .find_map(|t| t.find_clip(clip_id))
            .and_then(|clip| {
                let selected_match = selected_id.and_then(|sid| {
                    clip.grade().nodes.iter().find(|n|
                        n.id == sid && n.kind == klipp_nodes::NodeKind::Mask)
                });
                let fallback = clip.grade().nodes.iter()
                    .find(|n| n.kind == klipp_nodes::NodeKind::Mask);
                selected_match.or(fallback).map(|n| (
                    n.id,
                    n.params.masks.clone().unwrap_or_default(),
                ))
            });
        let (node_id, masks) = match mask_node {
            Some((id, ms)) if !ms.masks.is_empty() => (id, ms),
            _ => {
                state.viewer_mask_drag = None;
                return;
            }
        };

        // Compute clip-time at the current playhead so we can render
        // each mask at its evaluated (keyframe-aware) position rather
        // than its static one. Without this the outline lies after a
        // tracking pass writes centre keyframes — the rendered shape
        // stays at the original drop point while the actual matte
        // follows the tracked path.
        let clip_time_secs: f64 = state.sequence().tracks.iter()
            .find_map(|t| t.find_clip(clip_id))
            .map(|c| (state.playhead_smooth_secs - c.timeline_in.to_secs_f64()).max(0.0))
            .unwrap_or(0.0);

        let iw = image_rect.width();
        let ih = image_rect.height();
        let handle_radius = 5.0_f32;
        let outline_color = ui.visuals().selection.stroke.color;
        let handle_color = egui::Color32::WHITE;

        // Helper: convert normalized coords to screen coords
        let to_screen = |nx: f32, ny: f32| -> egui::Pos2 {
            egui::pos2(image_rect.min.x + nx * iw, image_rect.min.y + ny * ih)
        };

        for (mask_idx, raw_mask) in masks.masks.iter().enumerate() {
            if !raw_mask.enabled { continue; }
            // Render + drag against the keyframe-evaluated shape so
            // the outline matches the actual matte. The drag-start
            // arms below capture this evaluated shape as
            // initial_shape so drag math operates on the same
            // coordinate frame the user is looking at.
            // A tracked window is drawn where its track has it (#238);
            // a drag edits it as drawn on its reference frame, so the
            // handles' start shape is the untracked one.
            let untracked_shape = raw_mask.evaluated_at(clip_time_secs).shape;
            let mask_owned = raw_mask.evaluated_in(clip_time_secs, ih / iw.max(1.0));
            let mask = &mask_owned;
            // A window tracked in perspective keeps its shape as drawn,
            // and its outline and handles are carried onto the picture
            // by the track's map.
            let ahead = mask.keyframes.warp.and_then(|w| w.inverse());
            let to_screen = |nx: f32, ny: f32| -> egui::Pos2 {
                let (x, y) = ahead.and_then(|f| f.apply((nx, ny))).unwrap_or((nx, ny));
                to_screen(x, y)
            };
            // The outline as the matte draws it (klipp_color::masks): a
            // shape's sizes are in frame widths both ways, so a height of
            // `v` reaches `v × reach` of the frame's height down, `reach`
            // being its width over its height. Through the map, too, where
            // straight lines stay straight.
            let reach = iw / ih.max(1.0);
            let outline = |pts: &[(f32, f32)]| {
                let screen: Vec<egui::Pos2> = pts.iter().map(|&(x, y)| to_screen(x, y)).collect();
                ui.painter().add(egui::Shape::closed_line(screen, egui::Stroke::new(1.5_f32, outline_color)));
            };

            match &mask.shape {
                klipp_core::MaskShape::Circle { center_x, center_y, radius } => {
                    let center = to_screen(*center_x, *center_y);

                    let ring: Vec<(f32, f32)> = (0..64).map(|i| {
                        let (sin, cos) = (i as f32 * std::f32::consts::TAU / 64.0).sin_cos();
                        (center_x + radius * cos, center_y + radius * reach * sin)
                    }).collect();
                    outline(&ring);

                    // Center handle
                    let center_handle = egui::Rect::from_center_size(center, egui::vec2(handle_radius * 2.0, handle_radius * 2.0));
                    ui.painter().circle_filled(center, handle_radius, handle_color);

                    // Right edge handle (radius control)
                    let right = to_screen(center_x + radius, *center_y);
                    let right_handle = egui::Rect::from_center_size(right, egui::vec2(handle_radius * 2.0, handle_radius * 2.0));
                    ui.painter().circle_filled(right, handle_radius, handle_color);

                    // Bottom edge handle (radius control)
                    let bottom = to_screen(*center_x, center_y + radius * reach);
                    let bottom_handle = egui::Rect::from_center_size(bottom, egui::vec2(handle_radius * 2.0, handle_radius * 2.0));
                    ui.painter().circle_filled(bottom, handle_radius, handle_color);

                    // Interaction — start drags
                    if state.viewer_mask_drag.is_none() {
                        for (handle_rect, handle_kind) in [
                            (right_handle, MaskHandle::EdgeRight),
                            (bottom_handle, MaskHandle::EdgeBottom),
                            (center_handle, MaskHandle::Center),
                        ] {
                            let resp = ui.allocate_rect(handle_rect.expand(4.0), egui::Sense::click_and_drag());
                            if resp.clicked() {
                                state.selected_node_id = Some(node_id);
                                cmds.push(AppCommand::FocusMaskInspector);
                            }
                            if resp.drag_started() {
                                if let Some(pos) = resp.interact_pointer_pos() {
                                    state.selected_node_id = Some(node_id);
                                    cmds.push(AppCommand::FocusMaskInspector);
                                    cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                                    // If this mask is keyframed (i.e. a
                                    // tracker is driving its centre),
                                    // dragging the outline can't have
                                    // any visible effect — the
                                    // keyframes win in evaluated_at.
                                    // Treat the drag as "I want to
                                    // start over from this position":
                                    // tear the tracker down + clear
                                    // the centre keyframes so the
                                    // static drag we're about to do
                                    // becomes the new starting point.
                                    // User clicks 🎯 Track again
                                    // afterwards to redo analysis.
                                    if !raw_mask.keyframes.is_empty() {
                                        cmds.push(AppCommand::RemoveMaskTracking {
                                            clip_id, node_id, mask_index: mask_idx,
                                        });
                                    }
                                    state.viewer_mask_drag = Some(ViewerMaskDrag {
                                        clip_id,
                                        node_id,
                                        mask_index: mask_idx,
                                        handle: handle_kind,
                                        initial_shape: untracked_shape.clone(),
                                        drag_start_pos: pos,
                                    });
                                }
                            }
                            if resp.hovered() || resp.dragged() {
                                ui.ctx().set_cursor_icon(match handle_kind {
                                    MaskHandle::Center => egui::CursorIcon::Move,
                                    MaskHandle::EdgeRight => egui::CursorIcon::ResizeHorizontal,
                                    MaskHandle::EdgeBottom => egui::CursorIcon::ResizeVertical,
                                    MaskHandle::BezierPoint(_) => egui::CursorIcon::Move,
                                });
                            }
                        }
                    }
                }
                klipp_core::MaskShape::RadialGradient { center_x, center_y, inner, outer } => {
                    let center = to_screen(*center_x, *center_y);

                    // Whole inside the inner ring, gone past the outer (#2704).
                    for radius in [*inner, *outer] {
                        let ring: Vec<(f32, f32)> = (0..64).map(|i| {
                            let (sin, cos) = (i as f32 * std::f32::consts::TAU / 64.0).sin_cos();
                            (center_x + radius * cos, center_y + radius * reach * sin)
                        }).collect();
                        outline(&ring);
                    }

                    // Center handle
                    let center_handle = egui::Rect::from_center_size(center, egui::vec2(handle_radius * 2.0, handle_radius * 2.0));
                    ui.painter().circle_filled(center, handle_radius, handle_color);

                    // Right edge handle: the outer radius
                    let right = to_screen(center_x + outer, *center_y);
                    let right_handle = egui::Rect::from_center_size(right, egui::vec2(handle_radius * 2.0, handle_radius * 2.0));
                    ui.painter().circle_filled(right, handle_radius, handle_color);

                    // Bottom edge handle: the inner radius
                    let bottom = to_screen(*center_x, center_y + inner * reach);
                    let bottom_handle = egui::Rect::from_center_size(bottom, egui::vec2(handle_radius * 2.0, handle_radius * 2.0));
                    ui.painter().circle_filled(bottom, handle_radius, handle_color);

                    // Interaction — start drags
                    if state.viewer_mask_drag.is_none() {
                        for (handle_rect, handle_kind) in [
                            (right_handle, MaskHandle::EdgeRight),
                            (bottom_handle, MaskHandle::EdgeBottom),
                            (center_handle, MaskHandle::Center),
                        ] {
                            let resp = ui.allocate_rect(handle_rect.expand(4.0), egui::Sense::click_and_drag());
                            if resp.clicked() {
                                state.selected_node_id = Some(node_id);
                                cmds.push(AppCommand::FocusMaskInspector);
                            }
                            if resp.drag_started() {
                                if let Some(pos) = resp.interact_pointer_pos() {
                                    state.selected_node_id = Some(node_id);
                                    cmds.push(AppCommand::FocusMaskInspector);
                                    cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                                    // If this mask is keyframed (i.e. a
                                    // tracker is driving its centre),
                                    // dragging the outline can't have
                                    // any visible effect — the
                                    // keyframes win in evaluated_at.
                                    // Treat the drag as "I want to
                                    // start over from this position":
                                    // tear the tracker down + clear
                                    // the centre keyframes so the
                                    // static drag we're about to do
                                    // becomes the new starting point.
                                    // User clicks 🎯 Track again
                                    // afterwards to redo analysis.
                                    if !raw_mask.keyframes.is_empty() {
                                        cmds.push(AppCommand::RemoveMaskTracking {
                                            clip_id, node_id, mask_index: mask_idx,
                                        });
                                    }
                                    state.viewer_mask_drag = Some(ViewerMaskDrag {
                                        clip_id,
                                        node_id,
                                        mask_index: mask_idx,
                                        handle: handle_kind,
                                        initial_shape: untracked_shape.clone(),
                                        drag_start_pos: pos,
                                    });
                                }
                            }
                            if resp.hovered() || resp.dragged() {
                                ui.ctx().set_cursor_icon(match handle_kind {
                                    MaskHandle::Center => egui::CursorIcon::Move,
                                    MaskHandle::EdgeRight => egui::CursorIcon::ResizeHorizontal,
                                    MaskHandle::EdgeBottom => egui::CursorIcon::ResizeVertical,
                                    MaskHandle::BezierPoint(_) => egui::CursorIcon::Move,
                                });
                            }
                        }
                    }
                }
                klipp_core::MaskShape::Rectangle { center_x, center_y, width, height, rotation } => {
                    let center = to_screen(*center_x, *center_y);

                    // Turned as the matte turns it (the handles are not).
                    let (sin, cos) = rotation.to_radians().sin_cos();
                    let corner = |lx: f32, ly: f32| (center_x + lx * cos - ly * sin, center_y + (lx * sin + ly * cos) * reach);
                    let (a, b) = (width * 0.5, height * 0.5);
                    outline(&[corner(-a, -b), corner(a, -b), corner(a, b), corner(-a, b)]);

                    // Center handle
                    let center_handle = egui::Rect::from_center_size(center, egui::vec2(handle_radius * 2.0, handle_radius * 2.0));
                    ui.painter().circle_filled(center, handle_radius, handle_color);

                    // Right edge handle (width control)
                    let right = to_screen(center_x + width * 0.5, *center_y);
                    let right_handle = egui::Rect::from_center_size(right, egui::vec2(handle_radius * 2.0, handle_radius * 2.0));
                    ui.painter().circle_filled(right, handle_radius, handle_color);

                    // Bottom edge handle (height control)
                    let bottom = to_screen(*center_x, center_y + height * 0.5 * reach);
                    let bottom_handle = egui::Rect::from_center_size(bottom, egui::vec2(handle_radius * 2.0, handle_radius * 2.0));
                    ui.painter().circle_filled(bottom, handle_radius, handle_color);

                    if state.viewer_mask_drag.is_none() {
                        for (handle_rect, handle_kind) in [
                            (right_handle, MaskHandle::EdgeRight),
                            (bottom_handle, MaskHandle::EdgeBottom),
                            (center_handle, MaskHandle::Center),
                        ] {
                            let resp = ui.allocate_rect(handle_rect.expand(4.0), egui::Sense::click_and_drag());
                            if resp.clicked() {
                                state.selected_node_id = Some(node_id);
                                cmds.push(AppCommand::FocusMaskInspector);
                            }
                            if resp.drag_started() {
                                if let Some(pos) = resp.interact_pointer_pos() {
                                    state.selected_node_id = Some(node_id);
                                    cmds.push(AppCommand::FocusMaskInspector);
                                    cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                                    // If this mask is keyframed (i.e. a
                                    // tracker is driving its centre),
                                    // dragging the outline can't have
                                    // any visible effect — the
                                    // keyframes win in evaluated_at.
                                    // Treat the drag as "I want to
                                    // start over from this position":
                                    // tear the tracker down + clear
                                    // the centre keyframes so the
                                    // static drag we're about to do
                                    // becomes the new starting point.
                                    // User clicks 🎯 Track again
                                    // afterwards to redo analysis.
                                    if !raw_mask.keyframes.is_empty() {
                                        cmds.push(AppCommand::RemoveMaskTracking {
                                            clip_id, node_id, mask_index: mask_idx,
                                        });
                                    }
                                    state.viewer_mask_drag = Some(ViewerMaskDrag {
                                        clip_id,
                                        node_id,
                                        mask_index: mask_idx,
                                        handle: handle_kind,
                                        initial_shape: untracked_shape.clone(),
                                        drag_start_pos: pos,
                                    });
                                }
                            }
                            if resp.hovered() || resp.dragged() {
                                ui.ctx().set_cursor_icon(match handle_kind {
                                    MaskHandle::Center => egui::CursorIcon::Move,
                                    MaskHandle::EdgeRight => egui::CursorIcon::ResizeHorizontal,
                                    MaskHandle::EdgeBottom => egui::CursorIcon::ResizeVertical,
                                    MaskHandle::BezierPoint(_) => egui::CursorIcon::Move,
                                });
                            }
                        }
                    }
                }
                klipp_core::MaskShape::LinearGradient { start_x, start_y, end_x, end_y } => {
                    // Draw a line from start to end
                    let start = to_screen(*start_x, *start_y);
                    let end = to_screen(*end_x, *end_y);
                    ui.painter().line_segment([start, end], egui::Stroke::new(1.5_f32, outline_color));
                    ui.painter().circle_filled(start, handle_radius, handle_color);
                    ui.painter().circle_filled(end, handle_radius, handle_color);
                    // Linear gradient handles: use Center for start, EdgeRight for end
                    if state.viewer_mask_drag.is_none() {
                        let start_rect = egui::Rect::from_center_size(start, egui::vec2(handle_radius * 2.0, handle_radius * 2.0));
                        let end_rect = egui::Rect::from_center_size(end, egui::vec2(handle_radius * 2.0, handle_radius * 2.0));
                        for (handle_rect, handle_kind) in [
                            (end_rect, MaskHandle::EdgeRight),
                            (start_rect, MaskHandle::Center),
                        ] {
                            let resp = ui.allocate_rect(handle_rect.expand(4.0), egui::Sense::click_and_drag());
                            if resp.clicked() {
                                state.selected_node_id = Some(node_id);
                                cmds.push(AppCommand::FocusMaskInspector);
                            }
                            if resp.drag_started() {
                                if let Some(pos) = resp.interact_pointer_pos() {
                                    state.selected_node_id = Some(node_id);
                                    cmds.push(AppCommand::FocusMaskInspector);
                                    cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                                    // If this mask is keyframed (i.e. a
                                    // tracker is driving its centre),
                                    // dragging the outline can't have
                                    // any visible effect — the
                                    // keyframes win in evaluated_at.
                                    // Treat the drag as "I want to
                                    // start over from this position":
                                    // tear the tracker down + clear
                                    // the centre keyframes so the
                                    // static drag we're about to do
                                    // becomes the new starting point.
                                    // User clicks 🎯 Track again
                                    // afterwards to redo analysis.
                                    if !raw_mask.keyframes.is_empty() {
                                        cmds.push(AppCommand::RemoveMaskTracking {
                                            clip_id, node_id, mask_index: mask_idx,
                                        });
                                    }
                                    state.viewer_mask_drag = Some(ViewerMaskDrag {
                                        clip_id,
                                        node_id,
                                        mask_index: mask_idx,
                                        handle: handle_kind,
                                        initial_shape: untracked_shape.clone(),
                                        drag_start_pos: pos,
                                    });
                                }
                            }
                            if resp.hovered() || resp.dragged() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
                            }
                        }
                    }
                }
                // Bezier freeform path
                klipp_core::MaskShape::Bezier { .. } if mask_tool => {}
                klipp_core::MaskShape::Bezier { points } => {
                    if points.len() >= 2 {
                        let screen_pts: Vec<egui::Pos2> = points.iter().map(|p| to_screen(p.x, p.y)).collect();
                        // Draw the path as it is filled: its curves
                        // flattened at the drawn size (#74).
                        let aspect = image_rect.height() / image_rect.width().max(1.0);
                        let outline: Vec<egui::Pos2> = klipp_core::mask_path::flatten(points, 0.5 / image_rect.width().max(1.0), aspect)
                            .into_iter()
                            .map(|(px, py)| to_screen(px, py))
                            .collect();
                        for i in 0..outline.len() {
                            let a = outline[i];
                            let b = outline[(i + 1) % outline.len()];
                            ui.painter().line_segment([a, b], egui::Stroke::new(1.5_f32, outline_color));
                        }
                        // Adding, removing and reshaping points is the Mask
                        // tool's (#74); here a point only moves.
                        // Draw control point handles
                        if state.viewer_mask_drag.is_none() {
                            for (pi, sp) in screen_pts.iter().enumerate() {
                                let handle_rect = egui::Rect::from_center_size(*sp, egui::vec2(12.0, 12.0));
                                let resp = ui.allocate_rect(handle_rect, egui::Sense::click_and_drag());
                                let color = if resp.hovered() || resp.dragged() {
                                    egui::Color32::WHITE
                                } else {
                                    outline_color
                                };
                                ui.painter().circle_filled(*sp, 4.0, color);
                                if resp.drag_started() {
                                    if let Some(pos) = resp.interact_pointer_pos() {
                                        state.selected_node_id = Some(node_id);
                                        cmds.push(AppCommand::FocusMaskInspector);
                                        cmds.push(AppCommand::BeginNodeParamEdit { clip_id });
                                        if !raw_mask.keyframes.is_empty() {
                                            cmds.push(AppCommand::RemoveMaskTracking {
                                                clip_id, node_id, mask_index: mask_idx,
                                            });
                                        }
                                        state.viewer_mask_drag = Some(ViewerMaskDrag {
                                            clip_id,
                                            node_id,
                                            mask_index: mask_idx,
                                            handle: MaskHandle::BezierPoint(pi),
                                            initial_shape: untracked_shape.clone(),
                                            drag_start_pos: pos,
                                        });
                                    }
                                }
                                if resp.hovered() || resp.dragged() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Process ongoing drag
        if let Some(drag) = &state.viewer_mask_drag {
            let drag_clip_id = drag.clip_id;
            let drag_node_id = drag.node_id;
            let drag_mask_idx = drag.mask_index;
            let drag_handle = drag.handle;
            let drag_start = drag.drag_start_pos;
            let initial_shape = drag.initial_shape.clone();

            // Check if pointer is still down
            let pointer_down = ui.input(|i| i.pointer.any_down());
            let current_pos = ui.input(|i| i.pointer.interact_pos()).unwrap_or(drag_start);
            // On a tracked window (#238), the pointer is where the window
            // is now; the edit is to the window as drawn on its reference
            // frame, so both ends of the drag are taken back through the
            // track.
            // In perspective, back through the track's map first.
            let tracked = masks.masks.get(drag_mask_idx).map(|m| (m.track_at(clip_time_secs), m.evaluated_in(clip_time_secs, ih / iw.max(1.0)).keyframes.warp));
            let (drag_start, current_pos) = match tracked {
                Some((motion, warp)) if motion.is_some() || warp.is_some() => {
                    let pivot = initial_shape.pivot();
                    let back = |p: egui::Pos2| {
                        let mut n = ((p.x - image_rect.min.x) / iw, (p.y - image_rect.min.y) / ih);
                        if let Some(w) = warp {
                            n = w.apply(n).unwrap_or(n);
                        }
                        if let Some(motion) = motion {
                            n = motion.undo(n, pivot, ih / iw.max(1.0));
                        }
                        egui::pos2(image_rect.min.x + n.0 * iw, image_rect.min.y + n.1 * ih)
                    };
                    (back(drag_start), back(current_pos))
                }
                _ => (drag_start, current_pos),
            };

            if pointer_down {
                let dx_norm = (current_pos.x - drag_start.x) / iw;
                let dy_norm = (current_pos.y - drag_start.y) / ih;

                let new_shape = match (&initial_shape, drag_handle) {
                    (klipp_core::MaskShape::Circle { center_x, center_y, radius }, MaskHandle::Center) => {
                        klipp_core::MaskShape::Circle {
                            center_x: (center_x + dx_norm).clamp(0.0, 1.0),
                            center_y: (center_y + dy_norm).clamp(0.0, 1.0),
                            radius: *radius,
                        }
                    }
                    (klipp_core::MaskShape::Circle { center_x, center_y, radius: _ }, MaskHandle::EdgeRight) => {
                        let new_r = ((current_pos.x - (image_rect.min.x + center_x * iw)) / iw).abs().max(0.01);
                        klipp_core::MaskShape::Circle { center_x: *center_x, center_y: *center_y, radius: new_r }
                    }
                    (klipp_core::MaskShape::Circle { center_x, center_y, radius: _ }, MaskHandle::EdgeBottom) => {
                        // Sizes are in widths both ways: a drag down is
                        // measured across.
                        let new_r = ((current_pos.y - (image_rect.min.y + center_y * ih)) / iw).abs().max(0.01);
                        klipp_core::MaskShape::Circle { center_x: *center_x, center_y: *center_y, radius: new_r }
                    }
                    (klipp_core::MaskShape::RadialGradient { center_x, center_y, inner, outer }, MaskHandle::Center) => {
                        klipp_core::MaskShape::RadialGradient {
                            center_x: (center_x + dx_norm).clamp(0.0, 1.0),
                            center_y: (center_y + dy_norm).clamp(0.0, 1.0),
                            inner: *inner,
                            outer: *outer,
                        }
                    }
                    // The outer ring, never inside the inner.
                    (klipp_core::MaskShape::RadialGradient { center_x, center_y, inner, .. }, MaskHandle::EdgeRight) => {
                        let r = ((current_pos.x - (image_rect.min.x + center_x * iw)) / iw).abs().max(*inner).max(0.01);
                        klipp_core::MaskShape::RadialGradient { center_x: *center_x, center_y: *center_y, inner: *inner, outer: r }
                    }
                    // The inner ring, measured across as sizes are, never
                    // past the outer.
                    (klipp_core::MaskShape::RadialGradient { center_x, center_y, outer, .. }, MaskHandle::EdgeBottom) => {
                        let r = ((current_pos.y - (image_rect.min.y + center_y * ih)) / iw).abs().min(*outer);
                        klipp_core::MaskShape::RadialGradient { center_x: *center_x, center_y: *center_y, inner: r, outer: *outer }
                    }
                    (klipp_core::MaskShape::Rectangle { center_x, center_y, width, height, rotation }, MaskHandle::Center) => {
                        klipp_core::MaskShape::Rectangle {
                            center_x: (center_x + dx_norm).clamp(0.0, 1.0),
                            center_y: (center_y + dy_norm).clamp(0.0, 1.0),
                            width: *width,
                            height: *height,
                            rotation: *rotation,
                        }
                    }
                    (klipp_core::MaskShape::Rectangle { center_x, center_y, width: _, height, rotation }, MaskHandle::EdgeRight) => {
                        let new_w = ((current_pos.x - (image_rect.min.x + center_x * iw)) / iw * 2.0).abs().max(0.01);
                        klipp_core::MaskShape::Rectangle { center_x: *center_x, center_y: *center_y, width: new_w, height: *height, rotation: *rotation }
                    }
                    (klipp_core::MaskShape::Rectangle { center_x, center_y, width, height: _, rotation }, MaskHandle::EdgeBottom) => {
                        let new_h = ((current_pos.y - (image_rect.min.y + center_y * ih)) / iw * 2.0).abs().max(0.01);
                        klipp_core::MaskShape::Rectangle { center_x: *center_x, center_y: *center_y, width: *width, height: new_h, rotation: *rotation }
                    }
                    (klipp_core::MaskShape::LinearGradient { start_x, start_y, end_x, end_y }, MaskHandle::Center) => {
                        klipp_core::MaskShape::LinearGradient {
                            start_x: (start_x + dx_norm).clamp(0.0, 1.0),
                            start_y: (start_y + dy_norm).clamp(0.0, 1.0),
                            end_x: (end_x + dx_norm).clamp(0.0, 1.0),
                            end_y: (end_y + dy_norm).clamp(0.0, 1.0),
                        }
                    }
                    (klipp_core::MaskShape::LinearGradient { start_x, start_y, .. }, MaskHandle::EdgeRight) => {
                        klipp_core::MaskShape::LinearGradient {
                            start_x: *start_x,
                            start_y: *start_y,
                            end_x: ((current_pos.x - image_rect.min.x) / iw).clamp(0.0, 1.0),
                            end_y: ((current_pos.y - image_rect.min.y) / ih).clamp(0.0, 1.0),
                        }
                    }
                    (klipp_core::MaskShape::Bezier { points }, MaskHandle::BezierPoint(idx)) => {
                        let mut new_points = points.clone();
                        // The point moves; its handles go with it.
                        if let Some(p) = new_points.get_mut(idx) {
                            p.x = ((current_pos.x - image_rect.min.x) / iw).clamp(0.0, 1.0);
                            p.y = ((current_pos.y - image_rect.min.y) / ih).clamp(0.0, 1.0);
                        }
                        klipp_core::MaskShape::Bezier { points: new_points }
                    }
                    _ => initial_shape.clone(),
                };

                // Build updated mask stack and emit command
                let mut updated_masks = masks.clone();
                if drag_mask_idx < updated_masks.masks.len() {
                    updated_masks.masks[drag_mask_idx].shape = new_shape;
                }
                cmds.push(AppCommand::SetNodeMasks {
                    clip_id: drag_clip_id,
                    node_id: drag_node_id,
                    masks: updated_masks,
                });
            } else {
                cmds.push(AppCommand::EndNodeParamEdit { clip_id: drag_clip_id });
                state.viewer_mask_drag = None;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    /// On-canvas Title-object editor: bounding boxes + 8 resize
    /// handles + rotation handle for the active title's objects.
    /// Pointer behaviour:
    ///
    /// - Click on an object's body → select that object.
    /// - Click on a handle of the selected object → start drag.
    /// - Drag body → translate `transform.center`.
    /// - Drag corner handle → uniform scale (resize box AND
    ///   the TextBox's font_size, pro-NLE convention).
    /// - Drag edge handle → resize the box on that axis only, along
    ///   the box's own sides when it is turned.
    /// - Drag rotation handle → rotate around the box's centre; near a
    ///   right angle it lands on it, Shift turns in 15° steps.
    /// - Drag the diamond on a text set on an arc → where the arc starts.
    /// - Click empty image area → deselect the object (reverts to
    ///   clip-level transform handles).
    ///
    /// A move and an edge snap to the frame's edges and centre lines,
    /// the safe areas and the other layers' edges and centres, with a
    /// guide drawn along the line; Alt drags free. Each drag is one undo
    /// step, between `BeginTitleObjectDrag` and `EndTitleObjectDrag`
    /// (#1714).
    ///
    /// Coordinates: object transforms live in normalised 0..1 frame
    /// space; the editor maps to/from `image_rect` (the rendered
    /// video's on-screen rect) for hit-testing and painting.
    fn draw_title_object_editor(
        &self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        image_rect: egui::Rect,
        cmds: &mut Vec<AppCommand>,
    ) {
        let clip_id = match state.selected_clip {
            Some(id) => id,
            None => return,
        };
        // Only operate on Title clips.
        let Some(clip) = state.sequence().tracks.iter()
            .flat_map(|t| t.clips.iter()).find(|c| c.id == clip_id) else { return };
        let Some(title) = clip.title_data() else { return };
        let raw_title = title.clone();
        let time = (state.playhead_smooth_secs - clip.timeline_in.to_secs_f64()).max(0.0);
        let resolution = state.sequence().resolution;
        // Where the title is drawn, as the renderer places it (#2014): its
        // own transform and the camera at its depth. Placed in a way the
        // canvas can't follow (tilted, scaled away, moved again by its
        // track or an adjustment layer), it says so instead of drawing
        // handles where the title isn't.
        let view = match klipp_compose::eval::placement_for_editing(clip, state.sequence(), title_edit_time(state)) {
            Ok(placement) => TitleView { image_rect, placement, frame: (resolution.width as f32, resolution.height as f32) },
            Err(why) => {
                if let Some(drag) = state.title_object_drag.take() {
                    cmds.push(AppCommand::EndTitleObjectDrag { clip_id: drag.clip_id });
                }
                let painter = ui.painter_at(image_rect);
                let galley = painter.layout(why.hint().to_string(), egui::FontId::proportional(crate::theme::type_scale::CAPTION), egui::Color32::WHITE, (image_rect.width() - 24.0).max(40.0));
                let r = egui::Rect::from_min_size(image_rect.left_top() + egui::vec2(12.0, 12.0), galley.size() + egui::vec2(12.0, 6.0));
                painter.rect_filled(r, 3.0, egui::Color32::from_rgba_unmultiplied(20, 20, 24, 220));
                painter.galley(r.min + egui::vec2(6.0, 3.0), galley, egui::Color32::WHITE);
                return;
            }
        };
        let td = editor_title_geometry(
            title, (resolution.width, resolution.height), time,
            &mut self.title_fit_geometry.borrow_mut(),
        );

        // Object-editor colours are deliberately fixed (not theme-
        // The theme's accent, as every selection in Klipp is drawn.
        let accent = ui.visuals().selection.stroke.color;
        let inactive_stroke = egui::Color32::from_rgba_unmultiplied(220, 220, 230, 180);
        let handle_fill = egui::Color32::from_rgb(20, 20, 24);
        let painter = ui.painter_at(image_rect);

        // Local closures bridging to the file-level viewer-geom
        // helpers — keeps the rest of `draw_title_object_editor`'s
        // body byte-identical (avoids touching every `to_screen` /
        // `to_norm` site for a no-behaviour-change refactor).
        // Through the title's placement (#2014): canvas points where they
        // are drawn, the pointer back onto the canvas.
        let to_screen = |nx: f32, ny: f32| view.to_screen(nx, ny);
        let to_norm = |p: egui::Pos2| view.to_norm(p);

        // ── Paint each visible object's bounding box ──
        // Skip the object that's currently in inline-edit mode —
        // its bounding box is drawn by the text-edit widget instead
        // (dashed accent stroke + Esc-to-exit badge), and painting
        // the standard solid box on top would double-stroke.
        let edit_oid = if state.viewer_text_edit {
            state.viewer_text_edit_object
        } else {
            None
        };
        for obj in &td.objects {
            if !obj.visible {
                continue;
            }
            if Some(obj.id) == edit_oid {
                continue;
            }
            let points = object_screen_points(obj, image_rect).map(|p| view.place(p));
            let bb = points_bb(&points);
            let is_selected = state.selected_title_object == Some((clip_id, obj.id));
            let stroke = if is_selected {
                egui::Stroke::new(2.0_f32, accent)
            } else {
                egui::Stroke::new(1.0_f32, inactive_stroke)
            };
            for i in 0..4 {
                painter.line_segment([points[i], points[(i + 1) % 4]], stroke);
            }

            if !is_selected || is_fitted_shape(obj) {
                continue;
            }

            // Handles: 4 corners + 4 mid-edges + rotation stalk.
            let handle_size = 8.0_f32;
            let handle_stroke = egui::Stroke::new(1.5_f32, accent);
            let handle_rect = |p: egui::Pos2| egui::Rect::from_center_size(p, egui::vec2(handle_size, handle_size));

            let mid_top = points[0].lerp(points[1], 0.5);
            let mid_bottom = points[3].lerp(points[2], 0.5);
            let mid_left = points[0].lerp(points[3], 0.5);
            let mid_right = points[1].lerp(points[2], 0.5);
            for hp in [
                points[0], points[1], points[3], points[2],
                mid_top, mid_bottom, mid_left, mid_right,
            ] {
                let r = handle_rect(hp);
                painter.rect_filled(r, 1.5, handle_fill);
                painter.rect_stroke(r, 1.5, handle_stroke);
            }

            // Rotation stalk above the top-centre handle.
            let outward = (mid_top - bb.center()).normalized();
            let stalk_top = mid_top + outward * 24.0;
            painter.line_segment(
                [mid_top, stalk_top],
                handle_stroke,
            );
            painter.circle_filled(stalk_top, 5.0, accent);
            painter.circle_stroke(
                stalk_top,
                5.0,
                egui::Stroke::new(1.0_f32, handle_fill),
            );

            // Anchor dot — drag-handle for `transform.anchor_norm`.
            // Positioned at the bbox-relative anchor; uses a small
            // crosshair shape so it reads as a separate affordance
            // from the corner / edge resize handles.
            let an_p = points[0]
                + (points[1] - points[0]) * obj.transform.anchor_norm.x
                + (points[3] - points[0]) * obj.transform.anchor_norm.y;
            let an_x = an_p.x;
            let an_y = an_p.y;
            let an_r = 4.5;
            painter.circle_filled(an_p, an_r, handle_fill);
            painter.circle_stroke(an_p, an_r, handle_stroke);
            painter.line_segment(
                [egui::pos2(an_x - an_r - 2.0, an_y), egui::pos2(an_x + an_r + 2.0, an_y)],
                egui::Stroke::new(1.0_f32, accent),
            );
            painter.line_segment(
                [egui::pos2(an_x, an_y - an_r - 2.0), egui::pos2(an_x, an_y + an_r + 2.0)],
                egui::Stroke::new(1.0_f32, accent),
            );

            // The start of a text set on an arc (#1714): a diamond on
            // the arc, dragged round the layer's centre.
            if let Some(a) = arc_start_screen_point(obj, image_rect, (resolution.width, resolution.height)).map(|a| view.place(a)) {
                let d = 5.5;
                painter.add(egui::Shape::convex_polygon(
                    vec![a + egui::vec2(0.0, -d), a + egui::vec2(d, 0.0), a + egui::vec2(0.0, d), a + egui::vec2(-d, 0.0)],
                    handle_fill,
                    handle_stroke,
                ));
            }
        }

        // ── Pointer interaction ──
        // Allocated invisibly over the image so we can read input
        // without stealing focus from other widgets.
        let resp = ui.interact(image_rect, ui.id().with("title_object_editor"), egui::Sense::click_and_drag());

        if resp.hovered() {
            if let Some(p) = resp.interact_pointer_pos() {
                let over_text = td.objects.iter().rev().any(|obj| {
                    obj.visible && !obj.locked && object_contains_screen_point(obj, image_rect, view.unplace(p))
                });
                if over_text {
                    resp.clone().on_hover_text("Double-click text to edit");
                }
            }
        }

        // Resolve pointer position once per frame.
        let pointer = resp.interact_pointer_pos().or_else(|| ui.input(|i| i.pointer.hover_pos()));

        // ── Drag: ongoing? ──
        if let Some(drag) = state.title_object_drag.clone() {
            if drag.clip_id != clip_id || td.object(drag.object_id).is_some_and(is_fitted_shape) {
                state.title_object_drag = None;
                cmds.push(AppCommand::EndTitleObjectDrag { clip_id: drag.clip_id });
            } else if resp.dragged() && drag.handle == crate::state::TitleObjectHandle::PathStart {
                // The arc starts where the pointer is, seen from the
                // layer's centre (#1714).
                if let Some(p) = pointer {
                    let (nx, ny) = to_norm(p);
                    let (frame_w, frame_h) = (resolution.width.max(1) as f32, resolution.height.max(1) as f32);
                    let c = drag.initial_transform.center;
                    let start_angle = ((ny - c.y) * frame_h).atan2((nx - c.x) * frame_w).to_degrees();
                    cmds.push(AppCommand::SetTitleArcStart { clip_id, object_id: drag.object_id.0, start_angle });
                }
            } else if resp.dragged() {
                if let Some(p) = pointer {
                    use crate::state::TitleObjectHandle as H;
                    let (nx, ny) = to_norm(p);
                    let (start_nx, start_ny) = to_norm(drag.drag_start_pos);
                    let dnx = nx - start_nx;
                    let dny = ny - start_ny;
                    let free = ui.input(|i| i.modifiers.alt);
                    let mut new_t = compute_drag_transform(
                        &drag, dnx, dny,
                        (start_nx, start_ny), (nx, ny),
                    );
                    let mut guides: Vec<(bool, f32)> = Vec::new();
                    // Snapping (#1714): the frame's edges and centre
                    // lines, the safe areas and the other layers' edges
                    // and centres, a guide drawn along each line snapped
                    // to. A move snaps the layer's edges and centre; an
                    // edge of an upright box snaps itself. Alt drags free.
                    let (xs, ys) = title_snap_lines(&td, drag.object_id, &state.viewer_prefs, drag.frame_aspect);
                    // The snap distance as it is seen: on a scaled title,
                    // the same pixels on screen (#2014).
                    let (within_x, within_y) = (TITLE_SNAP_PX / (image_rect.width().max(1.0) * view.scale()), TITLE_SNAP_PX / (image_rect.height().max(1.0) * view.scale()));
                    let upright = new_t.rotation.rem_euclid(360.0) == 0.0;
                    match drag.handle {
                        H::Body if !free => {
                            // The layer as it is seen: its box hugs its text.
                            let mut seen = td.object(drag.object_id).map(|o| o.transform.clone()).unwrap_or_else(|| new_t.clone());
                            seen.center = new_t.center;
                            let (hw, hh) = layer_half_extent(&seen, drag.frame_aspect);
                            let c = new_t.center;
                            if let Some((shift, line)) = snap_to_lines(&[c.x - hw, c.x, c.x + hw], &xs, within_x) {
                                new_t.center.x += shift;
                                guides.push((true, line));
                            }
                            if let Some((shift, line)) = snap_to_lines(&[c.y - hh, c.y, c.y + hh], &ys, within_y) {
                                new_t.center.y += shift;
                                guides.push((false, line));
                            }
                        }
                        H::EdgeLeft | H::EdgeRight if !free && upright => {
                            let (left, right) = (new_t.center.x - new_t.size.x * 0.5, new_t.center.x + new_t.size.x * 0.5);
                            let moving = if drag.handle == H::EdgeLeft { left } else { right };
                            if let Some((shift, line)) = snap_to_lines(&[moving], &xs, within_x) {
                                let (l, r) = if drag.handle == H::EdgeLeft { (left + shift, right) } else { (left, right + shift) };
                                if r - l >= 0.01 {
                                    new_t.center.x = (l + r) * 0.5;
                                    new_t.size.x = r - l;
                                    guides.push((true, line));
                                }
                            }
                        }
                        H::EdgeTop | H::EdgeBottom if !free && upright => {
                            let (top, bottom) = (new_t.center.y - new_t.size.y * 0.5, new_t.center.y + new_t.size.y * 0.5);
                            let moving = if drag.handle == H::EdgeTop { top } else { bottom };
                            if let Some((shift, line)) = snap_to_lines(&[moving], &ys, within_y) {
                                let (t0, b0) = if drag.handle == H::EdgeTop { (top + shift, bottom) } else { (top, bottom + shift) };
                                if b0 - t0 >= 0.01 {
                                    new_t.center.y = (t0 + b0) * 0.5;
                                    new_t.size.y = b0 - t0;
                                    guides.push((false, line));
                                }
                            }
                        }
                        H::Rotation if !free => {
                            // Shift turns in 15° steps; otherwise a turn
                            // within 2° of a right angle lands on it.
                            let step = if ui.input(|i| i.modifiers.shift) { 15.0 } else { 90.0 };
                            let near = (new_t.rotation / step).round() * step;
                            if step < 90.0 || (new_t.rotation - near).abs() <= 2.0 {
                                new_t.rotation = near;
                            }
                        }
                        _ => {}
                    }
                    let guide_stroke = egui::Stroke::new(1.0_f32, accent);
                    for (vertical, g) in guides {
                        // The canvas's line, where the placed title draws it.
                        if vertical {
                            painter.line_segment([to_screen(g, 0.0), to_screen(g, 1.0)], guide_stroke);
                        } else {
                            painter.line_segment([to_screen(0.0, g), to_screen(1.0, g)], guide_stroke);
                        }
                    }
                    // A resize draws the box being drawn: the frame drawn
                    // above hugs the text, which wraps to it once set.
                    if !matches!(drag.handle, H::Body | H::Anchor) {
                        if let Some(mut live) = td.object(drag.object_id).cloned() {
                            live.transform = new_t.clone();
                            let pts = object_screen_points(&live, image_rect).map(|p| view.place(p));
                            for i in 0..4 {
                                painter.line_segment([pts[i], pts[(i + 1) % 4]], egui::Stroke::new(1.0_f32, accent));
                            }
                        }
                    }
                    // The readout: the layer's centre in frame pixels,
                    // beside the pointer; its size for a resize, its
                    // angle for a turn.
                    {
                        let res = state.sequence().resolution;
                        let text = match drag.handle {
                            H::Rotation => format!("{:.1}\u{b0}", new_t.rotation),
                            H::Body | H::Anchor => format!("x {:.0}  y {:.0}", new_t.center.x * res.width as f32, new_t.center.y * res.height as f32),
                            _ => format!("w {:.0}  h {:.0}", new_t.size.x * res.width as f32, new_t.size.y * res.height as f32),
                        };
                        let galley = painter.layout_no_wrap(text, egui::FontId::monospace(crate::theme::type_scale::CAPTION), egui::Color32::WHITE);
                        let pad = egui::vec2(6.0, 3.0);
                        let size = galley.size() + pad * 2.0;
                        let mut at = p + egui::vec2(14.0, 14.0);
                        if at.x + size.x > image_rect.max.x {
                            at.x = p.x - 14.0 - size.x;
                        }
                        if at.y + size.y > image_rect.max.y {
                            at.y = p.y - 14.0 - size.y;
                        }
                        let r = egui::Rect::from_min_size(at, size);
                        painter.rect_filled(r, 2.0, egui::Color32::from_rgba_unmultiplied(20, 20, 24, 220));
                        painter.galley(r.min + pad, galley, egui::Color32::WHITE);
                    }
                    let new_font_size = if matches!(
                        drag.handle,
                        crate::state::TitleObjectHandle::CornerTopLeft
                            | crate::state::TitleObjectHandle::CornerTopRight
                            | crate::state::TitleObjectHandle::CornerBottomLeft
                            | crate::state::TitleObjectHandle::CornerBottomRight
                    ) {
                        // Uniform font scale = ratio of new size to
                        // old size on whichever axis grew. Use the
                        // smaller of the two axis scales so corner
                        // drag stays uniform even if the user hasn't
                        // held shift (we treat corner-drag as always
                        // uniform — pro convention).
                        let sx_scale = if drag.initial_transform.size.x.abs() > 0.0001 {
                            new_t.size.x / drag.initial_transform.size.x
                        } else { 1.0 };
                        let sy_scale = if drag.initial_transform.size.y.abs() > 0.0001 {
                            new_t.size.y / drag.initial_transform.size.y
                        } else { 1.0 };
                        let scale = sx_scale.min(sy_scale).max(0.05);
                        Some((drag.initial_font_size * scale).clamp(4.0, 1000.0))
                    } else {
                        None
                    };

                    if let Some(fs) = new_font_size {
                        cmds.push(AppCommand::SetTitleObjectTransformAndFont {
                            clip_id,
                            object_id: drag.object_id.0,
                            center_x: new_t.center.x,
                            center_y: new_t.center.y,
                            size_x: new_t.size.x,
                            size_y: new_t.size.y,
                            rotation: new_t.rotation,
                            anchor_x: new_t.anchor_norm.x,
                            anchor_y: new_t.anchor_norm.y,
                            font_size: fs,
                        });
                    } else {
                        cmds.push(AppCommand::SetTitleObjectTransform {
                            clip_id,
                            object_id: drag.object_id.0,
                            center_x: new_t.center.x,
                            center_y: new_t.center.y,
                            size_x: new_t.size.x,
                            size_y: new_t.size.y,
                            rotation: new_t.rotation,
                            anchor_x: new_t.anchor_norm.x,
                            anchor_y: new_t.anchor_norm.y,
                        });
                    }
                }
            }
            if resp.drag_stopped() {
                state.title_object_drag = None;
                cmds.push(AppCommand::EndTitleObjectDrag { clip_id: drag.clip_id });
            }
            return;
        }

        // ── Drag start: only when no drag is active and the user
        // pressed inside the image. Hit-test in priority order:
        // selected object's handles → any object's body → empty.
        if resp.drag_started() {
            // Where the press was, not where the pointer has got to by
            // the time the move counts as a drag: a handle is grabbed
            // where it was pressed, and none of the move is lost.
            let pointer = ui.input(|i| i.pointer.press_origin()).or(pointer);
            if let Some(p) = pointer {
                if !image_rect.contains(p) {
                    return;
                }
                let frame_aspect = image_rect.width().max(1.0) / image_rect.height().max(1.0);
                // 1. If we have a selected object, check its handles.
                let selected = state.selected_title_object
                    .filter(|(c, _)| *c == clip_id)
                    .and_then(|(_, oid)| td.object(oid).cloned());
                if let Some(obj) = &selected {
                    let on_arc_start = arc_start_screen_point(obj, image_rect, (resolution.width, resolution.height))
                        .is_some_and(|a| view.place(a).distance(p) <= 10.0);
                    let handle = if on_arc_start {
                        Some(crate::state::TitleObjectHandle::PathStart)
                    } else {
                        hit_test_handles(obj, image_rect, view.unplace(p))
                    };
                    if let Some(handle) = handle {
                        let mut initial_transform = raw_title.object(obj.id)
                            .map(|raw| raw.transform.clone())
                            .unwrap_or_else(|| obj.transform.clone());
                        // A resize starts from the box as it is seen,
                        // which hugs the text, not from the layout box
                        // behind it, often the whole frame (#1714).
                        if is_resize_handle(handle) && matches!(obj.content, klipp_timeline::TitleObjectContent::TextBox(_)) {
                            initial_transform.size = obj.transform.size;
                        }
                        let initial_font_size = match &obj.content {
                            klipp_timeline::TitleObjectContent::TextBox(tb) => tb.font_size,
                            _ => 0.0,
                        };
                        state.title_object_drag = Some(crate::state::TitleObjectDrag {
                            clip_id,
                            object_id: obj.id,
                            handle,
                            initial_transform,
                            initial_font_size,
                            drag_start_pos: p,
                            frame_aspect,
                        });
                        cmds.push(AppCommand::BeginTitleObjectDrag { clip_id });
                        return;
                    }
                }
                // 2. Hit-test any object's body (top-most first → reverse).
                for obj in td.objects.iter().rev() {
                    if !obj.visible || obj.locked {
                        continue;
                    }
                    if object_contains_screen_point(obj, image_rect, view.unplace(p)) {
                        // Select this object and start a body drag.
                        cmds.push(AppCommand::SelectTitleObject {
                            clip_id,
                            object_id: Some(obj.id.0),
                        });
                        if is_fitted_shape(obj) { return; }
                        let initial_transform = raw_title.object(obj.id)
                            .map(|raw| raw.transform.clone())
                            .unwrap_or_else(|| obj.transform.clone());
                        let initial_font_size = match &obj.content {
                            klipp_timeline::TitleObjectContent::TextBox(tb) => tb.font_size,
                            _ => 0.0,
                        };
                        state.title_object_drag = Some(crate::state::TitleObjectDrag {
                            clip_id,
                            object_id: obj.id,
                            handle: crate::state::TitleObjectHandle::Body,
                            initial_transform,
                            initial_font_size,
                            drag_start_pos: p,
                            frame_aspect,
                        });
                        cmds.push(AppCommand::BeginTitleObjectDrag { clip_id });
                        return;
                    }
                }
                // 3. Empty image area → deselect the object.
                if state.selected_title_object.is_some() {
                    cmds.push(AppCommand::SelectTitleObject {
                        clip_id,
                        object_id: None,
                    });
                }
            }
        } else if resp.clicked() {
            // Single click without drag: same hit-test for selection
            // change (allows clicking to switch between stacked
            // objects without immediately starting a drag).
            if let Some(p) = pointer {
                let mut hit = None;
                for obj in td.objects.iter().rev() {
                    if !obj.visible || obj.locked {
                        continue;
                    }
                    if object_contains_screen_point(obj, image_rect, p) {
                        hit = Some(obj.id);
                        break;
                    }
                }
                if let Some(id) = hit {
                    if state.selected_title_object != Some((clip_id, id)) {
                        cmds.push(AppCommand::SelectTitleObject {
                            clip_id,
                            object_id: Some(id.0),
                        });
                    }
                } else if state.selected_title_object.is_some() {
                    cmds.push(AppCommand::SelectTitleObject {
                        clip_id,
                        object_id: None,
                    });
                }
            }
        }

        // Make every title affordance discoverable before a drag begins.
        if let Some(p) = pointer {
            if let Some(obj) = state.selected_title_object
                .filter(|(c, _)| *c == clip_id)
                .and_then(|(_, oid)| td.object(oid))
            {
                let on_arc_start = arc_start_screen_point(obj, image_rect, (resolution.width, resolution.height))
                    .is_some_and(|a| a.distance(p) <= 10.0);
                let handle = if on_arc_start { Some(crate::state::TitleObjectHandle::PathStart) } else { hit_test_handles(obj, image_rect, p) };
                if let Some(handle) = handle {
                    let icon = match handle {
                        crate::state::TitleObjectHandle::Rotation => egui::CursorIcon::Alias,
                        crate::state::TitleObjectHandle::Anchor => egui::CursorIcon::Crosshair,
                        crate::state::TitleObjectHandle::PathStart => egui::CursorIcon::Grab,
                        crate::state::TitleObjectHandle::Body => egui::CursorIcon::Move,
                        crate::state::TitleObjectHandle::EdgeLeft | crate::state::TitleObjectHandle::EdgeRight => egui::CursorIcon::ResizeHorizontal,
                        crate::state::TitleObjectHandle::EdgeTop | crate::state::TitleObjectHandle::EdgeBottom => egui::CursorIcon::ResizeVertical,
                        crate::state::TitleObjectHandle::CornerTopLeft | crate::state::TitleObjectHandle::CornerBottomRight => egui::CursorIcon::ResizeNwSe,
                        crate::state::TitleObjectHandle::CornerTopRight | crate::state::TitleObjectHandle::CornerBottomLeft => egui::CursorIcon::ResizeNeSw,
                    };
                    ui.ctx().set_cursor_icon(icon);
                } else if object_contains_screen_point(obj, image_rect, p) {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
                }
            }
        }
    }

    /// Draggable focal-point gizmo on the viewer for the HYPERZOOM
    /// inspector section. Visual language matches the title-object
    /// anchor handle (filled circle + ring + cross) — that's the
    /// established "this is a draggable pivot point" affordance, so
    /// reusing it makes the focal-point handle read instantly. The
    /// scratch state lives in egui memory keyed by clip-id (see
    /// `crate::hyperzoom::hyperzoom_scratch_id`), shared with the
    /// inspector spinboxes — drag here updates the spinboxes live;
    /// type into the spinboxes and the crosshair jumps.
    ///
    /// Skipped when:
    ///
    /// * No clip is selected.
    /// * Selected clip is on a kind that hyperzoom doesn't apply to
    ///   (`AdjustmentLayer` / `Gap` / nested `Sequence`) — these are
    ///   the same kinds the inspector's HYPERZOOM section refuses
    ///   to render for.
    /// * The image rect is degenerate (zero width or height).
    /// * The clip's HYPERZOOM section isn't open on screen in the
    ///   inspector (#2032): the handle otherwise sat on the middle of
    ///   every selected clip and took the press that should move it.
    fn draw_hyperzoom_focal_handle(
        &self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        image_rect: egui::Rect,
    ) {
        let Some(clip_id) = state.selected_clip else { return; };
        if image_rect.width() < 1.0 || image_rect.height() < 1.0 {
            return;
        }
        if !crate::hyperzoom::section_shown(ui.ctx(), clip_id) {
            return;
        }
        // Source-of-truth check: the clip must still be in the active
        // sequence and its kind must qualify for hyperzoom.
        let qualifies = state
            .sequence()
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.id == clip_id)
            .map(|c| {
                !matches!(
                    c.kind,
                    klipp_timeline::ClipKind::AdjustmentLayer
                        | klipp_timeline::ClipKind::Gap
                        | klipp_timeline::ClipKind::Sequence { .. }
                )
            })
            .unwrap_or(false);
        if !qualifies {
            return;
        }

        let scratch = crate::hyperzoom::read_hyperzoom_scratch(ui.ctx(), clip_id);
        let fx = scratch.focal_x.clamp(0.0, 1.0);
        let fy = scratch.focal_y.clamp(0.0, 1.0);

        // Same accent + handle colours as the title-object editor so
        // the two gizmos read as the same visual family.
        let accent = ui.visuals().selection.stroke.color;
        let handle_fill = egui::Color32::from_rgb(20, 20, 24);
        let handle_stroke = egui::Stroke::new(1.5_f32, accent);

        let painter = ui.painter_at(image_rect);
        let centre = viewer_norm_to_screen(image_rect, fx, fy);
        let cx = centre.x;
        let cy = centre.y;
        let r = 5.5_f32;

        // Filled disc + accent ring + cross arms. Slightly bigger
        // than the title-object anchor (4.5 → 5.5) so the focal
        // handle is the more prominent affordance when both are on
        // screen — the focal point is "the thing the user is
        // tuning" while the title anchor is a static reference.
        painter.circle_filled(centre, r, handle_fill);
        painter.circle_stroke(centre, r, handle_stroke);
        let arm = r + 4.0;
        painter.line_segment(
            [egui::pos2(cx - arm, cy), egui::pos2(cx + arm, cy)],
            egui::Stroke::new(1.0_f32, accent),
        );
        painter.line_segment(
            [egui::pos2(cx, cy - arm), egui::pos2(cx, cy + arm)],
            egui::Stroke::new(1.0_f32, accent),
        );

        // Hit test only the handle itself, not the whole image rect —
        // dragging anywhere else on the canvas should still fall
        // through to the title-object editor / transform controls.
        let hit = egui::Rect::from_center_size(centre, egui::vec2(arm * 2.0, arm * 2.0));
        let resp = ui.interact(
            hit,
            ui.id().with(("hyperzoom_focal_handle", clip_id)),
            egui::Sense::click_and_drag(),
        );
        if resp.dragged() {
            if let Some(p) = resp.interact_pointer_pos() {
                let (nx, ny) = viewer_screen_to_norm(image_rect, p);
                crate::hyperzoom::update_hyperzoom_scratch(ui.ctx(), clip_id, |s| {
                    s.focal_x = nx.clamp(0.0, 1.0);
                    s.focal_y = ny.clamp(0.0, 1.0);
                });
            }
        }

        // Hover affordance: tiny tooltip that names the value the
        // user is dragging — without it the floating crosshair on a
        // black letterbox is a UFO.
        if resp.hovered() || resp.dragged() {
            resp.on_hover_text(format!(
                "Hyperzoom focal point ({:.3}, {:.3})\nDrag to move; spinboxes in HYPERZOOM stay in sync.",
                fx, fy,
            ));
        }
    }

    fn draw_transform_controls(
        &self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        image_rect: egui::Rect,
        viewer_rect: egui::Rect,
        frame_w: f32,
        frame_h: f32,
        cmds: &mut Vec<AppCommand>,
    ) {
        // Only show when a video clip is selected
        let clip_id = match state.selected_clip {
            Some(id) => id,
            None => return,
        };

        // Find the clip in the sequence
        let clip = match state.sequence().tracks.iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.id == clip_id)
        {
            Some(c) => c,
            None => return,
        };

        // Only show transform controls for clips (skip gaps)
        if matches!(clip.kind, klipp_timeline::ClipKind::Gap) {
            return;
        }

        // The clip's placement as its picture holds it (M3 step 13).
        let transform = crate::inspector::placement_by_path::placement(clip);
        let effect_index = find_transform_index(clip);
        let n_builtin_effects = clip.builtin_effects().len();

        // Coordinate mapping: frame pixels -> screen pixels
        let scale_to_screen_x = image_rect.width() / frame_w;
        let scale_to_screen_y = image_rect.height() / frame_h;
        let center_screen = image_rect.center();

        // Anchor point (0.0-1.0, default 0.5 = center)
        let anchor_x = transform.anchor_x;
        let anchor_y = transform.anchor_y;

        // pos_screen is the anchor/pivot point in screen space
        let pos_screen = egui::pos2(
            center_screen.x + transform.position_x * scale_to_screen_x,
            center_screen.y + transform.position_y * scale_to_screen_y,
        );

        // Bounding box half-extents in screen space
        let half_w = frame_w * 0.5 * transform.scale_x * scale_to_screen_x;
        let half_h = frame_h * 0.5 * transform.scale_y * scale_to_screen_y;

        // The anchor offset: how far the box center is from the anchor point.
        // When anchor is (0.5, 0.5) this is zero (center pivot).
        // When anchor is (0.0, 0.0) the pivot is at the top-left, so the box
        // center is shifted right and down by half_w/half_h.
        // A flipped picture lies on the other side of its anchor (#2705).
        let (flip_x, flip_y) = clip.picture_v2().map(|p| (p.picture.placement.sign_x(), p.picture.placement.sign_y())).unwrap_or((1.0, 1.0));
        let anchor_offset_x = (anchor_x - 0.5) * frame_w * transform.scale_x * scale_to_screen_x * flip_x;
        let anchor_offset_y = (anchor_y - 0.5) * frame_h * transform.scale_y * scale_to_screen_y * flip_y;

        // The bounding box center, offset so the anchor maps to pos_screen
        let box_center_x = pos_screen.x - anchor_offset_x;
        let box_center_y = pos_screen.y - anchor_offset_y;

        let rotation_rad = transform.rotation.to_radians();

        // Unrotated corners relative to box center
        let raw_corners = [
            egui::pos2(box_center_x - half_w, box_center_y - half_h), // top-left
            egui::pos2(box_center_x + half_w, box_center_y - half_h), // top-right
            egui::pos2(box_center_x + half_w, box_center_y + half_h), // bottom-right
            egui::pos2(box_center_x - half_w, box_center_y + half_h), // bottom-left
        ];

        // Rotate corners around pos_screen (the anchor/pivot point)
        let corners: [egui::Pos2; 4] = std::array::from_fn(|i| {
            rotate_point(raw_corners[i], pos_screen, rotation_rad)
        });

        // Painter for the transform overlay. Sits in `Order::Middle` —
        // above the viewer image (which paints in the central panel
        // at `Order::Background`) but BELOW everything in
        // `Order::Foreground`: context menus, dropdowns, modal
        // dialogs, tooltips. Earlier this layer was on `Foreground`,
        // which made the bounding-box outline + handles bleed
        // through any open clip-context menu (egui's menus also live
        // on `Foreground`, and within the same Order they stack by
        // creation sequence — the per-frame redraw of the overlay
        // ended up on top). `Middle` keeps the overlay visible over
        // the viewer image (the toolbar / transport / panel chrome
        // don't overlap the viewer rect, so they don't visually
        // cover the handles in regular dock layouts) while letting
        // every popup naturally occlude it.
        //
        // Clipped to the viewer: a picture that fills the viewer's height
        // put its rotation handle above the panel, over the page tabs,
        // where a click on "Colour" grabbed it.
        let painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Middle,
            ui.id().with("transform_controls"),
        )).with_clip_rect(viewer_rect);

        // Draw bounding box outline
        let outline_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 180);
        let outline_stroke = egui::Stroke::new(1.5_f32, outline_color);
        for i in 0..4 {
            painter.line_segment([corners[i], corners[(i + 1) % 4]], outline_stroke);
        }

        // Draw diagonal crosshairs through center (subtle)
        let cross_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 60);
        let cross_stroke = egui::Stroke::new(0.5_f32, cross_color);
        let mid_top = egui::pos2(
            (corners[0].x + corners[1].x) * 0.5,
            (corners[0].y + corners[1].y) * 0.5,
        );
        let mid_bottom = egui::pos2(
            (corners[2].x + corners[3].x) * 0.5,
            (corners[2].y + corners[3].y) * 0.5,
        );
        let mid_left = egui::pos2(
            (corners[0].x + corners[3].x) * 0.5,
            (corners[0].y + corners[3].y) * 0.5,
        );
        let mid_right = egui::pos2(
            (corners[1].x + corners[2].x) * 0.5,
            (corners[1].y + corners[2].y) * 0.5,
        );
        painter.line_segment([mid_top, mid_bottom], cross_stroke);
        painter.line_segment([mid_left, mid_right], cross_stroke);

        // Corner handles (scale)
        let handle_size = state.viewer_prefs.transform_handle_size;
        let handle_half = handle_size / 2.0;
        for corner in &corners {
            let handle_rect = egui::Rect::from_center_size(*corner, egui::vec2(handle_size, handle_size));
            painter.rect_filled(handle_rect, 0.0, egui::Color32::WHITE);
            painter.rect_stroke(handle_rect, 0.0, egui::Stroke::new(1.0_f32, egui::Color32::BLACK));
        }

        // Center handle (position) — circle
        let center_radius = 6.0;
        painter.circle_filled(pos_screen, center_radius, egui::Color32::WHITE);
        painter.circle_stroke(pos_screen, center_radius, egui::Stroke::new(1.0_f32, egui::Color32::BLACK));
        // Small crosshair inside center handle
        let ch = 3.0;
        painter.line_segment(
            [egui::pos2(pos_screen.x - ch, pos_screen.y), egui::pos2(pos_screen.x + ch, pos_screen.y)],
            egui::Stroke::new(1.0_f32, egui::Color32::BLACK),
        );
        painter.line_segment(
            [egui::pos2(pos_screen.x, pos_screen.y - ch), egui::pos2(pos_screen.x, pos_screen.y + ch)],
            egui::Stroke::new(1.0_f32, egui::Color32::BLACK),
        );

        // Anchor point indicator — small diamond at the pivot
        let anchor_diamond = 4.0;
        let anchor_pts = [
            egui::pos2(pos_screen.x, pos_screen.y - anchor_diamond),
            egui::pos2(pos_screen.x + anchor_diamond, pos_screen.y),
            egui::pos2(pos_screen.x, pos_screen.y + anchor_diamond),
            egui::pos2(pos_screen.x - anchor_diamond, pos_screen.y),
        ];
        let anchor_color = egui::Color32::from_rgba_unmultiplied(255, 200, 50, 220);
        for i in 0..4 {
            painter.line_segment(
                [anchor_pts[i], anchor_pts[(i + 1) % 4]],
                egui::Stroke::new(1.5_f32, anchor_color),
            );
        }

        // Rotation handle — small circle above the top edge
        let rot_distance = 25.0;
        let rot_radius = 5.0;
        let rot_handle_pos = rotation_handle_pos(
            egui::pos2(box_center_x, box_center_y - half_h),
            pos_screen,
            rotation_rad,
            rot_distance,
            viewer_rect.shrink(rot_radius + 2.0),
        );
        let rot_connector_base = mid_top;
        painter.line_segment(
            [rot_connector_base, rot_handle_pos],
            egui::Stroke::new(1.5_f32, outline_color),
        );
        painter.circle_filled(rot_handle_pos, rot_radius, egui::Color32::WHITE);
        painter.circle_stroke(rot_handle_pos, rot_radius, egui::Stroke::new(1.0_f32, egui::Color32::BLACK));
        // Small arc icon inside rotation handle
        let arc_r = 2.5;
        painter.line_segment(
            [
                egui::pos2(rot_handle_pos.x - arc_r, rot_handle_pos.y),
                egui::pos2(rot_handle_pos.x, rot_handle_pos.y - arc_r),
            ],
            egui::Stroke::new(0.8_f32, egui::Color32::BLACK),
        );
        painter.line_segment(
            [
                egui::pos2(rot_handle_pos.x, rot_handle_pos.y - arc_r),
                egui::pos2(rot_handle_pos.x + arc_r, rot_handle_pos.y),
            ],
            egui::Stroke::new(0.8_f32, egui::Color32::BLACK),
        );

        // ── Interaction ──
        let pointer_pos = ui.input(|i| i.pointer.interact_pos());
        let pointer_down = ui.input(|i| i.pointer.primary_down());
        let pointer_released = ui.input(|i| i.pointer.primary_released());
        let shift_held = ui.input(|i| i.modifiers.shift);
        let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let hit_radius = 10.0;
        let drag_key = format!("viewer_transform_{}", clip_id.0);

        // Handle ongoing drag
        if let Some(mut drag) = state.viewer_transform_drag.take() {
            let idx = effect_index.unwrap_or(drag.effect_index);
            let mut step = if escape {
                GizmoStep::Abandon
            } else {
                gizmo_step(drag.started, drag.drag_start_pos, pointer_pos, pointer_down, pointer_released)
            };
            let transform_now = self
                .compute_dragged_transform(&drag, pointer_pos, scale_to_screen_x, scale_to_screen_y, center_screen, shift_held)
                .to_builtin_effect();
            // Back where it began, however far the pointer went between:
            // a click, and nothing to keep (#408).
            if step == GizmoStep::End && transform_now == drag.initial_transform.to_builtin_effect() {
                step = GizmoStep::Abandon;
            }
            if matches!(step, GizmoStep::Start | GizmoStep::End) && !drag.started {
                // The pointer has crossed the threshold: only now is the
                // effect added and the undo gesture opened. A flick that
                // crossed it and released within one frame opens and
                // closes the gesture in that frame.
                if effect_index.is_none() {
                    cmds.push(AppCommand::AddBuiltinEffect {
                        clip_id,
                        effect: drag.initial_transform.to_builtin_effect(),
                    });
                    drag.added = true;
                }
                cmds.push(AppCommand::BeginEffectDrag {
                    key: drag_key.clone(),
                    clip_id,
                    effect_index: drag.effect_index,
                    old_effect: drag.initial_transform.to_builtin_effect(),
                });
                drag.started = true;
            }
            match step {
                GizmoStep::Nothing | GizmoStep::Start => {
                    state.viewer_transform_drag = Some(drag);
                    ui.ctx().request_repaint();
                }
                GizmoStep::Update => {
                    cmds.push(AppCommand::UpdateBuiltinEffect { clip_id, index: idx, effect: transform_now });
                    ui.ctx().set_cursor_icon(match drag.handle {
                        TransformHandle::Position | TransformHandle::Rotation => egui::CursorIcon::Grabbing,
                        _ => egui::CursorIcon::ResizeNwSe,
                    });
                    state.viewer_transform_drag = Some(drag);
                    ui.ctx().request_repaint();
                }
                GizmoStep::End => {
                    // Apply before closing the gesture: EndEffectDrag only
                    // writes history, so without this a release frame's
                    // movement is recorded and never applied — a flick
                    // added an identity Transform and an undo entry for a
                    // move that never happened.
                    cmds.push(AppCommand::UpdateBuiltinEffect { clip_id, index: idx, effect: transform_now.clone() });
                    cmds.push(AppCommand::EndEffectDrag { key: drag_key, clip_id, effect_index: idx, new_effect: transform_now });
                }
                GizmoStep::Abandon => {
                    // A gesture that opened and cannot close as an edit:
                    // released back at its start, Escape, the pointer
                    // gone from the window, or its release never seen
                    // because the gizmo was not drawn on that frame (a
                    // workspace or modal opened by shortcut mid-drag).
                    // Left open, it swallowed every later inspector
                    // edit's undo entry (#409). One that never opened
                    // (a click) has nothing to take back.
                    if drag.started {
                        // By path, `placement_by_path` takes this as the
                        // placement drag's abandon.
                        cmds.push(AppCommand::AbandonEffectDrag { clip_id, effect_index: idx, added: drag.added });
                    }
                }
            }
        } else if let Some(pointer) = pointer_pos {
            // Check for new drag start; not one the motion path took (#2276).
            if pointer_down && ui.input(|i| i.pointer.primary_pressed()) && state.motion_path_drag.is_none() {
                // Only start a drag if the pointer is within the image_rect (with some margin)
                let expanded = image_rect.expand(40.0);
                if expanded.contains(pointer) && viewer_rect.contains(pointer) {
                    let handle = self.hit_test_handles(
                        pointer, pos_screen, &corners, rot_handle_pos,
                        center_radius, handle_half, rot_radius, hit_radius,
                    );

                    if let Some(handle) = handle {
                        // Armed, not started: the effect and the undo
                        // gesture wait for the pointer to move. A new
                        // transform is appended after the clip's other
                        // effects, so that is where it will be.
                        let eff_idx = effect_index.unwrap_or(n_builtin_effects);
                        state.viewer_transform_drag = Some(ViewerTransformDrag {
                            clip_id,
                            effect_index: eff_idx,
                            handle,
                            initial_transform: transform,
                            drag_start_pos: pointer,
                            started: false,
                            added: false,
                        });
                    }
                }
            } else {
                // Hover cursor feedback
                let handle = self.hit_test_handles(
                    pointer, pos_screen, &corners, rot_handle_pos,
                    center_radius, handle_half, rot_radius, hit_radius,
                );
                if let Some(h) = handle {
                    match h {
                        TransformHandle::Position => {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                        }
                        TransformHandle::Rotation => {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                        }
                        TransformHandle::ScaleTopLeft | TransformHandle::ScaleBottomRight => {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
                        }
                        TransformHandle::ScaleTopRight | TransformHandle::ScaleBottomLeft => {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNeSw);
                        }
                    }
                }
            }
        }
    }

    /// The corner pin's four handles: the quadrilateral the picture is
    /// pinned to, drawn over the frame, each corner draggable. Only for
    /// a selected clip that carries a pin; the pin itself is added from
    /// the inspector. The corners are shown as they are at the playhead,
    /// keys included; a drag rewrites the corner and, where that corner
    /// is keyed, keys it at the playhead, as the inspector's rows do.
    /// The drag is bracketed under the key the effect handlers coalesce
    /// on, so it is one undo entry however far the handle went. The
    /// handles are in the frame before the clip's transform, where the
    /// pin runs. As with the transform gizmo, a press is taken only
    /// inside the viewer (a portrait picture's corners cross into the
    /// toolbar), and opens the gesture only once the pointer has moved:
    /// a click on a corner changes nothing and records nothing (#408).
    /// The surface tool (#684 phase 2): four corners to drag onto a flat
    /// surface in the picture (or the whole quad, grabbed inside it), and
    /// Track, or Continue where a track was lost; on a surface tracked
    /// already, Correct here holds it on this frame and solves the track
    /// either side again (#2754). Area to follow drags a
    /// larger quad of the same plane, followed instead (#2754), and Leave
    /// out draws rectangles over parts that are not of it. Esc cancels it,
    /// in the shortcuts, before the clip would be deselected.
    fn draw_surface_tool(&self, ui: &mut egui::Ui, state: &mut AppState, image_rect: egui::Rect, cmds: &mut Vec<AppCommand>) {
        use crate::state::SurfaceEdit;
        let Some(mut tool) = state.surface_tool.clone() else { return };
        let painter = ui.painter().with_clip_rect(ui.clip_rect());
        let to_screen = |q: &[[f32; 2]]| -> Vec<egui::Pos2> { q.iter().map(|c| viewer_norm_to_screen(image_rect, c[0], c[1])).collect() };
        // The quad a drag changes: the corners, or the area followed (the
        // corners until it is given one of its own).
        let held_quad = |t: &crate::state::SurfaceTool| if t.edit == SurfaceEdit::Area { t.track.unwrap_or(t.corners) } else { t.corners };
        let screen = to_screen(&held_quad(&tool));
        let (pointer, pressed, down, released) = ui.input(|i| (i.pointer.interact_pos(), i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.primary_released()));
        let edit = tool.edit;
        let hit = |p: egui::Pos2| (edit != SurfaceEdit::LeaveOut).then(|| screen.iter().position(|s| (p - *s).length() <= 12.0)).flatten();
        let inside = |p: egui::Pos2| edit != SurfaceEdit::LeaveOut && in_screen_polygon(&screen, p);

        // The bar: what to do, what a drag changes, and the two ways out.
        // Laid out first so a press on it is not taken as grabbing the quad.
        let mut bar_rect = egui::Rect::NOTHING;
        let mut run = false;
        let mut correct = false;
        let mut cancel = false;
        egui::Area::new(egui::Id::new("surface_tool_bar"))
            .fixed_pos(image_rect.left_top() + egui::vec2(8.0, 8.0))
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                let frame = egui::Frame::popup(ui.style());
                let r = frame.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let what = match (tool.edit, tool.direction, tool.surface.is_some()) {
                            (SurfaceEdit::Area, ..) => "Drag the area to follow over more of the same flat plane",
                            (SurfaceEdit::LeaveOut, ..) => "Draw over parts that move on their own; click one to put it back",
                            (_, 0, false) => "Click the surface to find its corners, or drag them onto it",
                            (_, 0, true) => "Put the corners right, then correct this frame or track again",
                            _ => "Put the corners on the surface here, then continue",
                        };
                        ui.label(what);
                        ui.separator();
                        ui.selectable_value(&mut tool.edit, SurfaceEdit::Corners, "Corners").on_hover_text("The four corners things are pinned to");
                        ui.selectable_value(&mut tool.edit, SurfaceEdit::Area, "Area to follow")
                            .on_hover_text("A larger part of the same plane with more on it, followed instead of the corners: the wall round a sign");
                        ui.selectable_value(&mut tool.edit, SurfaceEdit::LeaveOut, "Leave out")
                            .on_hover_text("Parts of the area that are not of the plane: a screen playing in a wall, a reflection");
                        if tool.edit == SurfaceEdit::Area && tool.track.is_some() && ui.button("Same as corners").clicked() {
                            tool.track = None;
                        }
                        if tool.edit == SurfaceEdit::LeaveOut && !tool.exclude.is_empty() && ui.button("Clear").clicked() {
                            tool.exclude.clear();
                        }
                        ui.separator();
                        if tool.surface.is_some() && tool.direction == 0 && tool.edit == SurfaceEdit::Corners && ui.button("Correct here")
                            .on_hover_text("Hold the surface here as set, and solve the track again either side as far as the next corrections, keeping the rest")
                            .clicked()
                        {
                            correct = true;
                        }
                        let go = if tool.direction == 0 { if tool.surface.is_some() { "Track again" } else { "Track" } } else { "Continue" };
                        if ui.button(go).on_hover_text("Follow it through the clip from this frame").clicked() {
                            run = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                    });
                });
                bar_rect = r.response.rect;
            });
        if cancel {
            state.surface_tool = None;
            return;
        }
        if let (true, Some(surface)) = (correct, tool.surface) {
            cmds.push(AppCommand::CorrectSurface { clip_id: tool.clip_id, surface, corners: tool.corners, at_secs: None });
            state.surface_tool = None;
            return;
        }
        if run {
            cmds.push(AppCommand::TrackSurface {
                clip_id: tool.clip_id,
                corners: tool.corners,
                at_secs: None,
                name: tool.name.clone(),
                surface: tool.surface,
                direction: tool.direction,
                interactive: true,
                // A surface tracked again keeps the area it is shown with
                // here; set back to its corners, it follows them.
                track: tool.track.or(tool.surface.map(|_| tool.corners)),
                exclude: Some(tool.exclude.clone()),
            });
            state.surface_tool = None;
            return;
        }

        // Grab a corner, or the whole quad from inside it; drag; let go. In
        // Leave out, a drag draws a rectangle.
        if let Some(p) = pointer.filter(|p| !bar_rect.contains(*p)) {
            if pressed && image_rect.expand(16.0).contains(p) {
                if tool.edit == SurfaceEdit::LeaveOut {
                    if image_rect.contains(p) {
                        tool.drag = Some((6, p, tool.corners));
                    }
                } else if let Some(c) = hit(p) {
                    tool.drag = Some((c, p, held_quad(&tool)));
                } else if inside(p) {
                    tool.drag = Some((4, p, held_quad(&tool)));
                } else if image_rect.contains(p) {
                    // Off the quad: a click, to find the surface there.
                    tool.drag = Some((5, p, tool.corners));
                }
            }
            if let Some((which, from, _)) = tool.drag {
                let click = released && (p - from).length() < 3.0;
                // A press let go where it was, not on a corner: find the
                // surface's edges round it.
                if click && (which == 4 || which == 5) && tool.edit == SurfaceEdit::Corners {
                    let (nx, ny) = viewer_screen_to_norm(image_rect, from);
                    cmds.push(AppCommand::SnapSurface { clip_id: tool.clip_id, click: [nx, ny], at_secs: None, then_track: false, name: tool.name.clone(), request_id: None });
                }
                // A rectangle drawn is left out; a click on one puts it back.
                if released && which == 6 {
                    if click {
                        if let Some(i) = tool.exclude.iter().rposition(|e| in_screen_polygon(&to_screen(e), p)) {
                            tool.exclude.remove(i);
                        }
                    } else {
                        let (a, b) = (viewer_screen_to_norm(image_rect, from), viewer_screen_to_norm(image_rect, p));
                        let (x0, x1, y0, y1) = (a.0.min(b.0), a.0.max(b.0), a.1.min(b.1), a.1.max(b.1));
                        tool.exclude.push(vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]);
                    }
                }
            }
            if let Some((which, from, start)) = tool.drag.filter(|d| d.0 < 5) {
                if down {
                    let (nx, ny) = viewer_screen_to_norm(image_rect, p);
                    let (fx, fy) = viewer_screen_to_norm(image_rect, from);
                    let mut q = start;
                    if which < 4 {
                        q[which] = [nx.clamp(-0.5, 1.5), ny.clamp(-0.5, 1.5)];
                    } else {
                        q = start.map(|c| [c[0] + nx - fx, c[1] + ny - fy]);
                    }
                    match tool.edit {
                        SurfaceEdit::Area => tool.track = Some(q),
                        _ => tool.corners = q,
                    }
                }
            }
            let hover = hit(p).is_some() || tool.drag.is_some_and(|d| d.0 < 4);
            let icon = if tool.drag.is_some_and(|d| d.0 < 5) {
                egui::CursorIcon::Grabbing
            } else if hover || inside(p) {
                egui::CursorIcon::Grab
            } else {
                egui::CursorIcon::Crosshair
            };
            ui.ctx().set_cursor_icon(icon);
            // The rectangle being drawn.
            if let Some((6, from, _)) = tool.drag.filter(|_| down) {
                let r = egui::Rect::from_two_pos(from, p);
                painter.rect(r, 0.0, egui::Color32::from_rgba_unmultiplied(255, 90, 90, 40), egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(255, 110, 110)));
            }
        }
        if !down {
            tool.drag = None;
        }
        // Take the pointer from the viewer under it, so a drag here does not
        // also scrub or pan.
        let _ = ui.allocate_rect(image_rect, egui::Sense::click_and_drag());

        // The parts left out, the area followed, then the corners on top;
        // what a drag changes is drawn strong, the rest faint.
        let leave_out = egui::Color32::from_rgb(255, 110, 110);
        for e in &tool.exclude {
            let pts = to_screen(e);
            let strong = tool.edit == SurfaceEdit::LeaveOut;
            painter.add(egui::Shape::convex_polygon(pts.clone(), egui::Color32::from_rgba_unmultiplied(255, 90, 90, if strong { 48 } else { 24 }), egui::Stroke::NONE));
            painter.add(egui::Shape::closed_line(pts, egui::Stroke::new(if strong { 1.5_f32 } else { 1.0 }, leave_out)));
        }
        let area = egui::Color32::from_rgb(255, 190, 70);
        if let Some(t) = tool.track {
            let pts = to_screen(&t);
            painter.add(egui::Shape::dashed_line(&[pts.clone(), vec![pts[0]]].concat(), egui::Stroke::new(1.5_f32, area), 6.0, 4.0));
            if tool.edit == SurfaceEdit::Area {
                for (i, p) in pts.iter().enumerate() {
                    let held = tool.drag.is_some_and(|d| d.0 == i);
                    painter.circle_filled(*p, if held { 7.0 } else { 5.5 }, egui::Color32::WHITE);
                    painter.circle_stroke(*p, if held { 7.0 } else { 5.5 }, egui::Stroke::new(1.5_f32, area));
                }
            }
        }
        let accent = egui::Color32::from_rgb(90, 200, 255);
        let corners = to_screen(&tool.corners);
        let pinning = tool.edit == SurfaceEdit::Corners || (tool.edit == SurfaceEdit::Area && tool.track.is_none());
        painter.add(egui::Shape::convex_polygon(corners.clone(), egui::Color32::from_rgba_unmultiplied(90, 200, 255, 28), egui::Stroke::NONE));
        painter.add(egui::Shape::closed_line(corners.clone(), egui::Stroke::new(1.5_f32, if tool.edit == SurfaceEdit::Area && tool.track.is_none() { area } else { accent })));
        if pinning {
            for (i, p) in corners.iter().enumerate() {
                let held = tool.drag.is_some_and(|d| d.0 == i);
                let ring = if tool.edit == SurfaceEdit::Area { area } else { accent };
                painter.circle_filled(*p, if held { 7.0 } else { 5.5 }, egui::Color32::WHITE);
                painter.circle_stroke(*p, if held { 7.0 } else { 5.5 }, egui::Stroke::new(1.5_f32, ring));
            }
        }
        state.surface_tool = Some(tool);
    }

    /// A file from the media pool dragged over a tracked surface at the
    /// playhead (#684 phase 2): the surface lights up, and let go there the
    /// file is pinned to it, one undo step, its shape kept or stretched as
    /// the shapes suggest and a little in from the edges for a bezel.
    fn handle_surface_drop(&self, ui: &mut egui::Ui, state: &mut AppState, image_rect: egui::Rect) {
        let Some(media_id) = state.dragging_media else { return };
        let Some(pointer) = ui.input(|i| i.pointer.hover_pos()) else { return };
        if !image_rect.contains(pointer) {
            return;
        }
        let at = state.playhead.to_secs_f64();
        let seq = state.sequence();
        let under: Option<(klipp_core::ClipId, klipp_timeline::Surface, [[f32; 2]; 4])> = seq
            .tracks
            .iter()
            .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
            .flat_map(|t| t.clips.iter())
            .filter(|c| c.timeline_in.to_secs_f64() <= at && at < c.timeline_out.to_secs_f64())
            .flat_map(|c| c.surfaces.iter().filter(|s| !s.keys.is_empty()).map(move |s| (c, s)))
            .map(|(c, s)| (c.id, s.clone(), crate::app::surface_on_frame(seq, c, s, at, None)))
            .find(|(_, _, q)| {
                let screen: Vec<egui::Pos2> = q.iter().map(|c| viewer_norm_to_screen(image_rect, c[0], c[1])).collect();
                let side = |a: egui::Pos2, b: egui::Pos2| (b.x - a.x) * (pointer.y - a.y) - (b.y - a.y) * (pointer.x - a.x);
                let sides: Vec<f32> = (0..4).map(|i| side(screen[i], screen[(i + 1) % 4])).collect();
                sides.iter().all(|v| *v >= 0.0) || sides.iter().all(|v| *v <= 0.0)
            });
        let Some((host, surface, quad)) = under else { return };
        let screen: Vec<egui::Pos2> = quad.iter().map(|c| viewer_norm_to_screen(image_rect, c[0], c[1])).collect();
        let painter = ui.painter().with_clip_rect(ui.clip_rect());
        let accent = egui::Color32::from_rgb(90, 200, 255);
        painter.add(egui::Shape::convex_polygon(screen.clone(), egui::Color32::from_rgba_unmultiplied(90, 200, 255, 60), egui::Stroke::new(2.0_f32, accent)));
        painter.text(screen[0] + egui::vec2(0.0, -4.0), egui::Align2::LEFT_BOTTOM, format!("Pin to \u{201c}{}\u{201d}", surface.name), egui::FontId::proportional(12.0), accent);
        // Let go here: pinned, and the drag is over before the timeline
        // can take it as a drop of its own.
        if !ui.input(|i| i.pointer.any_down()) {
            let op = klipp_document::ops::Op::Pin {
                clip_id: klipp_document::ops::ClipRef::Id(host),
                media_id: Some(media_id),
                layer: None,
                surface: Some(surface.id.to_string()),
                from: None,
                to: None,
                fit: None,
                inset: Some(SURFACE_DROP_INSET),
                on_loss: None,
                fade_secs: None,
            };
            state.pending_doc_ops.push(crate::state::DocOpsRequest { label: format!("Pin to {}", surface.name), ops: vec![op], independent: false });
            state.dragging_media = None;
            state.dragging_media_group.clear();
        }
    }

    /// While a surface is tracked in the selected clip: how far it has got,
    /// and a way to stop it.
    fn draw_surface_tracking_badge(&self, ui: &mut egui::Ui, state: &AppState, image_rect: egui::Rect, cmds: &mut Vec<AppCommand>) {
        let Some(clip_id) = state.selected_clip else { return };
        if !state.surface_track_cancel.contains_key(&clip_id) {
            return;
        }
        let pct = (state.tracking_progress.get(&clip_id).copied().unwrap_or(0.0) * 100.0).round();
        egui::Area::new(egui::Id::new("surface_tracking_badge"))
            .fixed_pos(image_rect.left_top() + egui::vec2(8.0, 8.0))
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(format!("Tracking the surface\u{2026} {pct:.0}%"));
                        if ui.small_button("Cancel").clicked() {
                            cmds.push(AppCommand::CancelTrackAnalysis { clip_id });
                        }
                    });
                });
            });
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }

    /// The selected clip's tracked surfaces where they are at the playhead,
    /// or the surface it is pinned to, outlined and named (#684).
    fn draw_surface_outlines(&self, ui: &mut egui::Ui, state: &AppState, image_rect: egui::Rect) {
        let Some(clip_id) = state.selected_clip else { return };
        let seq = state.sequence();
        let fps = seq.fps.as_f64();
        let find = |id: klipp_core::ClipId| seq.tracks.iter().find_map(|t| t.find_clip(id));
        let Some(clip) = find(clip_id) else { return };
        let playhead = state.playhead.to_secs_f64();
        let frame = [seq.resolution.width, seq.resolution.height];
        // On the frame as the renderer puts it: the host's picture fitted,
        // then placed by its own transform.
        let at = |host: &klipp_timeline::Clip, s: &klipp_timeline::Surface| {
            let elapsed = playhead - host.timeline_in.to_secs_f64();
            let on_source = s.corners_shown_at(host.source_secs_at(elapsed, fps));
            let ctx = klipp_compose::eval::eval_context(host, state.playhead, fps);
            let placed = klipp_compose::eval::evaluate_clip_own_transform(host, elapsed.max(0.0), &ctx);
            klipp_timeline::surface::source_quad_in_frame(&on_source, s.source_size, frame, Some(placed))
        };
        let mut quads: Vec<(String, klipp_timeline::surface::Quad)> = Vec::new();
        let inside = playhead >= clip.timeline_in.to_secs_f64() && playhead < clip.timeline_out.to_secs_f64();
        if inside {
            quads.extend(clip.surfaces.iter().map(|s| (s.name.clone(), at(clip, s))));
        }
        if let Some(pin) = clip.surface_pin {
            if let Some((host, s)) = find(pin.host).and_then(|h| h.surfaces.iter().find(|s| s.id == pin.surface).map(|s| (h, s))) {
                quads.push((s.name.clone(), at(host, s)));
            }
        }
        if quads.is_empty() {
            return;
        }
        let painter = ui.painter().with_clip_rect(ui.clip_rect());
        let colour = egui::Color32::from_rgb(90, 200, 255);
        for (name, q) in quads {
            let screen: Vec<egui::Pos2> = q.iter().map(|c| viewer_norm_to_screen(image_rect, c[0], c[1])).collect();
            painter.add(egui::Shape::closed_line(screen.clone(), egui::Stroke::new(1.5_f32, colour)));
            painter.text(screen[0] + egui::vec2(0.0, -4.0), egui::Align2::LEFT_BOTTOM, name, egui::FontId::proportional(11.0), colour);
        }
    }

    fn draw_corner_pin_handles(
        &self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        image_rect: egui::Rect,
        viewer_rect: egui::Rect,
        cmds: &mut Vec<AppCommand>,
    ) {
        use klipp_effects::corner_pin::{corners, with_corners, PARAMS};
        let Some(clip_id) = state.selected_clip else { return };
        let Some(clip) = state.sequence().tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == clip_id) else { return };
        // A pin on a surface goes where the surface does: its outline is
        // drawn instead, and its own corners are not for dragging.
        if clip.surface_pin.is_some() {
            return;
        }
        let Some(effect_index) = clip.builtin_effects().iter().position(|e| matches!(e, klipp_effects::BuiltinEffect::CornerPin { .. })) else { return };
        let Some(base) = corners(&clip.builtin_effects()[effect_index]) else { return };
        let clip_time = (state.playhead.to_secs_f64() - clip.timeline_in.to_secs_f64()).max(0.0);
        // The pin is its node, its corners its element keys, edited by
        // path (M3 step 13).
        let addr = clip.effect_node_at(false, effect_index).map(|n| klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: clip_id.0 }, [n]));
        let node_keys: Vec<Vec<klipp_core::Keyframe>> = PARAMS.iter().flatten().map(|name| addr.as_ref().and_then(|a| clip.node_key_state(a, &(*name).into())).map(|s| s.keys).unwrap_or_default()).collect();
        let keys_of = |name: &str| -> Option<Vec<klipp_core::Keyframe>> {
            let at = PARAMS.iter().flatten().position(|n| *n == name)?;
            Some(node_keys[at].clone()).filter(|k| !k.is_empty())
        };
        let mut shown = base;
        for (corner, names) in shown.iter_mut().zip(PARAMS) {
            for axis in 0..2 {
                if let Some(keys) = keys_of(names[axis]) {
                    corner[axis] = klipp_core::keyframe::evaluate(&keys, clip_time, corner[axis]);
                }
            }
        }
        let old_effect = clip.builtin_effects()[effect_index].clone();
        let keyed: [[bool; 2]; 4] = std::array::from_fn(|i| [keys_of(PARAMS[i][0]).is_some(), keys_of(PARAMS[i][1]).is_some()]);

        let screen: [egui::Pos2; 4] = std::array::from_fn(|i| viewer_norm_to_screen(image_rect, shown[i][0], shown[i][1]));
        let painter = ui.painter().with_clip_rect(ui.clip_rect());
        // White on black, as the transform gizmo's handles are: visible
        // over any picture, and not mistaken for the picture.
        let (fill, edge) = (egui::Color32::WHITE, egui::Color32::BLACK);
        let hit_radius = 10.0;
        let pointer_pos = ui.input(|i| i.pointer.interact_pos());
        let hit = |p: egui::Pos2| screen.iter().position(|s| (p - *s).length() <= hit_radius);
        let hot = state.viewer_corner_pin_drag.map(|d| d.corner).or_else(|| pointer_pos.and_then(hit));

        painter.add(egui::Shape::closed_line(screen.to_vec(), egui::Stroke::new(1.0_f32, fill)));
        for (i, s) in screen.iter().enumerate() {
            let half = if hot == Some(i) { 5.0 } else { 4.0 };
            let rect = egui::Rect::from_center_size(*s, egui::vec2(half * 2.0, half * 2.0));
            painter.rect_filled(rect, 0.0, fill);
            painter.rect_stroke(rect, 0.0, egui::Stroke::new(1.0_f32, edge));
        }

        let drag_key = format!("{:?}:builtin:{}", clip_id, effect_index);
        let pressed = ui.input(|i| i.pointer.primary_pressed());
        let down = ui.input(|i| i.pointer.primary_down());
        let released = ui.input(|i| i.pointer.primary_released());
        let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
        if let Some(mut drag) = state.viewer_corner_pin_drag.take() {
            if drag.clip_id != clip_id || drag.effect_index != effect_index {
                // The clip or the effect changed under the drag: take
                // the gesture back rather than leave it open.
                if drag.started {
                    cmds.push(AppCommand::AbandonEffectDrag { clip_id: drag.clip_id, effect_index: drag.effect_index, added: false });
                }
                return;
            }
            let step = if escape {
                GizmoStep::Abandon
            } else {
                gizmo_step(drag.started, drag.drag_start_pos, pointer_pos, down, released)
            };
            // The drag by path: one parameter edit at a time on the pin's
            // node, in a drag over its parameters together (an empty key).
            let path_key = |corner: usize, axis: usize| klipp_graph_format::ParamKey::from(PARAMS[corner][axis]);
            let whole = klipp_graph_format::ParamKey::from("");
            let path = addr.clone();
            if matches!(step, GizmoStep::Start | GizmoStep::End) && !drag.started {
                match &path {
                    Some(a) => cmds.push(AppCommand::BeginParamEditAt { addr: a.clone(), key: whole.clone() }),
                    None => cmds.push(AppCommand::BeginEffectDrag { key: drag_key.clone(), clip_id, effect_index, old_effect }),
                }
                drag.started = true;
            }
            match step {
                GizmoStep::Nothing | GizmoStep::Start => {
                    state.viewer_corner_pin_drag = Some(drag);
                    ui.ctx().request_repaint();
                }
                GizmoStep::Update | GizmoStep::End => {
                    if let Some(p) = pointer_pos {
                        let (nx, ny) = viewer_screen_to_norm(image_rect, p);
                        let mut next = drag.corners;
                        next[drag.corner] = [nx.clamp(-2.0, 3.0), ny.clamp(-2.0, 3.0)];
                        match &path {
                            Some(a) => {
                                for axis in 0..2 {
                                    let value = next[drag.corner][axis];
                                    cmds.push(AppCommand::SetParamAt { addr: a.clone(), key: path_key(drag.corner, axis), value: klipp_graph_format::ParamValue::Float(value) });
                                    if keyed[drag.corner][axis] {
                                        // A key already there keeps its time
                                        // and curve shape, as the old route's
                                        // upsert kept them.
                                        let key = klipp_core::keyframe::upserted(&node_keys[drag.corner * 2 + axis], clip_time, value);
                                        if let Ok(keyframe) = serde_json::to_value(key) {
                                            cmds.push(AppCommand::AddKeyAt { addr: a.clone(), key: path_key(drag.corner, axis), keyframe });
                                        }
                                    }
                                }
                            }
                            None => {
                                cmds.push(AppCommand::UpdateBuiltinEffect { clip_id, index: effect_index, effect: with_corners(next) });
                                for axis in 0..2 {
                                    if keyed[drag.corner][axis] {
                                        cmds.push(AppCommand::AddEffectKeyframe {
                                            clip_id,
                                            effect_index,
                                            param_name: PARAMS[drag.corner][axis].to_string(),
                                            time_secs: clip_time,
                                            value: next[drag.corner][axis],
                                        });
                                    }
                                }
                            }
                        }
                        drag.corners = next;
                    }
                    if step == GizmoStep::Update {
                        state.viewer_corner_pin_drag = Some(drag);
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                        ui.ctx().request_repaint();
                    } else {
                        match &path {
                            Some(a) => cmds.push(AppCommand::EndParamEditAt { addr: a.clone(), key: whole.clone() }),
                            None => cmds.push(AppCommand::EndEffectDrag {
                                key: drag_key,
                                clip_id,
                                effect_index,
                                new_effect: with_corners(drag.corners),
                            }),
                        }
                    }
                }
                GizmoStep::Abandon => {
                    if drag.started {
                        match &path {
                            // The pin as the drag found it, its values and
                            // keys, and no step recorded.
                            Some(a) => cmds.push(AppCommand::AbandonParamEditAt { addr: a.clone(), key: whole.clone() }),
                            None => cmds.push(AppCommand::AbandonEffectDrag { clip_id, effect_index, added: false }),
                        }
                    }
                }
            }
        } else if let Some(p) = pointer_pos {
            if pressed && state.viewer_transform_drag.is_none() {
                if viewer_rect.contains(p) {
                    if let Some(corner) = hit(p) {
                        state.viewer_corner_pin_drag = Some(crate::state::ViewerCornerPinDrag {
                            clip_id,
                            effect_index,
                            corner,
                            corners: shown,
                            drag_start_pos: p,
                            started: false,
                        });
                    }
                }
            } else if hit(p).is_some() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
            }
        }
    }

    /// The selected clip's reflection's waterline (#2707): a dashed line
    /// across the picture, dragged up and down by any point on it or the
    /// handle at its end. Edited on the effect's node by path, a key at
    /// the playhead kept when the line is keyed.
    fn draw_waterline(
        &self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        image_rect: egui::Rect,
        viewer_rect: egui::Rect,
        cmds: &mut Vec<AppCommand>,
    ) {
        let Some(clip_id) = state.selected_clip else { return };
        let Some(clip) = state.sequence().tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == clip_id) else { return };
        // The reflection as the clip's picture holds it: its node, edited
        // by path.
        let Ok(effects) = clip.picture_effects() else { return };
        let Some((node, waterline)) = effects.iter().find_map(|e| match e.effect {
            klipp_effects::BuiltinEffect::Reflection { waterline, .. } => Some((e.node, waterline)),
            _ => None,
        }) else {
            return;
        };
        let clip_time = (state.playhead.to_secs_f64() - clip.timeline_in.to_secs_f64()).max(0.0);
        let addr = klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: clip_id.0 }, [node]);
        let keys = clip.node_key_state(&addr, &"waterline".into()).map(|s| s.keys).unwrap_or_default();
        let shown = if keys.is_empty() { waterline } else { klipp_core::keyframe::evaluate(&keys, clip_time, waterline) };

        let y = viewer_norm_to_screen(image_rect, 0.0, shown).y;
        let (x0, x1) = (image_rect.left(), image_rect.right());
        let painter = ui.painter().with_clip_rect(ui.clip_rect());
        let pointer_pos = ui.input(|i| i.pointer.interact_pos());
        let near = |p: egui::Pos2| (p.y - y).abs() <= 6.0 && p.x >= x0 - 8.0 && p.x <= x1 + 8.0;
        let hot = state.viewer_waterline_drag.is_some() || pointer_pos.is_some_and(near);
        // White dashes on black, visible over any water.
        let width: f32 = if hot { 2.0 } else { 1.0 };
        painter.add(egui::Shape::dashed_line(&[egui::pos2(x0, y), egui::pos2(x1, y)], egui::Stroke::new(width + 1.0, egui::Color32::BLACK), 8.0, 5.0));
        painter.add(egui::Shape::dashed_line(&[egui::pos2(x0, y), egui::pos2(x1, y)], egui::Stroke::new(width, egui::Color32::WHITE), 8.0, 5.0));
        let handle = egui::Rect::from_center_size(egui::pos2(x1 - 10.0, y), egui::vec2(10.0, 10.0));
        painter.rect_filled(handle, 0.0, egui::Color32::WHITE);
        painter.rect_stroke(handle, 0.0, egui::Stroke::new(1.0_f32, egui::Color32::BLACK));
        painter.text(egui::pos2(x0 + 6.0, y - 4.0), egui::Align2::LEFT_BOTTOM, "Waterline", egui::FontId::proportional(11.0), egui::Color32::WHITE);

        let key = klipp_graph_format::ParamKey::from("waterline");
        let down = ui.input(|i| i.pointer.primary_down());
        let released = ui.input(|i| i.pointer.primary_released());
        let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
        if let Some(mut drag) = state.viewer_waterline_drag.take() {
            if drag.clip_id != clip_id || drag.node != node {
                // The clip or the effect changed under the drag: take the
                // gesture back rather than leave it open.
                if drag.started {
                    let was = klipp_graph_format::path::NodePath::new(klipp_graph_format::path::Root::Clip { clip: drag.clip_id.0 }, [drag.node]);
                    cmds.push(AppCommand::AbandonParamEditAt { addr: was, key: key.clone() });
                }
                return;
            }
            let step = if escape { GizmoStep::Abandon } else { gizmo_step(drag.started, drag.drag_start_pos, pointer_pos, down, released) };
            if matches!(step, GizmoStep::Start | GizmoStep::End) && !drag.started {
                cmds.push(AppCommand::BeginParamEditAt { addr: addr.clone(), key: key.clone() });
                drag.started = true;
            }
            match step {
                GizmoStep::Nothing | GizmoStep::Start => {
                    state.viewer_waterline_drag = Some(drag);
                    ui.ctx().request_repaint();
                }
                GizmoStep::Update | GizmoStep::End => {
                    let value = pointer_pos.map_or(shown, |p| viewer_screen_to_norm(image_rect, p).1.clamp(0.0, 1.0));
                    cmds.push(AppCommand::SetParamAt { addr: addr.clone(), key: key.clone(), value: klipp_graph_format::ParamValue::Float(value) });
                    // A key already there keeps its time and curve shape.
                    if !keys.is_empty() {
                        if let Ok(keyframe) = serde_json::to_value(klipp_core::keyframe::upserted(&keys, clip_time, value)) {
                            cmds.push(AppCommand::AddKeyAt { addr: addr.clone(), key: key.clone(), keyframe });
                        }
                    }
                    if step == GizmoStep::Update {
                        state.viewer_waterline_drag = Some(drag);
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                        ui.ctx().request_repaint();
                    } else {
                        cmds.push(AppCommand::EndParamEditAt { addr: addr.clone(), key: key.clone() });
                    }
                }
                GizmoStep::Abandon => {
                    if drag.started {
                        cmds.push(AppCommand::AbandonParamEditAt { addr: addr.clone(), key: key.clone() });
                    }
                }
            }
        } else if let Some(p) = pointer_pos {
            let pressed = ui.input(|i| i.pointer.primary_pressed());
            if pressed && state.viewer_transform_drag.is_none() && state.viewer_corner_pin_drag.is_none() && viewer_rect.contains(p) && near(p) {
                state.viewer_waterline_drag = Some(crate::state::ViewerWaterlineDrag { clip_id, node, drag_start_pos: p, started: false });
            } else if near(p) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
            }
        }
    }

    /// Hit-test transform handles and return which one the pointer is over, if any.
    #[allow(clippy::too_many_arguments)]
    fn hit_test_handles(
        &self,
        pointer: egui::Pos2,
        center: egui::Pos2,
        corners: &[egui::Pos2; 4],
        rot_handle: egui::Pos2,
        center_radius: f32,
        handle_half: f32,
        rot_radius: f32,
        hit_radius: f32,
    ) -> Option<TransformHandle> {
        // Check rotation handle first (smallest, on top)
        if (pointer - rot_handle).length() <= rot_radius + hit_radius {
            return Some(TransformHandle::Rotation);
        }
        // Corner handles (scale)
        let corner_handles = [
            TransformHandle::ScaleTopLeft,
            TransformHandle::ScaleTopRight,
            TransformHandle::ScaleBottomRight,
            TransformHandle::ScaleBottomLeft,
        ];
        for (i, corner) in corners.iter().enumerate() {
            if (pointer - *corner).length() <= handle_half + hit_radius {
                return Some(corner_handles[i]);
            }
        }
        // Center handle (position)
        if (pointer - center).length() <= center_radius + hit_radius {
            return Some(TransformHandle::Position);
        }
        None
    }

    /// Compute the new transform parameters based on the current drag state.
    #[allow(clippy::too_many_arguments)]
    fn compute_dragged_transform(
        &self,
        drag: &ViewerTransformDrag,
        pointer_pos: Option<egui::Pos2>,
        scale_to_screen_x: f32,
        scale_to_screen_y: f32,
        center_screen: egui::Pos2,
        shift_held: bool,
    ) -> TransformParams {
        let pointer = match pointer_pos {
            Some(p) => p,
            None => return drag.initial_transform,
        };

        let initial = &drag.initial_transform;

        // The anchor/pivot point in screen space (based on initial transform)
        let initial_pos_screen = egui::pos2(
            center_screen.x + initial.position_x * scale_to_screen_x,
            center_screen.y + initial.position_y * scale_to_screen_y,
        );

        match drag.handle {
            TransformHandle::Position => {
                let delta_screen_x = pointer.x - drag.drag_start_pos.x;
                let delta_screen_y = pointer.y - drag.drag_start_pos.y;
                let delta_frame_x = delta_screen_x / scale_to_screen_x;
                let delta_frame_y = delta_screen_y / scale_to_screen_y;
                TransformParams {
                    position_x: initial.position_x + delta_frame_x,
                    position_y: initial.position_y + delta_frame_y,
                    ..*initial
                }
            }
            TransformHandle::ScaleTopLeft
            | TransformHandle::ScaleTopRight
            | TransformHandle::ScaleBottomLeft
            | TransformHandle::ScaleBottomRight => {
                if shift_held {
                    // Non-uniform scale: X and Y independently based on drag direction
                    let initial_dx = (drag.drag_start_pos.x - initial_pos_screen.x).abs();
                    let initial_dy = (drag.drag_start_pos.y - initial_pos_screen.y).abs();
                    let current_dx = (pointer.x - initial_pos_screen.x).abs();
                    let current_dy = (pointer.y - initial_pos_screen.y).abs();
                    let scale_x = if initial_dx > 1.0 { current_dx / initial_dx } else { 1.0 };
                    let scale_y = if initial_dy > 1.0 { current_dy / initial_dy } else { 1.0 };
                    TransformParams {
                        scale_x: (initial.scale_x * scale_x).max(0.01),
                        scale_y: (initial.scale_y * scale_y).max(0.01),
                        ..*initial
                    }
                } else {
                    // Uniform scale: distance from anchor point
                    let initial_dist = (drag.drag_start_pos - initial_pos_screen).length().max(1.0);
                    let current_dist = (pointer - initial_pos_screen).length();
                    let scale_factor = current_dist / initial_dist;
                    TransformParams {
                        scale_x: (initial.scale_x * scale_factor).max(0.01),
                        scale_y: (initial.scale_y * scale_factor).max(0.01),
                        ..*initial
                    }
                }
            }
            TransformHandle::Rotation => {
                let initial_angle = (drag.drag_start_pos.y - initial_pos_screen.y)
                    .atan2(drag.drag_start_pos.x - initial_pos_screen.x);
                let current_angle = (pointer.y - initial_pos_screen.y)
                    .atan2(pointer.x - initial_pos_screen.x);
                let delta_degrees = (current_angle - initial_angle).to_degrees();
                TransformParams {
                    rotation: initial.rotation + delta_degrees,
                    ..*initial
                }
            }
        }
    }

    /// Mouse-driven zoom + pan input for a viewer's video area.
    ///
    /// * **Scroll wheel** zooms in/out by a 1.1× step per "tick",
    ///   anchored at the cursor so the image pixel under the cursor
    ///   stays under the cursor. Scrolling out past the fit zoom
    ///   snaps back to Fit and resets the pan so the user can return
    ///   to the default view without manual recentering.
    /// * **Middle-mouse drag** pans when the viewer is zoomed in.
    ///   Disabled in Fit mode (there's nothing to pan).
    ///
    /// Call this once per frame for each viewer's video rect, BEFORE
    /// any other interactive widgets allocate inside the same rect —
    /// it senses only middle-button drag and scroll, so click-to-focus
    /// / transform handles / etc. that consume the left button keep
    /// working unchanged.
    pub fn handle_viewer_zoom_pan_input(
        &self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        video_rect: egui::Rect,
        tex_size: egui::Vec2,
        viewer_kind: ViewerMode,
    ) {
        if tex_size.x <= 0.0 || tex_size.y <= 0.0 { return; }
        if video_rect.width() <= 0.0 || video_rect.height() <= 0.0 { return; }

        const ZOOM_MIN: f32 = 0.05;
        const ZOOM_MAX: f32 = 16.0;
        const SCROLL_FACTOR_PER_LINE: f32 = 1.10;

        // `Sense::hover()` claims hover priority for scroll routing
        // without consuming LMB clicks — that way the click-to-focus
        // helper allocated earlier, on-canvas transform handles, the
        // title-object editor, etc. all keep their normal LMB behavior.
        // Middle-mouse pan is driven from raw input state below since
        // `Sense::drag()` would also block LMB-drag gestures on widgets
        // painted into the same rect.
        let id = ui.id().with(match viewer_kind {
            ViewerMode::Source => "source_viewer_zoom_pan",
            ViewerMode::Program => "program_viewer_zoom_pan",
        });
        let resp = ui.interact(video_rect, id, egui::Sense::hover());
        if !resp.hovered() { return; }

        let (cursor_opt, middle_held, middle_delta, scroll_y) = ui.input(|i| {
            (
                i.pointer.hover_pos(),
                i.pointer.button_down(egui::PointerButton::Middle),
                i.pointer.delta(),
                i.smooth_scroll_delta.y,
            )
        });

        let view = state.view_for_mut(viewer_kind);

        // Middle-mouse drag → pan. Only meaningful when zoomed in.
        if view.zoom > 0.0 && middle_held && middle_delta != egui::Vec2::ZERO {
            view.pan += middle_delta;
        }

        if scroll_y.abs() < f32::EPSILON { return; }
        let cursor = match cursor_opt {
            Some(p) => p,
            None => return,
        };

        // The "fit" zoom is the effective scale of the image when in
        // Fit mode. Computed once so scroll-out-to-Fit can snap cleanly.
        let fit_zoom = (video_rect.width() / tex_size.x)
            .min(video_rect.height() / tex_size.y);

        let old_zoom_effective = if view.zoom <= 0.0 { fit_zoom } else { view.zoom };
        // Smooth scrolling reports a continuous delta — we map it onto
        // 1.1× per ~50 px of wheel travel so a single notch on a mouse
        // wheel matches the Resolve / Premiere zoom-step intuition.
        let factor = SCROLL_FACTOR_PER_LINE.powf(scroll_y / 50.0);
        let new_zoom_raw = (old_zoom_effective * factor).clamp(ZOOM_MIN, ZOOM_MAX);

        // Snap-to-Fit: scrolling out below the fit threshold collapses
        // to Fit mode with pan reset. The 0.5 % slop avoids float churn
        // where we'd otherwise tip in and out of Fit on rounding noise.
        if new_zoom_raw <= fit_zoom * 1.005 {
            view.zoom = 0.0;
            view.pan = egui::Vec2::ZERO;
            return;
        }

        // Cursor-anchored zoom: the image pixel under the cursor stays
        // under the cursor. Derived from
        //     pixel_norm = (cursor - image.center)/image.size + 0.5
        //     image.center = viewer.center + pan
        //   ⇒ pan_new = c - (c - pan_old) * (new/old)   where c = cursor - viewer.center
        let c = cursor - video_rect.center();
        let pan_old = if view.zoom <= 0.0 { egui::Vec2::ZERO } else { view.pan };
        let scale = new_zoom_raw / old_zoom_effective;
        let pan_new = c - (c - pan_old) * scale;

        view.zoom = new_zoom_raw;
        view.pan = pan_new;
    }

    /// Floating "viewer options" button anchored over the top-right
    /// corner of the video area, with a popup menu containing the
    /// controls that don't fit in the responsive playbar (volume slider,
    /// volume %, etc.) plus viewer-wide power-user toggles (zoom presets,
    /// safe-area / grid / zebra / false-color / focus-peaking overlays,
    /// match-frame, and — on the Program side — extract-frame and
    /// transform-controls).
    ///
    /// The trigger fades in only when the pointer is inside `video_rect`
    /// or the popup is already open, so it doesn't obscure the picture
    /// during normal viewing. Resolve and Kdenlive use the same pattern.
    pub fn draw_viewer_overflow_menu(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        video_rect: egui::Rect,
        viewer_kind: ViewerMode,
        kb: &crate::keybindings::KeyBindings,
        cmds: &mut Vec<AppCommand>,
    ) {
        const BTN_SIZE: f32 = 24.0;
        const INSET: f32 = 10.0;

        let trigger_rect = egui::Rect::from_min_size(
            egui::pos2(
                video_rect.max.x - INSET - BTN_SIZE,
                video_rect.min.y + INSET,
            ),
            egui::vec2(BTN_SIZE, BTN_SIZE),
        );

        let popup_id = ui.make_persistent_id(match viewer_kind {
            ViewerMode::Source => "viewer_overflow_menu_source",
            ViewerMode::Program => "viewer_overflow_menu_program",
        });

        let popup_open = ui.memory(|m| m.is_popup_open(popup_id));
        let trigger_visible = popup_open || ui.rect_contains_pointer(video_rect);

        let resp = ui.allocate_rect(trigger_rect, egui::Sense::click());

        if trigger_visible {
            let hovered = resp.hovered();
            let bg_alpha = if hovered { 230 } else { 160 };
            let bg = egui::Color32::from_rgba_unmultiplied(
                theme.bg_panel.r(),
                theme.bg_panel.g(),
                theme.bg_panel.b(),
                bg_alpha,
            );
            let painter = ui.painter();
            painter.rect_filled(trigger_rect, BTN_SIZE * 0.25, bg);
            let stroke_alpha = if hovered { 200 } else { 90 };
            let stroke_col = egui::Color32::from_rgba_unmultiplied(
                theme.text_secondary.r(),
                theme.text_secondary.g(),
                theme.text_secondary.b(),
                stroke_alpha,
            );
            painter.rect_stroke(
                trigger_rect,
                BTN_SIZE * 0.25,
                egui::Stroke::new(1.0_f32, stroke_col),
            );

            // Three vertical dots (kebab).
            let cx = trigger_rect.center().x;
            let cy = trigger_rect.center().y;
            let dot_color = if hovered { theme.text_primary } else { theme.text_secondary };
            let dot_r = 1.6;
            let dot_gap = 4.5;
            painter.circle_filled(egui::pos2(cx, cy - dot_gap), dot_r, dot_color);
            painter.circle_filled(egui::pos2(cx, cy), dot_r, dot_color);
            painter.circle_filled(egui::pos2(cx, cy + dot_gap), dot_r, dot_color);
        }

        let resp = resp.on_hover_text("Viewer options");
        if resp.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup_id));
        }

        egui::popup::popup_below_widget(
            ui,
            popup_id,
            &resp,
            egui::PopupCloseBehavior::CloseOnClickOutside,
            |ui| {
                ui.set_min_width(220.0);
                Self::draw_viewer_overflow_menu_content(ui, state, theme, viewer_kind, kb, cmds);
            },
        );

        // A right-click on the picture opens the same menu at the pointer
        // (#2675). Read from the input, not a widget over the picture, so
        // the viewer's own clicks and drags stay where they are; a
        // right-click the AI tools' prompts take, or one over anything
        // drawn above the viewer, is left alone.
        let at_pointer = popup_id.with("at_pointer");
        let prompts_take_it = state.ai_tools_state.panel_visible
            && state.ai_tools_state.active_task.is_some()
            && state.eyedropper_mode == crate::state::EyedropperMode::None;
        let right_click = ui
            .input(|i| if i.pointer.secondary_released() { i.pointer.interact_pos() } else { None })
            .filter(|p| video_rect.contains(*p) && !trigger_rect.contains(*p))
            .filter(|p| ui.ctx().layer_id_at(*p) == Some(ui.layer_id()));
        let opened_now = right_click.is_some() && !prompts_take_it;
        if let (Some(p), true) = (right_click, opened_now) {
            ui.memory_mut(|m| {
                m.data.insert_temp(at_pointer, p);
                m.open_popup(at_pointer);
            });
        }
        if ui.memory(|m| m.is_popup_open(at_pointer)) {
            let at = ui.memory(|m| m.data.get_temp::<egui::Pos2>(at_pointer)).unwrap_or(video_rect.center());
            let menu = egui::Area::new(at_pointer)
                .order(egui::Order::Foreground)
                .fixed_pos(at)
                .constrain(true)
                .show(ui.ctx(), |ui| {
                    egui::Frame::menu(ui.style()).show(ui, |ui| {
                        ui.set_min_width(220.0);
                        Self::draw_viewer_overflow_menu_content(ui, state, theme, viewer_kind, kb, cmds);
                    });
                });
            let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
            if !opened_now && (escape || menu.response.clicked_elsewhere()) {
                ui.memory_mut(|m| m.close_popup());
            }
        }
    }

    fn draw_viewer_overflow_menu_content(
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        viewer_kind: ViewerMode,
        kb: &crate::keybindings::KeyBindings,
        cmds: &mut Vec<AppCommand>,
    ) {
        let section_label = |ui: &mut egui::Ui, text: &str| {
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(text)
                    .size(10.0)
                    .color(theme.text_dim),
            );
        };

        // ── Zoom ──
        //
        // Continuous + / − stepping (× 1.25 per click, clamped to a
        // sensible 5 % – 1600 % range), with a "Fit" toggle next to it
        // for the letterbox-to-viewer default. Zoom of `0.0` is the
        // sentinel for fit mode — anything else is a native-pixel
        // multiplier consumed by the source / program render path.
        const ZOOM_MIN: f32 = 0.05;
        const ZOOM_MAX: f32 = 16.0;
        const ZOOM_STEP: f32 = 1.25;
        section_label(ui, "ZOOM");
        ui.horizontal(|ui| {
            let view = state.view_for(viewer_kind);
            let in_fit = view.zoom <= 0.0;
            let mut fit_btn = egui::Button::new(
                egui::RichText::new("Fit").size(11.0),
            )
            .min_size(egui::vec2(40.0, 22.0));
            if in_fit {
                fit_btn = fit_btn.fill(theme.accent_dim);
            }
            if ui.add(fit_btn).clicked() {
                let v = state.view_for_mut(viewer_kind);
                v.zoom = 0.0;
                v.pan = egui::Vec2::ZERO;
            }

            let minus = egui::Button::new(egui::RichText::new("−").size(13.0))
                .min_size(egui::vec2(22.0, 22.0));
            if ui.add(minus).on_hover_text("Zoom out").clicked() {
                let current = if view.zoom <= 0.0 { 1.0 } else { view.zoom };
                state.view_for_mut(viewer_kind).zoom = (current / ZOOM_STEP).max(ZOOM_MIN);
            }

            let pct_label = if in_fit {
                "Fit".to_string()
            } else {
                format!("{}%", (view.zoom * 100.0).round() as i32)
            };
            ui.add_sized(
                egui::vec2(56.0, 22.0),
                egui::Label::new(
                    egui::RichText::new(pct_label)
                        .monospace()
                        .size(12.0)
                        .color(theme.text_primary),
                ),
            );

            let plus = egui::Button::new(egui::RichText::new("+").size(13.0))
                .min_size(egui::vec2(22.0, 22.0));
            if ui.add(plus).on_hover_text("Zoom in").clicked() {
                let current = if view.zoom <= 0.0 { 1.0 } else { view.zoom };
                state.view_for_mut(viewer_kind).zoom = (current * ZOOM_STEP).min(ZOOM_MAX);
            }
        });

        ui.separator();

        // ── Volume ── (always reachable here so power-users have a
        // home for the slider even when the chrome row drops it.)
        section_label(ui, "VOLUME");
        let mut vol = state.master_volume;
        let vol_label = format!("{}%", (vol * 100.0) as i32);
        let slider = egui::Slider::new(&mut vol, 0.0..=1.3).text(vol_label);
        if ui.add(slider).changed() {
            cmds.push(AppCommand::SetMasterVolume(vol));
        }
        // The rows below in the menus' one row layout (#2632): toggles
        // with their check in the slot, keys in a column.
        crate::widgets::menu::menu_body(ui, |ui| {
            use crate::widgets::menu::{menu_heading, menu_item, menu_toggle, menu_toggle_value};
            if menu_toggle(ui, "Mute Master Output", "", state.master_muted).clicked() {
                cmds.push(AppCommand::SetMasterMute(!state.master_muted));
            }

            ui.separator();

            // ── Overlays ──
            menu_heading(ui, "OVERLAYS");
            // Shortcuts read from the bindings, so a rebind shows here.
            let key = |action: &str| kb.display_for(action);
            menu_toggle_value(ui, &mut state.viewer_overlays.safe_areas, "Safe Areas", &key("toggle_safe_areas"));
            menu_toggle_value(ui, &mut state.viewer_overlays.grid, "Grid", &key("toggle_grid_overlay"));
            menu_toggle_value(ui, &mut state.viewer_overlays.zebras, "Zebras", &key("toggle_zebra"));
            menu_toggle_value(ui, &mut state.viewer_overlays.false_color, "False Colour", &key("toggle_false_color"));
            menu_toggle_value(ui, &mut state.viewer_overlays.focus_peaking, "Focus Peaking", "Ctrl+P");
            menu_toggle_value(ui, &mut state.viewer_overlays.checkerboard, "Checkerboard", "");

            ui.separator();

            // ── Layout ──
            menu_heading(ui, "LAYOUT");
            if menu_toggle(ui, "Dual Viewers", "", state.viewers_side_by_side()).clicked() {
                state.toggle_viewer_layout();
            }
            if menu_toggle(ui, "Scopes", "Alt+V", state.scopes_shown()).clicked() {
                state.toggle_scopes();
            }

            ui.separator();

            // ── Actions ──
            menu_heading(ui, "ACTIONS");
            match viewer_kind {
                ViewerMode::Source => {
                    let enabled = state.source_media_id.is_some();
                    if ui.add_enabled_ui(enabled, |ui| menu_item(ui, "Match Frame \u{2192} Timeline", "Shift+F")).inner.clicked() {
                        cmds.push(AppCommand::MatchFrameSourceToProgram);
                    }
                }
                ViewerMode::Program => {
                    if menu_item(ui, "Match Frame \u{2190} Source", "F").clicked() {
                        cmds.push(AppCommand::MatchFrameProgramToSource);
                    }
                    if menu_item(ui, "Extract Frame to Bin", "Ctrl+Alt+B").clicked() {
                        cmds.push(AppCommand::ExtractFrameToBin);
                    }
                    if menu_item(ui, "Extract Frame to File\u{2026}", "Ctrl+Alt+S").clicked() {
                        cmds.push(AppCommand::FrameGrab);
                    }
                    if menu_toggle(ui, "Transform Controls", "", state.show_transform_controls).clicked() {
                        state.show_transform_controls = !state.show_transform_controls;
                    }
                }
            }
        });
    }

    pub fn show_source_viewer(
        &mut self,
        ui: &mut egui::Ui,
        state: &AppState,
        theme: &KlippTheme,
        viewer_rect: egui::Rect,
        cmds: &mut Vec<AppCommand>,
    ) {
        // Resolve which texture to paint into the source video rect:
        //   1. The browser/timeline hover-skim texture, when active.
        //      Hover is a transient overlay and must win so the user can
        //      preview any clip without losing what's loaded.
        //   2. The decoded first-frame of the committed source clip,
        //      gated on its MediaId matching `source_media_id` so a
        //      stale frame from a previous load never flashes.
        // Falling all the way through paints the metadata card.
        let committed_tex = state
            .source_video_texture
            .as_ref()
            .and_then(|(mid, tex)| (Some(*mid) == state.source_media_id).then_some(tex));
        let live_tex = state.source_live_texture.as_ref();
        // Live texture (engine-decoded) always wins over the cold-
        // start thumbnail. Previously we deferred to the thumbnail
        // when paused because the engine seek was 500 ms-per-frame
        // and the thumbnail decoder was the only thing fast enough
        // to follow a drag — that's no longer true: seek_source
        // now dispatches the seek without blocking, and the per-
        // tick paused-drain pumps the resulting frame into
        // `source_live_texture` within ~30 ms. Showing the stale
        // thumbnail during scrub was why "frames don't follow the
        // progressbar." Browser skim hover overlay still wins on
        // top of everything.
        // No branch on playback state: as the note above says, the live
        // texture always wins now. The `if playing { .. } else { .. }` this
        // replaces had identical arms — a leftover from when paused
        // deferred to the thumbnail, and misleading to read as though the
        // playback state still mattered here.
        let playback_tex = live_tex.or(committed_tex);
        let active_tex = state.browser_skim_texture.as_ref().or(playback_tex);
        if let Some(tex) = active_tex {
            let tex_size = tex.size_vec2();
            if tex_size.x > 0.0 && tex_size.y > 0.0 {
                let video_aspect = tex_size.x / tex_size.y;
                let panel_aspect = viewer_rect.width() / viewer_rect.height();
                // Same zoom semantics as the Program viewer: 0.0 = fit,
                // any other value is a multiplier on native source pixels.
                let view = state.source_view;
                let (dw, dh) = if view.zoom <= 0.0 {
                    if video_aspect > panel_aspect {
                        (viewer_rect.width(), viewer_rect.width() / video_aspect)
                    } else {
                        (viewer_rect.height() * video_aspect, viewer_rect.height())
                    }
                } else {
                    (tex_size.x * view.zoom, tex_size.y * view.zoom)
                };
                let pan = if view.zoom <= 0.0 { egui::Vec2::ZERO } else { view.pan };
                let image_rect = egui::Rect::from_center_size(
                    viewer_rect.center() + pan,
                    egui::vec2(dw, dh),
                );
                // Clip to `viewer_rect` so high-zoom overflow stays inside
                // the source-viewer panel.
                ui.painter_at(viewer_rect).image(
                    tex.id(),
                    image_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );

                // Show filename overlay in top-left
                if let Some(media) = state.source_media_id.and_then(|mid| state.media_pool.get(&mid)) {
                    ui.painter().text(
                        egui::pos2(viewer_rect.min.x + 12.0, viewer_rect.min.y + 28.0),
                        egui::Align2::LEFT_TOP,
                        &media.filename,
                        egui::FontId::proportional(11.0),
                        egui::Color32::from_rgba_premultiplied(200, 200, 210, 180),
                    );

                    // Graphical IN/OUT markers
                    Self::draw_source_in_out_markers(ui, &image_rect, media, theme);
                }
                return;
            }
        }

        let media = state.source_media_id
            .and_then(|mid| state.media_pool.get(&mid));

        if let Some(media) = media {
            let cx = viewer_rect.center().x;
            let cy = viewer_rect.center().y;
            let dim = theme.text_dim;
            let label_col = theme.text_secondary;
            let val_col = theme.text_primary;
            // Everything drawn here stays inside the viewer: a narrow
            // viewer once let this text and these buttons bleed over the
            // panel beside it.
            let painter = ui.painter_at(viewer_rect);

            painter.text(
                egui::pos2(cx, cy - 60.0),
                egui::Align2::CENTER_CENTER,
                &media.filename,
                egui::FontId::proportional(13.0),
                label_col,
            );

            let dur_secs = if media.duration.rate > 0 {
                media.duration.value as f64 / media.duration.rate as f64
            } else { 0.0 };
            let dur_m = (dur_secs / 60.0) as u32;
            let dur_s = dur_secs % 60.0;
            let fps_str = if media.fps.den == 1 {
                format!("{}", media.fps.num)
            } else {
                format!("{:.2}", media.fps.num as f64 / media.fps.den as f64)
            };
            let res_line = format!(
                "{}x{}  {}fps  {}:{:05.2}",
                media.resolution.width, media.resolution.height, fps_str, dur_m, dur_s,
            );
            painter.text(
                egui::pos2(cx, cy - 38.0),
                egui::Align2::CENTER_CENTER,
                &res_line,
                egui::FontId::monospace(12.0),
                val_col,
            );

            let size_mb = media.file_size_bytes as f64 / (1024.0 * 1024.0);
            let size_str = if size_mb >= 1024.0 {
                format!("{:.1} GB", size_mb / 1024.0)
            } else {
                format!("{:.1} MB", size_mb)
            };
            let audio_str = if media.has_audio {
                media.audio_codec.to_lowercase()
            } else {
                "none".to_string()
            };
            let codec_line = format!(
                "{}  \u{2502}  {}  \u{2502}  {}",
                media.video_codec.to_lowercase(), audio_str, size_str,
            );
            painter.text(
                egui::pos2(cx, cy - 20.0),
                egui::Align2::CENTER_CENTER,
                &codec_line,
                egui::FontId::monospace(11.0),
                dim,
            );

            let in_str = media.default_source_in
                .map(|t| klipp_core::TimecodeDisplay::plain(media.fps).position(t))
                .unwrap_or_else(|| "00:00:00:00".to_string());
            let out_str = media.default_source_out
                .map(|t| klipp_core::TimecodeDisplay::plain(media.fps).position(t))
                .unwrap_or_else(|| klipp_core::TimecodeDisplay::plain(media.fps).position(media.duration));
            painter.text(
                egui::pos2(cx, cy + 6.0),
                egui::Align2::CENTER_CENTER,
                format!("In: {}  Out: {}", in_str, out_str),
                egui::FontId::monospace(12.0),
                theme.text_accent,
            );

            // Graphical IN/OUT markers on the viewer rect
            Self::draw_source_in_out_markers(ui, &viewer_rect, media, theme);

            // Edit buttons. Subclip is enabled only when the source clip has
            // an IN and OUT set AND is not itself a subclip — creating a
            // subclip of a subclip is rejected in the handler anyway.
            let has_in_out = state.sequence().in_point.is_some() && state.sequence().out_point.is_some();
            let subclip_enabled = media
                .default_source_in.is_some()
                && media.default_source_out.is_some()
                && !media.is_subclip();
            let edit_buttons: &[(&str, AppCommand, bool)] = &[
                ("Insert (,)", AppCommand::InsertEdit, true),
                ("Overwrite (.)", AppCommand::OverwriteEdit, true),
                ("Lift (;)", AppCommand::LiftEdit, has_in_out),
                ("Extract (')", AppCommand::ExtractEdit, has_in_out),
                ("Subclip", AppCommand::CreateSubclipFromZone { source_media_id: media.id, name: None }, subclip_enabled),
            ];

            // As wide as fits the viewer, up to 90 px each.
            let btn_gap = 6.0;
            let btn_count = edit_buttons.len() as f32;
            let btn_w = ((viewer_rect.width() - 16.0 - btn_gap * (btn_count - 1.0)) / btn_count).clamp(44.0, 90.0);
            let total_w = btn_w * btn_count + btn_gap * (btn_count - 1.0);
            let btn_y = cy + 34.0;
            let btn_x_start = cx - total_w / 2.0;

            for (i, (label, cmd, enabled)) in edit_buttons.iter().enumerate() {
                let bx = btn_x_start + (btn_w + btn_gap) * i as f32;
                let rect = egui::Rect::from_min_size(
                    egui::pos2(bx, btn_y),
                    egui::vec2(btn_w, 24.0),
                );
                let resp = ui.allocate_rect(rect, if *enabled { egui::Sense::click() } else { egui::Sense::hover() });
                let fill = if !enabled {
                    theme.bg_deep
                } else if resp.hovered() {
                    theme.bg_hover
                } else {
                    theme.bg_raised
                };
                let text_color = if *enabled {
                    theme.text_primary
                } else {
                    theme.text_dim
                };
                painter.rect_filled(rect, 2.0, fill);
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    *label,
                    egui::FontId::proportional(if btn_w < 70.0 { 9.0 } else { 10.0 }),
                    text_color,
                );
                if *enabled && resp.clicked() {
                    cmds.push(cmd.clone());
                }
            }
        } else {
            ui.painter().text(
                viewer_rect.center(),
                egui::Align2::CENTER_CENTER,
                "Click a media item to load it here",
                egui::FontId::proportional(13.0),
                theme.text_dim,
            );
        }
    }

    /// Draw graphical IN/OUT markers on the source viewer.
    fn draw_source_in_out_markers(
        ui: &mut egui::Ui,
        image_rect: &egui::Rect,
        media: &klipp_media::MediaItem,
        theme: &KlippTheme,
    ) {
        let amber = theme.accent;
        let bar_h = 4.0_f32;
        let marker_w = 3.0_f32;
        let label_font = egui::FontId::monospace(10.0);

        let has_in = media.default_source_in.is_some();
        let has_out = media.default_source_out.is_some();

        // IN marker — amber vertical bar on left edge
        if has_in {
            let in_rect = egui::Rect::from_min_size(
                egui::pos2(image_rect.left(), image_rect.top()),
                egui::vec2(marker_w, image_rect.height()),
            );
            ui.painter().rect_filled(in_rect, 0.0, amber);
            // Small downward triangle at bottom
            let tri_size = 5.0;
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(image_rect.left() + marker_w / 2.0, image_rect.bottom() - bar_h - tri_size),
                    egui::pos2(image_rect.left() - tri_size + marker_w / 2.0, image_rect.bottom() - bar_h - tri_size * 2.0),
                    egui::pos2(image_rect.left() + tri_size + marker_w / 2.0, image_rect.bottom() - bar_h - tri_size * 2.0),
                ],
                amber, egui::Stroke::NONE,
            ));
            // "IN" label
            ui.painter().text(
                egui::pos2(image_rect.left() + marker_w + 4.0, image_rect.bottom() - bar_h - 10.0),
                egui::Align2::LEFT_CENTER,
                "IN",
                label_font.clone(),
                amber,
            );
        }

        // OUT marker — amber vertical bar on right edge
        if has_out {
            let out_rect = egui::Rect::from_min_size(
                egui::pos2(image_rect.right() - marker_w, image_rect.top()),
                egui::vec2(marker_w, image_rect.height()),
            );
            ui.painter().rect_filled(out_rect, 0.0, amber);
            // Small downward triangle at bottom
            let tri_size = 5.0;
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(image_rect.right() - marker_w / 2.0, image_rect.bottom() - bar_h - tri_size),
                    egui::pos2(image_rect.right() - tri_size - marker_w / 2.0, image_rect.bottom() - bar_h - tri_size * 2.0),
                    egui::pos2(image_rect.right() + tri_size - marker_w / 2.0, image_rect.bottom() - bar_h - tri_size * 2.0),
                ],
                amber, egui::Stroke::NONE,
            ));
            // "OUT" label
            ui.painter().text(
                egui::pos2(image_rect.right() - marker_w - 4.0, image_rect.bottom() - bar_h - 10.0),
                egui::Align2::RIGHT_CENTER,
                "OUT",
                label_font,
                amber,
            );
        }

        // Progress bar between IN and OUT at bottom of viewer
        if has_in && has_out {
            let fill_rect = egui::Rect::from_min_size(
                egui::pos2(image_rect.left(), image_rect.bottom() - bar_h),
                egui::vec2(image_rect.width(), bar_h),
            );
            ui.painter().rect_filled(
                fill_rect, 0.0,
                egui::Color32::from_rgba_unmultiplied(
                    theme.accent.r(), theme.accent.g(), theme.accent.b(), 60,
                ),
            );
        }
    }
}

/// Rotate a point around a center by the given angle in radians.
/// Compute the bounding box of a Title object in screen coords,
/// derived from its normalised `transform.center` / `transform.size`
/// mapped through the rendered video's `image_rect`.
/// One rectangle, or two side by side: the viewer layout the click
/// will switch to is the one drawn.
fn draw_viewer_layout_icon(painter: &egui::Painter, btn: egui::Rect, dual_now: bool, c: egui::Color32) {
    let ir = btn.shrink(4.0);
    let s = egui::Stroke::new(1.2_f32, c);
    if dual_now {
        painter.rect_stroke(ir, 1.0, s);
    } else {
        let half = (ir.width() - 2.0) / 2.0;
        painter.rect_stroke(egui::Rect::from_min_size(ir.min, egui::vec2(half, ir.height())), 1.0, s);
        painter.rect_stroke(egui::Rect::from_min_size(egui::pos2(ir.min.x + half + 2.0, ir.min.y), egui::vec2(half, ir.height())), 1.0, s);
    }
}

fn is_fitted_shape(obj: &klipp_timeline::TitleObject) -> bool {
    matches!(&obj.content, klipp_timeline::TitleObjectContent::Shape(shape) if shape.fit.is_some())
}

fn editor_title_geometry(
    title: &klipp_timeline::TitleData,
    dimensions: (u32, u32),
    time: f64,
    cache: &mut Option<TitleFitGeometry>,
) -> klipp_timeline::TitleData {
    if title.objects.iter().any(is_fitted_shape)
        && cache.as_ref().is_none_or(|c| c.title != *title || c.dimensions != dimensions)
    {
        let transforms = with_editor_renderer(|renderer| title.objects.iter().filter_map(|obj| {
            let klipp_timeline::TitleObjectContent::Shape(shape) = &obj.content else { return None };
            let fit = shape.fit?;
            Some((obj.id, renderer.resolved_shape_fit(title, fit, dimensions.0, dimensions.1, None)))
        }).collect());
        *cache = Some(TitleFitGeometry { title: title.clone(), dimensions, transforms });
    }
    let mut resolved = title.clone();
    if let Some(cache) = cache {
        for (id, transform) in &cache.transforms {
            let Some(obj) = resolved.object_mut(*id) else { continue };
            let Some(transform) = transform else { obj.visible = false; continue };
            obj.transform = transform.clone();
            if let klipp_timeline::TitleObjectContent::Shape(shape) = &obj.content {
                if let Some(target) = shape.fit.and_then(|fit| title.object(fit.target)) {
                    let center = target.transform.evaluated_center(time);
                    obj.transform.center.x += center.x - target.transform.center.x;
                    obj.transform.center.y += center.y - target.transform.center.y;
                }
            }
        }
    }
    tighten_text_editor_boxes(&mut resolved, dimensions);
    resolved
}

/// A title layer as the viewer's editor draws it, its box hugging its
/// text: where a test finds the handles it drags (#1714).
#[cfg(test)]
pub(crate) fn shown_title_layer(
    title: &klipp_timeline::TitleData,
    frame: (u32, u32),
    object: klipp_timeline::TitleObjectId,
) -> Option<klipp_timeline::TitleObject> {
    editor_title_geometry(title, frame, 0.0, &mut None).object(object).cloned()
}

/// Match the editor frame to the renderer's measured glyph bounds while
/// preserving the authored transform centre and wrapping constraint.
fn tighten_text_editor_boxes(title: &mut klipp_timeline::TitleData, dimensions: (u32, u32)) {
    with_editor_renderer(|renderer| tighten_with(renderer, title, dimensions));
}

/// `f` with the viewer's title renderer, one kept for the thread: the
/// editor measures titles every frame one is on screen, and a new
/// renderer scans the system's fonts (~10 ms each, a hundred a second).
fn with_editor_renderer<R>(f: impl FnOnce(&mut klipp_title::TitleRenderer) -> R) -> R {
    thread_local! {
        static RENDERER: std::cell::RefCell<Option<klipp_title::TitleRenderer>> = const { std::cell::RefCell::new(None) };
    }
    RENDERER.with(|cell| f(cell.borrow_mut().get_or_insert_with(klipp_title::TitleRenderer::new)))
}

fn tighten_with(renderer: &mut klipp_title::TitleRenderer, title: &mut klipp_timeline::TitleData, dimensions: (u32, u32)) {
    let (width, height) = dimensions;
    for obj in &mut title.objects {
        let klipp_timeline::TitleObjectContent::TextBox(tb) = &obj.content else { continue };
        let mut single = klipp_timeline::TitleData::single_text_fx(tb.clone(), obj.fx().clone());
        single.objects[0].transform = obj.transform.clone();
        let layout = renderer.render_layout(&single, width.max(1), height.max(1));
        if layout.glyph_count == 0 { continue; }
        let pad_x = (layout.scale * 8.0).clamp(4.0, 12.0);
        let pad_y = (layout.scale * 6.0).clamp(3.0, 10.0);
        let w = (layout.text_right - layout.text_left + pad_x * 2.0) / width.max(1) as f32;
        let h = (layout.text_bottom - layout.text_top + pad_y * 2.0) / height.max(1) as f32;
        obj.transform.size.x = w.clamp(0.01, obj.transform.size.x.max(0.01));
        obj.transform.size.y = h.clamp(0.01, obj.transform.size.y.max(0.01));
    }
}

/// The upright box round four points: a layer's corners as drawn.
fn points_bb(points: &[egui::Pos2; 4]) -> egui::Rect {
    let min_x = points.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
    let max_x = points.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
    let min_y = points.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
    let max_y = points.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
    egui::Rect::from_min_max(egui::pos2(min_x, min_y), egui::pos2(max_x, max_y))
}

fn object_screen_bb(
    obj: &klipp_timeline::TitleObject,
    image_rect: egui::Rect,
) -> egui::Rect {
    let points = object_screen_points(obj, image_rect);
    let min_x = points.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
    let max_x = points.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
    let min_y = points.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
    let max_y = points.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
    egui::Rect::from_min_max(egui::pos2(min_x, min_y), egui::pos2(max_x, max_y))
}

fn object_screen_points(
    obj: &klipp_timeline::TitleObject,
    image_rect: egui::Rect,
) -> [egui::Pos2; 4] {
    let center = egui::pos2(
        image_rect.min.x + obj.transform.center.x * image_rect.width(),
        image_rect.min.y + obj.transform.center.y * image_rect.height(),
    );
    // A TextBox's authored size is its layout/wrapping region.  It is
    // often intentionally much larger than the rendered glyphs (the
    // default is a full-frame layout box), so using it as the selection
    // frame makes direct manipulation feel disconnected from the text.
    // The editor frame hugs the visible text with a small cushion while
    // retaining the authored box as the renderer's wrapping constraint.
        let size = obj.transform.size;
    let half = egui::vec2(
        size.x * image_rect.width() * 0.5,
        size.y * image_rect.height() * 0.5,
    );
    let angle = obj.transform.rotation.to_radians();
    let (sin, cos) = angle.sin_cos();
    let rotate = |v: egui::Vec2| egui::vec2(v.x * cos - v.y * sin, v.x * sin + v.y * cos);
    [
        center + rotate(egui::vec2(-half.x, -half.y)),
        center + rotate(egui::vec2(half.x, -half.y)),
        center + rotate(egui::vec2(half.x, half.y)),
        center + rotate(egui::vec2(-half.x, half.y)),
    ]
}


fn object_contains_screen_point(
    obj: &klipp_timeline::TitleObject,
    image_rect: egui::Rect,
    p: egui::Pos2,
) -> bool {
    let points = object_screen_points(obj, image_rect);
    (0..4).all(|i| {
        let edge = points[(i + 1) % 4] - points[i];
        let to_point = p - points[i];
        edge.x * to_point.y - edge.y * to_point.x >= -0.5
    })
}

/// Hit-test a click against a selected object's handle hot-spots.
/// Returns the handle the user grabbed, or `None` if they hit empty
/// space (the caller falls back to body-drag / object-switch).
/// Hit zones use a small inflation around each handle so the user
/// doesn't need pixel-perfect aim.
fn hit_test_handles(
    obj: &klipp_timeline::TitleObject,
    image_rect: egui::Rect,
    p: egui::Pos2,
) -> Option<crate::state::TitleObjectHandle> {
    if is_fitted_shape(obj) { return None; }
    use crate::state::TitleObjectHandle as H;
    let points = object_screen_points(obj, image_rect);
    let bb = object_screen_bb(obj, image_rect);
    let hit_radius = 10.0_f32;
    let inflate = egui::vec2(hit_radius, hit_radius);
    let near = |a: egui::Pos2| egui::Rect::from_center_size(a, inflate * 2.0).contains(p);

    let mid_top = points[0].lerp(points[1], 0.5);
    let mid_bottom = points[3].lerp(points[2], 0.5);
    let mid_left = points[0].lerp(points[3], 0.5);
    let mid_right = points[1].lerp(points[2], 0.5);
    let outward = (mid_top - bb.center()).normalized();
    if near(mid_top + outward * 24.0) {
        return Some(H::Rotation);
    }
    // Anchor dot — checked before the other handles so a near-
    // simultaneous overlap with an edge handle prefers the anchor.
    let anchor = points[0]
        + (points[1] - points[0]) * obj.transform.anchor_norm.x
        + (points[3] - points[0]) * obj.transform.anchor_norm.y;
    if near(anchor) {
        return Some(H::Anchor);
    }
    if near(points[0]) { return Some(H::CornerTopLeft); }
    if near(points[1]) { return Some(H::CornerTopRight); }
    if near(points[3]) { return Some(H::CornerBottomLeft); }
    if near(points[2]) { return Some(H::CornerBottomRight); }
    if near(mid_top) { return Some(H::EdgeTop); }
    if near(mid_bottom) { return Some(H::EdgeBottom); }
    if near(mid_left) { return Some(H::EdgeLeft); }
    if near(mid_right) { return Some(H::EdgeRight); }
    None
}

/// How near, in screen points, a dragged title layer is pulled onto a
/// snapping line (#1714).
const TITLE_SNAP_PX: f32 = 6.0;

fn is_resize_handle(handle: crate::state::TitleObjectHandle) -> bool {
    use crate::state::TitleObjectHandle as H;
    matches!(
        handle,
        H::EdgeLeft | H::EdgeRight | H::EdgeTop | H::EdgeBottom
            | H::CornerTopLeft | H::CornerTopRight | H::CornerBottomLeft | H::CornerBottomRight
    )
}

/// Where a text set on an arc starts, on screen: `None` for a layer that
/// is not text on an arc. The arc is about the layer's centre, its
/// radius in frame pixels, as the renderer lays it.
fn arc_start_screen_point(obj: &klipp_timeline::TitleObject, image_rect: egui::Rect, frame: (u32, u32)) -> Option<egui::Pos2> {
    let klipp_timeline::TitleObjectContent::TextBox(tb) = &obj.content else { return None };
    let Some(klipp_timeline::TextPath::Arc { radius, start_angle, .. }) = &tb.text_path else { return None };
    let (w, h) = (frame.0.max(1) as f32, frame.1.max(1) as f32);
    let a = start_angle.to_radians();
    let c = obj.transform.center;
    Some(viewer_norm_to_screen(image_rect, c.x + radius * a.cos() / w, c.y + radius * a.sin() / h))
}

/// Half the width and height, in frame fractions, of the upright box
/// around a layer's turned box; `aspect` is the frame's width over its
/// height.
fn layer_half_extent(t: &klipp_timeline::ObjectTransform, aspect: f32) -> (f32, f32) {
    let aspect = aspect.max(0.01);
    let (sin, cos) = t.rotation.to_radians().sin_cos();
    let (sx, sy) = (t.size.x.abs(), t.size.y.abs());
    ((sx * cos.abs() + sy * sin.abs() / aspect) * 0.5, (sx * sin.abs() * aspect + sy * cos.abs()) * 0.5)
}

/// The lines a dragged title layer snaps to (#1714), across and down in
/// frame fractions: the frame's edges and centre lines, the title-safe
/// and action-safe edges, and each other visible layer's edges and
/// centre, as the layers are seen.
fn title_snap_lines(
    title: &klipp_timeline::TitleData,
    moving: klipp_timeline::TitleObjectId,
    prefs: &crate::prefs::ViewerPrefs,
    aspect: f32,
) -> (Vec<f32>, Vec<f32>) {
    let mut xs = vec![0.0, 0.5, 1.0];
    let mut ys = vec![0.0, 0.5, 1.0];
    for safe in [prefs.title_safe_pct, prefs.action_safe_pct] {
        let half = safe.clamp(0.5, 1.0) * 0.5;
        xs.extend([0.5 - half, 0.5 + half]);
        ys.extend([0.5 - half, 0.5 + half]);
    }
    for o in title.objects.iter().filter(|o| o.id != moving && o.visible) {
        let (hw, hh) = layer_half_extent(&o.transform, aspect);
        let c = o.transform.center;
        xs.extend([c.x - hw, c.x, c.x + hw]);
        ys.extend([c.y - hh, c.y, c.y + hh]);
    }
    (xs, ys)
}

/// The nearest of `lines` to any of `points`, within `within`: how far
/// to move the points to land on it, and the line.
fn snap_to_lines(points: &[f32], lines: &[f32], within: f32) -> Option<(f32, f32)> {
    points
        .iter()
        .flat_map(|p| lines.iter().map(move |l| (l - p, *l)))
        .filter(|(d, _)| d.abs() < within)
        .min_by(|a, b| a.0.abs().total_cmp(&b.0.abs()))
}

/// Compute the new `ObjectTransform` produced by an in-progress
/// drag, given the drag's normalised delta (drag-pos − drag-start
/// in 0..1 frame coords), the current and start pointer positions
/// in normalised coords (used by the rotation handle to compute
/// angles), and the handle being dragged.
///
/// Resize semantics, in the box's own axes, so a turned box resizes
/// along its sides as they are seen (#1714):
/// - **Edge handles** (top / bottom / left / right) anchor the
///   opposite edge in place and pivot the box from there. Center
///   shifts by half the size delta.
/// - **Corner handles** anchor the diagonally-opposite corner;
///   uniform aspect kept by clamping the larger axis to match.
///   The font-size scaling lives at the call site (the resulting
///   transform is fed back through `SetTitleObjectTransformAndFont`).
/// - **Body** drag translates `transform.center` by the delta.
/// - **Rotation** drag computes the angle change between the
///   pointer's start and current position relative to the box's
///   centre, added to the initial rotation, measured on the picture
///   as it is seen (`frame_aspect`). The centre is the pivot.
/// - **PathStart** leaves the box as it is; the caller sets the arc.
fn compute_drag_transform(
    drag: &crate::state::TitleObjectDrag,
    dnx: f32,
    dny: f32,
    start_norm: (f32, f32),
    cur_norm: (f32, f32),
) -> klipp_timeline::ObjectTransform {
    use crate::state::TitleObjectHandle as H;
    let mut t = drag.initial_transform.clone();
    // x in frame heights, so both axes share a unit as seen on screen.
    let aspect = if drag.frame_aspect.is_finite() && drag.frame_aspect > 0.0 { drag.frame_aspect } else { 1.0 };
    // Which sides a handle moves: -1 the left / top, 1 the right /
    // bottom, 0 neither.
    let sides = match drag.handle {
        H::EdgeLeft => Some((-1.0, 0.0)),
        H::EdgeRight => Some((1.0, 0.0)),
        H::EdgeTop => Some((0.0, -1.0)),
        H::EdgeBottom => Some((0.0, 1.0)),
        H::CornerTopLeft => Some((-1.0, -1.0)),
        H::CornerTopRight => Some((1.0, -1.0)),
        H::CornerBottomLeft => Some((-1.0, 1.0)),
        H::CornerBottomRight => Some((1.0, 1.0)),
        _ => None,
    };
    match drag.handle {
        H::Body => {
            t.center.x = (t.center.x + dnx).clamp(-2.0, 3.0);
            t.center.y = (t.center.y + dny).clamp(-2.0, 3.0);
        }
        H::Anchor => {
            // Anchor delta in normalised frame coords → bbox-local
            // delta. Need to divide by the box's normalised size so
            // dragging by 0.1 frame-units across a 0.4-frame-wide
            // box moves the anchor by 0.25 box-units. Clamp to 0..1
            // so the anchor stays inside the box (rotating around
            // an off-box anchor is a niche case worth surfacing
            // later, not in commit 6).
            let inv_w = if drag.initial_transform.size.x.abs() > 0.0001 {
                1.0 / drag.initial_transform.size.x
            } else { 0.0 };
            let inv_h = if drag.initial_transform.size.y.abs() > 0.0001 {
                1.0 / drag.initial_transform.size.y
            } else { 0.0 };
            t.anchor_norm.x = (drag.initial_transform.anchor_norm.x + dnx * inv_w)
                .clamp(0.0, 1.0);
            t.anchor_norm.y = (drag.initial_transform.anchor_norm.y + dny * inv_h)
                .clamp(0.0, 1.0);
        }
        H::Rotation => {
            // Angle change = (angle from centre to current pointer)
            //              − (angle from centre to start pointer),
            // added to the initial rotation so the gesture is relative
            // to where the handle was grabbed. Clockwise on screen is a
            // positive turn, as the box is drawn.
            let cx = drag.initial_transform.center.x;
            let cy = drag.initial_transform.center.y;
            let a_start = (start_norm.1 - cy).atan2((start_norm.0 - cx) * aspect);
            let a_cur = (cur_norm.1 - cy).atan2((cur_norm.0 - cx) * aspect);
            let mut delta_deg = (a_cur - a_start).to_degrees();
            if delta_deg > 180.0 {
                delta_deg -= 360.0;
            } else if delta_deg < -180.0 {
                delta_deg += 360.0;
            }
            t.rotation = drag.initial_transform.rotation + delta_deg;
        }
        _ => {}
    }
    if let Some((sx, sy)) = sides {
        // The pointer's move in the box's own axes.
        let (sin, cos) = drag.initial_transform.rotation.to_radians().sin_cos();
        let (dx, dy) = (dnx * aspect, dny);
        let (lx, ly) = (dx * cos + dy * sin, -dx * sin + dy * cos);
        let (w, h) = (drag.initial_transform.size.x.max(0.01) * aspect, drag.initial_transform.size.y.max(0.01));
        let (min_w, min_h) = (0.01 * aspect, 0.01);
        let mut nw = if sx != 0.0 { (w + sx * lx).max(min_w) } else { w };
        let mut nh = if sy != 0.0 { (h + sy * ly).max(min_h) } else { h };
        // Corner handles are proportional. Preserve the authored aspect
        // ratio even when the pointer moves farther on one axis; the
        // opposite corner remains the fixed pivot for the resize.
        if sx != 0.0 && sy != 0.0 {
            let (kx, ky) = ((nw / w).max(0.05), (nh / h).max(0.05));
            let k = if (kx - 1.0).abs() >= (ky - 1.0).abs() { kx } else { ky };
            nw = w * k;
            nh = h * k;
        }
        // The fixed side stays put: the centre moves half the growth,
        // along the box's axes, turned back onto the frame's.
        let (ox, oy) = (sx * (nw - w) * 0.5, sy * (nh - h) * 0.5);
        t.center.x = drag.initial_transform.center.x + (ox * cos - oy * sin) / aspect;
        t.center.y = drag.initial_transform.center.y + (ox * sin + oy * cos);
        t.size = klipp_timeline::TitlePosition { x: nw / aspect, y: nh };
    }
    t
}

fn rotate_point(point: egui::Pos2, center: egui::Pos2, angle_rad: f32) -> egui::Pos2 {
    let dx = point.x - center.x;
    let dy = point.y - center.y;
    let cos = angle_rad.cos();
    let sin = angle_rad.sin();
    egui::pos2(
        center.x + dx * cos - dy * sin,
        center.y + dx * sin + dy * cos,
    )
}

/// The playhead, as a position on the sequence's clock — so drop-frame
/// where the sequence counts that way, and marked with a semicolon.
fn format_timecode_hmsf(smooth_secs: f64, tc: klipp_core::TimecodeDisplay) -> String {
    tc.position_secs(smooth_secs)
}

fn parse_timecode_input(input: &str, fps: Fps) -> Option<RationalTime> {
    let input = input.trim();
    if let Some(tc) = parse_hmsf(input, fps.rate()) { return Some(tc); }
    if let Ok(f) = input.parse::<i64>() { return Some(RationalTime::from_frames(f, fps.rate())); }
    if let Ok(s) = input.parse::<f64>() { return Some(RationalTime::from_secs_f64(s, fps.rate())); }
    None
}

fn parse_hmsf(input: &str, rate: u32) -> Option<RationalTime> {
    let p: Vec<&str> = input.split(':').collect();
    if p.len() != 4 { return None; }
    let hh: i64 = p[0].parse().ok()?;
    let mm: i64 = p[1].parse().ok()?;
    let ss: i64 = p[2].parse().ok()?;
    let ff: i64 = p[3].parse().ok()?;
    Some(RationalTime::from_frames(((hh * 3600 + mm * 60 + ss) * rate as i64) + ff, rate))
}

// ── WYSIWYG text edit helpers ──

/// Convert a character index to a byte offset in the string.
/// The in-place editor's caret before character `cursor`, its two ends
/// in frame pixels: at the character's reading start, its right edge
/// when it reads right to left (#1693), across its top in a column
/// (#1704); past the last character, at its reading end.
pub(crate) fn caret_ends(positions: &[klipp_title::CharPosition], cursor: usize) -> Option<[(f32, f32); 2]> {
    if let Some(cp) = positions.iter().find(|cp| cp.char_index == cursor) {
        return Some(if cp.vertical {
            [(cp.x, cp.y), (cp.x + cp.height, cp.y)]
        } else {
            let x = if cp.rtl { cp.x + cp.advance } else { cp.x };
            [(x, cp.y), (x, cp.y + cp.height)]
        });
    }
    positions.last().map(|l| {
        if l.vertical {
            [(l.x, l.y + l.advance), (l.x + l.height, l.y + l.advance)]
        } else {
            let x = if l.rtl { l.x } else { l.x + l.advance };
            [(x, l.y), (x, l.y + l.height)]
        }
    })
}

fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(bi, _)| bi)
        .unwrap_or(s.len())
}

#[allow(dead_code)] // Used in tests; may be re-enabled for viewer click mapping
fn find_nearest_char_index(positions: &[klipp_title::CharPosition], fx: f32, fy: f32) -> usize {
    if positions.is_empty() {
        return 0;
    }

    // Group positions by line (characters with similar y values)
    // Find the line closest to fy first, then find the nearest char on that line.
    let mut best_line_y = positions[0].y;
    let mut best_line_dist = (fy - positions[0].y - positions[0].height * 0.5).abs();
    for cp in positions {
        let cy = cp.y + cp.height * 0.5;
        let dist = (fy - cy).abs();
        if dist < best_line_dist {
            best_line_dist = dist;
            best_line_y = cp.y;
        }
    }

    // Among chars on the best line (only those with advance > 0, skip end-of-text marker)
    let line_chars: Vec<&klipp_title::CharPosition> = positions.iter()
        .filter(|cp| (cp.y - best_line_y).abs() < cp.height * 0.5 && cp.advance > 0.0)
        .collect();

    if line_chars.is_empty() {
        // Only end-of-text positions on this line — return the first char_index on the line
        return positions.iter()
            .find(|cp| (cp.y - best_line_y).abs() < cp.height * 0.5)
            .map(|cp| cp.char_index)
            .unwrap_or(0);
    }

    // Check if click is before the first char
    if fx <= line_chars[0].x {
        return line_chars[0].char_index;
    }

    // Check each character: if fx is in the left half, place cursor before; right half, after
    for cp in &line_chars {
        let mid = cp.x + cp.advance * 0.5;
        if fx >= cp.x && fx < cp.x + cp.advance {
            return if fx < mid { cp.char_index } else { cp.char_index + 1 };
        }
    }

    // Click is past the last visible char on this line — place cursor after it
    if let Some(last) = line_chars.last() {
        last.char_index + 1
    } else {
        0
    }
}

#[allow(dead_code)] // Used in tests; may be re-enabled for vertical cursor nav
/// Move cursor up or down by `direction` lines (-1 = up, +1 = down).
fn move_cursor_vertical(
    positions: &[klipp_title::CharPosition],
    cursor: usize,
    direction: i32,
) -> usize {
    if positions.is_empty() {
        return cursor;
    }

    // Find the current cursor position
    let cur_cp = positions.iter().find(|cp| cp.char_index == cursor);
    let (cur_x, cur_y) = match cur_cp {
        Some(cp) => (cp.x, cp.y),
        None => {
            // Cursor at end — use last position
            if let Some(last) = positions.last() {
                (last.x + last.advance, last.y)
            } else {
                return cursor;
            }
        }
    };

    // Build a sorted list of unique line y-values
    let mut line_ys: Vec<f32> = Vec::new();
    for cp in positions {
        if (line_ys.is_empty() || (cp.y - line_ys.last().unwrap()).abs() > 1.0)
            && !line_ys.contains(&cp.y) {
                line_ys.push(cp.y);
            }
    }
    line_ys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    line_ys.dedup_by(|a, b| (*a - *b).abs() < 1.0);

    // Find current line index
    let cur_line_idx = line_ys.iter().position(|&y| (y - cur_y).abs() < 1.0).unwrap_or(0);

    // Target line
    let target_line_idx = if direction < 0 {
        if cur_line_idx == 0 { return 0; } // Already on first line, go to start
        cur_line_idx - 1
    } else {
        if cur_line_idx >= line_ys.len() - 1 {
            // Already on last line, go to end
            return positions.last().map(|p| p.char_index).unwrap_or(cursor);
        }
        cur_line_idx + 1
    };

    let target_y = line_ys[target_line_idx];

    // Find the char on the target line closest to cur_x
    let target_chars: Vec<&klipp_title::CharPosition> = positions.iter()
        .filter(|cp| (cp.y - target_y).abs() < 1.0)
        .collect();

    if target_chars.is_empty() {
        return cursor;
    }

    // Find nearest by x
    target_chars.iter()
        .min_by_key(|cp| ((cp.x - cur_x).abs() * 100.0) as i64)
        .map(|cp| cp.char_index)
        .unwrap_or(cursor)
}

#[allow(dead_code)]
fn move_cursor_to_line_edge(
    positions: &[klipp_title::CharPosition],
    cursor: usize,
    to_end: bool,
) -> usize {
    if positions.is_empty() {
        return cursor;
    }

    // Find current line y
    let cur_y = positions.iter()
        .find(|cp| cp.char_index == cursor)
        .or_else(|| positions.last())
        .map(|cp| cp.y)
        .unwrap_or(0.0);

    // Get chars on this line
    let line_chars: Vec<&klipp_title::CharPosition> = positions.iter()
        .filter(|cp| (cp.y - cur_y).abs() < 1.0)
        .collect();

    if line_chars.is_empty() {
        return cursor;
    }

    if to_end {
        line_chars.last().map(|cp| cp.char_index).unwrap_or(cursor)
    } else {
        line_chars.first().map(|cp| cp.char_index).unwrap_or(cursor)
    }
}

#[allow(dead_code)]
fn update_selection_from_anchor(state: &mut AppState) {
    if let Some(anchor) = state.viewer_text_select_anchor {
        let cursor = state.viewer_text_cursor;
        if anchor != cursor {
            let start = anchor.min(cursor);
            let end = anchor.max(cursor);
            state.viewer_text_selection = Some((start, end));
            state.viewer_text_selection_locked = Some((start, end));
        } else {
            state.viewer_text_selection = None;
            state.viewer_text_selection_locked = None;
        }
    }
}

/// Whether a clip that draws a picture is at `time`: on a picture track,
/// not a gap. The program viewer shows its frame then, playing or not.
fn pictured_at(seq: &klipp_timeline::Sequence, time: klipp_core::RationalTime) -> bool {
    seq.tracks
        .iter()
        .filter(|t| !matches!(t.kind, klipp_timeline::TrackKind::Audio))
        .any(|t| t.clip_at_timecode(time).is_some_and(|(_, c)| !c.kind.is_gap()))
}

#[cfg(test)]
mod pictured_tests {
    use super::pictured_at;
    use klipp_core::RationalTime;
    use klipp_timeline::{Clip, ClipKind, Sequence, TextBox, TitleData, Track, TrackKind};

    /// #2664: a project opened onto a title, a generator or a media shot
    /// has a picture to show at 0:00, though nothing has played; sound
    /// alone, or nothing, has none.
    #[test]
    fn an_opened_project_has_a_picture_at_its_start() {
        let tc = |s: f64| RationalTime::from_secs_f64(s, 24);
        let title = Clip { kind: ClipKind::Title(TitleData::single_text(TextBox { text: "Hi".into(), ..Default::default() })), ..Clip::gap(tc(0.0), tc(2.0)) };
        let generator = Clip { kind: ClipKind::Generator(klipp_core::GeneratorData::SolidColor { color: [200, 40, 40, 255] }), ..Clip::gap(tc(0.0), tc(2.0)) };
        let media = Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(2.0), tc(0.0), tc(2.0));
        for (kind, clip) in [(TrackKind::Text, title), (TrackKind::Video, generator), (TrackKind::Video, media.clone())] {
            let mut seq = Sequence::new_default();
            let mut t = Track::new("T", kind);
            t.clips = vec![clip];
            seq.tracks = vec![t];
            assert!(pictured_at(&seq, tc(0.0)), "{kind:?}");
            assert!(!pictured_at(&seq, tc(3.0)), "{kind:?}: past its end");
        }
        let mut seq = Sequence::new_default();
        let mut a = Track::new("A1", TrackKind::Audio);
        a.clips = vec![media];
        seq.tracks = vec![a];
        assert!(!pictured_at(&seq, tc(0.0)), "sound alone");
    }
}

#[cfg(test)]
mod viewer_geom_tests {
    //! Pin the three viewer-geom helpers so a future refactor (e.g.
    //! moving them into a sibling `viewer_geom.rs`, or unifying
    //! with title-edit cursor placement) can't silently change the
    //! coordinate-conversion semantics that the focal-point gizmo,
    //! title-object editor, and (incoming) letter picker all share.
    use super::*;
    use egui::{pos2, Rect};
    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(x, y), egui::vec2(w, h))
    }

    #[test]
    fn norm_to_screen_centre_lands_on_image_centre() {
        let r = rect(100.0, 200.0, 800.0, 600.0);
        let p = viewer_norm_to_screen(r, 0.5, 0.5);
        assert!((p.x - 500.0).abs() < 1e-6);
        assert!((p.y - 500.0).abs() < 1e-6);
    }

    #[test]
    fn norm_to_screen_corners() {
        let r = rect(100.0, 200.0, 800.0, 600.0);
        let tl = viewer_norm_to_screen(r, 0.0, 0.0);
        let br = viewer_norm_to_screen(r, 1.0, 1.0);
        assert!((tl.x - 100.0).abs() < 1e-6);
        assert!((tl.y - 200.0).abs() < 1e-6);
        assert!((br.x - 900.0).abs() < 1e-6);
        assert!((br.y - 800.0).abs() < 1e-6);
    }

    #[test]
    fn norm_to_screen_preserves_overflow() {
        // Title-object editor case: an object dragged past the
        // canvas edge has nx > 1.0 and the gizmo paints offscreen.
        // Helper must NOT clamp — the caller decides.
        let r = rect(0.0, 0.0, 100.0, 100.0);
        let p = viewer_norm_to_screen(r, 1.5, -0.25);
        assert!((p.x - 150.0).abs() < 1e-6);
        assert!((p.y - -25.0).abs() < 1e-6);
    }

    #[test]
    fn screen_to_norm_centre_round_trips() {
        let r = rect(100.0, 200.0, 800.0, 600.0);
        let (nx, ny) = viewer_screen_to_norm(r, pos2(500.0, 500.0));
        assert!((nx - 0.5).abs() < 1e-6);
        assert!((ny - 0.5).abs() < 1e-6);
    }

    #[test]
    fn screen_to_norm_returns_unclamped_for_drag_accuracy() {
        // Pointer dragged past the canvas — focal-point caller will
        // clamp to [0,1] before storing; title-object drag handler
        // needs the unclamped delta. Helper preserves both.
        let r = rect(0.0, 0.0, 100.0, 100.0);
        let (nx, ny) = viewer_screen_to_norm(r, pos2(150.0, -25.0));
        assert!((nx - 1.5).abs() < 1e-6);
        assert!((ny - -0.25).abs() < 1e-6);
    }

    #[test]
    fn screen_to_norm_handles_degenerate_image_rect() {
        // During layout settle the image rect can briefly be
        // zero-sized. The width/height >= 1.0 guard prevents NaN
        // from a divide-by-zero.
        let r = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(0.0, 0.0));
        let (nx, ny) = viewer_screen_to_norm(r, pos2(50.0, 50.0));
        assert!(nx.is_finite());
        assert!(ny.is_finite());
    }

    #[test]
    fn frame_px_to_screen_maps_glyph_centroid() {
        // Glyph centroid at frame-pixel (960, 540) on a 1920×1080
        // title source, viewed in a 800×450 image rect at (100,200)
        // — should land at the rect centre.
        let r = rect(100.0, 200.0, 800.0, 450.0);
        let p = viewer_frame_px_to_screen(r, 1920.0, 1080.0, 960.0, 540.0);
        assert!((p.x - 500.0).abs() < 1e-6);
        assert!((p.y - 425.0).abs() < 1e-6);
    }

    #[test]
    fn frame_px_to_screen_handles_zero_frame_size() {
        // Defensive: a degenerate frame size shouldn't NaN.
        let r = rect(0.0, 0.0, 100.0, 100.0);
        let p = viewer_frame_px_to_screen(r, 0.0, 0.0, 50.0, 50.0);
        assert!(p.x.is_finite());
        assert!(p.y.is_finite());
    }

    #[test]
    fn round_trip_norm_through_screen_preserves_value() {
        // Focal-point gizmo writes (norm) → reads pointer (screen) →
        // writes back (norm). Should be lossless within float epsilon.
        let r = rect(100.0, 200.0, 800.0, 600.0);
        for (nx, ny) in [(0.0, 0.0), (0.25, 0.75), (0.5, 0.5), (0.999, 0.001)] {
            let p = viewer_norm_to_screen(r, nx, ny);
            let (nx2, ny2) = viewer_screen_to_norm(r, p);
            assert!((nx - nx2).abs() < 1e-5, "x: {nx} vs {nx2}");
            assert!((ny - ny2).abs() < 1e-5, "y: {ny} vs {ny2}");
        }
    }
}

#[cfg(test)]
mod title_object_drag_tests {
    use super::*;
    use crate::state::{TitleObjectDrag, TitleObjectHandle};
    use klipp_timeline::{ObjectTransform, TitleObjectId, TitlePosition};

    fn baseline_transform() -> ObjectTransform {
        ObjectTransform {
            center: TitlePosition { x: 0.5, y: 0.5 },
            center_x_keyframes: Vec::new(),
            center_y_keyframes: Vec::new(),
            size: TitlePosition { x: 0.4, y: 0.2 },
            rotation: 0.0,
            anchor_norm: TitlePosition { x: 0.5, y: 0.5 },
            size_x_keyframes: Vec::new(),
            size_y_keyframes: Vec::new(),
            rotation_keyframes: Vec::new(),
            anchor_x_keyframes: Vec::new(),
            anchor_y_keyframes: Vec::new(),
        }
    }

    fn drag(handle: TitleObjectHandle) -> TitleObjectDrag {
        TitleObjectDrag {
            clip_id: klipp_core::ClipId::new(),
            object_id: TitleObjectId(0),
            handle,
            initial_transform: baseline_transform(),
            initial_font_size: 72.0,
            drag_start_pos: egui::pos2(0.0, 0.0),
            frame_aspect: 1.0,
        }
    }

    #[test]
    fn body_drag_translates_centre() {
        // Move +0.1 in x and -0.05 in y → centre shifts by exactly that.
        let t = compute_drag_transform(&drag(TitleObjectHandle::Body), 0.1, -0.05, (0.0, 0.0), (0.1, -0.05));
        assert!((t.center.x - 0.6).abs() < 1e-5);
        assert!((t.center.y - 0.45).abs() < 1e-5);
        // Size stays the same on body drag.
        assert!((t.size.x - 0.4).abs() < 1e-5);
        assert!((t.size.y - 0.2).abs() < 1e-5);
    }

    #[test]
    fn edge_right_drag_anchors_left() {
        // Initial right edge at 0.7. Drag +0.1 → new right at 0.8.
        // Left edge stays at 0.3. New centre = 0.55, new width = 0.5.
        let t = compute_drag_transform(&drag(TitleObjectHandle::EdgeRight), 0.1, 0.0, (0.0, 0.0), (0.1, 0.0));
        assert!((t.center.x - 0.55).abs() < 1e-5);
        assert!((t.size.x - 0.5).abs() < 1e-5);
        // Y axis untouched.
        assert!((t.center.y - 0.5).abs() < 1e-5);
        assert!((t.size.y - 0.2).abs() < 1e-5);
    }

    #[test]
    fn corner_top_left_drag_anchors_bottom_right() {
        // Initial bottom-right at (0.7, 0.6). Drag TL by (-0.1, -0.05)
        // → new TL at (0.2, 0.35). New centre = ((0.2+0.7)/2, (0.35+0.6)/2)
        // = (0.45, 0.475). New size = (0.5, 0.25).
        let t = compute_drag_transform(&drag(TitleObjectHandle::CornerTopLeft), -0.1, -0.05, (0.0, 0.0), (-0.1, -0.05));
        assert!((t.center.x - 0.45).abs() < 1e-5);
        assert!((t.center.y - 0.475).abs() < 1e-5);
        assert!((t.size.x - 0.5).abs() < 1e-5);
        assert!((t.size.y - 0.25).abs() < 1e-5);
    }

    #[test]
    fn corner_resize_keeps_authored_aspect_when_pointer_is_off_axis() {
        let t = compute_drag_transform(
            &drag(TitleObjectHandle::CornerTopLeft),
            -0.1,
            -0.01,
            (0.0, 0.0),
            (-0.1, -0.01),
        );
        assert!((t.size.x / t.size.y - 2.0).abs() < 1e-5);
        assert!((t.center.x - 0.45).abs() < 1e-5);
        assert!((t.center.y - 0.475).abs() < 1e-5);
    }

    #[test]
    fn edge_drag_clamps_minimum_size() {
        // Try to drag the right edge past the left edge — the math
        // must clamp size at the minimum so the box doesn't invert.
        let t = compute_drag_transform(&drag(TitleObjectHandle::EdgeRight), -10.0, 0.0, (0.0, 0.0), (-10.0, 0.0));
        assert!(t.size.x > 0.0, "size should never go negative");
        assert!(t.size.x >= 0.01 - 1e-6, "size clamped to minimum");
    }

    #[test]
    fn anchor_drag_translates_anchor_within_box() {
        // Initial anchor at (0.5, 0.5) — box centre. Drag by half
        // the box's width in X — anchor_norm.x should move by
        // dnx / size.x = 0.2 / 0.4 = 0.5 → ends at 1.0 (the right
        // edge of the box, clamped).
        let d = drag(TitleObjectHandle::Anchor);
        let t = compute_drag_transform(&d, 0.2, 0.0, (0.0, 0.0), (0.2, 0.0));
        assert!((t.anchor_norm.x - 1.0).abs() < 1e-5);
        assert!((t.anchor_norm.y - 0.5).abs() < 1e-5);
        // Object's size / centre / rotation untouched.
        assert!((t.size.x - 0.4).abs() < 1e-5);
        assert!((t.center.x - 0.5).abs() < 1e-5);
    }

    #[test]
    fn rotation_drag_changes_angle() {
        // Start pointer directly above centre, drag to directly
        // right of centre → 90° change CCW (atan2 convention).
        let d = drag(TitleObjectHandle::Rotation);
        let cx = d.initial_transform.center.x;
        let cy = d.initial_transform.center.y;
        let start = (cx, cy - 0.2);
        let end = (cx + 0.2, cy);
        let dnx = end.0 - start.0;
        let dny = end.1 - start.1;
        let t = compute_drag_transform(&d, dnx, dny, start, end);
        assert!((t.rotation - 90.0).abs() < 1.0, "got rotation {}", t.rotation);
    }
}

#[cfg(test)]
mod text_editor_tests {
    use super::*;

    // ── char_to_byte tests ──

    #[test]
    fn char_to_byte_ascii() {
        assert_eq!(char_to_byte("Hello", 0), 0);
        assert_eq!(char_to_byte("Hello", 1), 1);
        assert_eq!(char_to_byte("Hello", 4), 4);
        assert_eq!(char_to_byte("Hello", 5), 5); // past end
    }

    #[test]
    fn char_to_byte_unicode() {
        let s = "Héllo"; // é is 2 bytes
        assert_eq!(char_to_byte(s, 0), 0); // H
        assert_eq!(char_to_byte(s, 1), 1); // é
        assert_eq!(char_to_byte(s, 2), 3); // l (byte 3, not 2!)
        assert_eq!(char_to_byte(s, 3), 4); // l
        assert_eq!(char_to_byte(s, 4), 5); // o
        assert_eq!(char_to_byte(s, 5), 6); // past end
    }

    #[test]
    fn char_to_byte_past_end() {
        assert_eq!(char_to_byte("Hi", 99), 2); // returns s.len()
        assert_eq!(char_to_byte("", 0), 0);
    }

    // ── Text insertion simulation ──
    // This simulates exactly what the editor does when typing

    fn sim_insert(text: &mut String, cursor: &mut usize, ch: &str) {
        let bp = char_to_byte(text, *cursor);
        text.insert_str(bp, ch);
        *cursor += ch.chars().count();
    }

    fn sim_backspace(text: &mut String, cursor: &mut usize) {
        if *cursor > 0 {
            *cursor -= 1;
            let bp = char_to_byte(text, *cursor);
            if let Some(ch) = text[bp..].chars().next() {
                text.replace_range(bp..bp + ch.len_utf8(), "");
            }
        }
    }

    fn sim_arrow_left(cursor: &mut usize) {
        if *cursor > 0 { *cursor -= 1; }
    }

    fn sim_arrow_right(text: &str, cursor: &mut usize) {
        let cc = text.chars().count();
        if *cursor < cc { *cursor += 1; }
    }

    #[test]
    fn test_type_at_end() {
        let mut text = "Hello".to_string();
        let mut cursor = 5; // end of text
        sim_insert(&mut text, &mut cursor, " ");
        sim_insert(&mut text, &mut cursor, "W");
        sim_insert(&mut text, &mut cursor, "o");
        sim_insert(&mut text, &mut cursor, "r");
        sim_insert(&mut text, &mut cursor, "l");
        sim_insert(&mut text, &mut cursor, "d");
        assert_eq!(text, "Hello World");
        assert_eq!(cursor, 11);
    }

    #[test]
    fn test_type_at_beginning() {
        let mut text = "World".to_string();
        let mut cursor = 0; // beginning
        sim_insert(&mut text, &mut cursor, "H");
        assert_eq!(text, "HWorld");
        assert_eq!(cursor, 1);
        sim_insert(&mut text, &mut cursor, "i");
        assert_eq!(text, "HiWorld");
        assert_eq!(cursor, 2);
        sim_insert(&mut text, &mut cursor, " ");
        assert_eq!(text, "Hi World");
        assert_eq!(cursor, 3);
    }

    #[test]
    fn test_type_in_middle() {
        let mut text = "Hllo".to_string();
        let mut cursor = 1; // after H
        sim_insert(&mut text, &mut cursor, "e");
        assert_eq!(text, "Hello");
        assert_eq!(cursor, 2);
    }

    #[test]
    fn test_navigate_and_type() {
        let mut text = "Hello World".to_string();
        let mut cursor = 11; // end
        // Navigate left 6 times to position after "Hello"
        for _ in 0..6 { sim_arrow_left(&mut cursor); }
        assert_eq!(cursor, 5);
        // Insert " Beautiful"
        sim_insert(&mut text, &mut cursor, " ");
        sim_insert(&mut text, &mut cursor, "B");
        sim_insert(&mut text, &mut cursor, "e");
        sim_insert(&mut text, &mut cursor, "a");
        sim_insert(&mut text, &mut cursor, "u");
        sim_insert(&mut text, &mut cursor, "t");
        sim_insert(&mut text, &mut cursor, "i");
        sim_insert(&mut text, &mut cursor, "f");
        sim_insert(&mut text, &mut cursor, "u");
        sim_insert(&mut text, &mut cursor, "l");
        assert_eq!(text, "Hello Beautiful World");
        assert_eq!(cursor, 15);
    }

    #[test]
    fn test_backspace() {
        let mut text = "Hello".to_string();
        let mut cursor = 5;
        sim_backspace(&mut text, &mut cursor);
        assert_eq!(text, "Hell");
        assert_eq!(cursor, 4);
        sim_backspace(&mut text, &mut cursor);
        assert_eq!(text, "Hel");
        assert_eq!(cursor, 3);
    }

    #[test]
    fn test_backspace_at_beginning() {
        let mut text = "Hello".to_string();
        let mut cursor = 0;
        sim_backspace(&mut text, &mut cursor); // should do nothing
        assert_eq!(text, "Hello");
        assert_eq!(cursor, 0);
    }

    #[test]
    fn test_navigate_backspace_type() {
        let mut text = "Hello World".to_string();
        let mut cursor = 11;
        // Go back to position 5 (before " World")
        for _ in 0..6 { sim_arrow_left(&mut cursor); }
        assert_eq!(cursor, 5);
        // Backspace: delete 'o'
        sim_backspace(&mut text, &mut cursor);
        assert_eq!(text, "Hell World");
        assert_eq!(cursor, 4);
        // Type 'o'
        sim_insert(&mut text, &mut cursor, "o");
        assert_eq!(text, "Hello World");
        assert_eq!(cursor, 5);
    }

    #[test]
    fn test_selection_delete_and_type() {
        let mut text = "Hello World".to_string();
        let sel = Some((5, 11)); // select " World"
        // Simulate typing "!" with selection active (replaces selection)
        if let Some((ss, se)) = sel {
            let bs = char_to_byte(&text, ss);
            let be = char_to_byte(&text, se);
            text.replace_range(bs..be, "!");
            let cursor = ss + 1;
            assert_eq!(text, "Hello!");
            assert_eq!(cursor, 6);
        }
    }

    #[test]
    fn test_newline_insertion() {
        let mut text = "HelloWorld".to_string();
        let mut cursor = 5;
        sim_insert(&mut text, &mut cursor, "\n");
        assert_eq!(text, "Hello\nWorld");
        assert_eq!(cursor, 6);
    }

    // ── find_nearest_char_index tests ──

    fn make_positions(chars: &str, x_start: f32, advance: f32, y: f32, height: f32) -> Vec<klipp_title::CharPosition> {
        let mut positions = Vec::new();
        for (i, (bi, _)) in chars.char_indices().enumerate() {
            positions.push(klipp_title::CharPosition {
                char_index: i,
                byte_offset: bi,
                x: x_start + i as f32 * advance,
                y,
                advance,
                height,
                rtl: false,
                vertical: false,
            });
        }
        // End-of-text position
        let cc = chars.chars().count();
        positions.push(klipp_title::CharPosition {
            char_index: cc,
            byte_offset: chars.len(),
            x: x_start + cc as f32 * advance,
            y,
            advance: 0.0,
            height,
            rtl: false,
            vertical: false,
        });
        positions
    }

    #[test]
    fn find_nearest_click_before_text() {
        let positions = make_positions("Hello", 100.0, 20.0, 50.0, 30.0);
        let idx = find_nearest_char_index(&positions, 80.0, 60.0); // before first char
        assert_eq!(idx, 0);
    }

    #[test]
    fn find_nearest_click_on_first_char_left_half() {
        let positions = make_positions("Hello", 100.0, 20.0, 50.0, 30.0);
        let idx = find_nearest_char_index(&positions, 105.0, 60.0); // left half of 'H'
        assert_eq!(idx, 0);
    }

    #[test]
    fn find_nearest_click_on_first_char_right_half() {
        let positions = make_positions("Hello", 100.0, 20.0, 50.0, 30.0);
        let idx = find_nearest_char_index(&positions, 115.0, 60.0); // right half of 'H'
        assert_eq!(idx, 1);
    }

    #[test]
    fn find_nearest_click_on_middle_char() {
        let positions = make_positions("Hello", 100.0, 20.0, 50.0, 30.0);
        // 'l' (index 2) is at x=140..160. Click at x=155 (right half)
        let idx = find_nearest_char_index(&positions, 155.0, 60.0);
        assert_eq!(idx, 3);
    }

    #[test]
    fn find_nearest_click_after_text() {
        let positions = make_positions("Hello", 100.0, 20.0, 50.0, 30.0);
        let idx = find_nearest_char_index(&positions, 300.0, 60.0); // way past end
        // Last visible char is 'o' at index 4. Clicking past end returns 4+1 = 5.
        // 5 = char_count = valid end-of-text cursor position.
        assert_eq!(idx, 5);
    }

    #[test]
    fn find_nearest_multiline() {
        let mut positions = make_positions("Hello", 100.0, 20.0, 50.0, 30.0);
        // Remove end-of-text position, add second line
        positions.pop();
        let cc1 = 5;
        // Add newline position
        positions.push(klipp_title::CharPosition {
            char_index: 5,
            byte_offset: 5,
            x: 200.0, // end of line 1
            y: 50.0,
            advance: 0.0,
            height: 30.0,
            rtl: false,
            vertical: false,
        });
        // Second line: "World" at y=90
        for (i, (bi, _)) in "World".char_indices().enumerate() {
            positions.push(klipp_title::CharPosition {
                char_index: cc1 + 1 + i,
                byte_offset: 6 + bi,
                x: 100.0 + i as f32 * 20.0,
                y: 90.0,
                advance: 20.0,
                height: 30.0,
                rtl: false,
                vertical: false,
            });
        }
        // End of text
        positions.push(klipp_title::CharPosition {
            char_index: 11,
            byte_offset: 11,
            x: 200.0,
            y: 90.0,
            advance: 0.0,
            height: 30.0,
            rtl: false,
            vertical: false,
        });

        // Click on first line
        let idx = find_nearest_char_index(&positions, 105.0, 60.0);
        assert!(idx <= 1, "Click on line 1 should give index 0-1, got {}", idx);

        // Click on second line
        let idx = find_nearest_char_index(&positions, 105.0, 100.0);
        assert!((6..=7).contains(&idx), "Click on line 2 should give index 6-7, got {}", idx);
    }

    // ── Full editor simulation with char_positions ──

    #[test]
    fn test_full_editor_with_positions() {
        let mut renderer = klipp_title::TitleRenderer::new();
        let mut text = "Hello World".to_string();
        let mut cursor = text.chars().count(); // start at end

        // Compute positions
        let td = {
    let tb = klipp_timeline::TextBox {
        text: text.clone(),
        font_size: 72.0,
        ..Default::default()
    };
    
    klipp_timeline::TitleData::single_text(tb)
};
        let positions = renderer.compute_char_positions(&td, 1920, 1080);

        // Verify we have correct number of positions
        assert_eq!(positions.len(), 12, "11 chars + 1 end position");

        // Navigate left 6 chars (to position 5)
        for _ in 0..6 { sim_arrow_left(&mut cursor); }
        assert_eq!(cursor, 5);

        // Insert "," at cursor
        sim_insert(&mut text, &mut cursor, ",");
        assert_eq!(text, "Hello, World");
        assert_eq!(cursor, 6);

        // Navigate right 6 chars (to position 12, end)
        for _ in 0..6 { sim_arrow_right(&text, &mut cursor); }
        assert_eq!(cursor, 12);

        // Type "!" at end
        sim_insert(&mut text, &mut cursor, "!");
        assert_eq!(text, "Hello, World!");
        assert_eq!(cursor, 13);
    }

    /// Simulates the full round-trip: type chars, update title data, recompute positions,
    /// verify positions match the text at every step.
    #[test]
    fn test_full_roundtrip_typing() {
        let mut renderer = klipp_title::TitleRenderer::new();
        let mut text = String::new();
        let mut cursor = 0usize;

        let input = "Ok, this is interesting";
        for ch in input.chars() {
            // Type the character
            sim_insert(&mut text, &mut cursor, &ch.to_string());

            // Simulate UpdateTitleData: create TitleData with new text
            let td = {
    let tb = klipp_timeline::TextBox {
        text: text.clone(),
        font_size: 72.0,
        ..Default::default()
    };
    
    klipp_timeline::TitleData::single_text(tb)
};

            // Recompute char positions (as upload_title_overlay would)
            let positions = renderer.compute_char_positions(&td, 1920, 1080);

            // Verify: positions should have text.chars().count() + 1 entries
            let cc = text.chars().count();
            assert_eq!(positions.len(), cc + 1,
                "After typing '{}', text='{}': expected {} positions, got {}",
                ch, text, cc + 1, positions.len());

            // Verify: sequential char_indices
            for (i, cp) in positions.iter().enumerate() {
                assert_eq!(cp.char_index, i,
                    "After typing '{}', text='{}': position {} has wrong char_index {}",
                    ch, text, i, cp.char_index);
            }

            // Verify: cursor is valid
            assert!(cursor <= cc,
                "After typing '{}', text='{}': cursor {} exceeds char_count {}",
                ch, text, cursor, cc);

            // Verify: char_to_byte at cursor matches
            let cursor_byte = char_to_byte(&text, cursor);
            assert!(cursor_byte <= text.len(),
                "After typing '{}', text='{}': cursor_byte {} exceeds text.len() {}",
                ch, text, cursor_byte, text.len());
        }

        assert_eq!(text, "Ok, this is interesting");
        assert_eq!(cursor, 23);
    }

    /// Test that navigating left then typing inserts correctly
    #[test]
    fn test_roundtrip_navigate_and_type() {
        let mut renderer = klipp_title::TitleRenderer::new();
        let mut text = "Hello World".to_string();
        let mut cursor = text.chars().count(); // 11, end

        // Navigate left 6 chars
        for _ in 0..6 { sim_arrow_left(&mut cursor); }
        assert_eq!(cursor, 5);

        // Type "Beautiful "
        for ch in "Beautiful ".chars() {
            sim_insert(&mut text, &mut cursor, &ch.to_string());

            let td = {
    let tb = klipp_timeline::TextBox {
        text: text.clone(),
        font_size: 72.0,
        ..Default::default()
    };
    
    klipp_timeline::TitleData::single_text(tb)
};
            let positions = renderer.compute_char_positions(&td, 1920, 1080);

            let cc = text.chars().count();
            assert_eq!(positions.len(), cc + 1);
            for (i, cp) in positions.iter().enumerate() {
                assert_eq!(cp.char_index, i);
            }
        }

        assert_eq!(text, "HelloBeautiful  World");
        // Hmm that's wrong, we want "Hello Beautiful World"
        // The cursor was at position 5 (between 'o' and ' ')
        // Inserting "B" at 5 gives "HelloB World" not "Hello B World"
        // That's correct! Position 5 is BEFORE the space.
    }

    /// Simulate the EXACT egui event flow: each keypress generates both
    /// Event::Key AND Event::Text. Verify no double-processing occurs.
    #[test]
    fn test_event_processing_no_double_insert() {
        // Simulate what egui delivers when user types 'a':
        // [Event::Key { key: Key::A, pressed: true }, Event::Text("a")]
        // Our handler should only process Event::Text, not Event::Key for letter keys.

        let mut text = "Hello".to_string();
        let mut cursor = 5usize; // end

        // Simulate processing events as the viewer does
        let events = vec![
            // egui delivers Key event first, then Text event
            ("key_a", false), // Event::Key { key: A } — should NOT insert
            ("text_a", true), // Event::Text("a") — should insert
        ];

        for (event_type, _should_insert) in events {
            if let Some(ch) = event_type.strip_prefix("text_") {
                // Process Event::Text
                // "a"
                let bp = char_to_byte(&text, cursor);
                text.insert_str(bp, ch);
                cursor += ch.chars().count();
            }
            // Event::Key for letter keys falls through to _ => {} (no action)
        }

        assert_eq!(text, "Helloa");
        assert_eq!(cursor, 6);
    }

    /// Verify that typing a full sentence produces correct output
    #[test]
    fn test_type_full_sentence() {
        let mut text = String::new();
        let mut cursor = 0usize;

        let sentence = "Ok, this is interesting";
        for ch in sentence.chars() {
            sim_insert(&mut text, &mut cursor, &ch.to_string());
        }

        assert_eq!(text, sentence);
        assert_eq!(cursor, sentence.chars().count());
    }

    #[test]
    fn test_mouse_click_then_type() {
        let mut renderer = klipp_title::TitleRenderer::new();
        let text = "Hello World";
        let td = {
    let tb = klipp_timeline::TextBox {
        text: text.to_string(),
        font_size: 72.0,
        ..Default::default()
    };
    
    klipp_timeline::TitleData::single_text(tb)
};
        let positions = renderer.compute_char_positions(&td, 1920, 1080);

        // Verify positions are sequential
        for (i, cp) in positions.iter().enumerate() {
            assert_eq!(cp.char_index, i, "char_index mismatch at {}", i);
        }

        // Simulate click on the 'W' character (index 6)
        let w_pos = &positions[6];
        let click_x = w_pos.x + w_pos.advance * 0.3; // left half → cursor before W
        let click_y = w_pos.y + w_pos.height * 0.5;
        let nearest = find_nearest_char_index(&positions, click_x, click_y);
        assert_eq!(nearest, 6, "Click on left half of 'W' should give index 6, got {}", nearest);

        // Now insert at this position
        let mut text = text.to_string();
        let mut cursor = nearest;
        sim_insert(&mut text, &mut cursor, "B");
        assert_eq!(text, "Hello BWorld");
        assert_eq!(cursor, 7);
    }
}

#[cfg(test)]
mod master_meter_tests {
    use super::{master_meter_fraction, master_meter_is_clipping};

    fn from_db(db: f32) -> f32 {
        10.0f32.powf(db / 20.0)
    }

    /// The ends of the scale are where they say they are.
    #[test]
    fn the_scale_runs_from_the_floor_to_full_scale() {
        assert_eq!(master_meter_fraction(0.0), 0.0, "silence");
        assert!(master_meter_fraction(from_db(-60.0)).abs() < 1e-4, "the floor");
        assert!((master_meter_fraction(1.0) - 1.0).abs() < 1e-4, "0 dBFS");
        // Over full scale pins at the end rather than running off it.
        assert_eq!(master_meter_fraction(4.0), 1.0);
        // A negative sample magnitude should not happen, and must not
        // produce a negative width if it does.
        assert_eq!(master_meter_fraction(-0.5), 0.0);
    }

    /// The point of the scale: it is linear in decibels, so the range a
    /// dialogue editor lives in gets half the meter.
    ///
    /// On an amplitude-linear scale -30 dBFS sits at 3% of the width
    /// and -18 at 12%, which puts every level worth watching in the
    /// first eighth of the bar and spends the rest on the top 12 dB.
    #[test]
    fn the_scale_is_linear_in_decibels_not_amplitude() {
        // Halfway along the bar is halfway down the range.
        assert!((master_meter_fraction(from_db(-30.0)) - 0.5).abs() < 1e-3);
        // The usual dialogue target, and the mark the bar changes at.
        assert!((master_meter_fraction(from_db(-18.0)) - 0.7).abs() < 1e-3);
        assert!((master_meter_fraction(from_db(-6.0)) - 0.9).abs() < 1e-3);

        // And the mistake, on the record: amplitude would put -30 dB at
        // 3% rather than 50%.
        let amplitude_scale = from_db(-30.0);
        assert!(amplitude_scale < 0.04, "sanity: {amplitude_scale}");
        assert!(
            master_meter_fraction(from_db(-30.0)) > amplitude_scale * 10.0,
            "the dB scale must give the quiet end real width",
        );
    }

    /// Louder is always further along; there is no level where the bar
    /// goes backwards.
    #[test]
    fn the_bar_only_grows() {
        let mut last = -1.0;
        for i in 0..=1200 {
            let db = -60.0 + i as f32 * 0.05;
            let f = master_meter_fraction(from_db(db));
            assert!(f >= last - 1e-6, "dips at {db} dB");
            assert!((0.0..=1.0).contains(&f), "leaves the bar at {db} dB");
            last = f;
        }
    }

    /// Clipping is at or over full scale — not "close to it", which
    /// would cry wolf on a correctly hot mix.
    #[test]
    fn clipping_is_full_scale_and_not_before() {
        assert!(master_meter_is_clipping(1.0));
        assert!(master_meter_is_clipping(1.5));
        assert!(!master_meter_is_clipping(0.999));
        assert!(!master_meter_is_clipping(from_db(-0.1)));
        assert!(!master_meter_is_clipping(0.0));
    }
}

#[cfg(test)]
mod fitted_backing_editor_tests {
    use super::*;
    use klipp_timeline::{default_text_transform, Shape, ShapeFit, ShapeKind, TextBox, TitleData, TitleObjectContent, TitlePosition};

    #[test]
    fn fitted_backing_selection_matches_render_and_cannot_start_transform_drag() {
        let mut title = TitleData::single_text(TextBox { text: "Name".into(), ..Default::default() });
        let target = title.objects[0].id;
        let fit = ShapeFit { target, padding: TitlePosition { x: 20.0, y: 12.0 } };
        let backing = title.add_object(TitleObjectContent::Shape(Shape {
            kind: ShapeKind::Rectangle { corner_radius: 0.0 }, fit: Some(fit), fill: Some([0, 0, 0, 255]), gradient: None, stroke: None,
        }), default_text_transform());
        let mut cache = None;
        let resolved = editor_title_geometry(&title, (960, 540), 0.0, &mut cache);
        let object = resolved.object(backing).unwrap();
        let expected = klipp_title::TitleRenderer::new().resolved_shape_fit(&title, fit, 960, 540, None).unwrap();
        assert_eq!(object.transform, expected);
        let frame = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(960.0, 540.0));
        let bb = object_screen_bb(object, frame);
        assert!((bb.width() - expected.size.x * 960.0).abs() < 0.001);
        assert!(bb.width() < frame.width());
        assert!(hit_test_handles(object, frame, bb.min).is_none());
        assert!(hit_test_handles(object, frame, bb.center()).is_none());

        // Changing words invalidates the geometry cache, as does deleting
        // the target: a suppressed backing must not leave a selectable box.
        title.primary_text_mut().unwrap().text = "A much longer name".into();
        let longer = editor_title_geometry(&title, (960, 540), 0.0, &mut cache);
        assert!(longer.object(backing).unwrap().transform.size.x > expected.size.x);
        title.objects.retain(|o| o.id != target);
        let missing = editor_title_geometry(&title, (960, 540), 0.0, &mut cache);
        assert!(!missing.object(backing).unwrap().visible);
    }

    #[test]
    fn fitted_backing_position_animation_reuses_resting_geometry() {
        let mut title = TitleData::single_text(TextBox { text: "Name".into(), ..Default::default() });
        let target = title.objects[0].id;
        title.objects[0].transform.center_x_keyframes = vec![
            klipp_core::Keyframe { time_secs: 0.0, value: 0.3, interpolation: klipp_core::Interpolation::Linear, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() },
            klipp_core::Keyframe { time_secs: 1.0, value: 0.7, interpolation: klipp_core::Interpolation::Linear, tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() },
        ];
        let fit = ShapeFit { target, padding: TitlePosition { x: 20.0, y: 12.0 } };
        let backing = title.add_object(TitleObjectContent::Shape(Shape {
            kind: ShapeKind::Rectangle { corner_radius: 0.0 }, fit: Some(fit), fill: None, gradient: None, stroke: None,
        }), default_text_transform());
        let mut cache = None;
        let first = editor_title_geometry(&title, (960, 540), 0.0, &mut cache);
        let allocation = cache.as_ref().unwrap().transforms.as_ptr();
        let last = editor_title_geometry(&title, (960, 540), 1.0, &mut cache);
        assert_eq!(allocation, cache.as_ref().unwrap().transforms.as_ptr());
        let delta = last.object(backing).unwrap().transform.center.x - first.object(backing).unwrap().transform.center.x;
        assert!((delta - 0.4).abs() < 0.001);
    }

    #[test]
    fn text_editor_frame_hugs_single_line_glyphs() {
        let title = TitleData::single_text(TextBox {
            text: "A day at the beach".into(),
            font_size: 72.0,
            ..Default::default()
        });
        let frame = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0));
        let mut fitted = title.clone();
        tighten_text_editor_boxes(&mut fitted, (1920, 1080));
        let object = &fitted.objects[0];
        let size = object.transform.size;
        assert!(size.x < 0.6, "single-line text should not inherit the full-frame layout width");
        assert!(size.y < 0.25, "single-line text should not inherit the full-frame layout height");
        let bb = object_screen_bb(object, frame);
        assert!(bb.width() < frame.width() * 0.6);
        assert!(bb.height() < frame.height() * 0.25);
    }

    #[test]
    fn text_editor_frame_keeps_wrapped_lines_inside_authored_box() {
        let title = TitleData::single_text(TextBox {
            text: "one two three four five six seven eight".into(),
            font_size: 72.0,
            ..Default::default()
        });
        let mut fitted = title.clone();
        tighten_text_editor_boxes(&mut fitted, (1920, 1080));
        let object = &fitted.objects[0];
        let size = object.transform.size;
        assert!(size.x <= object.transform.size.x + 1e-6);
        assert!(size.y <= object.transform.size.y + 1e-6);
    }
}

/// How far the pointer must move from where it was pressed on a handle
/// before the press is a drag. A click alone must not edit the clip.
const TRANSFORM_DRAG_THRESHOLD: f32 = 3.0;

fn drag_has_started(from: egui::Pos2, to: egui::Pos2) -> bool {
    from.distance(to) > TRANSFORM_DRAG_THRESHOLD
}

/// What a viewer handle gesture does this frame, decided by
/// [`gizmo_step`] from the pointer alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GizmoStep {
    /// The press is held short of the threshold: keep waiting.
    Nothing,
    /// The pointer has crossed the threshold: add the effect if the
    /// clip has none, open the undo gesture, and keep the drag.
    Start,
    /// The drag goes on: write the pointer's transform.
    Update,
    /// Released past the threshold: write the final transform and close
    /// the gesture. When it never opened (pressed, moved and released
    /// within one frame, which egui coalesces), open it first.
    End,
    /// The gesture is over and leaves nothing behind: released back at
    /// its start, the pointer gone from the window, or its release
    /// never seen. One that opened is taken back; one that did not (a
    /// click) has nothing to take back.
    Abandon,
}

/// The state machine behind the transform gizmo and the corner-pin
/// handles: given a drag that is armed (`started` false) or opened,
/// where it was pressed, and the pointer this frame, which step to
/// take. A press alone must not edit the clip; a drag that comes back
/// to where it started must not either; and a drag must not be lost
/// because it was over within one frame (#408) or its release was
/// never delivered here (#409).
fn gizmo_step(started: bool, start: egui::Pos2, pointer: Option<egui::Pos2>, down: bool, released: bool) -> GizmoStep {
    let Some(pointer) = pointer else { return GizmoStep::Abandon };
    let moved = drag_has_started(start, pointer);
    if released {
        if moved { GizmoStep::End } else { GizmoStep::Abandon }
    } else if down {
        if started {
            GizmoStep::Update
        } else if moved {
            GizmoStep::Start
        } else {
            GizmoStep::Nothing
        }
    } else {
        GizmoStep::Abandon
    }
}

/// Where the rotation handle goes: `distance` above the top edge's
/// middle, turned with the clip — or, when that would leave `inside`
/// (the viewer, less the handle's own size), the same distance below
/// the edge, inside the box, where it can still be reached.
fn rotation_handle_pos(top_mid: egui::Pos2, pivot: egui::Pos2, rotation_rad: f32, distance: f32, inside: egui::Rect) -> egui::Pos2 {
    let above = rotate_point(egui::pos2(top_mid.x, top_mid.y - distance), pivot, rotation_rad);
    if inside.contains(above) {
        above
    } else {
        rotate_point(egui::pos2(top_mid.x, top_mid.y + distance), pivot, rotation_rad)
    }
}

#[cfg(test)]
mod transform_gizmo_tests {
    use super::*;

    #[test]
    fn a_press_that_does_not_move_is_not_a_drag() {
        let p = egui::pos2(100.0, 100.0);
        assert!(!drag_has_started(p, p));
        assert!(!drag_has_started(p, egui::pos2(102.0, 101.0)));
        assert!(drag_has_started(p, egui::pos2(104.0, 100.0)));
    }

    /// #322: a portrait picture fills the viewer's height, so its top
    /// edge is the viewer's top; the handle goes inside rather than
    /// over the page tabs above.
    #[test]
    fn the_rotation_handle_stays_inside_the_viewer() {
        let viewer = egui::Rect::from_min_max(egui::pos2(0.0, 40.0), egui::pos2(800.0, 600.0));
        let top_mid = egui::pos2(400.0, 40.0);
        let pivot = egui::pos2(400.0, 320.0);
        let h = rotation_handle_pos(top_mid, pivot, 0.0, 25.0, viewer.shrink(7.0));
        assert!(viewer.contains(h), "the handle is outside the viewer at {h:?}");
        assert_eq!(h, egui::pos2(400.0, 65.0));
        // With room above the box, it stays where it always was.
        let roomy = rotation_handle_pos(egui::pos2(400.0, 200.0), pivot, 0.0, 25.0, viewer.shrink(7.0));
        assert_eq!(roomy, egui::pos2(400.0, 175.0));
    }

    // ── #408: the gesture state machine ──────────────────────────────
    //
    // `gizmo_step(started, start, pointer, down, released)`. A drag is
    // armed on the press, at `start`; `started` says whether it has
    // opened (crossed the threshold on an earlier frame).

    const START: egui::Pos2 = egui::pos2(100.0, 100.0);
    fn at(dx: f32) -> Option<egui::Pos2> {
        Some(egui::pos2(START.x + dx, START.y))
    }

    /// Pressed, held with a twitch, released: nothing opens, and the
    /// release drops the arm. Neither Add nor Begin nor End is emitted.
    #[test]
    fn a_click_on_a_handle_opens_nothing() {
        assert_eq!(gizmo_step(false, START, at(0.0), true, false), GizmoStep::Nothing);
        assert_eq!(gizmo_step(false, START, at(2.0), true, false), GizmoStep::Nothing, "a twitch short of the threshold keeps waiting");
        assert_eq!(gizmo_step(false, START, at(2.0), false, true), GizmoStep::Abandon, "released short of it: the arm is dropped");
    }

    /// The ordinary drag, one frame per stage: the threshold opens the
    /// gesture, holding updates it, releasing away from the start ends it.
    #[test]
    fn a_press_move_release_across_frames_is_start_update_end() {
        assert_eq!(gizmo_step(false, START, at(10.0), true, false), GizmoStep::Start);
        assert_eq!(gizmo_step(true, START, at(20.0), true, false), GizmoStep::Update);
        assert_eq!(gizmo_step(true, START, at(30.0), false, true), GizmoStep::End);
    }

    /// Pressed on frame N; moved 40 px and released within frame N+1,
    /// which egui delivers as one frame. The release applies the
    /// threshold itself, so this is a drag: End, with the gesture
    /// opened first. Before, "released and not started" was tested
    /// first and the flick was a click that moved nothing.
    #[test]
    fn a_one_frame_flick_is_still_a_drag() {
        assert_eq!(gizmo_step(false, START, at(40.0), false, true), GizmoStep::End);
    }

    /// Opened, then released back within the threshold of the press:
    /// the gesture is taken back, and with it the Transform it added.
    #[test]
    fn a_drag_released_back_at_its_start_is_taken_back() {
        assert_eq!(gizmo_step(true, START, at(2.0), false, true), GizmoStep::Abandon);
        assert_eq!(gizmo_step(true, START, at(4.0), false, true), GizmoStep::End, "past the threshold it is an edit");
    }

    /// The release was never delivered here (the gizmo was not drawn on
    /// that frame), or the pointer left the window: the gesture cannot
    /// close as an edit and must not stay open (#409).
    #[test]
    fn a_missed_release_or_a_gone_pointer_takes_the_drag_back() {
        assert_eq!(gizmo_step(true, START, at(30.0), false, false), GizmoStep::Abandon, "neither down nor released");
        assert_eq!(gizmo_step(true, START, None, true, false), GizmoStep::Abandon, "pointer gone while held");
        assert_eq!(gizmo_step(false, START, None, true, false), GizmoStep::Abandon, "an armed press with the pointer gone is dropped");
        assert_eq!(gizmo_step(false, START, at(1.0), false, false), GizmoStep::Abandon, "an armed press whose release was missed is dropped");
    }
}

/// While a node is viewed, the program viewer says so in its corner,
/// with one click back to the finished picture (#1763): a node's output
/// taken for the picture would mislead every judgement made on it.
fn draw_viewed_node(ui: &mut egui::Ui, state: &AppState, image_rect: egui::Rect, cmds: &mut Vec<AppCommand>) {
    let Some(name) = state.solo_node.and(state.viewed_node_name.as_deref()) else { return };
    let text = format!("Viewing: {name}   \u{2715}");
    let font = egui::FontId::proportional(12.0);
    let galley = ui.painter().layout_no_wrap(text, font, egui::Color32::BLACK);
    let chip = egui::Rect::from_min_size(image_rect.min + egui::vec2(8.0, 8.0), galley.size() + egui::vec2(16.0, 8.0));
    let resp = ui.interact(chip, ui.id().with("viewed_node"), egui::Sense::click());
    let yellow = egui::Color32::from_rgb(0xff, 0xd5, 0x3a);
    ui.painter().rect_filled(chip, 4.0, if resp.hovered() { yellow } else { yellow.gamma_multiply(0.85) });
    ui.painter().galley(chip.min + egui::vec2(8.0, 4.0), galley, egui::Color32::BLACK);
    if resp.on_hover_text("Back to the finished picture (1)").clicked() {
        cmds.push(AppCommand::SetNodeSolo { at: None });
    }
}

#[cfg(test)]
mod viewed_node_tests {
    use super::*;

    /// While a node is viewed the program viewer names it in its corner,
    /// and a click there goes back to the finished picture (#1763); with
    /// nothing viewed there is no chip.
    #[test]
    fn the_viewer_names_the_viewed_node_and_clicks_back() {
        let ctx = egui::Context::default();
        let mut state = AppState::default();
        let image = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(640.0, 360.0));
        let run = |state: &AppState, events: Vec<egui::Event>| {
            let mut cmds = Vec::new();
            let out = ctx.run(egui::RawInput { screen_rect: Some(image), events, ..Default::default() }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| draw_viewed_node(ui, state, image, &mut cmds));
            });
            let texts: Vec<String> = out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some(t.galley.text().to_owned()), _ => None }).collect();
            (texts, cmds)
        };
        let (texts, _) = run(&state, Vec::new());
        assert!(!texts.iter().any(|t| t.starts_with("Viewing")), "nothing viewed, no chip: {texts:?}");
        state.solo_node = Some(klipp_nodes::NodeId::new_v4());
        state.viewed_node_name = Some("Film Grain".into());
        let (texts, _) = run(&state, Vec::new());
        assert!(texts.iter().any(|t| t.starts_with("Viewing: Film Grain")), "{texts:?}");
        let at = egui::pos2(20.0, 16.0);
        run(&state, vec![egui::Event::PointerMoved(at), egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }]);
        let (_, cmds) = run(&state, vec![egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }]);
        assert!(matches!(cmds.as_slice(), [AppCommand::SetNodeSolo { at: None }]), "{cmds:?}");
    }
}

#[cfg(test)]
mod title_view_tests {
    use super::TitleView;

    /// #2014: the placed canvas and the pointer's way back are each other's
    /// inverse, for a turn, an unequal scale and an anchor off centre; and
    /// unplaced, both are the identity.
    #[test]
    fn placing_and_unplacing_a_point_round_trip() {
        let rect = egui::Rect::from_min_size(egui::pos2(40.0, 30.0), egui::vec2(960.0, 540.0));
        let view = TitleView { image_rect: rect, placement: Some((120.0, -80.0, 0.6, 1.4, 33.0, 0.3, 0.7)), frame: (1920.0, 1080.0) };
        for p in [egui::pos2(40.0, 30.0), egui::pos2(500.0, 300.0), egui::pos2(1000.0, 570.0), egui::pos2(-200.0, 900.0)] {
            let back = view.unplace(view.place(p));
            assert!((back - p).length() < 1e-2, "{p:?} came back {back:?}");
            let there = view.place(view.unplace(p));
            assert!((there - p).length() < 1e-2, "{p:?} went to {there:?}");
        }
        assert!((view.place(egui::pos2(500.0, 300.0)) - egui::pos2(500.0, 300.0)).length() > 10.0, "the placement moves points");
        let flat = TitleView::flat(rect);
        assert_eq!(flat.place(egui::pos2(123.0, 45.0)), egui::pos2(123.0, 45.0));
        assert_eq!(flat.unplace(egui::pos2(123.0, 45.0)), egui::pos2(123.0, 45.0));
        assert_eq!(flat.scale(), 1.0);
        // A pure move: a canvas point goes as far on screen, in proportion.
        let moved = TitleView { image_rect: rect, placement: Some((192.0, 0.0, 1.0, 1.0, 0.0, 0.5, 0.5)), frame: (1920.0, 1080.0) };
        let d = moved.to_screen(0.5, 0.5) - TitleView::flat(rect).to_screen(0.5, 0.5);
        assert!((d.x - 96.0).abs() < 1e-3 && d.y.abs() < 1e-3, "{d:?}");
    }
}
