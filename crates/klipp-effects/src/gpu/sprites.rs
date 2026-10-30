//! Titles and sprites on the GPU (#2890): a title's pixels and glyphs
//! placed as layers, the resident sprites they come from, and the metal
//! look's shot pass, with their params and shaders. The public shapes
//! (`GlyphPlace`, `SpriteShape`, `SpriteBox`) stay in `gpu`.

use super::*;

impl GpuEffectsPipeline {
    /// Reserve a temporary title object slot after the frame's timeline slots.
    /// The caller reuses the returned index for every object in this title.
    pub fn stack_title_object_slot(&mut self, offset: usize) -> Option<usize> {
        if self.stack_buf_size == 0 || self.is_device_lost() { return None; }
        if offset > 3 { return None; }
        let index = self.stack_title_slot + offset;
        while self.stack_layers.len() <= index {
            self.stack_layers.push(std::sync::Arc::new(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("title object"), size: self.stack_buf_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })));
        }
        Some(index)
    }

    /// A title layer kept apart for the length of a frame, past the scratch
    /// slots the title routes use while drawing (#3160): the layers a
    /// blurred title places at each of the shutter's instants. `None` past
    /// sixteen.
    pub fn stack_title_kept_slot(&mut self, index: usize) -> Option<usize> {
        if self.stack_buf_size == 0 || self.is_device_lost() || index >= 16 { return None; }
        let index = self.stack_title_slot + 4 + index;
        while self.stack_layers.len() <= index {
            self.stack_layers.push(std::sync::Arc::new(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("title kept layer"), size: self.stack_buf_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })));
        }
        Some(index)
    }

    /// Source-local title effects, with no CPU break route. The caller orders
    /// these before title post-processing and the clip grade.
    pub fn stack_title_source_effects(&mut self, slot: usize, effects: &[BuiltinEffect]) -> bool {
        if effects.is_empty() { return true; }
        if self.is_device_lost() || slot >= self.stack_layers.len() { return false; }
        let (w,h) = self.stack_dims;
        self.ensure_recording_buffers(w,h);
        let Some(a) = self.buf_a.as_ref() else { return false };
        if !self.stack_copy(&self.stack_layers[slot],a,"title effect seed") { return false; }
        self.begin_chain();
        let mut in_a=true;
        let mut ok=true;
        for effect in effects {
            match self.record_in_chain(std::slice::from_ref(effect),in_a,w,h) {
                Recorded::Done => in_a = !in_a,
                Recorded::Identity => {},
                _ => { ok=false; break; }
            }
        }
        self.submit_chain();
        ok && self.stack_layer_from_chain(slot,in_a)
    }

    /// Layer `i` takes an sRGB RGBA8 straight-alpha sprite from the CPU
    /// (a title, a generator): decoded to linear on the GPU, alpha kept.
    /// Layer `i` from sRGB-encoded half floats, decoded on the way in.
    ///
    /// The sibling of [`Self::stack_layer_from_srgb_u8`], for a source
    /// with more than eight bits to give. The decode stays here, where
    /// the u8 path's decode is: sRGB is a power curve, and a `powf` a
    /// channel across a 1080p frame is 6.2 million of them, which on
    /// the CPU cost more than the whole generator did — 88 ms a frame
    /// against 35 — and on the GPU costs nothing anyone can measure.
    ///
    /// Half floats rather than whole ones because the source is
    /// display-referred: eleven bits of mantissa against sRGB's eight
    /// is already far more than the picture can hold, and it is eight
    /// bytes a pixel instead of sixteen. Two halves ride in each u32,
    /// unpacked with `unpack2x16float`, so it asks nothing of the
    /// device that the eight-bit path does not.
    pub fn stack_layer_from_srgb_f16(&mut self, i: usize, packed: &[u32]) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() {
            return false;
        }
        let (width, height) = self.stack_dims;
        let pixel_count = self.stack_pixel_count();
        if packed.len() as u64 != pixel_count * 2 {
            return false;
        }
        let layer = std::sync::Arc::clone(&self.stack_layers[i]);
        let bytes = pixel_count * 8;
        if self.stack_sprite_f16.as_ref().is_none_or(|b| b.size() < bytes) {
            self.stack_sprite_f16 = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stack_sprite_f16"),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let sprite = self.stack_sprite_f16.as_ref().unwrap();
        self.upload(sprite, 0, bytemuck::cast_slice(packed));
        self.write_params(&SpriteRowsParams { width, height, y0: 0, y1: height, to_working: self.authored_to_working });
        let px = width * height;
        self.stack_pass(&self.pipelines.sprite_rows_f16_pipeline, sprite, &layer, px.div_ceil(WORKGROUP_SIZE), "stack_sprite_f16");
        true
    }

    pub fn stack_layer_from_srgb_u8(&mut self, i: usize, rgba: &[u8]) -> bool {
        let h = self.stack_dims.1;
        self.stack_layer_from_srgb_u8_rows(i, rgba, 0, h)
    }

    /// `stack_layer_from_srgb_u8` for a sprite that only covers rows
    /// `y0..y1` of the full-frame `rgba`: only those rows go up and are
    /// decoded; the rest of the layer is cleared.
    pub fn stack_layer_from_srgb_u8_rows(&mut self, i: usize, rgba: &[u8], y0: u32, y1: u32) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() {
            return false;
        }
        let (width, height) = self.stack_dims;
        let pixel_count = self.stack_pixel_count();
        if rgba.len() as u64 != pixel_count * 4 {
            return false;
        }
        let y0 = y0.min(height);
        let y1 = y1.min(height);
        let layer = std::sync::Arc::clone(&self.stack_layers[i]);
        let mut enc = self.encoder(Some("stack_sprite_clear"));
        enc.clear_buffer(&layer, 0, Some(pixel_count * 16));
        self.submit(enc);
        if y1 <= y0 {
            return true;
        }
        if self.stack_sprite_u8.as_ref().is_none_or(|b| b.size() < pixel_count * 4) {
            self.stack_sprite_u8 = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stack_sprite_u8"),
                size: pixel_count * 4,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let sprite = self.stack_sprite_u8.as_ref().unwrap();
        let row_bytes = (width as u64) * 4;
        let (b0, b1) = ((y0 as u64 * row_bytes) as usize, (y1 as u64 * row_bytes) as usize);
        self.upload(sprite, b0 as u64, &rgba[b0..b1]);
        self.write_params(&SpriteRowsParams { width, height, y0, y1, to_working: self.authored_to_working });
        let rows_px = (y1 - y0) * width;
        self.stack_pass(&self.pipelines.sprite_rows_pipeline, sprite, &layer, rows_px.div_ceil(WORKGROUP_SIZE), "stack_sprite_rows");
        true
    }

    /// Layer `i` is the title sprite `id` moved by `(dx, dy)` with its
    /// alpha scaled by `opacity` — `klipp_title`'s `shift_and_fade`,
    /// done where the layer is. `pixels` is the sprite, uploaded only
    /// when `id` is not resident; rows `y0..y1` are the destination rows
    /// it covers, the rest of the layer is cleared.
    #[allow(clippy::too_many_arguments)]
    pub fn stack_layer_place_sprite(&mut self, i: usize, id: u64, texels: &[u32], bx: SpriteBox, dx: i32, dy: i32, opacity: f32, y0: u32, y1: u32) -> bool {
        self.stack_layer_place_sprite_posed(i, id, texels, bx, dx, dy, opacity, 1.0, (0.0, 0.0), None, None, y0, y1)
    }

    /// [`Self::stack_layer_place_sprite`] with the whole pose: the
    /// sprite scaled by `scale` about `anchor` (bilinear, premultiplied,
    /// in the sprite's sRGB-encoded units) and laid over a full-frame
    /// `under` colour (sRGB8, straight alpha) when there is one, its
    /// alpha shaped by where it is on the frame when `shape` is given.
    /// The twin of `klipp_title::pose_sprite`.
    #[allow(clippy::too_many_arguments)]
    pub fn stack_layer_place_sprite_posed(
        &mut self,
        i: usize,
        id: u64,
        texels: &[u32],
        bx: SpriteBox,
        dx: i32,
        dy: i32,
        opacity: f32,
        scale: f32,
        anchor: (f32, f32),
        under: Option<[u8; 4]>,
        shape: Option<SpriteShape>,
        y0: u32,
        y1: u32,
    ) -> bool {
        self.place_sprite(i, id, texels, bx, dx, dy, opacity, scale, anchor, under, shape, y0, y1, 0, None)
    }

    /// Layer `i` takes the resident sprite `id`, as it rests, with its
    /// alpha scaled by `opacity`, laid over what the layer already holds
    /// as the title renderer lays one text layer over another (#2069):
    /// straight alpha, in sRGB-encoded units. A title's text layers each
    /// fading are placed so, one after another, the first with
    /// [`Self::stack_layer_place_sprite_posed`]. Only rows `y0..y1` are
    /// touched. Existing pixels are converted back to authored light for
    /// blending, then returned to the project working space on the GPU.
    #[allow(clippy::too_many_arguments)]
    pub fn stack_layer_place_sprite_over(&mut self, i: usize, id: u64, texels: &[u32], bx: SpriteBox, opacity: f32, y0: u32, y1: u32) -> bool {
        self.place_sprite(i, id, texels, bx, 0, 0, opacity, 1.0, (0.0, 0.0), None, None, y0, y1, 1, None)
    }

    /// Assemble cached glyph coverage with the CPU mask's max operation.
    /// Fill is a frame parameter; changing colour does not replace the source.
    pub fn stack_title_glyph(&mut self, slot: usize, id: u64, texels: &[u32], size: [u32;2], origin: [i32;2], color: [u8;4], place: GlyphPlace) -> bool {
        let (y0,y1)=self.glyph_rows(origin,size[1],place);
        self.place_sprite(slot,id,texels,SpriteBox{x0:0,y0:0,w:size[0],h:size[1]},origin[0],origin[1],place.alpha,1.0,(0.0,0.0),Some(color),None,y0,y1,2,None)
    }

    /// Subtract native bitmap alpha from the shared glyph face mask, as
    /// colour_pixels does before the CPU paints the remaining text fill.
    pub fn stack_title_colour_coverage_subtract(&mut self,slot:usize,id:u64,texels:&[u32],size:[u32;2],origin:[i32;2],place:GlyphPlace)->bool {
        let (y0,y1)=self.glyph_rows(origin,size[1],place);
        self.place_sprite(slot,id,texels,SpriteBox{x0:0,y0:0,w:size[0],h:size[1]},origin[0],origin[1],place.alpha,1.0,(0.0,0.0),None,None,y0,y1,3,None)
    }

    /// Place a cached native-colour glyph without applying the text fill.
    pub fn stack_title_colour_glyph(&mut self,slot:usize,id:u64,texels:&[u32],size:[u32;2],origin:[i32;2],place:GlyphPlace)->bool {
        let (y0,y1)=self.glyph_rows(origin,size[1],place);
        self.place_sprite(slot,id,texels,SpriteBox{x0:0,y0:0,w:size[0],h:size[1]},origin[0],origin[1],place.alpha,1.0,(0.0,0.0),None,None,y0,y1,4,None)
    }

    /// Resize a retained byte-premultiplied font strike with the CPU triangle
    /// filter. mode: 2=max coverage, 3=subtract coverage, 1=native RGBA over.
    #[allow(clippy::too_many_arguments)]
    pub fn stack_title_resized_colour_glyph(&mut self,slot:usize,id:u64,texels:&[u32],size:[u32;2],target:[u32;2],origin:[i32;2],color:[u8;4],mode:u32,place:GlyphPlace)->bool {
        if target.contains(&0) || size.contains(&0) || !matches!(mode,1..=3) {return false;}
        let (y0,y1)=self.glyph_rows(origin,target[1],place);
        // Native RGBA over takes a posed glyph's alpha rounded, as the glyph modes do.
        self.place_sprite(slot,id,texels,SpriteBox{x0:0,y0:0,w:size[0],h:size[1]},origin[0],origin[1],place.alpha,1.0,(0.0,0.0),Some(color),None,y0,y1,if mode==1 {4} else {mode},Some(target))
    }

    /// The frame rows a glyph `height` tall at `origin` covers, inside its pose's clip.
    pub(super) fn glyph_rows(&self,origin:[i32;2],height:u32,place:GlyphPlace)->(u32,u32) {
        let (_,h)=self.stack_dims;
        let top=origin[1].max(place.clip_y[0]);
        let bottom=(i64::from(origin[1])+i64::from(height)).min(i64::from(place.clip_y[1]));
        (top.clamp(0,h as i32) as u32,bottom.clamp(0,i64::from(h)) as u32)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn place_sprite(
        &mut self,
        i: usize,
        id: u64,
        texels: &[u32],
        bx: SpriteBox,
        dx: i32,
        dy: i32,
        opacity: f32,
        scale: f32,
        anchor: (f32, f32),
        under: Option<[u8; 4]>,
        shape: Option<SpriteShape>,
        y0: u32,
        y1: u32,
        over: u32,
        native_size: Option<[u32;2]>,
    ) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() {
            return false;
        }
        let Some(to_authored) = title_styles::inverse(self.authored_to_working) else { return false };
        let (width, height) = self.stack_dims;
        let pixel_count = self.stack_pixel_count();
        if (native_size.is_none() && over != 2 && !bx.fits(texels,width,height))
            || texels.len() as u64 != u64::from(bx.w)*u64::from(bx.h)*2
        { return false; }
        let y0 = y0.min(height);
        let y1 = y1.min(height);
        let layer = std::sync::Arc::clone(&self.stack_layers[i]);
        if over == 0 {
            let mut enc = self.encoder(Some("stack_sprite_clear"));
            enc.clear_buffer(&layer, 0, Some(pixel_count * 16));
            self.submit(enc);
        }
        if y1 <= y0 || (opacity <= 0.001 && under.is_none()) {
            return true;
        }
        let srgb = |v: u8| {
            let c = v as f32 / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        let under = under.map(|c| [srgb(c[0]), srgb(c[1]), srgb(c[2]), c[3] as f32 / 255.0]).unwrap_or([0.0; 4]);
        let pos = self.resident_sprite(id, texels);
        self.write_params(&SpritePlaceParams {
            width,
            height,
            y0,
            y1,
            dx,
            dy,
            opacity,
            scale: if (scale - 1.0).abs() < 1e-6 { 1.0 } else { scale },
            anchor_x: anchor.0,
            anchor_y: anchor.1,
            under_r: under[0],
            under_g: under[1],
            under_b: under[2],
            under_a: under[3],
            focus_axis: match shape.and_then(|s| s.focus_vertical) {
                None => 0,
                Some(true) => 1,
                Some(false) => 2,
            },
            band: shape.map_or(0.5, |s| s.band),
            edge_opacity: shape.map_or(1.0, |s| s.edge_opacity),
            fade_top: shape.map_or(0.0, |s| s.top),
            fade_bottom: shape.map_or(0.0, |s| s.bottom),
            fade_left: shape.map_or(0.0, |s| s.left),
            fade_right: shape.map_or(0.0, |s| s.right),
            bx0: bx.x0,
            by0: bx.y0,
            bw: bx.w,
            bh: bx.h,
            over,
            _pad1: native_size.map_or(0,|s|s[0]),
            _pad2: native_size.map_or(0,|s|s[1]),
            to_working: self.authored_to_working,
            to_authored,
        });
        let rows_px = (y1 - y0) * width;
        let sprite = &self.stack_sprites[pos].buffer;
        self.stack_pass(&self.pipelines.sprite_place_pipeline, sprite, &layer, rows_px.div_ceil(WORKGROUP_SIZE), "stack_sprite_place");
        true
    }

    /// Whether title sprite `id` is on the GPU.
    /// Layer `i` takes the resident sprite `id`'s glyphs each placed on
    /// its own: `units` is the sprite's glyph map over `(x0, y0, w, h)`
    /// (`u16::MAX` for no glyph), `glyphs` one sixteen-float record per
    /// glyph as `klipp_title::GlyphPose::packed` lays it out (moved box,
    /// dx dy scale alpha, anchor and clip rows). The twin of
    /// `klipp_title::pose_sprite_glyphs`; only rows `y0..y1` are written.
    #[allow(clippy::too_many_arguments)]
    pub fn stack_layer_place_glyphs(
        &mut self,
        i: usize,
        id: u64,
        texels: &[u32],
        bx: SpriteBox,
        units: (u32, u32, u32, u32, &[u16]),
        glyphs: &[[f32; 16]],
        y0: u32,
        y1: u32,
    ) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() {
            return false;
        }
        let (width, height) = self.stack_dims;
        let pixel_count = self.stack_pixel_count();
        let (ux0, uy0, uw, uh, index) = units;
        if !bx.fits(texels, width, height) || index.len() as u64 != (uw as u64) * (uh as u64) || glyphs.is_empty() {
            return false;
        }
        let y0 = y0.min(height);
        let y1 = y1.min(height);
        let layer = std::sync::Arc::clone(&self.stack_layers[i]);
        let mut enc = self.encoder(Some("stack_sprite_clear"));
        enc.clear_buffer(&layer, 0, Some(pixel_count * 16));
        self.submit(enc);
        if y1 <= y0 {
            return true;
        }
        let pos = self.resident_sprite(id, texels);
        if self.stack_sprites[pos].units.as_ref().is_none_or(|u| (u.0, u.1, u.2, u.3) != (ux0, uy0, uw, uh)) {
            let mut packed: Vec<u32> = Vec::with_capacity(index.len().div_ceil(2));
            for pair in index.chunks(2) {
                packed.push(pair[0] as u32 | ((pair.get(1).copied().unwrap_or(0) as u32) << 16));
            }
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stack_sprite_units"),
                size: ((packed.len().max(1)) * 4) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.upload(&buffer, 0, bytemuck::cast_slice(&packed));
            self.stack_sprites[pos].units = Some((ux0, uy0, uw, uh, buffer));
        }
        let table_bytes = (glyphs.len() * 64) as u64;
        if self.sprite_glyph_table.as_ref().is_none_or(|b| b.size() < table_bytes) {
            self.sprite_glyph_table = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stack_sprite_glyphs"),
                size: table_bytes.max(64),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let flat: Vec<f32> = glyphs.iter().flatten().copied().collect();
        let table = self.sprite_glyph_table.as_ref().unwrap();
        self.upload(table, 0, bytemuck::cast_slice(&flat));
        self.write_params(&SpriteGlyphsParams { width, height, y0, y1, n: glyphs.len() as u32, ux0, uy0, uw, uh, bx0: bx.x0, by0: bx.y0, bw: bx.w, bh: bx.h, _p0: 0, _p1: 0, _p2: 0, to_working: self.authored_to_working });
        let sprite = &self.stack_sprites[pos];
        let units_buf = &sprite.units.as_ref().unwrap().4;
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stack_sprite_glyphs"),
            layout: &self.pipelines.sprite_glyphs_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: sprite.buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: layer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: units_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: table.as_entire_binding() },
            ],
        });
        let rows_px = (y1 - y0) * width;
        let mut enc = self.encoder(Some("stack_sprite_glyphs"));
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.pipelines.sprite_glyphs_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            dispatch_flat(&mut pass, rows_px.div_ceil(WORKGROUP_SIZE));
        }
        self.submit(enc);
        true
    }

    /// The resident sprite `id`, put on the GPU from `texels` when it is
    /// not there yet: the oldest go first when the count or the bytes
    /// would pass their caps (#190). Its index in `stack_sprites`.
    pub(super) fn resident_sprite(&mut self, id: u64, texels: &[u32]) -> usize {
        self.stack_sprite_tick += 1;
        let tick = self.stack_sprite_tick;
        if let Some(pos) = self.stack_sprites.iter().position(|r| r.id == id) {
            self.stack_sprites[pos].used = tick;
            return pos;
        }
        let size = ((texels.len().max(2)) * 4) as u64;
        while !self.stack_sprites.is_empty()
            && (self.stack_sprites.len() >= STACK_SPRITE_CAP || self.stack_sprites.iter().map(|r| r.buffer.size()).sum::<u64>() + size > STACK_SPRITE_BYTES)
        {
            let oldest = self.stack_sprites.iter().enumerate().min_by_key(|(_, r)| r.used).map(|(i, _)| i).unwrap();
            self.stack_sprites.remove(oldest);
        }
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("stack_resident_sprite"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        if !texels.is_empty() {
            self.upload(&buffer, 0, bytemuck::cast_slice(texels));
        }
        self.stack_sprites.push(ResidentSprite { id, buffer, used: tick, units: None });
        self.stack_sprites.len() - 1
    }

    /// The bytes the resident sprites take on the GPU.
    pub fn stack_sprite_bytes(&self) -> u64 {
        self.stack_sprites.iter().map(|r| r.buffer.size()).sum()
    }

    /// Layer `i` is a title's box of packed sRGB half floats (`bx`), the
    /// rest of the layer cleared (#190): only the box goes up.
    pub fn stack_layer_from_srgb_f16_box(&mut self, i: usize, texels: &[u32], bx: SpriteBox) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() {
            return false;
        }
        let (width, height) = self.stack_dims;
        if !bx.fits(texels, width, height) {
            return false;
        }
        let pixel_count = self.stack_pixel_count();
        let layer = std::sync::Arc::clone(&self.stack_layers[i]);
        let mut enc = self.encoder(Some("stack_sprite_clear"));
        enc.clear_buffer(&layer, 0, Some(pixel_count * 16));
        self.submit(enc);
        if bx.w == 0 || bx.h == 0 {
            return true;
        }
        let bytes = (texels.len() * 4) as u64;
        if self.stack_sprite_f16.as_ref().is_none_or(|b| b.size() < bytes) {
            self.stack_sprite_f16 = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stack_sprite_f16"),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let sprite = self.stack_sprite_f16.as_ref().unwrap();
        self.upload(sprite, 0, bytemuck::cast_slice(texels));
        self.write_params(&SpriteBoxParams { width, height, bx0: bx.x0, by0: bx.y0, bw: bx.w, bh: bx.h, _p0: 0, _p1: 0, to_working: self.authored_to_working });
        self.stack_pass(&self.pipelines.sprite_box_f16_pipeline, sprite, &layer, (bx.w * bx.h).div_ceil(WORKGROUP_SIZE), "stack_sprite_box_f16");
        true
    }

    pub fn stack_sprite_resident(&self, id: u64) -> bool {
        self.stack_sprites.iter().any(|r| r.id == id)
    }

    /// The accumulator exists and is a canvas if nothing has been laid
    /// on it yet.
    /// Whether the title in `slot` is a metal that reflects the shot
    /// behind it, drawn by [`Self::stack_title_metal_shot`] when the
    /// layers below are laid (#2706).
    pub fn stack_has_title_metal_shot(&self, slot: usize) -> bool {
        self.title_metal_shots.contains_key(&slot)
    }

    /// The shot laid so far, small and blurred, reflected in the metal
    /// title in `slot` (#2706): its record of where each pixel looks and
    /// its fresnel term, mixed by the metal's tint and its shot strength.
    /// Run just before the slot is blended; `true` with nothing to draw.
    pub fn stack_title_metal_shot(&mut self, slot: usize) -> bool {
        let Some(shot) = self.title_metal_shots.remove(&slot) else { return true };
        if !self.stack_ensure_acc() || slot >= self.stack_layers.len() {
            return false;
        }
        let (fw, fh) = self.stack_dims;
        let scale = title_styles::METAL_SHOT_SCALE;
        let (sw, sh) = (fw.div_ceil(scale), fh.div_ceil(scale));
        let small = u64::from(sw) * u64::from(sh);
        let [rx, ry, rw, rh] = shot.roi;
        let base = u64::from(rw) * u64::from(rh);
        if self.title_metal_shot_scratch.as_ref().is_none_or(|(_, n)| *n < small) {
            let b = self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("title metal shot blur"), size: small * 16, usage: wgpu::BufferUsages::STORAGE, mapped_at_creation: false });
            self.title_metal_shot_scratch = Some((b, small));
        }
        let radius = (shot.blur_px / scale as f32).round().max(1.0) as u32;
        let params = |op: u32| MetalShotParams {
            width: fw, height: fh, op, scale, roi_x: rx, roi_y: ry, roi_w: rw, roi_h: rh,
            small_w: sw, small_h: sh, base: base as u32, radius,
            tint_shot: [shot.tint[0], shot.tint[1], shot.tint[2], shot.shot],
        };
        let acc = self.stack_acc.clone().unwrap();
        let scratch = &self.title_metal_shot_scratch.as_ref().unwrap().0;
        let small_groups = (small as u32).div_ceil(WORKGROUP_SIZE);
        let pass = |this: &Self, op: u32, read: &wgpu::Buffer, rw: &wgpu::Buffer, groups: u32| {
            this.write_params(&params(op));
            this.stack_pass(&this.pipelines.title_metal_shot_pipeline, read, rw, groups, "title metal shot");
        };
        pass(self, 0, &acc, &shot.buffer, small_groups);
        for _ in 0..2 {
            pass(self, 1, &shot.buffer, scratch, small_groups);
            pass(self, 2, scratch, &shot.buffer, small_groups);
        }
        pass(self, 3, &shot.buffer, &self.stack_layers[slot], (base as u32).div_ceil(WORKGROUP_SIZE));
        true
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct SpriteGlyphsParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) y0: u32,
    pub(super) y1: u32,
    pub(super) n: u32,
    pub(super) ux0: u32,
    pub(super) uy0: u32,
    pub(super) uw: u32,
    pub(super) uh: u32,
    /// The box of the frame the sprite's half floats cover (#190).
    pub(super) bx0: u32,
    pub(super) by0: u32,
    pub(super) bw: u32,
    pub(super) bh: u32,
    pub(super) _p0: u32,
    pub(super) _p1: u32,
    pub(super) _p2: u32,
    /// BT.709 → working space, rows padded to four (#1279).
    pub(super) to_working: [[f32; 4]; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct SpriteBoxParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) bx0: u32,
    pub(super) by0: u32,
    pub(super) bw: u32,
    pub(super) bh: u32,
    pub(super) _p0: u32,
    pub(super) _p1: u32,
    /// BT.709 → working space, rows padded to four (#1279).
    pub(super) to_working: [[f32; 4]; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct SpriteRowsParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) y0: u32,
    pub(super) y1: u32,
    /// BT.709 → working space, rows padded to four (#1279).
    pub(super) to_working: [[f32; 4]; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct SpritePlaceParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) y0: u32,
    pub(super) y1: u32,
    pub(super) dx: i32,
    pub(super) dy: i32,
    pub(super) opacity: f32,
    pub(super) scale: f32,
    pub(super) anchor_x: f32,
    pub(super) anchor_y: f32,
    /// The full-frame colour under the sprite, linear, straight alpha;
    /// alpha 0 means none.
    pub(super) under_r: f32,
    pub(super) under_g: f32,
    pub(super) under_b: f32,
    pub(super) under_a: f32,
    /// [`SpriteShape`]'s focus band: axis 0 none, 1 down, 2 across.
    pub(super) focus_axis: u32,
    pub(super) band: f32,
    pub(super) edge_opacity: f32,
    /// [`SpriteShape`]'s edge fade zones, fractions of the frame.
    pub(super) fade_top: f32,
    pub(super) fade_bottom: f32,
    pub(super) fade_left: f32,
    pub(super) fade_right: f32,
    /// The box of the frame the sprite's half floats cover (#190).
    pub(super) bx0: u32,
    pub(super) by0: u32,
    pub(super) bw: u32,
    pub(super) bh: u32,
    /// 1: laid over what the layer holds, as the title renderer lays one
    /// text layer over another (#2069); 0: the layer is the sprite. A
    /// glyph's: 2 its coverage into the face mask, 3 taken out of it, 4 its
    /// native colour laid over; a posed one's alpha rounds to a byte (#2203).
    pub(super) over: u32,
    pub(super) _pad1: u32,
    pub(super) _pad2: u32,
    /// BT.709 → working space, rows padded to four (#1279).
    pub(super) to_working: [[f32; 4]; 3],
    pub(super) to_authored: [[f32; 4]; 3],
}

/// A title's resting render on the GPU, the box it covers as packed
/// sRGB half floats (#190); see `stack_sprites`.
pub(super) struct ResidentSprite {
    pub(super) id: u64,
    pub(super) buffer: wgpu::Buffer,
    pub(super) used: u64,
    /// The sprite's glyph map, `(x0, y0, w, h, two u16 per u32)`, once a
    /// staggered motion has asked for it.
    pub(super) units: Option<(u32, u32, u32, u32, wgpu::Buffer)>,
}

/// Sprites kept on the GPU at once: the box each covers, eight bytes a
/// pixel.
pub(super) const STACK_SPRITE_CAP: usize = 512;

/// The most the resident sprites take together (#190): a guard, since a
/// title's box is usually far smaller than its frame; the sprite being
/// placed is always kept.
pub(super) const STACK_SPRITE_BYTES: u64 = 200 << 20;

// A generator's frame as sRGB-encoded half floats, decoded to linear
// where every other layer is decoded — on the GPU. Two halves to a
// u32, so it needs no f16 shader feature, and eight bytes a pixel
// against the sixteen a linear f32 upload costs.
pub(super) const SHADER_SPRITE_ROWS_F16: &str = gpu_shader!("title/sprite_rows_f16.wgsl");

// A title's box of packed sRGB half floats into its place in a cleared
// layer (#190): one invocation a box pixel.
pub(super) const SHADER_SPRITE_BOX_F16: &str = gpu_shader!("title/sprite_box_f16.wgsl");

pub(super) const SHADER_SPRITE_ROWS: &str = gpu_shader!("title/sprite_rows.wgsl");

// A resident sRGB RGBA8 sprite moved by (dx, dy) and faded, decoded to
// linear with its alpha kept straight, over destination rows y0..y1:
// `klipp_title`'s `shift_and_fade` then the sprite decode, in one. The
// alpha rounds as the CPU rounds it (half away from zero, then a byte).
pub(super) const SHADER_SPRITE_PLACE: &str = gpu_shader!("title/sprite_place.wgsl");

// A resident sprite's glyphs each placed on its own — a motion that
// staggers by character, word or line: `klipp_title`'s
// `pose_sprite_glyphs`, then the sprite decode, in one. Every pixel in
// the rows asks every glyph whose moved box holds it for its bilinear,
// glyph-masked sample, and lays the glyphs over one another in order.
// A glyph an animator turns is sampled back through its turn.
pub(super) const SHADER_SPRITE_GLYPHS: &str = gpu_shader!("title/sprite_glyphs.wgsl");

// Packed sRGB RGBA8 bytes into a storage texture, a texel per
// invocation. A `copy_buffer_to_texture` wants rows padded to 256
// bytes, which a tightly packed frame of most widths is not; a store
// has no such condition.
pub(super) const SHADER_U8_TO_TEXTURE: &str = gpu_shader!("title/u8_to_texture.wgsl");

// A metal title's reflection of the shot behind it (#2706). One buffer
// holds the title's record (for each pixel of its box: where it looks,
// its fresnel term and its contour) then, from `base`, the shot laid so
// far at an eighth of the frame each way, premultiplied: op 0 makes that
// copy from the accumulator, ops 1 and 2 blur it across and down through
// the scratch, op 3 adds what each metal pixel sees to the layer. The
// record goes through `unpack` too: the title styles' op 63 lays it in
// the buffer as pixels are laid.
pub(super) const SHADER_TITLE_METAL_SHOT: &str = gpu_shader!("title/title_metal_shot.wgsl");
