//! Fine ASCII artwork: four glyphs in each direction per simulation cell.
//! Keeping these bounds aligned with the model preserves collision geometry
//! when the arena uses its smaller graphics font.

use crate::invade::Rig;
use crate::invade::content::{ASPECT, DeviceKind};

const DETAIL: f32 = 4.0;

pub(super) fn rig(rig: Rig) -> Vec<String> {
    double(match rig {
        Rig::Probe => &["  /==\\  ", "<==^^==>"],
        Rig::Ghost => &[r" /\/\ ", "<~==~>"],
        Rig::Bastion => &[" /======\\ ", "[|==^^==|]"],
    })
}

/// Rotate the rig in visual space, fitting the result inside the model's
/// unchanged one-row hitbox. Its separate nose marker keeps heading clear
/// even when a broad ship is seen end-on at this very fine resolution.
pub(super) fn flight_rig(kind: Rig, heading: f32) -> Vec<String> {
    let source = rig(kind);
    let width = source[0].len();
    let height = source.len();
    let (sin, cos) = heading.sin_cos();
    let visual_w = width as f32;
    let visual_h = height as f32 * ASPECT;
    let rotated_w = visual_w * cos.abs() + visual_h * sin.abs();
    let rotated_h = visual_w * sin.abs() + visual_h * cos.abs();
    let scale = (visual_w / rotated_w).min(visual_h / rotated_h);
    let mut out = Vec::with_capacity(height);
    for row in 0..height {
        let mut line = String::with_capacity(width);
        for column in 0..width {
            let x = (column as f32 + 0.5 - visual_w / 2.0) / scale;
            let y = (row as f32 + 0.5 - height as f32 / 2.0) * ASPECT / scale;
            let original_x = x * cos + y * sin + visual_w / 2.0;
            let original_y = (-x * sin + y * cos) / ASPECT + height as f32 / 2.0;
            let glyph =
                if original_x >= 0.0 && original_x < width as f32 && original_y >= 0.0 && original_y < height as f32 {
                    source[original_y as usize].as_bytes()[original_x as usize] as char
                } else {
                    ' '
                };
            line.push(rotate_stroke(glyph, sin, cos));
        }
        out.push(line);
    }
    out
}

fn rotate_stroke(glyph: char, sin: f32, cos: f32) -> char {
    let (dx, dy, arrow) = match glyph {
        '/' => (1.0, -1.0, false),
        '\\' => (1.0, 1.0, false),
        '-' | '_' | '=' => (1.0, 0.0, false),
        '|' => (0.0, 1.0, false),
        '^' => (0.0, -1.0, true),
        'v' => (0.0, 1.0, true),
        '<' => (-1.0, 0.0, true),
        '>' => (1.0, 0.0, true),
        _ => return glyph,
    };
    let (x, y) = (dx * cos - dy * sin, dx * sin + dy * cos);
    if x.abs() > y.abs() * 2.0 {
        if arrow { if x < 0.0 { '<' } else { '>' } } else { '-' }
    } else if y.abs() > x.abs() * 2.0 {
        if arrow { if y < 0.0 { '^' } else { 'v' } } else { '|' }
    } else if x * y < 0.0 {
        '/'
    } else {
        '\\'
    }
}

/// Collision-matched circle with a jagged ASCII rim and tumbling craters.
/// Reserved rock glyphs stay distinct from packets, XP and attack warnings,
/// including in monochrome themes where color cannot distinguish them.
pub(super) fn asteroid(radius: f32, spin: f32) -> Vec<String> {
    let width = (radius * 2.0 * DETAIL).ceil() as usize;
    let height = (radius * 2.0 / ASPECT * DETAIL).ceil() as usize;
    let inside = |x: i32, y: i32| {
        let dx = (x as f32 + 0.5) / DETAIL - radius;
        let dy = ((y as f32 + 0.5) / DETAIL - radius / ASPECT) * ASPECT;
        dx * dx + dy * dy <= radius * radius
    };
    let (sin, cos) = spin.sin_cos();
    (0..height)
        .map(|row| {
            (0..width)
                .map(|column| {
                    let (x, y) = (column as i32, row as i32);
                    if !inside(x, y) {
                        return ' ';
                    }
                    let dx = (column as f32 + 0.5) / DETAIL - radius;
                    let dy = ((row as f32 + 0.5) / DETAIL - radius / ASPECT) * ASPECT;
                    let rim = !inside(x - 1, y) || !inside(x + 1, y) || !inside(x, y - 1) || !inside(x, y + 1);
                    if rim {
                        let segment = ((dy.atan2(dx) + spin).rem_euclid(std::f32::consts::TAU) * 3.0) as usize;
                        [')', '}', ']', '(', '{', '['][segment % 6]
                    } else {
                        let (rx, ry) = (dx * cos + dy * sin, -dx * sin + dy * cos);
                        if (rx * 2.2).sin() + (ry * 3.1).cos() > 1.3 { '%' } else { ' ' }
                    }
                })
                .collect()
        })
        .collect()
}

pub(super) fn device(kind: DeviceKind, damaged: bool) -> Vec<String> {
    double(match kind {
        DeviceKind::Endpoint => &[" /==\\ ", "<.==.>"],
        DeviceKind::Router => &[" /++\\ ", "<+--+>"],
        DeviceKind::Firewall if damaged => &["[#/ ##", "|= :/|"],
        DeviceKind::Firewall => &["[####]", "|=##=|"],
        DeviceKind::Switch => &["[====]", "|::::|"],
        DeviceKind::IdsProbe => &[" /!!\\ ", "<!==!>"],
    })
}

pub(super) fn guard() -> Vec<String> {
    double(&["|/##\\|", "|=##=|"])
}

pub(super) fn detail(source: &[&str]) -> Vec<String> {
    let expanded = double(source);
    double(&expanded.iter().map(String::as_str).collect::<Vec<_>>())
}

/// Expand an encounter's outline into finer strokes and panel details.
/// Letters remain single letters, so boss labels are readable rather than
/// duplicated. Padding maintains the complete rectangular model footprint.
fn double(source: &[&str]) -> Vec<String> {
    let width = source.iter().map(|line| line.len()).max().unwrap_or(0);
    let mut out = Vec::with_capacity(source.len() * 2);
    for line in source {
        let mut top = String::with_capacity(width * 2);
        let mut bottom = String::with_capacity(width * 2);
        for ch in line.chars().chain(std::iter::repeat(' ')).take(width) {
            let (upper, lower) = match ch {
                '/' => (" /", "/ "),
                '\\' => ("\\ ", " \\"),
                '|' => ("| ", "| "),
                '-' => ("--", "  "),
                '_' => ("  ", "__"),
                '=' => ("==", "--"),
                '#' => ("##", ":#"),
                '[' => ("+-", "| "),
                ']' => ("-+", " |"),
                '(' => (" /", " \\"),
                ')' => ("\\ ", "/ "),
                '<' => (" /", " \\"),
                '>' => ("\\ ", "/ "),
                '{' => (" /", " \\"),
                '}' => ("\\ ", "/ "),
                '+' => (" |", "-+"),
                '~' => ("~_", "  "),
                '.' => ("  ", " ."),
                '\'' => ("' ", "  "),
                '`' => ("` ", "  "),
                'o' => ("()", ".."),
                '@' => ("@)", "(_"),
                '!' => ("! ", ". "),
                ' ' => ("  ", "  "),
                other => {
                    top.push(other);
                    top.push(' ');
                    bottom.push_str("  ");
                    continue;
                }
            };
            top.push_str(upper);
            bottom.push_str(lower);
        }
        out.push(top);
        out.push(bottom);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invade::content::{BossKind, MinibossKind};

    fn bounds(sprite: &[String], width: usize, height: usize) {
        assert_eq!(sprite.len(), height * 4);
        assert!(sprite.iter().all(|line| line.is_ascii() && line.len() == width * 4));
    }

    #[test]
    fn fine_sprites_preserve_every_model_footprint() {
        for kind in Rig::ALL {
            bounds(&rig(kind), kind.sprite().len(), 1);
        }
        for kind in DeviceKind::ALL {
            bounds(&device(kind, false), kind.sprite().len(), 1);
            bounds(&device(kind, true), kind.sprite().len(), 1);
        }
        bounds(&guard(), 3, 1);
        for source in BossKind::CYCLE
            .into_iter()
            .map(BossKind::sprite)
            .chain(MinibossKind::ALL.into_iter().map(MinibossKind::sprite))
        {
            let width = source.iter().map(|line| line.len()).max().unwrap();
            bounds(&detail(source), width, source.len());
        }
    }

    #[test]
    fn rotating_rigs_stay_inside_the_model_body_and_show_each_heading() {
        for kind in Rig::ALL {
            for turn in 0..32 {
                let heading = turn as f32 / 32.0 * std::f32::consts::TAU;
                let sprite = flight_rig(kind, heading);
                bounds(&sprite, kind.width() as usize, 1);
                assert!(sprite.iter().any(|line| line.bytes().any(|c| c != b' ')), "{kind:?} at {heading}");
            }
            assert_ne!(flight_rig(kind, 0.0), flight_rig(kind, std::f32::consts::FRAC_PI_2));
        }
        assert_eq!(rotate_stroke('^', 1.0, 0.0), '>');
        assert_eq!(rotate_stroke('>', 1.0, 0.0), 'v');
        assert_eq!(rotate_stroke('^', 0.0, -1.0), 'v');
    }

    #[test]
    fn asteroid_art_matches_visual_circle_and_reserved_glyphs_while_tumbling() {
        for radius in [2.2, 3.5, 5.0] {
            let first = asteroid(radius, 0.0);
            let turned = asteroid(radius, 0.9);
            assert_ne!(first, turned);
            assert_eq!(first.len(), (radius * 2.0 / ASPECT * DETAIL).ceil() as usize);
            for (row, (line, rotated)) in first.iter().zip(&turned).enumerate() {
                assert_eq!(line.len(), (radius * 2.0 * DETAIL).ceil() as usize);
                assert!(line.is_ascii());
                for (column, (glyph, rotated_glyph)) in line.chars().zip(rotated.chars()).enumerate() {
                    assert!(" (){}[]%".contains(glyph));
                    let dx = (column as f32 + 0.5) / DETAIL - radius;
                    let dy = ((row as f32 + 0.5) / DETAIL - radius / ASPECT) * ASPECT;
                    if dx * dx + dy * dy > radius * radius {
                        assert_eq!((glyph, rotated_glyph), (' ', ' '), "rock art escaped its collision circle");
                    }
                    if "(){}[]".contains(glyph) {
                        assert!("(){}[]".contains(rotated_glyph), "tumbling must preserve the solid rim");
                    }
                }
            }
        }
    }
}
