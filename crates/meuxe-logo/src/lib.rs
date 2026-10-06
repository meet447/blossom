//! Five-petal blossom used as the Meuxe boot mark.
//!
//! The shape is drawn from scratch: wide overlapping petals, a deeper throat,
//! a small notch at each tip, and a ring of stamens. Callers plot the returned
//! color; pixels outside the flower are left to the background.

#![no_std]

/// Screen-space petal axes. One unit is 1024. The first petal points up.
const DIRS: [(i32, i32); 5] = [
    (0, -1024),
    (974, -316),
    (602, 828),
    (-602, 828),
    (-974, -316),
];

/// Stamen tips, in hundredths of the radius. Screen y grows downward.
const ANTHERS: [(i32, i32); 12] = [
    (0, -24),
    (16, -18),
    (24, -4),
    (20, 14),
    (6, 24),
    (-8, 22),
    (-20, 12),
    (-24, -2),
    (-16, -18),
    (10, -10),
    (-8, 4),
    (8, 12),
];

const PALE: u32 = 0xF8D4E0;
const MID: u32 = 0xF27AA0;
const DEEP: u32 = 0xE23068;
const CENTER: u32 = 0xC21858;
const EYE: u32 = 0xF2C94A;
const ANTHER: u32 = 0xC9842A;
const FILAMENT: u32 = 0xFFF6F4;

/// Color of the blossom at an offset from its center, or nothing if that
/// pixel is background.
pub fn sample(dx: i32, dy: i32, radius: i32) -> Option<u32> {
    if radius <= 8 {
        return None;
    }
    let mut color = None;
    for (dir_x, dir_y) in DIRS {
        if let Some(petal) = petal(dx, dy, radius, dir_x, dir_y) {
            color = Some(petal);
        }
    }
    if let Some(disk) = disk(dx, dy, radius * 16 / 100, CENTER) {
        color = Some(disk);
    }
    let stem = (radius / 48).max(1);
    for (ax, ay) in ANTHERS {
        let tx = ax * radius / 100;
        let ty = ay * radius / 100;
        if near_segment(dx, dy, tx, ty, stem) {
            color = Some(FILAMENT);
        }
    }
    if let Some(disk) = disk(dx, dy, radius * 11 / 100, CENTER) {
        color = Some(disk);
    }
    let head = (radius / 22).max(2);
    for (ax, ay) in ANTHERS {
        let tx = ax * radius / 100;
        let ty = ay * radius / 100;
        if dist2(dx - tx, dy - ty) <= head as i64 * head as i64 {
            color = Some(ANTHER);
        }
    }
    if let Some(eye) = disk(dx, dy, radius * 5 / 100, EYE) {
        color = Some(eye);
    }
    color
}

/// Visit every pixel of the blossom. Offsets are relative to the center.
pub fn for_each_pixel(radius: i32, mut plot: impl FnMut(i32, i32, u32)) {
    if radius <= 8 {
        return;
    }
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            if let Some(rgb) = sample(dx, dy, radius) {
                plot(dx, dy, rgb);
            }
        }
    }
}

fn petal(dx: i32, dy: i32, radius: i32, dir_x: i32, dir_y: i32) -> Option<u32> {
    let along = (dx as i64 * dir_x as i64 + dy as i64 * dir_y as i64) / 1024;
    let across = (dx as i64 * -dir_y as i64 + dy as i64 * dir_x as i64) / 1024;
    let axis = radius as i64 * 50 / 100;
    let wide = radius as i64 * 40 / 100;
    let origin = radius as i64 * 26 / 100;
    if axis <= 0 || wide <= 0 {
        return None;
    }
    let local = along - origin;
    let left = local * local * wide * wide + across * across * axis * axis;
    let right = axis * axis * wide * wide;
    if left > right {
        return None;
    }
    let tip = origin + axis;
    let notch = radius as i64 / 9;
    let notch_y = along - (tip - notch / 5);
    if notch > 0 && notch_y * notch_y + across * across < notch * notch && along > tip - notch {
        return None;
    }
    let edge = (left * 255 / right) as i32;
    let dist = dist2(dx, dy);
    let limit = radius as i64 * radius as i64;
    let throat = if dist >= limit {
        255
    } else {
        (dist * 255 / limit) as i32
    };
    let body = lerp(MID, PALE, edge);
    let mut color = if throat < 150 {
        lerp(DEEP, body, throat * 255 / 150)
    } else {
        body
    };
    if across < 0 && edge > 150 {
        color = lerp(color, 0xFFEAF2, (edge - 150) / 3);
    }
    if across.abs() <= radius as i64 / 26 && along > radius as i64 / 8 && along < tip - notch {
        color = darken(color, 28);
    }
    Some(color)
}

fn disk(dx: i32, dy: i32, radius: i32, rgb: u32) -> Option<u32> {
    if radius <= 0 {
        return None;
    }
    if dist2(dx, dy) <= radius as i64 * radius as i64 {
        Some(rgb)
    } else {
        None
    }
}

fn near_segment(px: i32, py: i32, ax: i32, ay: i32, thick: i32) -> bool {
    let dx = ax as i64;
    let dy = ay as i64;
    let len2 = dx * dx + dy * dy;
    if len2 == 0 {
        return dist2(px, py) <= thick as i64 * thick as i64;
    }
    let t = (px as i64 * dx + py as i64 * dy).clamp(0, len2);
    let qx = dx * t / len2;
    let qy = dy * t / len2;
    let ex = px as i64 - qx;
    let ey = py as i64 - qy;
    ex * ex + ey * ey <= thick as i64 * thick as i64
}

fn dist2(dx: i32, dy: i32) -> i64 {
    dx as i64 * dx as i64 + dy as i64 * dy as i64
}

fn lerp(from: u32, to: u32, t: i32) -> u32 {
    let t = t.clamp(0, 255);
    let channel = |shift: u32| {
        let a = ((from >> shift) & 0xFF) as i32;
        let b = ((to >> shift) & 0xFF) as i32;
        (a + (b - a) * t / 255) as u32
    };
    (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

fn darken(rgb: u32, amount: i32) -> u32 {
    let channel = |shift: u32| {
        let value = ((rgb >> shift) & 0xFF) as i32 - amount;
        value.clamp(0, 255) as u32
    };
    (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_petals_and_a_yellow_eye() {
        let radius = 100;
        let eye = sample(0, 0, radius).unwrap();
        assert_eq!(eye, EYE);
        for (dir_x, dir_y) in DIRS {
            let dx = dir_x * radius * 45 / 100 / 1024;
            let dy = dir_y * radius * 45 / 100 / 1024;
            let pixel = sample(dx, dy, radius).expect("petal");
            let red = pixel >> 16;
            assert!(red > 0xC0, "petal {pixel:#x} is not pink");
        }
        assert!(sample(radius * 2, radius * 2, radius).is_none());
    }

    #[test]
    fn upper_petal_is_lighter_than_the_throat() {
        let radius = 120;
        let tip = sample(18, -radius * 58 / 100, radius).unwrap();
        let throat = sample(24, -radius * 24 / 100, radius).unwrap();
        assert!(
            tip >> 16 >= throat >> 16,
            "tip {tip:#x} throat {throat:#x}"
        );
    }
}
