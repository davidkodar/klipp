//! Universal mixing of straight RGBA, independent of graph execution.
//! RGB is neither premultiplied nor clamped. Mask inputs must already be
//! in their declared linear working space; a scalar matte uses `Red`.

/// Which component of an RGBA mask supplies coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MaskChannel {
    Red = 0,
    Luminance = 1,
    Alpha = 2,
}

impl MaskChannel {
    /// Stable saved mask-port value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Luminance => "luminance",
            Self::Alpha => "alpha",
        }
    }
}
impl std::str::FromStr for MaskChannel {
    type Err = MaskMixError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "red" => Ok(Self::Red),
            "luminance" => Ok(Self::Luminance),
            "alpha" => Ok(Self::Alpha),
            _ => Err(MaskMixError::UnknownChannel),
        }
    }
}

/// Invalid mix configuration or mismatched image grids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskMixError {
    UnknownChannel,
    NonFiniteMix,
    NonFiniteLuma,
    SizeMismatch,
    ParamsExhausted,
}

impl std::fmt::Display for MaskMixError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "mask mix: {self:?}")
    }
}
impl std::error::Error for MaskMixError {}

/// Validated settings shared by CPU and GPU mixing.
#[derive(Debug, Clone, Copy)]
pub struct MaskMix {
    pub(crate) mix: f32,
    pub(crate) channel: MaskChannel,
    pub(crate) luma: [f32; 3],
}

impl MaskMix {
    /// Reject nonfinite settings and clamp finite mix to [0, 1]. Luminance
    /// coefficients come from the mask image's declared working space.
    pub fn new(mix: f32, channel: MaskChannel, luma: [f32; 3]) -> Result<Self, MaskMixError> {
        if !mix.is_finite() {
            return Err(MaskMixError::NonFiniteMix);
        }
        if !luma.iter().all(|v| v.is_finite()) {
            return Err(MaskMixError::NonFiniteLuma);
        }
        Ok(Self {
            mix: mix.clamp(0.0, 1.0),
            channel,
            luma,
        })
    }

    /// Extract coverage without multiplying luminance by image alpha.
    /// Nonfinite extracted coverage is zero. An unwired mask is white.
    pub fn coverage(&self, mask: Option<[f32; 4]>) -> f32 {
        let Some(m) = mask else { return 1.0 };
        let value = match self.channel {
            MaskChannel::Red => m[0],
            MaskChannel::Alpha => m[3],
            MaskChannel::Luminance => {
                m[0] * self.luma[0] + m[1] * self.luma[1] + m[2] * self.luma[2]
            }
        };
        if value.is_finite() {
            value.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Mix all four straight channels. Exact endpoints preserve even
    /// signed zero and nonfinite values on the selected side.
    pub fn pixel(&self, input: [f32; 4], effect: [f32; 4], mask: Option<[f32; 4]>) -> [f32; 4] {
        let w = self.mix * self.coverage(mask);
        if w == 0.0 {
            return input;
        }
        if w == 1.0 {
            return effect;
        }
        std::array::from_fn(|c| input[c] + (effect[c] - input[c]) * w)
    }

    /// Explicit compatibility primitive for migrated alpha-one behavior.
    /// It forces alpha even at zero mix; callers must not bypass it.
    pub fn pixel_legacy_alpha_one(
        &self,
        input: [f32; 4],
        effect: [f32; 4],
        mask: Option<[f32; 4]>,
    ) -> [f32; 4] {
        let mut out = self.pixel(input, effect, mask);
        out[3] = 1.0;
        out
    }

    /// Apply to equal-sized RGBA grids. Reject a mismatch before writing.
    pub fn apply(
        &self,
        input: &[[f32; 4]],
        effect: &[[f32; 4]],
        mask: Option<&[[f32; 4]]>,
        output: &mut [[f32; 4]],
    ) -> Result<(), MaskMixError> {
        if input.len() != effect.len()
            || input.len() != output.len()
            || mask.is_some_and(|m| m.len() != input.len())
        {
            return Err(MaskMixError::SizeMismatch);
        }
        for (i, out) in output.iter_mut().enumerate() {
            *out = self.pixel(input[i], effect[i], mask.map(|m| m[i]));
        }
        Ok(())
    }
}
