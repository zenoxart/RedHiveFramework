//! Piano roll: notes of one channel in the selected pattern. Draw / paint / select / slice / erase / mute
//! tools, snap, scale highlighting and snapping, chord stamps, arpeggiator, strum, flam, quantize, legato,
//! humanize, chord progressions, velocity lane and ghost notes of the other channels.

use super::keyboard::is_black;
use super::nav::{self, AreaZoom, ViewTarget};
use super::widgets::{self, snap_floor};
use crate::state::{State, Tool};
use crate::theme;
use dragon_engine::device::Command as Cmd;
use dragon_engine::edit::{self, ArpMode, CHORDS, SCALES};
use dragon_engine::instruments::note_name;
use dragon_engine::model::*;
use dragon_engine::render::LiveEvent;
use egui::{Color32, Pos2, Rect, RichText, Sense, Stroke, Ui, Vec2};

const KEYS_W: f32 = 54.0;
const VEL_H: f32 = 70.0;
const RULER: f32 = 18.0;

#[derive(Debug, Clone)]
pub enum Drag {
    /// Moving the selected notes: `grab` is the note under the pointer (its start snaps to the grid, the rest
    /// keep their offsets), `last_dp` the last transposition (to audition pitch changes).
    Move {
        anchor_tick: i64,
        anchor_pitch: i32,
        grab: Note,
        last_dp: i32,
        orig: Vec<(usize, Note)>,
    },
    Resize {
        anchor_tick: i64,
        orig: Vec<(usize, Note)>,
    },
    Select {
        from: Pos2,
    },
    Velocity,
    Paint {
        last_tick: u32,
    },
    /// the slice (knife) tool: a line from where the drag started to the pointer; every note it crosses
    /// is cut where the line passes its row (snapped to the grid; Alt = no snap).
    Knife {
        from: Pos2,
        /// Right-drag (Right-Shift+right-click): the smaller piece of each cut note is deleted.
        delete_small: bool,
    },
    /// the zoom tool: drag a box to zoom into it.
    Zoom {
        from: Pos2,
    },
}

/// A note and the other notes of its group (Shift+G groups).
fn group_of(notes: &[Note], i: usize) -> Vec<usize> {
    match notes.get(i).map(|n| n.group) {
        Some(g) if g > 0 => notes.iter().enumerate().filter(|(_, n)| n.group == g).map(|(k, _)| k).collect(),
        _ => vec![i],
    }
}

pub fn show(ui: &mut Ui, st: &mut State) {
    let pat = st.ui.pattern;
    if st.ui.piano_channel.is_none() {
        st.ui.piano_channel = st.ui.channel.or(st.session.project.channels.first().map(|c| c.id));
    }
    let Some(ch) = st.ui.piano_channel else {
        ui.label("Add a channel to the rack to write notes.");
        return;
    };
    toolbar(ui, st, pat, ch);
    let notes: Vec<Note> = st.session.project.pattern(pat).and_then(|p| p.channels.get(&ch)).map(|c| c.notes.clone()).unwrap_or_default();
    let ghosts: Vec<(Rgb, Note)> = if !st.ui.ghosts {
        Vec::new()
    } else {
        st.session
            .project
            .pattern(pat)
            .map(|p| {
                p.channels
                    .iter()
                    .filter(|(id, _)| **id != ch)
                    .flat_map(|(id, c)| {
                        let col = st.session.project.channel(*id).map(|x| x.color).unwrap_or_default();
                        c.notes.iter().map(move |n| (col, *n))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let ch_color = st.session.project.channel(ch).map(|c| theme::rgb(c.color)).unwrap_or(theme::ACCENT);
    let pat_len = st.session.project.pattern(pat).map(|p| p.length_beats()).unwrap_or(4.0);

    let avail = ui.available_rect_before_wrap();
    // Switching channel or pattern: centre the view on its notes.
    let focus_id = egui::Id::new("pr.focus");
    if ui.data(|d| d.get_temp::<(Id, Id)>(focus_id)) != Some((pat, ch)) {
        ui.data_mut(|d| d.insert_temp(focus_id, (pat, ch)));
        if !notes.is_empty() {
            let mean = notes.iter().map(|n| n.pitch as f32).sum::<f32>() / notes.len() as f32;
            let h = avail.height() - RULER - VEL_H;
            st.ui.pr_y = ((127.0 - mean) * st.ui.key_height - h / 2.0).clamp(0.0, (128.0 * st.ui.key_height - h).max(0.0));
            st.ui.pr_x = 0.0;
        }
    }
    let (rect, resp) = ui.allocate_exact_size(avail.size(), Sense::click_and_drag());
    if resp.hovered() || resp.is_pointer_button_down_on() {
        st.ui.editor = crate::state::Editor::PianoRoll;
    }
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, theme::BG);
    let grid = Rect::from_min_max(Pos2::new(rect.left() + KEYS_W, rect.top() + RULER), Pos2::new(rect.right(), rect.bottom() - VEL_H));
    let vel = Rect::from_min_max(Pos2::new(grid.left(), grid.bottom() + 4.0), rect.max);
    ui.data_mut(|d| d.insert_temp(egui::Id::new("pr.grid"), grid));
    let max_y = |kh: f32| (128.0 * kh - grid.height()).max(0.0);
    // Animated zoom.
    if let Some(t) = st.ui.pr_anim {
        let cur = ViewTarget { zoom: st.ui.piano_zoom as f64, x: st.ui.pr_x, vscale: st.ui.key_height, y: st.ui.pr_y };
        let (n, running) = nav::step(cur, t);
        st.ui.piano_zoom = n.zoom as f32;
        st.ui.pr_x = n.x.max(0.0);
        st.ui.key_height = n.vscale;
        st.ui.pr_y = n.y.clamp(0.0, max_y(n.vscale));
        if running {
            ui.ctx().request_repaint();
        } else {
            st.ui.pr_anim = None;
        }
    }
    let kh = st.ui.key_height;
    let zoom = st.ui.piano_zoom as f64;
    ui.data_mut(|d| d.insert_temp(egui::Id::new("pr.grid"), grid));

    // Ctrl + right-drag: zoom into the box (time and key height); Ctrl + right-click: zoom out.
    match nav::area_zoom(ui, grid, "piano") {
        Some(AreaZoom::To(r)) => {
            let b0 = st.ui.pr_x + (r.left() - grid.left()) as f64 / zoom;
            let b1 = st.ui.pr_x + (r.right() - grid.left()) as f64 / zoom;
            let nz = (grid.width() as f64 / (b1 - b0).max(1e-3)).clamp(12.0, 800.0);
            let nkh = if r.height() > kh * 2.0 { (kh * grid.height() / r.height()).clamp(6.0, 40.0) } else { kh };
            let rows_top = (r.top() - grid.top() + st.ui.pr_y) / kh;
            st.ui.pr_anim = Some(ViewTarget { zoom: nz, x: b0.max(0.0), vscale: nkh, y: (rows_top * nkh).clamp(0.0, max_y(nkh)) });
        }
        Some(AreaZoom::Out(at)) => {
            let b = st.ui.pr_x + (at.x - grid.left()) as f64 / zoom;
            let nz = (zoom / 2.0).clamp(12.0, 800.0);
            let nkh = (kh / 1.5).max(10.0).min(kh);
            let row = (at.y - grid.top() + st.ui.pr_y) / kh;
            let ny = row * nkh - (at.y - grid.top());
            st.ui.pr_anim = Some(ViewTarget { zoom: nz, x: (b - (at.x - grid.left()) as f64 / nz).max(0.0), vscale: nkh, y: ny.clamp(0.0, max_y(nkh)) });
        }
        None => {}
    }
    // Middle-drag pans.
    use super::keys::{side, SideKey};
    let shift_held = ui.input(|i| i.modifiers.shift);
    let alt_held = ui.input(|i| i.modifiers.alt);
    let rsh = shift_held && side(st, SideKey::RightShift);
    let lsh = shift_held && !rsh && side(st, SideKey::LeftShift);
    let ralt = alt_held && side(st, SideKey::RightAlt);
    let pan = nav::pan_drag(ui, rect, lsh);
    if pan != Vec2::ZERO {
        st.ui.pr_anim = None;
        st.ui.pr_x = (st.ui.pr_x - pan.x as f64 / zoom).max(0.0);
        st.ui.pr_y = (st.ui.pr_y - pan.y).clamp(0.0, max_y(kh));
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    // Wheel: scroll; Shift = horizontal; Ctrl = zoom around the mouse; Alt = key height around the mouse.
    if resp.hovered() {
        let (scroll, zoom_d, shift, alt) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers.shift, i.modifiers.alt));
        if zoom_d != 1.0 || scroll != Vec2::ZERO {
            st.ui.pr_anim = None;
        }
        let mouse = ui.input(|i| i.pointer.hover_pos()).unwrap_or(grid.center());
        let raw = ui.input(|i| i.raw_scroll_delta);
        let notch = raw.y + raw.x;
        if shift && alt {
            // Shift+Alt+wheel: nudge the selected notes by a quarter of the snap.
            if notch != 0.0 && !st.ui.notes.is_empty() {
                let step = ((PPQ as f64 * st.ui.snap.max(0.0625) / 4.0) as i64).max(1) * notch.signum() as i64;
                let sel = st.ui.notes.clone();
                st.session.edit("Nudge notes", |p| {
                    if let Some(c) = p.pattern_mut(pat).and_then(|pt| pt.channels.get_mut(&ch)) {
                        for i in &sel {
                            if let Some(n) = c.notes.get_mut(*i) {
                                n.start = (n.start as i64 + step).max(0) as u32;
                            }
                        }
                    }
                });
            }
        } else if alt && scroll.y != 0.0 {
            let nkh = (kh * (1.0 + scroll.y / 300.0)).clamp(6.0, 40.0);
            let my = mouse.y - grid.top();
            st.ui.pr_y = ((st.ui.pr_y + my) * nkh / kh - my).clamp(0.0, max_y(nkh));
            st.ui.key_height = nkh;
        } else if zoom_d != 1.0 {
            let b = st.ui.pr_x + (mouse.x - grid.left()) as f64 / zoom;
            st.ui.piano_zoom = (st.ui.piano_zoom * zoom_d).clamp(12.0, 800.0);
            st.ui.pr_x = (b - (mouse.x - grid.left()) as f64 / st.ui.piano_zoom as f64).max(0.0);
        } else if shift || scroll.x != 0.0 {
            st.ui.pr_x = (st.ui.pr_x - (if shift { scroll.y } else { scroll.x }) as f64 / zoom).max(0.0);
        } else {
            st.ui.pr_y = (st.ui.pr_y - scroll.y).clamp(0.0, max_y(kh));
        }
    }
    let kh = st.ui.key_height;
    // A key played live that is out of view: scroll so it's in the middle (not while dragging).
    if let Some(n) = st.ui.follow_note.take() {
        let row = (127 - n as i32) as f32 * kh;
        let (top, h) = (st.ui.pr_y, grid.height());
        if st.ui.pr_drag.is_none() && (row < top || row + kh > top + h) {
            st.ui.pr_anim = None;
            st.ui.pr_y = (row + kh / 2.0 - h / 2.0).clamp(0.0, max_y(kh));
        }
    }
    let zoom = st.ui.piano_zoom as f64;
    let (sx, sy) = (st.ui.pr_x, st.ui.pr_y);
    let x_of = |tick: f64| grid.left() + ((tick / PPQ as f64 - sx) * zoom) as f32;
    let tick_of = |x: f32| ((sx + (x - grid.left()) as f64 / zoom) * PPQ as f64).max(0.0);
    let y_of = |pitch: u8| grid.top() + (127 - pitch as i32) as f32 * kh - sy;
    let pitch_of = |y: f32| (127 - ((y - grid.top() + sy) / kh).floor() as i32).clamp(0, 127) as u8;
    // Note helpers (Alt+B) show the scale; off hides them.
    let scale: Option<&[u8]> = st.ui.scale.filter(|_| st.ui.helpers).map(|i| SCALES[i].1);
    let root = st.ui.scale_root;

    // key rows
    let gp = p.with_clip_rect(grid);
    let top_pitch = pitch_of(grid.top());
    let bottom_pitch = pitch_of(grid.bottom());
    for pitch in bottom_pitch..=top_pitch {
        let y = y_of(pitch);
        let row = Rect::from_x_y_ranges(grid.x_range(), y..=y + kh);
        let col = match scale {
            // With a scale, in-scale rows are lit (the root more) and the others stay dark.
            Some(sc) => {
                if (pitch as i32 - root as i32).rem_euclid(12) == 0 {
                    theme::ACCENT.lerp_to_gamma(Color32::from_gray(34), 0.62)
                } else if edit::in_scale(pitch, root, sc) {
                    theme::ACCENT_2.lerp_to_gamma(Color32::from_gray(38), 0.82)
                } else {
                    Color32::from_gray(20)
                }
            }
            None => {
                if is_black(pitch) {
                    Color32::from_gray(26)
                } else {
                    Color32::from_gray(34)
                }
            }
        };
        gp.rect_filled(row, 0.0, col);
        // A note being played live lights its whole row, so you see where it would land.
        let l = key_light(st, pitch);
        if l > 0.0 {
            gp.rect_filled(row, 0.0, theme::ACCENT_2.gamma_multiply(0.16 * l));
        }
        gp.line_segment([row.left_bottom(), row.right_bottom()], Stroke::new(0.5_f32, Color32::from_black_alpha(90)));
        if pitch % 12 == 0 {
            gp.line_segment([row.left_bottom(), row.right_bottom()], Stroke::new(1.0_f32, theme::GRID_BAR));
        }
    }
    // beat grid
    let end_beat = sx + grid.width() as f64 / zoom;
    let step = if zoom < 40.0 {
        1.0
    } else if zoom < 160.0 {
        0.25
    } else {
        0.125
    };
    let mut b = snap_floor(sx, step);
    while b <= end_beat {
        let x = grid.left() + ((b - sx) * zoom) as f32;
        let strong = (b % 4.0).abs() < 1e-9;
        let beat = (b.fract()).abs() < 1e-9;
        gp.line_segment(
            [Pos2::new(x, grid.top()), Pos2::new(x, grid.bottom())],
            Stroke::new(
                1.0_f32,
                if strong {
                    theme::GRID_BAR
                } else if beat {
                    theme::GRID
                } else {
                    Color32::from_gray(38)
                },
            ),
        );
        b += step;
    }
    // pattern end
    let ex = grid.left() + ((pat_len - sx) * zoom) as f32;
    gp.rect_filled(Rect::from_x_y_ranges(ex..=grid.right().max(ex), grid.y_range()), 0.0, Color32::from_black_alpha(70));

    // Waveform helper (Alt+N): the song's audio under this pattern's first playlist clip, faint, so notes
    // can be lined up with a recording.
    if st.ui.wave_helper {
        let at = st
            .session
            .project
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .filter(|c| matches!(c.kind, ClipKind::Pattern { pattern } if pattern == pat))
            .map(|c| c.start)
            .fold(None, |a: Option<f64>, b| Some(a.map_or(b, |x| x.min(b))));
        if let Some(p0) = at {
            let bpm = st.session.project.bpm.max(1.0);
            let rate = st.audio.rate() as f64;
            let audio: Vec<(f64, f64, f64, std::path::PathBuf)> = st
                .session
                .project
                .tracks
                .iter()
                .filter(|t| !t.muted)
                .flat_map(|t| t.clips.iter())
                .filter_map(|c| match &c.kind {
                    ClipKind::Audio(a) if c.end() > p0 && c.start < p0 + pat_len => Some((c.start, c.end(), a.offset, a.source.clone())),
                    _ => None,
                })
                .collect();
            for (cs, ce, off, path) in audio {
                let Some(w) = st.waveform(&path) else { continue };
                let mid = grid.center().y;
                let amp = grid.height() * 0.3;
                let mut x = grid.left();
                while x < grid.right() {
                    let beat = p0 + st.ui.pr_x + (x - grid.left()) as f64 / zoom;
                    if beat >= cs && beat < ce {
                        let t = off + (beat - cs) * 60.0 / bpm;
                        let k = (t * rate / crate::state::WAVE_BUCKET as f64) as usize;
                        if let Some((lo, hi)) = w.get(k) {
                            gp.line_segment(
                                [Pos2::new(x, mid - super::playlist::wave_disp(*hi) * amp), Pos2::new(x, mid - super::playlist::wave_disp(*lo) * amp)],
                                Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(120, 200, 255, 40)),
                            );
                        }
                    }
                    x += 1.0;
                }
            }
        }
    }
    // ghost notes
    for (c, n) in &ghosts {
        let r = Rect::from_min_max(Pos2::new(x_of(n.start as f64), y_of(n.pitch) + 1.0), Pos2::new(x_of(n.end() as f64), y_of(n.pitch) + kh - 1.0));
        gp.rect_filled(r, 2.0, theme::rgb(*c).gamma_multiply(0.18));
    }
    // notes
    let mut note_rects = Vec::with_capacity(notes.len());
    for (i, n) in notes.iter().enumerate() {
        let r = Rect::from_min_max(Pos2::new(x_of(n.start as f64), y_of(n.pitch) + 1.0), Pos2::new(x_of(n.end() as f64).max(x_of(n.start as f64) + 3.0), y_of(n.pitch) + kh - 1.0));
        note_rects.push(r);
        let sel = st.ui.notes.contains(&i);
        let base = if n.muted { Color32::from_gray(90) } else { ch_color.lerp_to_gamma(Color32::WHITE, 0.15) };
        gp.rect_filled(r, 2.0, base.gamma_multiply(0.45 + 0.55 * n.velocity));
        gp.rect_stroke(r, 2.0, Stroke::new(if sel { 2.0_f32 } else { 1.0 }, if sel { Color32::WHITE } else { Color32::BLACK.gamma_multiply(0.6) }), egui::StrokeKind::Inside);
        if n.slide {
            gp.text(r.left_center() + Vec2::new(2.0, 0.0), egui::Align2::LEFT_CENTER, "⤴", egui::FontId::proportional(9.0), Color32::BLACK);
        }
        if r.width() > 26.0 && kh >= 11.0 {
            gp.with_clip_rect(r).text(r.left_center() + Vec2::new(3.0, 0.0), egui::Align2::LEFT_CENTER, note_name(n.pitch), egui::FontId::proportional(9.0), Color32::BLACK);
        }
    }
    // playhead (pattern mode only maps cleanly)
    let t = st.audio.transport().state;
    if t.playing {
        let beat = if st.ui.pattern_mode { st.beat_now().rem_euclid(pat_len.max(1e-6)) } else { st.beat_now() };
        let x = grid.left() + ((beat - sx) * zoom) as f32;
        if x >= grid.left() {
            p.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, grid.bottom())], Stroke::new(1.5_f32, theme::PLAYHEAD));
        }
    }
    // ruler
    let ruler = Rect::from_min_max(Pos2::new(grid.left(), rect.top()), Pos2::new(rect.right(), grid.top()));
    let rp = p.with_clip_rect(ruler);
    rp.rect_filled(ruler, 0.0, theme::PANEL);
    // Label every beat when there is room, otherwise every bar (or every few bars).
    let label_step = [1.0, 4.0, 8.0, 16.0, 32.0].into_iter().find(|s| s * zoom >= 38.0).unwrap_or(64.0);
    let mut b = snap_floor(sx, label_step);
    while b <= end_beat {
        let x = grid.left() + ((b - sx) * zoom) as f32;
        let text = if label_step >= 4.0 { format!("{}", (b / 4.0).floor() as i64 + 1) } else { format!("{}.{}", (b / 4.0).floor() as i64 + 1, (b % 4.0) as i64 + 1) };
        rp.text(Pos2::new(x + 2.0, ruler.center().y), egui::Align2::LEFT_CENTER, text, egui::FontId::monospace(9.0), theme::TEXT_DIM);
        b += label_step;
    }
    // keyboard
    let keys = Rect::from_min_max(Pos2::new(rect.left(), grid.top()), Pos2::new(grid.left(), grid.bottom()));
    let kp = p.with_clip_rect(keys);
    for pitch in bottom_pitch..=top_pitch {
        let y = y_of(pitch);
        let r = Rect::from_x_y_ranges(keys.x_range(), y..=y + kh);
        let black = is_black(pitch);
        let light = key_light(st, pitch);
        let base = if black { Color32::from_gray(20) } else { Color32::from_gray(225) };
        kp.rect_filled(r.shrink2(Vec2::new(0.0, 0.5)), 1.0, base.lerp_to_gamma(theme::ACCENT_2, light));
        if light > 0.0 {
            // Pressed: a shadow at the key's root, an accent edge and the velocity as a bar at its tip.
            kp.rect_filled(Rect::from_x_y_ranges(r.left()..=r.left() + 3.0, r.y_range()).shrink2(Vec2::new(0.0, 0.5)), 0.0, Color32::from_black_alpha((90.0 * light) as u8));
            kp.rect_stroke(r.shrink(0.5), 1.0, Stroke::new(1.0_f32, theme::ACCENT.gamma_multiply(light)), egui::StrokeKind::Inside);
            let v = st.ui.sounding.get(pitch as usize).copied().unwrap_or(0.0);
            if v > 0.0 {
                let w = (r.width() * 0.35 * v).max(2.0);
                kp.rect_filled(Rect::from_x_y_ranges(r.right() - w..=r.right(), r.y_range()).shrink2(Vec2::new(0.0, 1.5)), 1.0, theme::ACCENT);
            }
        }
        if let Some(sc) = scale {
            if edit::in_scale(pitch, root, sc) {
                let is_root = (pitch as i32 - root as i32).rem_euclid(12) == 0;
                kp.circle_filled(Pos2::new(r.left() + 6.0, r.center().y), if is_root { 3.5 } else { 2.5 }, if is_root { theme::ACCENT } else { theme::ACCENT_2 });
            }
        }
        if pitch % 12 == 0 || kh >= 13.0 && !black || st.ui.pr_key_names && kh >= 8.0 {
            kp.text(r.right_center() - Vec2::new(3.0, 0.0), egui::Align2::RIGHT_CENTER, note_name(pitch), egui::FontId::proportional(9.0), if black { Color32::WHITE } else { Color32::BLACK });
        }
    }
    // Keep redrawing while a released key fades out.
    if st.ui.released.iter().flatten().any(|t| t.elapsed().as_secs_f32() < 0.3) {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
    }
    // velocity lane
    p.rect_filled(vel, 0.0, theme::PANEL);
    let vp = p.with_clip_rect(vel);
    // The event lane shows velocity, pan or release (F / Shift+F).
    let prop_of = |n: &Note| match st.ui.pr_prop {
        1 => (n.pan + 1.0) * 0.5,
        2 => n.release,
        _ => n.velocity,
    };
    for (i, n) in notes.iter().enumerate() {
        let x = x_of(n.start as f64);
        let h = prop_of(n) * (vel.height() - 6.0);
        let sel = st.ui.notes.contains(&i);
        vp.line_segment([Pos2::new(x, vel.bottom() - 2.0), Pos2::new(x, vel.bottom() - 2.0 - h)], Stroke::new(3.0_f32, if sel { Color32::WHITE } else { ch_color }));
        vp.circle_filled(Pos2::new(x, vel.bottom() - 2.0 - h), 3.0, if sel { Color32::WHITE } else { ch_color });
    }
    p.text(Pos2::new(rect.left() + 4.0, vel.center().y), egui::Align2::LEFT_CENTER, ["velocity", "pan", "release"][st.ui.pr_prop.min(2)], egui::FontId::proportional(10.0), theme::TEXT_DIM);

    // ---------------- interaction
    let grid_ticks = (st.ui.snap * PPQ as f64).round().max(0.0);
    let snap_tick = |t: f64| if grid_ticks > 0.0 { (t / grid_ticks).round() * grid_ticks } else { t };
    let snap_tick_floor = |t: f64| if grid_ticks > 0.0 { (t / grid_ticks).floor() * grid_ticks } else { t };
    let note_at = |pos: Pos2| note_rects.iter().enumerate().rev().find(|(_, r)| r.contains(pos)).map(|(i, r)| (i, *r));
    let tool = st.ui.tool;
    let preview = |st: &mut State, pitch: u8, vel: f32| {
        st.audio.send(Cmd::Live(LiveEvent::NoteOn { channel: ch, note: pitch, velocity: vel }));
        st.audio.send(Cmd::Live(LiveEvent::NoteOff { channel: ch, note: pitch }));
    };

    // keyboard click previews
    if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos().filter(|p| keys.contains(*p)) {
            preview(st, pitch_of(pos.y), 0.8);
        }
    }

    // Right-click and hold on empty space opens the slice tool ("click and hold" function).
    let now_t = ui.input(|i| i.time);
    if ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Secondary) && !i.pointer.middle_down()) && !ui.input(|i| i.modifiers.ctrl) && !lsh {
        if let Some(pos) = ui.input(|i| i.pointer.interact_pos()).filter(|p| grid.contains(*p) && note_at(*p).is_none()) {
            st.ui.hold_slice = Some((now_t, pos));
        }
    }
    if let Some((t0, origin)) = st.ui.hold_slice {
        let moved = ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| p.distance(origin) > 6.0);
        if !ui.input(|i| i.pointer.secondary_down()) {
            st.ui.hold_slice = None;
        } else if now_t - t0 > 0.4 && st.ui.pr_drag.is_none() && !moved {
            st.ui.hold_slice = None;
            st.ui.pr_drag = Some(Drag::Knife { from: origin, delete_small: false });
        } else {
            ui.ctx().request_repaint();
        }
    }
    // Slice tool (or Right-Shift), right-drag: slice and delete the smaller piece (Right-Shift+right-click).
    if (tool == Tool::Slice || rsh) && resp.drag_started_by(egui::PointerButton::Secondary) && st.ui.pr_drag.is_none() && !ui.input(|i| i.modifiers.ctrl) {
        if let Some(pos) = ui.input(|i| i.pointer.press_origin()).filter(|p| grid.contains(*p)) {
            st.ui.pr_drag = Some(Drag::Knife { from: pos, delete_small: true });
        }
    }
    if resp.drag_started_by(egui::PointerButton::Primary) && st.ui.pr_drag.is_none() {
        // Hit-test where the button went down, not where the drag threshold was crossed.
        if let Some(pos) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()) {
            let (shift, ctrl) = ui.input(|i| (i.modifiers.shift, i.modifiers.ctrl));
            if vel.contains(pos) {
                st.ui.pr_drag = Some(Drag::Velocity);
            } else if grid.contains(pos) {
                match note_at(pos) {
                    Some((i, r)) if tool != Tool::Erase && tool != Tool::Slice && tool != Tool::Mute => {
                        if !st.ui.notes.contains(&i) {
                            st.ui.notes = group_of(&notes, i);
                        }
                        let anchor_tick = tick_of(pos.x) as i64;
                        if pos.x > r.right() - 5.0 && !shift {
                            let orig: Vec<(usize, Note)> = st.ui.notes.iter().filter_map(|k| notes.get(*k).map(|n| (*k, *n))).collect();
                            st.ui.pr_drag = Some(Drag::Resize { anchor_tick, orig });
                        } else {
                            // Shift-drag moves copies and leaves the originals in place.
                            let mut sel = st.ui.notes.clone();
                            if shift {
                                let copies: Vec<Note> = sel.iter().filter_map(|k| notes.get(*k).copied()).collect();
                                let first = notes.len();
                                st.session.gesture("Copy notes", |p| {
                                    if let Some(c) = p.pattern_mut(pat).and_then(|pt| pt.channels.get_mut(&ch)) {
                                        c.notes.extend(copies.iter().copied());
                                    }
                                });
                                sel = (first..first + copies.len()).collect();
                                st.ui.notes = sel.clone();
                            }
                            let cur = st.session.project.pattern(pat).and_then(|pt| pt.channels.get(&ch));
                            let orig: Vec<(usize, Note)> = sel.iter().filter_map(|k| cur.and_then(|c| c.notes.get(*k)).map(|n| (*k, *n))).collect();
                            let grab = notes[i];
                            st.ui.pr_drag = Some(Drag::Move { anchor_tick, anchor_pitch: pitch_of(pos.y) as i32, grab, last_dp: 0, orig });
                        }
                        preview(st, notes[i].pitch, notes[i].velocity);
                    }
                    _ if tool == Tool::Zoom => st.ui.pr_drag = Some(Drag::Zoom { from: pos }),
                    _ if tool == Tool::Playback => {}
                    _ if tool == Tool::Slice || rsh => st.ui.pr_drag = Some(Drag::Knife { from: pos, delete_small: false }),
                    None if tool == Tool::Select || ctrl => st.ui.pr_drag = Some(Drag::Select { from: pos }),
                    None if tool == Tool::Paint => st.ui.pr_drag = Some(Drag::Paint { last_tick: u32::MAX }),
                    None if tool == Tool::Draw => {
                        // Press on empty space draws a note and keeps it on the pointer.
                        let start = snap_tick_floor(tick_of(pos.x)) as u32;
                        let pitch = snap_pitch(st, pitch_of(pos.y));
                        let n = Note { pitch, start, length: st.ui.last_note_len, velocity: 0.78, slide: st.ui.porta, ..Default::default() };
                        let k = notes.len();
                        st.session.gesture("Add note", |p| {
                            if let Some(pt) = p.pattern_mut(pat) {
                                pt.channels.entry(ch).or_default().notes.push(n);
                            }
                        });
                        st.ui.notes = vec![k];
                        preview(st, pitch, 0.78);
                        st.ui.pr_drag = Some(Drag::Move { anchor_tick: tick_of(pos.x) as i64, anchor_pitch: pitch as i32, grab: n, last_dp: 0, orig: vec![(k, n)] });
                    }
                    _ => {}
                }
            }
        }
    }
    if let (Some(d), Some(pos)) = (st.ui.pr_drag.clone(), ui.input(|i| i.pointer.interact_pos())) {
        match d {
            Drag::Move { anchor_tick, anchor_pitch, grab, last_dp, orig } => {
                // The grabbed note's start lands on the grid; the others keep their offsets to it.
                let raw = grab.start as f64 + (tick_of(pos.x) - anchor_tick as f64);
                let dt = if ui.input(|i| i.modifiers.alt) { raw.round() as i64 - grab.start as i64 } else { snap_tick(raw) as i64 - grab.start as i64 };
                let mut dp = pitch_of(pos.y) as i32 - anchor_pitch;
                let (lo, hi) = orig.iter().fold((127i32, 0i32), |(lo, hi), (_, n)| (lo.min(n.pitch as i32), hi.max(n.pitch as i32)));
                dp = dp.clamp(-lo, 127 - hi);
                let min_start = orig.iter().map(|(_, n)| n.start as i64).min().unwrap_or(0);
                let dt = dt.max(-min_start);
                if dp != last_dp {
                    preview(st, (grab.pitch as i32 + dp).clamp(0, 127) as u8, grab.velocity);
                    if let Some(Drag::Move { last_dp, .. }) = &mut st.ui.pr_drag {
                        *last_dp = dp;
                    }
                }
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                st.session.gesture("Move notes", |p| {
                    if let Some(v) = p.pattern_mut(pat).and_then(|pt| pt.channels.get_mut(&ch)) {
                        for (i, n) in &orig {
                            if let Some(x) = v.notes.get_mut(*i) {
                                x.start = (n.start as i64 + dt) as u32;
                                x.pitch = (n.pitch as i32 + dp).clamp(0, 127) as u8;
                            }
                        }
                    }
                });
            }
            Drag::Resize { anchor_tick, orig } => {
                let dl = (tick_of(pos.x) - anchor_tick as f64).round() as i64;
                st.session.gesture("Resize notes", |p| {
                    if let Some(v) = p.pattern_mut(pat).and_then(|pt| pt.channels.get_mut(&ch)) {
                        for (i, n) in &orig {
                            if let Some(x) = v.notes.get_mut(*i) {
                                let end = snap_tick((n.end() as i64 + dl) as f64).max(n.start as f64 + 1.0);
                                x.length = (end as u32).saturating_sub(n.start).max(1);
                            }
                        }
                    }
                });
                if let Some((_, n)) = orig.first() {
                    st.ui.last_note_len = st.session.project.pattern(pat).and_then(|pt| pt.channels.get(&ch)).and_then(|c| c.notes.get(orig[0].0)).map(|x| x.length).unwrap_or(n.length);
                }
            }
            Drag::Select { from } => {
                let r = Rect::from_two_pos(from, pos);
                p.rect(r, 0.0, theme::ACCENT_2.gamma_multiply(0.12), Stroke::new(1.0_f32, theme::ACCENT_2), egui::StrokeKind::Inside);
                st.ui.notes = note_rects.iter().enumerate().filter(|(_, nr)| nr.intersects(r)).map(|(i, _)| i).collect();
            }
            Drag::Knife { from, delete_small } => {
                let alt = ui.input(|i| i.modifiers.alt);
                let snapx = |x: f32| if alt { x } else { x_of(snap_tick(tick_of(x))) };
                let (a, b) = (Pos2::new(snapx(from.x), from.y), Pos2::new(snapx(pos.x), pos.y));
                // Where the line crosses each crossed note's row.
                let cuts: Vec<(usize, u32)> = note_rects
                    .iter()
                    .enumerate()
                    .filter_map(|(i, r)| {
                        let (y0, y1) = (a.y.min(b.y), a.y.max(b.y));
                        let cy = r.center().y;
                        if cy < y0 - r.height() / 2.0 || cy > y1 + r.height() / 2.0 {
                            return None;
                        }
                        let t = if (b.y - a.y).abs() < 1e-3 { 0.0 } else { ((cy - a.y) / (b.y - a.y)).clamp(0.0, 1.0) };
                        let x = a.x + (b.x - a.x) * t;
                        let tick = if alt { tick_of(x).round() } else { snap_tick(tick_of(x)) } as u32;
                        let n = notes.get(i)?;
                        (tick > n.start && tick < n.end()).then_some((i, tick))
                    })
                    .collect();
                p.line_segment([a, b], Stroke::new(2.0_f32, theme::REC));
                for (i, tick) in &cuts {
                    if let Some(r) = note_rects.get(*i) {
                        let x = x_of(*tick as f64);
                        p.line_segment([Pos2::new(x, r.top() - 2.0), Pos2::new(x, r.bottom() + 2.0)], Stroke::new(2.0_f32, Color32::WHITE));
                    }
                }
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                if !ui.input(|i| i.pointer.any_down()) && !cuts.is_empty() {
                    st.session.edit("Slice notes", |p| {
                        if let Some(c) = p.pattern_mut(pat).and_then(|pt| pt.channels.get_mut(&ch)) {
                            let mut rest = Vec::new();
                            let mut drop = Vec::new();
                            for (i, tick) in &cuts {
                                if let Some(r) = c.notes.get_mut(*i).and_then(|n| edit::split_note(n, *tick)) {
                                    if delete_small {
                                        // Keep the longer piece.
                                        if r.length > c.notes[*i].length {
                                            drop.push(*i);
                                            rest.push(r);
                                        }
                                    } else {
                                        rest.push(r);
                                    }
                                }
                            }
                            drop.sort_unstable();
                            for i in drop.into_iter().rev() {
                                c.notes.remove(i);
                            }
                            c.notes.extend(rest);
                        }
                    });
                }
            }
            Drag::Zoom { from } => {
                let r = Rect::from_two_pos(from, pos);
                p.rect(r, 2.0, theme::ACCENT_2.gamma_multiply(0.10), Stroke::new(1.0_f32, theme::ACCENT_2), egui::StrokeKind::Inside);
                if !ui.input(|i| i.pointer.any_down()) && r.width() > 6.0 {
                    let b0 = st.ui.pr_x + (r.left() - grid.left()) as f64 / zoom;
                    let b1 = st.ui.pr_x + (r.right() - grid.left()) as f64 / zoom;
                    let nz = (grid.width() as f64 / (b1 - b0).max(1e-3)).clamp(12.0, 800.0);
                    let nkh = if r.height() > kh * 2.0 { (kh * grid.height() / r.height()).clamp(6.0, 40.0) } else { kh };
                    let rows_top = (r.top() - grid.top() + st.ui.pr_y) / kh;
                    st.ui.pr_anim = Some(ViewTarget { zoom: nz, x: b0.max(0.0), vscale: nkh, y: (rows_top * nkh).clamp(0.0, max_y(nkh)) });
                }
            }
            Drag::Velocity => {
                if vel.x_range().contains(pos.x) {
                    let v = ((vel.bottom() - 2.0 - pos.y) / (vel.height() - 6.0)).clamp(0.02, 1.0);
                    let hit: Vec<usize> = notes.iter().enumerate().filter(|(i, n)| (x_of(n.start as f64) - pos.x).abs() < 5.0 && (st.ui.notes.is_empty() || st.ui.notes.contains(i))).map(|(i, _)| i).collect();
                    let prop = st.ui.pr_prop;
                    if !hit.is_empty() {
                        st.session.gesture(["Velocity", "Note pan", "Note release"][prop.min(2)], |p| {
                            if let Some(c) = p.pattern_mut(pat).and_then(|pt| pt.channels.get_mut(&ch)) {
                                for i in hit {
                                    match prop {
                                        1 => c.notes[i].pan = v * 2.0 - 1.0,
                                        2 => c.notes[i].release = v,
                                        _ => c.notes[i].velocity = v,
                                    }
                                }
                            }
                        });
                    }
                }
            }
            Drag::Paint { last_tick } => {
                if grid.contains(pos) {
                    let t = snap_tick_floor(tick_of(pos.x)) as u32;
                    if t != last_tick {
                        let pitch = snap_pitch(st, pitch_of(pos.y));
                        let len = st.ui.last_note_len;
                        if !notes.iter().any(|n| n.pitch == pitch && n.start <= t && n.end() > t) {
                            st.session.gesture("Paint notes", |p| {
                                if let Some(pt) = p.pattern_mut(pat) {
                                    pt.channels.entry(ch).or_default().notes.push(Note { pitch, start: t, length: len, velocity: 0.78, slide: st.ui.porta, ..Default::default() });
                                }
                            });
                        }
                        st.ui.pr_drag = Some(Drag::Paint { last_tick: t });
                    }
                }
            }
        }
        if !ui.input(|i| i.pointer.any_down()) {
            st.ui.pr_drag = None;
            st.session.end_gesture();
        }
    }

    // the playback tool: hold to play from the pointer, drag to scrub, release to stop.
    let scrub_id = egui::Id::new("pr.scrub");
    let scrubbing: bool = ui.data(|d| d.get_temp(scrub_id)).unwrap_or(false);
    if tool == Tool::Playback && resp.is_pointer_button_down_on() && ui.input(|i| i.pointer.primary_down()) {
        if let Some(pos) = ui.input(|i| i.pointer.interact_pos()).filter(|p| grid.contains(*p)) {
            let beat = (tick_of(pos.x) / PPQ as f64).max(0.0);
            if !scrubbing {
                st.seek(beat);
                if !st.transport().playing {
                    st.toggle_play();
                }
                ui.data_mut(|d| d.insert_temp(scrub_id, true));
            } else if resp.dragged() {
                st.seek(beat);
            }
        }
    } else if scrubbing && !ui.input(|i| i.pointer.primary_down()) {
        ui.data_mut(|d| d.insert_temp(scrub_id, false));
        st.stop();
    }
    if tool == Tool::Zoom && resp.secondary_clicked() {
        if let Some(at) = resp.interact_pointer_pos() {
            let b = st.ui.pr_x + (at.x - grid.left()) as f64 / zoom;
            let nz = (zoom / 2.0).clamp(12.0, 800.0);
            st.ui.pr_anim = Some(ViewTarget { zoom: nz, x: (b - (at.x - grid.left()) as f64 / nz).max(0.0), vscale: kh, y: st.ui.pr_y });
        }
    }
    super::keys::tool_menu(ui, st, &resp);
    // Right-Shift + click slices the note under the pointer (+ right-click: and deletes the smaller piece);
    // Right-Alt + right-click quantizes the selection.
    let side_click = (rsh || ralt) && (resp.clicked() || resp.secondary_clicked()) && st.ui.pr_drag.is_none();
    if side_click {
        if let Some(pos) = resp.interact_pointer_pos().filter(|p| grid.contains(*p)) {
            if rsh {
                if let Some((i, _)) = note_at(pos) {
                    let at = if alt_held { tick_of(pos.x).round() } else { snap_tick(tick_of(pos.x)) } as u32;
                    let del = resp.secondary_clicked();
                    st.session.edit("Slice note", |p| {
                        if let Some(c) = p.pattern_mut(pat).and_then(|pt| pt.channels.get_mut(&ch)) {
                            if let Some(r) = c.notes.get_mut(i).and_then(|n| edit::split_note(n, at)) {
                                if !del {
                                    c.notes.push(r);
                                } else if r.length > c.notes[i].length {
                                    c.notes[i] = r;
                                }
                            }
                        }
                    });
                }
            } else if resp.secondary_clicked() {
                st.ctx.run("dragon.edit.quantize", serde_json::Value::Null);
            }
        }
    }
    if (resp.clicked() || resp.secondary_clicked()) && !side_click && st.ui.tool_menu.is_none() && !(lsh && resp.secondary_clicked()) && st.ui.pr_drag.is_none() && !matches!(tool, Tool::Playback | Tool::Zoom) {
        if let Some(pos) = resp.interact_pointer_pos().filter(|p| grid.contains(*p)) {
            let hit = note_at(pos);
            let tick = tick_of(pos.x);
            if resp.secondary_clicked() && ui.input(|i| i.modifiers.ctrl) {
                // Ctrl + right-click: zoom out (handled by the navigation).
            } else if resp.secondary_clicked() || (tool == Tool::Erase && hit.is_some()) {
                if let Some((i, _)) = hit {
                    st.session.edit("Delete note", |p| {
                        if let Some(c) = p.pattern_mut(pat).and_then(|pt| pt.channels.get_mut(&ch)) {
                            c.notes.remove(i);
                        }
                    });
                    st.ui.notes.clear();
                }
            } else if let Some((i, _)) = hit {
                match tool {
                    Tool::Mute => st.session.edit("Mute note", |p| {
                        if let Some(c) = p.pattern_mut(pat).and_then(|pt| pt.channels.get_mut(&ch)) {
                            c.notes[i].muted = !c.notes[i].muted;
                        }
                    }),
                    Tool::Slice => {
                        let at = snap_tick(tick) as u32;
                        st.session.edit("Slice note", |p| {
                            if let Some(c) = p.pattern_mut(pat).and_then(|pt| pt.channels.get_mut(&ch)) {
                                if let Some(rest) = edit::split_note(&mut c.notes[i], at) {
                                    c.notes.push(rest);
                                }
                            }
                        });
                    }
                    _ => {
                        if ui.input(|i| i.modifiers.shift) {
                            if let Some(k) = st.ui.notes.iter().position(|x| *x == i) {
                                st.ui.notes.remove(k);
                            } else {
                                st.ui.notes.push(i);
                            }
                        } else {
                            st.ui.notes = group_of(&notes, i);
                        }
                        st.ui.last_note_len = notes[i].length;
                    }
                }
            } else if matches!(tool, Tool::Draw | Tool::Paint) {
                let start = snap_tick_floor(tick) as u32;
                let pitch = snap_pitch(st, pitch_of(pos.y));
                let len = st.ui.last_note_len;
                let chord = st.ui.chord;
                let new: Vec<Note> =
                    if chord == 0 { vec![Note { pitch, start, length: len, velocity: 0.78, slide: st.ui.porta, ..Default::default() }] } else { edit::chord(pitch, CHORDS[chord - 1].1, start, len, 0.78, false) };
                preview(st, pitch, 0.78);
                let first = notes.len();
                let n = new.len();
                st.session.edit("Add note", |p| {
                    if let Some(pt) = p.pattern_mut(pat) {
                        pt.channels.entry(ch).or_default().notes.extend(new);
                    }
                });
                st.ui.notes = (first..first + n).collect();
            } else {
                st.ui.notes.clear();
            }
        }
    }
    if let Some(pos) = resp.hover_pos() {
        if let Some((_, r)) = note_at(pos) {
            if pos.x > r.right() - 5.0 {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            } else if matches!(tool, Tool::Draw | Tool::Paint | Tool::Select) && st.ui.pr_drag.is_none() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
            }
        }
    }
    // Arrow keys nudge the selection: ←/→ by the snap grid, ↑/↓ by a semitone. Shift+arrows (the // "move selection") and Ctrl+↑/↓ (octave) are global shortcuts, so they are left alone here.
    if resp.hovered() && !st.ui.notes.is_empty() && !ui.ctx().wants_keyboard_input() && !ui.input(|i| i.modifiers.shift) {
        let (l, r, u, d, shift, ctrl) = ui.input_mut(|i| {
            let ctrl = i.modifiers.ctrl;
            let take = |i: &mut egui::InputState, k: egui::Key| !ctrl && i.consume_key(i.modifiers, k);
            (take(i, egui::Key::ArrowLeft), take(i, egui::Key::ArrowRight), take(i, egui::Key::ArrowUp), take(i, egui::Key::ArrowDown), i.modifiers.shift, ctrl)
        });
        if !ctrl && (l || r || u || d) {
            let step = if grid_ticks > 0.0 { grid_ticks as i64 } else { (PPQ / 4) as i64 };
            let dt = if l {
                -step
            } else if r {
                step
            } else {
                0
            };
            let dp = (if u {
                1
            } else if d {
                -1
            } else {
                0
            }) * if shift { 12 } else { 1 }; // (never shifted: see above)
            let sel = st.ui.notes.clone();
            let ok = sel.iter().filter_map(|i| notes.get(*i)).all(|n| n.start as i64 + dt >= 0 && (0..=127).contains(&(n.pitch as i32 + dp)));
            if ok {
                st.session.edit("Nudge notes", |p| {
                    if let Some(c) = p.pattern_mut(pat).and_then(|pt| pt.channels.get_mut(&ch)) {
                        for i in &sel {
                            if let Some(n) = c.notes.get_mut(*i) {
                                n.start = (n.start as i64 + dt) as u32;
                                n.pitch = (n.pitch as i32 + dp) as u8;
                            }
                        }
                    }
                });
                if dp != 0 {
                    if let Some(n) = sel.first().and_then(|i| notes.get(*i)) {
                        preview(st, (n.pitch as i32 + dp) as u8, n.velocity);
                    }
                }
            }
        }
    }
}

fn snap_pitch(st: &State, pitch: u8) -> u8 {
    match st.ui.scale {
        Some(i) => edit::snap_to_scale(pitch, st.ui.scale_root, SCALES[i].1),
        None => pitch,
    }
}

fn toolbar(ui: &mut Ui, st: &mut State, pat: Id, ch: Id) {
    ui.horizontal_wrapped(|ui| {
        let name = st.session.project.channel(ch).map(|c| c.name.clone()).unwrap_or_default();
        egui::ComboBox::from_id_salt("pr.channel").selected_text(name).width(110.0).show_ui(ui, |ui| {
            for c in &st.session.project.channels {
                if ui.selectable_label(c.id == ch, &c.name).clicked() {
                    st.ui.piano_channel = Some(c.id);
                    st.ui.channel = Some(c.id);
                    st.ui.notes.clear();
                }
            }
        });
        if ui.button("Import MIDI…").on_hover_text("Open a MIDI file (.mid) – its notes come in as a new pattern (Ctrl+M is the metronome)").clicked() {
            if let Some(f) = rfd::FileDialog::new().add_filter("MIDI", &["mid", "midi"]).pick_file() {
                st.import_midi(&f);
            }
        }
        widgets::tool_buttons(ui, st);
        ui.separator();
        let cur = widgets::SNAPS.iter().find(|s| (s.1 - st.ui.snap).abs() < 1e-9).map(|s| s.0).unwrap_or("custom");
        egui::ComboBox::from_id_salt("pr.snap").selected_text(format!("Snap {cur}")).width(100.0).show_ui(ui, |ui| {
            for (n, v) in widgets::SNAPS {
                ui.selectable_value(&mut st.ui.snap, *v, *n);
            }
        });
        // Key (root note) and scale are picked separately; the grid lights up the notes of the scale.
        const KEYS: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
        ui.label(RichText::new("Key").small().color(theme::TEXT_DIM));
        egui::ComboBox::from_id_salt("pr.key").selected_text(KEYS[st.ui.scale_root as usize % 12]).width(46.0).show_ui(ui, |ui| {
            for (r, k) in KEYS.iter().enumerate() {
                ui.selectable_value(&mut st.ui.scale_root, r as u8, *k);
            }
        });
        let scale_name = st.ui.scale.map(|i| SCALES[i].0).unwrap_or("No scale");
        egui::ComboBox::from_id_salt("pr.scale").selected_text(scale_name).width(150.0).height(420.0).show_ui(ui, |ui| {
            ui.selectable_value(&mut st.ui.scale, None, "No scale");
            for (i, (n, iv)) in SCALES.iter().enumerate() {
                let notes: Vec<&str> = iv.iter().map(|x| KEYS[(st.ui.scale_root + x) as usize % 12]).collect();
                ui.selectable_value(&mut st.ui.scale, Some(i), *n).on_hover_text(notes.join(" "));
            }
        });
        if let Some(i) = st.ui.scale {
            let notes: Vec<&str> = SCALES[i].1.iter().map(|x| KEYS[(st.ui.scale_root + x) as usize % 12]).collect();
            ui.label(RichText::new(notes.join(" ")).small().monospace().color(theme::ACCENT_2));
        }
        let chord_name = if st.ui.chord == 0 { "Single notes".to_string() } else { CHORDS[st.ui.chord - 1].0.to_string() };
        egui::ComboBox::from_id_salt("pr.chord").selected_text(format!("Stamp: {chord_name}")).width(130.0).show_ui(ui, |ui| {
            ui.selectable_value(&mut st.ui.chord, 0, "Single notes");
            for (i, (n, _)) in CHORDS.iter().enumerate() {
                ui.selectable_value(&mut st.ui.chord, i + 1, *n);
            }
        });
        ui.separator();
        for (id, label) in [("edit.quantize", "Quantize"), ("edit.legato", "Legato"), ("edit.strum", "Strum"), ("edit.humanize", "Humanize")] {
            if ui.small_button(label).clicked() {
                st.ctx.run(&format!("dragon.{id}"), serde_json::Value::Null);
            }
        }
        ui.menu_button("Arp", |ui| {
            for (mode, n) in [(ArpMode::Up, "Up"), (ArpMode::Down, "Down"), (ArpMode::UpDown, "Up-down"), (ArpMode::Random, "Random")] {
                if ui.button(n).clicked() {
                    let sel = st.ui.notes.clone();
                    let grid = ((st.ui.snap.max(0.125)) * PPQ as f64) as u32;
                    st.session.edit("Arpeggiate", |p| {
                        if let Some(c) = p.pattern_mut(pat).and_then(|pt| pt.channels.get_mut(&ch)) {
                            let src: Vec<Note> = if sel.is_empty() { c.notes.clone() } else { sel.iter().filter_map(|i| c.notes.get(*i).copied()).collect() };
                            let arp = edit::arpeggiate(&src, mode, grid, 0.9, 1);
                            let mut keep: Vec<Note> = c.notes.iter().enumerate().filter(|(i, _)| !sel.is_empty() && !sel.contains(i)).map(|(_, n)| *n).collect();
                            keep.extend(arp);
                            c.notes = keep;
                        }
                    });
                    st.ui.notes.clear();
                    ui.close_menu();
                }
            }
        });
        ui.menu_button("Tools", |ui| {
            if ui.button("Flam (Alt+F)").clicked() {
                st.ctx.run("dragon.edit.flam", serde_json::Value::Null);
                ui.close_menu();
            }
            ui.separator();
            for (t, name, key) in crate::views::note_tools::Tool::ALL {
                if ui.button(format!("{name}… ({key})")).clicked() {
                    crate::views::note_tools::open(st, t);
                    ui.close_menu();
                }
            }
        });
        ui.menu_button("Progression", |ui| {
            let scale = st.ui.scale.unwrap_or(0);
            ui.label(RichText::new(format!("in {} {}", note_name(st.ui.scale_root + 48), SCALES[scale].0)).small());
            for (name, degrees) in [("I–V–vi–IV", &[0usize, 4, 5, 3][..]), ("vi–IV–I–V", &[5, 3, 0, 4]), ("ii–V–I", &[1, 4, 0, 0]), ("i–VI–III–VII", &[0, 5, 2, 6])] {
                if ui.button(name).clicked() {
                    let root = 48 + st.ui.scale_root;
                    let notes = edit::progression(root, SCALES[scale].1, degrees, PPQ * 4, true);
                    st.session.edit("Chord progression", |p| {
                        if let Some(pt) = p.pattern_mut(pat) {
                            pt.channels.entry(ch).or_default().notes.extend(notes);
                        }
                    });
                    ui.ctx().data_mut(|d| d.remove::<(Id, Id)>(egui::Id::new("pr.focus")));
                    ui.close_menu();
                }
            }
        });
        let sel = st.ui.notes.clone();
        if let Some(&i) = sel.first() {
            ui.separator();
            if let Some(n) = st.session.project.pattern(pat).and_then(|pt| pt.channels.get(&ch)).and_then(|c| c.notes.get(i)).copied() {
                let mut slide = n.slide;
                if ui.checkbox(&mut slide, "slide").changed() {
                    st.session.edit("Slide", |p| {
                        if let Some(c) = p.pattern_mut(pat).and_then(|pt| pt.channels.get_mut(&ch)) {
                            for k in &sel {
                                if let Some(x) = c.notes.get_mut(*k) {
                                    x.slide = slide;
                                }
                            }
                        }
                    });
                }
                let mut pan = n.pan;
                let r = ui.add(egui::Slider::new(&mut pan, -1.0..=1.0).text("pan").show_value(false));
                widgets::apply(st, &r, "Note pan", |p| {
                    if let Some(c) = p.pattern_mut(pat).and_then(|pt| pt.channels.get_mut(&ch)) {
                        for k in &sel {
                            if let Some(x) = c.notes.get_mut(*k) {
                                x.pan = pan;
                            }
                        }
                    }
                });
            }
        }
    });
}

/// How lit a key is (0 … 1): held keys by their velocity, released ones fading out over 0.3 s.
fn key_light(st: &State, pitch: u8) -> f32 {
    let v = st.ui.sounding.get(pitch as usize).copied().unwrap_or(0.0);
    if v > 0.0 {
        return 0.65 + 0.35 * v.min(1.0);
    }
    st.ui.released.get(pitch as usize).copied().flatten().map(|t| (1.0 - t.elapsed().as_secs_f32() / 0.3).max(0.0) * 0.6).unwrap_or(0.0)
}
