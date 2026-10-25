//! 状态图标生成（T-33）——程序化绘制实心圆（绿/黄/红），无外部图标资源
//! （壳不携带资产文件；32×32 RGBA，纯函数可单测）。

use crate::supervisor::TrayColor;

/// 图标边长（px）。
pub const ICON_SIZE: u32 = 32;

/// 各状态的主色（RGB，无 alpha 通道）。
pub fn status_rgb(color: TrayColor) -> (u8, u8, u8) {
    match color {
        // 绿 #2ECC40 / 黄 #FFDC00 / 红 #FF4136
        TrayColor::Green => (0x2E, 0xCC, 0x40),
        TrayColor::Yellow => (0xFF, 0xDC, 0x00),
        TrayColor::Red => (0xFF, 0x41, 0x36),
    }
}

/// 生成状态图标 RGBA 缓冲（行优先，4 通道），实心圆 + 1px 抗锯齿边缘。
///
/// 返回 `(rgba, width, height)`，可直接喂 `tray_icon::Icon::from_rgba`。
pub fn status_icon_rgba(color: TrayColor) -> (Vec<u8>, u32, u32) {
    let (r, g, b) = status_rgb(color);
    let size = ICON_SIZE;
    let center = (size as f32 - 1.0) / 2.0;
    let radius = size as f32 / 2.0 - 2.0; // 留 2px 透明边

    let mut rgba = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let dist = (dx * dx + dy * dy).sqrt();
            // 距离插值 alpha：圆内不透明，边缘 1px 渐变，圆外透明
            let alpha = if dist <= radius - 1.0 {
                255.0
            } else if dist <= radius {
                (255.0 * (radius - dist)).round().clamp(0.0, 255.0)
            } else {
                0.0
            };
            let idx = ((y * size + x) * 4) as usize;
            rgba[idx] = r;
            rgba[idx + 1] = g;
            rgba[idx + 2] = b;
            rgba[idx + 3] = alpha as u8;
        }
    }
    (rgba, size, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_size_matches_icon_dimensions() {
        for color in [TrayColor::Green, TrayColor::Yellow, TrayColor::Red] {
            let (rgba, w, h) = status_icon_rgba(color);
            assert_eq!((w, h), (ICON_SIZE, ICON_SIZE));
            assert_eq!(rgba.len(), (ICON_SIZE * ICON_SIZE * 4) as usize);
        }
    }

    #[test]
    fn center_pixel_is_opaque_state_color_and_corner_transparent() {
        for color in [TrayColor::Green, TrayColor::Yellow, TrayColor::Red] {
            let (rgba, w, _) = status_icon_rgba(color);
            let (r, g, b) = status_rgb(color);
            let center = ((ICON_SIZE / 2) * w + (ICON_SIZE / 2)) as usize * 4;
            assert_eq!(rgba[center], r);
            assert_eq!(rgba[center + 1], g);
            assert_eq!(rgba[center + 2], b);
            assert_eq!(rgba[center + 3], 255, "圆心必须完全不透明");

            let corner = 0; // (0,0) 在半径外
            assert_eq!(rgba[corner + 3], 0, "角落必须透明");
        }
    }

    #[test]
    fn colors_differ_across_states() {
        let (green, _, _) = status_icon_rgba(TrayColor::Green);
        let (red, _, _) = status_icon_rgba(TrayColor::Red);
        assert_ne!(green, red, "绿/红图标必须可区分");

        let (yellow, _, _) = status_icon_rgba(TrayColor::Yellow);
        assert_ne!(green, yellow);
        assert_ne!(yellow, red);
    }
}
