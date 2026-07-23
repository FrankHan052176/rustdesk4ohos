use std::fmt;

use opus_head_sys as ffi;

#[derive(Clone, Copy)]
pub enum Channels {
    Mono = 1,
    Stereo = 2,
}

#[derive(Debug)]
pub struct Error(i32);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Opus error {}", self.0)
    }
}

pub struct Decoder {
    inner: *mut ffi::OpusDecoder,
    channels: Channels,
}

unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new(sample_rate: u32, channels: Channels) -> Result<Self, Error> {
        let mut error = 0;
        let inner = unsafe {
            ffi::opus_decoder_create(sample_rate as i32, channels as i32, &mut error)
        };
        if error != 0 || inner.is_null() {
            Err(Error(error))
        } else {
            Ok(Self { inner, channels })
        }
    }

    pub fn decode(&mut self, input: &[u8], output: &mut [i16], fec: bool) -> Result<usize, Error> {
        let input_len = input.len();
        let input_ptr = if input.is_empty() {
            std::ptr::null()
        } else {
            input.as_ptr()
        };
        let frame_size = output.len() / self.channels as usize;
        let decoded = unsafe {
            ffi::opus_decode(
                self.inner,
                input_ptr,
                input_len as i32,
                output.as_mut_ptr(),
                frame_size as i32,
                fec as i32,
            )
        };
        if decoded < 0 {
            Err(Error(decoded))
        } else {
            Ok(decoded as usize)
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe {
            ffi::opus_decoder_destroy(self.inner);
        }
    }
}
