//! The native hit region follows the rendered alpha, including fractional DPI.
use std::sync::{Mutex, OnceLock};
static CURRENT: Mutex<(usize, usize)> = Mutex::new((0, 0));

fn image(index: usize) -> Result<&'static image::RgbaImage, String> {
    static IMAGES: OnceLock<Result<Vec<image::RgbaImage>, String>> = OnceLock::new();
    IMAGES
        .get_or_init(|| {
            crate::pet_sprites::SPRITES
                .iter()
                .map(|(_, bytes, frames)| {
                    let im = image::load_from_memory(bytes)
                        .map_err(|_| "PET_IMAGE_INVALID".to_string())?
                        .to_rgba8();
                    if im.width() != 144 * *frames as u32 || im.height() != 192 {
                        return Err("PET_IMAGE_DIMENSIONS".into());
                    }
                    Ok(im)
                })
                .collect()
        })
        .as_ref()
        .map_err(Clone::clone)
        .and_then(|images| images.get(index).ok_or_else(|| "PET_STATE_UNKNOWN".into()))
}

pub fn hit(x: f64, y: f64, width: u32, height: u32) -> bool {
    let Ok(current) = CURRENT.lock() else {
        return false;
    };
    hit_frame(current.0, current.1, x, y, width, height)
}
fn hit_frame(sprite: usize, frame: usize, x: f64, y: f64, width: u32, height: u32) -> bool {
    if !crate::pet_drop::inside(x, y, width, height) {
        return false;
    }
    let Ok(im) = image(sprite) else {
        return false;
    };
    let sx = ((x.floor() + 0.5) * 144. / width as f64).floor() as u32;
    let sy = ((y.floor() + 0.5) * 192. / height as f64).floor() as u32;
    let sx = sx.min(143) + frame as u32 * 144;
    sx < im.width() && im.get_pixel(sx, sy.min(191))[3] >= 128
}

fn spans(sprite: usize, frame: usize, width: u32, height: u32) -> Result<Vec<[i32; 4]>, String> {
    if width == 0 || height == 0 || width > 2048 || height > 2048 {
        return Err("PET_REGION_SIZE".into());
    }
    image(sprite)?;
    let mut rects = Vec::new();
    for y in 0..height {
        let mut x = 0;
        while x < width {
            if !hit_frame(sprite, frame, x as f64, y as f64, width, height) {
                x += 1;
                continue;
            }
            let left = x;
            while x < width && hit_frame(sprite, frame, x as f64, y as f64, width, height) {
                x += 1;
            }
            rects.push([left as i32, y as i32, x as i32, y as i32 + 1]);
        }
    }
    Ok(rects)
}

pub fn set_frame(window: &tauri::WebviewWindow, state: &str, frame: usize) -> Result<(), String> {
    if window.label() != "pet" {
        return Err("PET_WINDOW_REQUIRED".into());
    }
    let sprite = crate::pet_sprites::SPRITES
        .iter()
        .position(|s| s.0 == state)
        .ok_or("PET_STATE_UNKNOWN")?;
    if frame >= crate::pet_sprites::SPRITES[sprite].2 {
        return Err("PET_FRAME_RANGE".into());
    }
    *CURRENT.lock().map_err(|_| "PET_REGION_LOCK")? = (sprite, frame);
    apply(window)
}

#[cfg(windows)]
pub fn apply(window: &tauri::WebviewWindow) -> Result<(), String> {
    use windows::Win32::{
        Foundation::{HWND, RECT},
        Graphics::Gdi::{
            DeleteObject, ExtCreateRegion, SetWindowRgn, HGDIOBJ, RDH_RECTANGLES, RGNDATA,
            RGNDATAHEADER,
        },
    };
    if window.label() != "pet" {
        return Err("PET_WINDOW_REQUIRED".into());
    }
    let size = window.inner_size().map_err(|_| "PET_SIZE")?;
    let outer = window.outer_position().map_err(|_| "PET_POSITION")?;
    let inner = window.inner_position().map_err(|_| "PET_POSITION")?;
    let offset = (inner.x - outer.x, inner.y - outer.y);
    let hwnd = HWND(window.hwnd().map_err(|_| "PET_HWND")?.0);
    let current = *CURRENT.lock().map_err(|_| "PET_REGION_LOCK")?;
    let key = (
        hwnd.0 as usize,
        size.width,
        size.height,
        offset.0,
        offset.1,
        current.0,
        current.1,
    );
    type RegionKey = (usize, u32, u32, i32, i32, usize, usize);
    static LAST: Mutex<Option<RegionKey>> = Mutex::new(None);
    {
        let mut last = LAST.lock().map_err(|_| "PET_REGION_LOCK")?;
        if *last == Some(key) {
            return Ok(());
        }
        // SetWindowRgn can synchronously dispatch window messages. Never hold
        // our mutex across that call, and suppress a re-entrant identical apply.
        *last = Some(key);
    }
    let rects = spans(current.0, current.1, size.width, size.height)?;
    let header_size = std::mem::size_of::<RGNDATAHEADER>();
    let rect_size = std::mem::size_of::<RECT>();
    let bytes = header_size + rects.len() * rect_size;
    // u32 storage guarantees the alignment required by RGNDATAHEADER and RECT.
    let mut storage = vec![0u32; bytes.div_ceil(4)];
    unsafe {
        let pointer = storage.as_mut_ptr().cast::<u8>();
        pointer.cast::<RGNDATAHEADER>().write(RGNDATAHEADER {
            dwSize: header_size as u32,
            iType: RDH_RECTANGLES,
            nCount: rects.len() as u32,
            nRgnSize: (rects.len() * rect_size) as u32,
            rcBound: RECT {
                left: offset.0,
                top: offset.1,
                right: offset.0 + size.width as i32,
                bottom: offset.1 + size.height as i32,
            },
        });
        for (index, r) in rects.iter().enumerate() {
            pointer
                .add(header_size + index * rect_size)
                .cast::<RECT>()
                .write(RECT {
                    left: r[0] + offset.0,
                    top: r[1] + offset.1,
                    right: r[2] + offset.0,
                    bottom: r[3] + offset.1,
                });
        }
        let region = ExtCreateRegion(None, bytes as u32, pointer.cast::<RGNDATA>());
        if region.0.is_null() {
            *LAST.lock().map_err(|_| "PET_REGION_LOCK")? = None;
            return Err("PET_REGION_CREATE".into());
        }
        if SetWindowRgn(hwnd, Some(region), true) == 0 {
            let _ = DeleteObject(HGDIOBJ(region.0));
            *LAST.lock().map_err(|_| "PET_REGION_LOCK")? = None;
            return Err("PET_REGION_APPLY".into());
        }
        // Windows owns the region after a successful SetWindowRgn.
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn apply(_: &tauri::WebviewWindow) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transparent_padding_is_not_a_target_at_any_supported_dpi() {
        for scale in [1., 1.25, 1.5, 1.75, 2.] {
            let (w, h) = ((144. * scale) as u32, (192. * scale) as u32);
            assert!(!hit(0., 0., w, h));
            assert!(!hit(30. * scale, 90. * scale, w, h));
            assert!(hit(75. * scale, 50. * scale, w, h));
            assert!(!hit(f64::NAN, 30., w, h));
            assert!(!hit(w as f64, 30., w, h));
            let rects = spans(0, 0, w, h).unwrap();
            let first = image::imageops::crop_imm(image(0).unwrap(), 0, 0, 144, 192).to_image();
            let expected =
                image::imageops::resize(&first, w, h, image::imageops::FilterType::Nearest);
            for y in 0..h {
                for x in 0..w {
                    let covered = rects.iter().any(|r| {
                        x as i32 >= r[0]
                            && (x as i32) < r[2]
                            && y as i32 >= r[1]
                            && (y as i32) < r[3]
                    });
                    assert_eq!(
                        covered,
                        expected.get_pixel(x, y)[3] >= 128,
                        "{scale}: {x},{y}"
                    );
                }
            }
        }
    }
    #[test]
    fn generated_states_have_identical_approved_endpoints_and_bounded_frames() {
        let reference = image::load_from_memory(crate::edition::PET_REFERENCE)
            .unwrap()
            .to_rgba8();
        for (index, (name, _, frames)) in crate::pet_sprites::SPRITES.iter().enumerate() {
            let im = image(index).unwrap();
            let mut expected = reference.clone();
            if let Some((_, bytes)) = crate::pet_sprites::BADGES.iter().find(|s| s.0 == *name) {
                let badge = image::load_from_memory(bytes).unwrap().to_rgba8();
                image::imageops::overlay(&mut expected, &badge, 108, 4);
            }
            for frame in [0, frames - 1] {
                assert_eq!(
                    image::imageops::crop_imm(im, frame as u32 * 144, 0, 144, 192).to_image(),
                    expected,
                    "{name}"
                );
            }
            assert!(!hit_frame(index, *frames, 75., 50., 144, 192));
        }
    }

    #[test]
    fn every_animation_frame_matches_nearest_alpha_at_all_dpi() {
        for (sprite, (_, _, frames)) in crate::pet_sprites::SPRITES.iter().enumerate() {
            for frame in 0..*frames {
                let source = image::imageops::crop_imm(
                    image(sprite).unwrap(),
                    frame as u32 * 144,
                    0,
                    144,
                    192,
                )
                .to_image();
                for scale in [1., 1.25, 1.5, 1.75, 2.] {
                    let (w, h) = ((144. * scale) as u32, (192. * scale) as u32);
                    let expected = image::imageops::resize(
                        &source,
                        w,
                        h,
                        image::imageops::FilterType::Nearest,
                    );
                    let mut coverage = vec![false; (w * h) as usize];
                    for r in spans(sprite, frame, w, h).unwrap() {
                        for y in r[1]..r[3] {
                            for x in r[0]..r[2] {
                                coverage[(y as u32 * w + x as u32) as usize] = true;
                            }
                        }
                    }
                    for (index, pixel) in expected.pixels().enumerate() {
                        assert_eq!(
                            coverage[index],
                            pixel[3] >= 128,
                            "sprite {sprite} frame {frame} scale {scale} pixel {index}"
                        );
                    }
                }
            }
        }
    }
}
