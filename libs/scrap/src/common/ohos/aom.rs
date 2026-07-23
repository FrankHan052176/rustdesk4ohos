use crate::codec::{EncoderApi, EncoderCfg};
use crate::common::ohos_avcodec::{OhosImage, OhosVideoDecoder};
use crate::{common::GoogleImage, EncodeInput, EncodeYuvFormat, Pixfmt};
use hbb_common::{
    anyhow::anyhow,
    message_proto::{Chroma, VideoFrame},
    ResultType,
};

#[derive(Clone, Copy, Debug)]
pub struct AomEncoderConfig {
    pub width: u32,
    pub height: u32,
    pub quality: f32,
    pub keyframe_interval: Option<usize>,
}

pub struct AomEncoder;

fn empty_yuvfmt() -> EncodeYuvFormat {
    EncodeYuvFormat {
        pixfmt: Pixfmt::I420,
        w: 0,
        h: 0,
        stride: vec![0, 0, 0],
        u: 0,
        v: 0,
    }
}

impl EncoderApi for AomEncoder {
    fn new(_cfg: EncoderCfg, _i444: bool) -> ResultType<Self>
    where
        Self: Sized,
    {
        Err(anyhow!("OHOS AV1 encoder is not implemented"))
    }

    fn encode_to_message(&mut self, _frame: EncodeInput, _ms: i64) -> ResultType<VideoFrame> {
        Err(anyhow!("OHOS AV1 encoder is not implemented"))
    }

    fn yuvfmt(&self) -> EncodeYuvFormat {
        empty_yuvfmt()
    }

    fn set_quality(&mut self, _ratio: f32) -> ResultType<()> {
        Err(anyhow!("OHOS AV1 encoder is not implemented"))
    }

    fn bitrate(&self) -> u32 {
        0
    }
    fn support_changing_quality(&self) -> bool {
        false
    }
    fn latency_free(&self) -> bool {
        false
    }
    fn is_hardware(&self) -> bool {
        false
    }
    fn disable(&self) {}
}

impl AomEncoder {
    pub fn encode(&mut self, _pts: i64, _data: &[u8], _stride_align: usize) -> ResultType<()> {
        Err(anyhow!("OHOS AV1 encoder is not implemented"))
    }
}

pub struct AomDecoder {
    inner: OhosVideoDecoder,
    frames: Vec<OhosImage>,
}

impl AomDecoder {
    pub fn new() -> ResultType<Self> {
        let (width, height) = (64, 64);
        Ok(Self {
            inner: OhosVideoDecoder::new_av1(width, height)?,
            frames: Vec::new(),
        })
    }

    pub fn decode<'a>(&'a mut self, data: &[u8]) -> ResultType<DecodeFrames<'a>> {
        self.frames = self.inner.decode(data)?.collect();
        Ok(DecodeFrames {
            inner: self.frames.drain(..),
        })
    }

    pub fn flush<'a>(&'a mut self) -> ResultType<DecodeFrames<'a>> {
        self.frames = self.inner.flush()?.collect();
        Ok(DecodeFrames {
            inner: self.frames.drain(..),
        })
    }
}

pub struct DecodeFrames<'a> {
    inner: std::vec::Drain<'a, OhosImage>,
}

impl<'a> Iterator for DecodeFrames<'a> {
    type Item = Image;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(Image)
    }
}

pub struct Image(OhosImage);

impl Image {
    pub fn new() -> Self {
        Self(OhosImage::empty())
    }

    pub fn is_null(&self) -> bool {
        self.0.is_null()
    }
}

impl GoogleImage for Image {
    fn width(&self) -> usize {
        self.0.width()
    }
    fn height(&self) -> usize {
        self.0.height()
    }
    fn stride(&self) -> Vec<i32> {
        self.0.stride()
    }
    fn planes(&self) -> Vec<*mut u8> {
        self.0.planes()
    }
    fn chroma(&self) -> Chroma {
        self.0.chroma()
    }
}
