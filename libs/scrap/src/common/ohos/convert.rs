#![allow(non_snake_case)]

use super::PixelBuffer;
use crate::{EncodeYuvFormat, Pixfmt, TraitPixelBuffer};
use hbb_common::{bail, ResultType};
use std::ffi::c_int;

fn clamp_u8(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

fn yuv_to_rgb(y: u8, u: u8, v: u8) -> (u8, u8, u8) {
    let c = y as i32 - 16;
    let d = u as i32 - 128;
    let e = v as i32 - 128;
    let c = c.max(0);
    // Desktop and screen-content encoders typically signal or assume BT.709 for HD frames.
    // Using BT.601 here skews reds/blues noticeably on remote desktop content.
    let r = (298 * c + 459 * e + 128) >> 8;
    let g = (298 * c - 55 * d - 136 * e + 128) >> 8;
    let b = (298 * c + 541 * d + 128) >> 8;
    (clamp_u8(r), clamp_u8(g), clamp_u8(b))
}

unsafe fn write_rgb24(
    dst: *mut u8,
    dst_stride: i32,
    width: i32,
    height: i32,
    sample: impl Fn(i32, i32) -> (u8, u8, u8),
) {
    for row in 0..height {
        let line = dst.offset((row * dst_stride) as isize);
        for col in 0..width {
            let (r, g, b) = sample(row, col);
            let pixel = line.offset((col * 3) as isize);
            *pixel = r;
            *pixel.offset(1) = g;
            *pixel.offset(2) = b;
        }
    }
}

unsafe fn write_argb32(
    dst: *mut u8,
    dst_stride: i32,
    width: i32,
    height: i32,
    abgr: bool,
    sample: impl Fn(i32, i32) -> (u8, u8, u8),
) {
    for row in 0..height {
        let line = dst.offset((row * dst_stride) as isize);
        for col in 0..width {
            let (r, g, b) = sample(row, col);
            let pixel = line.offset((col * 4) as isize);
            if abgr {
                // Match libyuv's I420ToABGR memory layout on little-endian targets:
                // RGBA bytes, which is what ArkTS PixelMapFormat.RGBA_8888 expects.
                *pixel = r;
                *pixel.offset(1) = g;
                *pixel.offset(2) = b;
                *pixel.offset(3) = 0xFF;
            } else {
                // Match libyuv's I420ToARGB memory layout on little-endian targets:
                // BGRA bytes.
                *pixel = b;
                *pixel.offset(1) = g;
                *pixel.offset(2) = r;
                *pixel.offset(3) = 0xFF;
            }
        }
    }
}

pub unsafe fn I420ToRAW(
    src_y: *mut u8,
    src_stride_y: i32,
    src_u: *mut u8,
    src_stride_u: i32,
    src_v: *mut u8,
    src_stride_v: i32,
    dst: *mut u8,
    dst_stride: i32,
    width: i32,
    height: i32,
) -> c_int {
    write_rgb24(dst, dst_stride, width, height, |row, col| {
        let y = *src_y.offset((row * src_stride_y + col) as isize);
        let u = *src_u.offset(((row / 2) * src_stride_u + (col / 2)) as isize);
        let v = *src_v.offset(((row / 2) * src_stride_v + (col / 2)) as isize);
        yuv_to_rgb(y, u, v)
    });
    0
}

pub unsafe fn I420ToARGB(
    src_y: *mut u8,
    src_stride_y: i32,
    src_u: *mut u8,
    src_stride_u: i32,
    src_v: *mut u8,
    src_stride_v: i32,
    dst: *mut u8,
    dst_stride: i32,
    width: i32,
    height: i32,
) -> c_int {
    write_argb32(dst, dst_stride, width, height, false, |row, col| {
        let y = *src_y.offset((row * src_stride_y + col) as isize);
        let u = *src_u.offset(((row / 2) * src_stride_u + (col / 2)) as isize);
        let v = *src_v.offset(((row / 2) * src_stride_v + (col / 2)) as isize);
        yuv_to_rgb(y, u, v)
    });
    0
}

pub unsafe fn I420ToABGR(
    src_y: *mut u8,
    src_stride_y: i32,
    src_u: *mut u8,
    src_stride_u: i32,
    src_v: *mut u8,
    src_stride_v: i32,
    dst: *mut u8,
    dst_stride: i32,
    width: i32,
    height: i32,
) -> c_int {
    write_argb32(dst, dst_stride, width, height, true, |row, col| {
        let y = *src_y.offset((row * src_stride_y + col) as isize);
        let u = *src_u.offset(((row / 2) * src_stride_u + (col / 2)) as isize);
        let v = *src_v.offset(((row / 2) * src_stride_v + (col / 2)) as isize);
        yuv_to_rgb(y, u, v)
    });
    0
}

pub unsafe fn I444ToARGB(
    src_y: *mut u8,
    src_stride_y: i32,
    src_u: *mut u8,
    src_stride_u: i32,
    src_v: *mut u8,
    src_stride_v: i32,
    dst: *mut u8,
    dst_stride: i32,
    width: i32,
    height: i32,
) -> c_int {
    write_argb32(dst, dst_stride, width, height, false, |row, col| {
        let y = *src_y.offset((row * src_stride_y + col) as isize);
        let u = *src_u.offset((row * src_stride_u + col) as isize);
        let v = *src_v.offset((row * src_stride_v + col) as isize);
        yuv_to_rgb(y, u, v)
    });
    0
}

pub unsafe fn I444ToABGR(
    src_y: *mut u8,
    src_stride_y: i32,
    src_u: *mut u8,
    src_stride_u: i32,
    src_v: *mut u8,
    src_stride_v: i32,
    dst: *mut u8,
    dst_stride: i32,
    width: i32,
    height: i32,
) -> c_int {
    write_argb32(dst, dst_stride, width, height, true, |row, col| {
        let y = *src_y.offset((row * src_stride_y + col) as isize);
        let u = *src_u.offset((row * src_stride_u + col) as isize);
        let v = *src_v.offset((row * src_stride_v + col) as isize);
        yuv_to_rgb(y, u, v)
    });
    0
}

pub unsafe fn ARGBToI420(
    _: *const u8,
    _: i32,
    _: *mut u8,
    _: i32,
    _: *mut u8,
    _: i32,
    _: *mut u8,
    _: i32,
    _: i32,
    _: i32,
) -> c_int {
    -1
}
pub unsafe fn ABGRToI420(
    _: *const u8,
    _: i32,
    _: *mut u8,
    _: i32,
    _: *mut u8,
    _: i32,
    _: *mut u8,
    _: i32,
    _: i32,
    _: i32,
) -> c_int {
    -1
}
pub unsafe fn RGB565ToI420(
    _: *const u8,
    _: i32,
    _: *mut u8,
    _: i32,
    _: *mut u8,
    _: i32,
    _: *mut u8,
    _: i32,
    _: i32,
    _: i32,
) -> c_int {
    -1
}
pub unsafe fn ARGBToNV12(
    _: *const u8,
    _: i32,
    _: *mut u8,
    _: i32,
    _: *mut u8,
    _: i32,
    _: i32,
    _: i32,
) -> c_int {
    -1
}
pub unsafe fn ABGRToNV12(
    _: *const u8,
    _: i32,
    _: *mut u8,
    _: i32,
    _: *mut u8,
    _: i32,
    _: i32,
    _: i32,
) -> c_int {
    -1
}
pub unsafe fn RGB565ToARGB(_: *const u8, _: i32, _: *mut u8, _: i32, _: i32, _: i32) -> c_int {
    -1
}
pub unsafe fn ABGRToARGB(_: *const u8, _: i32, _: *mut u8, _: i32, _: i32, _: i32) -> c_int {
    -1
}
pub unsafe fn ARGBToI444(
    _: *const u8,
    _: i32,
    _: *mut u8,
    _: i32,
    _: *mut u8,
    _: i32,
    _: *mut u8,
    _: i32,
    _: i32,
    _: i32,
) -> c_int {
    -1
}

pub fn convert_to_yuv(
    _captured: &PixelBuffer,
    _dst_fmt: EncodeYuvFormat,
    _dst: &mut Vec<u8>,
    _mid_data: &mut Vec<u8>,
) -> ResultType<()> {
    bail!("OHOS client-only build does not support software YUV encoding conversion yet")
}

pub fn convert(captured: &PixelBuffer, pixfmt: Pixfmt, dst: &mut Vec<u8>) -> ResultType<()> {
    if captured.pixfmt() == pixfmt {
        dst.extend_from_slice(captured.data());
        return Ok(());
    }
    match (captured.pixfmt(), pixfmt) {
        (Pixfmt::BGRA, Pixfmt::RGBA) | (Pixfmt::RGBA, Pixfmt::BGRA) => {
            dst.resize(captured.data().len(), 0);
            for (src, out) in captured.data().chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
                out[0] = src[2];
                out[1] = src[1];
                out[2] = src[0];
                out[3] = src[3];
            }
            Ok(())
        }
        _ => bail!(
            "unsupported pixfmt conversion for OHOS: {:?} -> {:?}",
            captured.pixfmt(),
            pixfmt
        ),
    }
}
