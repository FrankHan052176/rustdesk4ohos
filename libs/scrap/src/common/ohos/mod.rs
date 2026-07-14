// Stub types for OHOS (controller-only, no display capture)
pub struct Display;
pub struct Capturer;

impl Display {
    pub fn new() -> Result<Self, String> {
        Err("Display capture is unavailable on OpenHarmony controller".to_owned())
    }
    pub fn all() -> Result<Vec<Display>, String> {
        Ok(Vec::new())
    }
    pub fn primary() -> Result<Display, String> {
        Err("Display capture is unavailable on OpenHarmony controller".to_owned())
    }
    pub fn width(&self) -> usize {
        0
    }
    pub fn height(&self) -> usize {
        0
    }
    pub fn origin(&self) -> (i32, i32) {
        (0, 0)
    }
    pub fn scale(&self) -> f64 {
        1.0
    }
    pub fn is_primary(&self) -> bool {
        false
    }
    pub fn name(&self) -> String {
        String::new()
    }
    pub fn is_online(&self) -> bool {
        false
    }
}

impl Capturer {
    pub fn new(_display: Display) -> Result<Self, String> {
        Err("Display capture is unavailable on OpenHarmony controller".to_owned())
    }
    pub fn width(&self) -> usize {
        0
    }
    pub fn height(&self) -> usize {
        0
    }
}

use super::{Pixfmt, TraitPixelBuffer};

pub mod aom;
pub mod avcodec;
pub mod convert;
pub mod direct_render;
pub use direct_render::{
    lookup_direct_render_target, register_direct_render_target_lookup, DirectRenderTarget,
    DirectRenderTargetLookup,
};
pub mod record;
pub mod vpxcodec;

pub struct PixelBuffer<'a> {
    data: &'a [u8],
    width: usize,
    height: usize,
    stride: Vec<usize>,
    pixfmt: Pixfmt,
}

impl<'a> PixelBuffer<'a> {
    pub fn new(
        data: &'a [u8],
        width: usize,
        height: usize,
        stride: Vec<usize>,
        pixfmt: Pixfmt,
    ) -> Self {
        Self {
            data,
            width,
            height,
            stride,
            pixfmt,
        }
    }
}

impl<'a> TraitPixelBuffer for PixelBuffer<'a> {
    fn data(&self) -> &[u8] {
        self.data
    }

    fn width(&self) -> usize {
        self.width
    }

    fn height(&self) -> usize {
        self.height
    }

    fn stride(&self) -> Vec<usize> {
        self.stride.clone()
    }

    fn pixfmt(&self) -> Pixfmt {
        self.pixfmt
    }
}
