//! Alpha-mask operations on caller-owned GPU buffers. The caller keeps and
//! reuses its buffers; these operations neither allocate picture buffers nor
//! upload/download pixels. They join the active stack encoder when present.
use super::*;

impl GpuEffectsPipeline {
    fn alpha_buffers_valid(&self, buffers: &[&wgpu::Buffer], w: u32, h: u32) -> bool {
        let count = u64::from(w) * u64::from(h);
        !self.is_device_lost()
            && count > 0
            && count <= u64::from(u32::MAX)
            && count.div_ceil(u64::from(WORKGROUP_SIZE))
                <= u64::from(self.device.limits().max_compute_workgroups_per_dimension)
            && buffers
                .iter()
                .all(|b| b.size() >= count * 4 && b.usage().contains(wgpu::BufferUsages::STORAGE))
    }

    fn copy_alpha(&self, src: &wgpu::Buffer, dst: &wgpu::Buffer, bytes: u64) -> bool {
        if std::ptr::eq(src, dst) {
            return true;
        }
        if !src.usage().contains(wgpu::BufferUsages::COPY_SRC)
            || !dst.usage().contains(wgpu::BufferUsages::COPY_DST)
        {
            return false;
        }
        let mut encoder = self.encoder(Some("alpha_mask_copy"));
        encoder.copy_buffer_to_buffer(src, 0, dst, 0, bytes);
        self.submit(encoder);
        true
    }

    /// Blur one-u32-per-pixel coverage (0..255), retaining the result in `dst`.
    /// `scratch` must be distinct from source and destination. Source may equal
    /// destination. Buffers must carry STORAGE and, for an identity copy,
    /// COPY_SRC/COPY_DST usages. Returns false before recording on invalid input.
    #[allow(clippy::too_many_arguments)]
    pub fn record_alpha_blur(
        &self,
        src: &wgpu::Buffer,
        dst: &wgpu::Buffer,
        scratch: &wgpu::Buffer,
        w: u32,
        h: u32,
        radius: f32,
    ) -> bool {
        if !radius.is_finite()
            || radius > 32767.0
            || !self.alpha_buffers_valid(&[src, dst, scratch], w, h)
            || std::ptr::eq(src, scratch)
            || std::ptr::eq(dst, scratch)
        {
            return false;
        }
        let mut input = src;
        let mut did_work = false;
        let workgroups = (w * h).div_ceil(WORKGROUP_SIZE);
        for radius in [radius.max(0.0).round() as usize; 3] {
            if radius == 0 {
                continue;
            }
            self.write_params(&AlphaBlurParams {
                width: w,
                height: h,
                radius: radius as u32,
                _pad: 0,
            });
            self.stack_pass(
                &self.pipelines.alpha_blur_h_pipeline,
                input,
                scratch,
                workgroups,
                "resident_alpha_blur_h",
            );
            self.write_params(&AlphaBlurParams {
                width: w,
                height: h,
                radius: radius as u32,
                _pad: 0,
            });
            self.stack_pass(
                &self.pipelines.alpha_blur_v_pipeline,
                scratch,
                dst,
                workgroups,
                "resident_alpha_blur_v",
            );
            input = dst;
            did_work = true;
        }
        did_work || self.copy_alpha(src, dst, u64::from(w) * u64::from(h) * 4)
    }

    /// Circular dilation of GPU-resident coverage into a distinct buffer.
    /// The same radius rounding and kernel as [`Self::dilate_alpha_gpu`].
    pub fn record_alpha_dilate(
        &self,
        src: &wgpu::Buffer,
        dst: &wgpu::Buffer,
        w: u32,
        h: u32,
        radius: f32,
    ) -> bool {
        if !radius.is_finite()
            || radius > 32767.0
            || !self.alpha_buffers_valid(&[src, dst], w, h)
            || std::ptr::eq(src, dst)
        {
            return false;
        }
        let radius_sq = (radius.max(0.0) * radius.max(0.0)) as u32;
        let radius = radius.max(0.0).ceil() as u32;
        if radius == 0 {
            return self.copy_alpha(src, dst, u64::from(w) * u64::from(h) * 4);
        }
        self.write_params(&AlphaDilateParams {
            width: w,
            height: h,
            radius,
            radius_sq,
        });
        self.stack_pass(
            &self.pipelines.alpha_dilate_pipeline,
            src,
            dst,
            (w * h).div_ceil(WORKGROUP_SIZE),
            "resident_alpha_dilate",
        );
        true
    }
}
