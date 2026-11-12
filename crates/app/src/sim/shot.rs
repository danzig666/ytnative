//! Simulator screenshots: the frame just drawn (back buffer, before the swap) read with
//! glReadPixels and written as an RGB PNG.

use std::os::raw::{c_int, c_uint, c_void};
use std::path::Path;

extern "C" {
    fn glReadPixels(x: c_int, y: c_int, w: c_int, h: c_int, format: c_uint, ty: c_uint, data: *mut c_void);
    fn glPixelStorei(pname: c_uint, param: c_int);
    fn glBindFramebuffer(target: c_uint, fb: c_uint);
}

const GL_RGBA: c_uint = 0x1908;
const GL_UNSIGNED_BYTE: c_uint = 0x1401;
const GL_PACK_ALIGNMENT: c_uint = 0x0D05;
const GL_FRAMEBUFFER: c_uint = 0x8D40;

/// RGBA rows, bottom-up as GL returns them.
pub fn read_frame(w: i32, h: i32) -> Vec<u8> {
    let mut px = vec![0u8; (w * h * 4) as usize];
    unsafe {
        glBindFramebuffer(GL_FRAMEBUFFER, plx_base::surface::frame_fb());
        glPixelStorei(GL_PACK_ALIGNMENT, 1);
        glReadPixels(0, 0, w, h, GL_RGBA, GL_UNSIGNED_BYTE, px.as_mut_ptr().cast());
    }
    px
}

pub fn save_png(path: &Path, w: i32, h: i32, rgba: &[u8]) -> anyhow::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let (w, h) = (w as usize, h as usize);
    // Flip to top-down and drop alpha (the UI plane is drawn with alpha 0 where video would show).
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in (0..h).rev() {
        for px in rgba[y * w * 4..(y + 1) * w * 4].chunks_exact(4) {
            rgb.extend_from_slice(&px[..3]);
        }
    }
    image::save_buffer(path, &rgb, w as u32, h as u32, image::ColorType::Rgb8)?;
    Ok(())
}
