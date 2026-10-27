//! What an AI agent is doing, drawn where the user is looking: a cyan glow
//! with a light sweeping down the output while it reads, an amber pulse and
//! typing dots while it types. Without motion, the same colors hold steady.

use std::f64::consts::TAU;

use egui::{Color32, Painter, Rect, Stroke, StrokeKind, Ui, pos2, vec2};

use crate::mcp::Cue;

pub const READING: Color32 = Color32::from_rgb(0x38, 0xc8, 0xf0);
pub const WRITING: Color32 = Color32::from_rgb(0xff, 0xa8, 0x2e);

/// The animation clock, scaled by the Animations speed. `None` holds cues steady.
#[derive(Clone, Copy)]
pub struct Motion {
    pub time: f64,
    pub speed: f32,
}

fn color(cue: Cue) -> Color32 {
    match cue {
        Cue::Reading => READING,
        Cue::Writing(_) => WRITING,
    }
}

/// The cue's color dark enough to read as text on a light theme.
pub fn text_color(ui: &Ui, cue: Cue) -> Color32 {
    match (cue, ui.visuals().dark_mode) {
        (_, true) => color(cue),
        (Cue::Reading, false) => Color32::from_rgb(0x00, 0x6a, 0x8a),
        (Cue::Writing(_), false) => Color32::from_rgb(0x96, 0x50, 0x00),
    }
}

/// How bright the glow is now: reading breathes, typing flares and settles.
fn strength(cue: Cue, motion: Option<Motion>) -> f32 {
    match (cue, motion) {
        (_, None) => 0.8,
        (Cue::Reading, Some(m)) => 0.55 + 0.45 * (0.5 + 0.5 * (m.time * TAU / 1.6).sin()) as f32,
        (Cue::Writing(age), Some(m)) => 0.7 + 0.3 * (1.0 - age * m.speed / PULSE).max(0.0),
    }
}

/// Seconds a typing pulse takes to spread out and fade.
const PULSE: f32 = 0.7;

/// Light falling off outward from `rect`.
fn glow(painter: &Painter, rect: Rect, radius: f32, color: Color32, strength: f32, spread: f32) {
    const RINGS: usize = 6;
    for ring in 0..RINGS {
        let t = ring as f32 / RINGS as f32;
        let grow = t * spread;
        let stroke = Stroke::new(spread / RINGS as f32 + 0.5, color.gamma_multiply(strength * (1.0 - t).powi(2)));
        painter.rect_stroke(rect.expand(grow), radius + grow, stroke, StrokeKind::Outside);
    }
}

/// A ring leaving `rect` once per command.
fn pulse(painter: &Painter, rect: Rect, radius: f32, age: f32, reach: f32) {
    let t = age / PULSE;
    if t < 1.0 {
        let grow = 1.0 + t * reach;
        let stroke = Stroke::new(2.0, WRITING.gamma_multiply(0.8 * (1.0 - t)));
        painter.rect_stroke(rect.expand(grow), radius + grow, stroke, StrokeKind::Outside);
    }
}

/// Fill for the tab's background, under its label.
pub fn tab_fill(cue: Cue, motion: Option<Motion>) -> Color32 {
    color(cue).gamma_multiply(0.06 + 0.10 * strength(cue, motion))
}

/// Border, glow and, while typing, three dots along the tab's bottom edge.
pub fn tab(painter: &Painter, rect: Rect, cue: Cue, motion: Option<Motion>) {
    let color = color(cue);
    let strength = strength(cue, motion);
    glow(painter, rect, 3.0, color, 0.45 * strength, 5.0);
    painter.rect_stroke(rect, 3.0, Stroke::new(1.0, color.gamma_multiply(strength)), StrokeKind::Inside);
    if let Cue::Writing(age) = cue {
        if let Some(m) = motion {
            pulse(painter, rect, 3.0, age * m.speed, 6.0);
        }
        for dot in 0..3 {
            let lift = motion.map_or(0.0, |m| 1.5 * ((m.time * 9.0 - f64::from(dot) * 0.9).sin().max(0.0)) as f32);
            let center = pos2(rect.center().x + (dot as f32 - 1.0) * 5.0, rect.bottom() - 3.5 - lift);
            painter.circle_filled(center, 1.4, WRITING);
        }
    }
}

/// Glow around the terminal; while reading, a band of light sweeps down it.
pub fn terminal(painter: &Painter, rect: Rect, cue: Cue, motion: Option<Motion>) {
    let color = color(cue);
    let strength = strength(cue, motion);
    glow(painter, rect, 2.0, color, 0.5 * strength, 7.0);
    painter.rect_stroke(rect, 2.0, Stroke::new(1.0, color.gamma_multiply(0.7 * strength)), StrokeKind::Inside);
    let Some(m) = motion else { return };
    match cue {
        Cue::Writing(age) => pulse(painter, rect, 2.0, age * m.speed, 10.0),
        Cue::Reading => {
            // Faint enough that output under it stays readable.
            let band = (rect.height() * 0.16).clamp(24.0, 90.0);
            let phase = (m.time / 1.8).fract() as f32;
            let center = rect.top() - band + phase * (rect.height() + 2.0 * band);
            let clipped = painter.with_clip_rect(rect);
            const SLICES: usize = 12;
            let slice = 2.0 * band / SLICES as f32;
            for i in 0..SLICES {
                let offset = (i as f32 + 0.5) * slice - band;
                let alpha = 0.14 * (1.0 - offset.abs() / band).powi(2);
                let top = center + offset - slice / 2.0;
                let strip = Rect::from_min_size(pos2(rect.left(), top), vec2(rect.width(), slice));
                clipped.rect_filled(strip, 0.0, READING.gamma_multiply(alpha));
            }
            clipped.hline(rect.x_range(), center, Stroke::new(1.0, READING.gamma_multiply(0.35)));
        }
    }
}
