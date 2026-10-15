use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Monitor {
    pub id: String,
    pub work: Rect,
    pub scale: f64,
}
pub fn pet_size(m: &Monitor) -> (f64, f64) {
    // The user approved a 192px logical canvas for facial legibility, also on
    // small displays. Only constrain it when the actual work area cannot fit it.
    let fit = (m.work.width / (144.0 * m.scale))
        .min(m.work.height / (192.0 * m.scale))
        .clamp(0.0, 1.0);
    (144.0 * m.scale * fit, 192.0 * m.scale * fit)
}
pub fn settle(x: f64, m: &Monitor) -> Rect {
    let (w, h) = pet_size(m);
    settle_sized(x, m, w, h)
}
pub fn settle_sized(x: f64, m: &Monitor, w: f64, h: f64) -> Rect {
    Rect {
        x: x.clamp(m.work.x, (m.work.x + m.work.width - w).max(m.work.x)),
        y: m.work.y + m.work.height - h,
        width: w,
        height: h,
    }
}
#[cfg(test)]
pub fn anchor(pet: Rect, m: &Monitor) -> Rect {
    let w = (420.0 * m.scale).min(m.work.width);
    let h = (620.0 * m.scale).min(m.work.height);
    anchor_sized(pet, m, w, h)
}
pub fn anchor_sized(pet: Rect, m: &Monitor, width: f64, height: f64) -> Rect {
    let w = width.min(m.work.width);
    let h = height.min(m.work.height);
    let gap = 12.0 * m.scale;
    let left = pet.x - w - gap;
    let right = pet.x + pet.width + gap;
    let x = if left >= m.work.x {
        left
    } else if right + w <= m.work.x + m.work.width {
        right
    } else {
        m.work.x + (m.work.width - w) / 2.0
    };
    Rect {
        x,
        y: (pet.y + pet.height - h).clamp(m.work.y, m.work.y + m.work.height - h),
        width: w,
        height: h,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multiple_dpi_negative_and_small() {
        for (x, y, w, h, s) in [
            (0., 0., 1920., 1040., 1.),
            (-2560., -400., 2560., 1400., 1.25),
            (1920., 0., 3840., 2120., 1.5),
            (100., 100., 1280., 760., 1.),
            (900., -1080., 1280., 1000., 2.),
        ] {
            let m = Monitor {
                id: "fixture".into(),
                work: Rect {
                    x,
                    y,
                    width: w,
                    height: h,
                },
                scale: s,
            };
            let p = settle(-99999., &m);
            assert_eq!(p.x, x);
            assert_eq!(p.y + p.height, y + h);
            let a = anchor(p, &m);
            assert!(a.x >= x && a.x + a.width <= x + w);
            assert!(a.y >= y && a.y + a.height <= y + h);
        }
    }

    #[test]
    fn approved_canvas_retains_legibility_on_small_displays_at_all_dpi() {
        for scale in [1., 1.25, 1.5, 1.75, 2.] {
            let m = Monitor {
                id: "small".into(),
                work: Rect {
                    x: -1280.,
                    y: 40.,
                    width: 1280.,
                    height: 720.,
                },
                scale,
            };
            assert_eq!(pet_size(&m), (144. * scale, 192. * scale));
            let p = settle(-99999., &m);
            assert_eq!(p.y + p.height, m.work.y + m.work.height);
        }
    }

    #[test]
    fn native_frame_is_included_above_bottom_or_top_taskbars() {
        for scale in [1., 1.25, 1.5, 2.] {
            let m = Monitor {
                id: "frame".into(),
                scale,
                work: Rect {
                    x: -1200.,
                    y: 48.,
                    width: 1200.,
                    height: 700.,
                },
            };
            let p = settle(-500., &m);
            let a = anchor_sized(p, &m, 420. * scale + 16., 620. * scale + 39.);
            assert!(a.x >= m.work.x && a.x + a.width <= m.work.x + m.work.width);
            assert!(a.y >= m.work.y && a.y + a.height <= m.work.y + m.work.height);
            assert_eq!(a.y + a.height, m.work.y + m.work.height);
        }
    }
    #[test]
    fn undecorated_pet_resize_margins_also_fit_at_landing() {
        let m = Monitor {
            id: "pet-frame".into(),
            scale: 1.,
            work: Rect {
                x: 0.,
                y: 0.,
                width: 1920.,
                height: 1032.,
            },
        };
        let p = settle_sized(99999., &m, 104., 113.);
        assert_eq!(p.y + p.height, 1032.);
        assert_eq!(p.x + p.width, 1920.);
    }
}
