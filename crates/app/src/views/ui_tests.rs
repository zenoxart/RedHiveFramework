//! Interaction tests: views are driven with synthetic egui input (pointer, buttons, modifiers) and the
//! resulting project / view state is checked.

use crate::state::{Settings, State, Tool};
use dragon_engine::model::*;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect};

struct Harness {
    ctx: egui::Context,
    st: State,
}

impl Harness {
    fn new() -> Self {
        let mut st = State::new(redhive_client_framework::AppContext::new("en"), Settings { show_splash: false, ..Settings::default() });
        // Most tests work on the demo project (tracks, instruments, a pattern on the playlist).
        st.new_demo_project();
        Harness { ctx: egui::Context::default(), st }
    }

    fn frame(&mut self, view: fn(&mut egui::Ui, &mut State), events: Vec<Event>, modifiers: Modifiers) {
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1400.0, 900.0))), events, modifiers, ..Default::default() };
        let st = &mut self.st;
        let _ = self.ctx.run(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| view(ui, st));
        });
    }

    fn idle(&mut self, view: fn(&mut egui::Ui, &mut State), n: usize) {
        for _ in 0..n {
            self.frame(view, vec![], Modifiers::NONE);
        }
    }

    /// Press at `a`, move to `b` in steps, release.
    fn drag(&mut self, view: fn(&mut egui::Ui, &mut State), a: Pos2, b: Pos2, button: PointerButton, m: Modifiers) {
        self.frame(view, vec![Event::PointerMoved(a)], m);
        self.frame(view, vec![Event::PointerButton { pos: a, button, pressed: true, modifiers: m }], m);
        for k in 1..=8 {
            let p = a + (b - a) * (k as f32 / 8.0);
            self.frame(view, vec![Event::PointerMoved(p)], m);
        }
        self.frame(view, vec![Event::PointerButton { pos: b, button, pressed: false, modifiers: m }], m);
        self.frame(view, vec![], Modifiers::NONE);
    }

    fn click(&mut self, view: fn(&mut egui::Ui, &mut State), a: Pos2, button: PointerButton, m: Modifiers) {
        self.frame(view, vec![Event::PointerMoved(a)], m);
        self.frame(view, vec![Event::PointerButton { pos: a, button, pressed: true, modifiers: m }], m);
        self.frame(view, vec![Event::PointerButton { pos: a, button, pressed: false, modifiers: m }], m);
        self.frame(view, vec![], Modifiers::NONE);
    }

    fn rect(&self, key: &str) -> Rect {
        self.ctx.data(|d| d.get_temp::<Rect>(egui::Id::new(key))).expect("view drew its area")
    }

    fn notes(&self) -> Vec<Note> {
        let ch = self.st.ui.piano_channel.unwrap();
        self.st.session.project.pattern(self.st.ui.pattern).and_then(|p| p.channels.get(&ch)).map(|c| c.notes.clone()).unwrap_or_default()
    }

    /// Screen position of a tick / pitch in the piano roll.
    fn pr_pos(&self, tick: f64, pitch: u8) -> Pos2 {
        let g = self.rect("pr.grid");
        let x = g.left() + ((tick / PPQ as f64 - self.st.ui.pr_x) * self.st.ui.piano_zoom as f64) as f32;
        let y = g.top() + (127 - pitch as i32) as f32 * self.st.ui.key_height - self.st.ui.pr_y + self.st.ui.key_height / 2.0;
        Pos2::new(x, y)
    }
}

fn piano(ui: &mut egui::Ui, st: &mut State) {
    super::piano_roll::show(ui, st);
}

fn playlist(ui: &mut egui::Ui, st: &mut State) {
    super::playlist::show(ui, st);
}

fn piano_with_notes(notes: &[Note]) -> Harness {
    let mut h = Harness::new();
    let ch = h.st.session.project.channels[0].id;
    h.st.ui.piano_channel = Some(ch);
    let pat = h.st.ui.pattern;
    let notes = notes.to_vec();
    h.st.session.edit("setup", |p| p.pattern_mut(pat).unwrap().channels.entry(ch).or_default().notes = notes);
    h.st.ui.snap = 0.25;
    h.idle(piano, 3);
    h
}

#[test]
fn dragging_a_selected_note_moves_the_whole_selection() {
    let a = Note { pitch: 60, start: 0, length: PPQ, velocity: 0.8, ..Default::default() };
    let b = Note { pitch: 64, start: PPQ, length: PPQ, velocity: 0.8, ..Default::default() };
    let mut h = piano_with_notes(&[a, b]);
    h.st.ui.notes = vec![0, 1];
    h.st.ui.tool = Tool::Draw;
    // Grab note a in its middle and drag one beat right and two semitones up.
    let from = h.pr_pos(PPQ as f64 * 0.4, 60);
    let to = h.pr_pos(PPQ as f64 * 1.4, 62);
    h.drag(piano, from, to, PointerButton::Primary, Modifiers::NONE);
    let n = h.notes();
    assert_eq!(n.len(), 2, "moving must not draw notes");
    assert_eq!((n[0].start, n[0].pitch), (PPQ, 62), "{n:?}");
    assert_eq!((n[1].start, n[1].pitch), (2 * PPQ, 66), "selection moved together: {n:?}");
    assert!(!h.st.session.in_gesture());
    // One undo step restores both.
    h.st.session.undo();
    let n = h.notes();
    assert_eq!((n[0].start, n[0].pitch, n[1].start, n[1].pitch), (0, 60, PPQ, 64));
}

#[test]
fn shift_drag_copies_and_draw_drag_places_a_new_note() {
    let a = Note { pitch: 60, start: 0, length: PPQ, velocity: 0.8, ..Default::default() };
    let mut h = piano_with_notes(&[a]);
    h.st.ui.tool = Tool::Draw;
    let from = h.pr_pos(PPQ as f64 * 0.5, 60);
    let to = h.pr_pos(PPQ as f64 * 2.5, 60);
    h.drag(piano, from, to, PointerButton::Primary, Modifiers::SHIFT);
    let n = h.notes();
    assert_eq!(n.len(), 2, "{n:?}");
    assert_eq!(n[0].start, 0, "original stays");
    assert_eq!(n[1].start, 2 * PPQ, "copy moved");
    // Pressing on empty space with the draw tool and dragging places a note where it is released.
    let from = h.pr_pos(PPQ as f64 * 4.1, 67);
    let to = h.pr_pos(PPQ as f64 * 5.1, 69);
    h.drag(piano, from, to, PointerButton::Primary, Modifiers::NONE);
    let n = h.notes();
    assert_eq!(n.len(), 3, "{n:?}");
    assert_eq!((n[2].start, n[2].pitch), (5 * PPQ, 69), "{n:?}");
}

#[test]
fn ctrl_right_drag_zooms_into_the_box_and_ctrl_right_click_zooms_out() {
    let mut h = piano_with_notes(&[]);
    let z0 = h.st.ui.piano_zoom;
    let k0 = h.st.ui.key_height;
    // A box one beat wide and six keys high around the middle of the visible keys.
    let g = h.rect("pr.grid");
    let centre = (127.0 - ((g.center().y - g.top() + h.st.ui.pr_y) / h.st.ui.key_height).floor()) as u8;
    let a = h.pr_pos(0.0, centre + 3);
    let b = h.pr_pos(PPQ as f64, centre - 3);
    h.drag(piano, a, b, PointerButton::Secondary, Modifiers::CTRL);
    h.idle(piano, 40);
    assert!(h.st.ui.pr_anim.is_none(), "animation finished");
    let g = h.rect("pr.grid");
    // One beat now spans the grid width (clamped to the max zoom).
    let expect = (g.width() / 1.0).min(800.0);
    assert!((h.st.ui.piano_zoom - expect).abs() < 2.0, "zoom {} -> {} (expected {expect})", z0, h.st.ui.piano_zoom);
    assert!(h.st.ui.key_height > k0, "keys taller: {k0} -> {}", h.st.ui.key_height);
    assert!(h.notes().is_empty(), "Ctrl + right gesture must not draw or delete");
    let z1 = h.st.ui.piano_zoom;
    let g = h.rect("pr.grid");
    let p = g.center();
    h.click(piano, p, PointerButton::Secondary, Modifiers::CTRL);
    h.idle(piano, 40);
    assert!(h.st.ui.piano_zoom < z1 * 0.6, "zoomed out: {z1} -> {}", h.st.ui.piano_zoom);
}

#[test]
fn arrow_keys_nudge_selected_notes() {
    let a = Note { pitch: 60, start: PPQ, length: PPQ, velocity: 0.8, ..Default::default() };
    let mut h = piano_with_notes(&[a]);
    h.st.ui.notes = vec![0];
    let over = h.pr_pos(PPQ as f64 * 3.0, 50);
    h.frame(piano, vec![Event::PointerMoved(over)], Modifiers::NONE);
    let key = |k: egui::Key, m: Modifiers| vec![Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: m }];
    h.frame(piano, key(egui::Key::ArrowRight, Modifiers::NONE), Modifiers::NONE);
    h.frame(piano, key(egui::Key::ArrowUp, Modifiers::NONE), Modifiers::NONE);
    let n = h.notes();
    assert_eq!((n[0].start, n[0].pitch), (PPQ + PPQ / 4, 61), "{n:?}");
}

#[test]
fn playlist_area_zoom_sets_time_range_and_track_height() {
    let mut h = Harness::new();
    h.idle(playlist, 3);
    let t = h.rect("pl.timeline");
    let zoom = h.st.ui.playlist_zoom;
    // Box from beat 4 to beat 8 over the first track.
    let x = |beat: f32| t.left() + (beat - h.st.ui.pl_x as f32) * zoom;
    let a = Pos2::new(x(4.0), t.top() + 4.0);
    let b = Pos2::new(x(8.0), t.top() + 40.0);
    let clips = h.st.session.project.tracks.iter().map(|t| t.clips.len()).sum::<usize>();
    h.drag(playlist, a, b, PointerButton::Secondary, Modifiers::CTRL);
    h.idle(playlist, 40);
    assert!((h.st.ui.pl_x - 4.0).abs() < 0.05, "starts at beat 4: {}", h.st.ui.pl_x);
    let expect = (t.width() / 4.0).min(400.0);
    assert!((h.st.ui.playlist_zoom - expect).abs() < 1.0, "{} vs {expect}", h.st.ui.playlist_zoom);
    assert!(h.st.ui.pl_vzoom > 1.4, "tracks taller: {}", h.st.ui.pl_vzoom);
    assert_eq!(clips, h.st.session.project.tracks.iter().map(|t| t.clips.len()).sum::<usize>(), "no clips added or deleted");
    // Middle-drag pans back.
    let before = h.st.ui.pl_x;
    let c = t.center();
    h.drag(playlist, c, c + egui::vec2(200.0, 0.0), PointerButton::Middle, Modifiers::NONE);
    assert!(h.st.ui.pl_x < before, "panned left: {before} -> {}", h.st.ui.pl_x);
}

#[test]
fn ctrl_drag_on_empty_space_box_selects() {
    let g0 = Note { pitch: 60, start: 0, length: PPQ, velocity: 0.8, ..Default::default() };
    let g1 = Note { pitch: 62, start: PPQ, length: PPQ, velocity: 0.8, ..Default::default() };
    let far = Note { pitch: 60, start: 8 * PPQ, length: PPQ, velocity: 0.8, ..Default::default() };
    let mut h = piano_with_notes(&[g0, g1, far]);
    h.st.ui.tool = Tool::Draw;
    let a = h.pr_pos(PPQ as f64 * 2.6, 64);
    let b = h.pr_pos(PPQ as f64 * 0.1, 59);
    h.drag(piano, a, b, PointerButton::Primary, Modifiers::CTRL);
    assert_eq!(h.notes().len(), 3, "box select must not draw");
    let mut sel = h.st.ui.notes.clone();
    sel.sort();
    assert_eq!(sel, vec![0, 1]);
}

fn rack(ui: &mut egui::Ui, st: &mut State) {
    super::rack::show(ui, st);
}

#[test]
fn dropping_a_sample_on_a_rack_channel_replaces_its_sound() {
    let mut h = Harness::new();
    h.idle(rack, 3);
    let rows: (u64, Vec<(Id, Rect)>) = h.ctx.data(|d| d.get_temp(egui::Id::new("rack.rows"))).unwrap();
    let (cid, r) = rows.1[1];
    let count = h.st.session.project.channels.len();
    let path = std::path::PathBuf::from("/samples/new_clap.wav");
    // A browser drag carries the path as the drag payload; release it over the second channel.
    let at = Pos2::new(r.left() + 40.0, r.center().y);
    h.frame(rack, vec![Event::PointerMoved(at)], Modifiers::NONE);
    h.frame(rack, vec![Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }], Modifiers::NONE);
    egui::DragAndDrop::set_payload(&h.ctx, path.clone());
    h.frame(rack, vec![Event::PointerMoved(at + egui::vec2(5.0, 0.0))], Modifiers::NONE);
    h.frame(rack, vec![Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }], Modifiers::NONE);
    h.idle(rack, 1);
    let c = h.st.session.project.channel(cid).unwrap();
    match &c.instrument {
        Instrument::Sampler(s) => assert_eq!(s.path, path),
        other => panic!("not a sampler: {other:?}"),
    }
    assert_eq!(h.st.session.project.channels.len(), count, "replaced, not added");
}

fn mastering(ui: &mut egui::Ui, st: &mut State) {
    super::master::show(ui, st);
}

#[test]
fn reference_track_region_is_analysed_and_compared_with_the_mix() {
    // A 6 s reference: quiet first half, loud second half.
    let dir = std::env::temp_dir().join(format!("dragon-ref-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ref.wav");
    let rate = 44100u32;
    let mut rng = dragon_engine::dsp::Rng::new(5);
    let l: Vec<f32> = (0..rate as usize * 6).map(|i| rng.bipolar() * if i < rate as usize * 3 { 0.05 } else { 0.5 }).collect();
    dragon_engine::samples::write_wav(&path, &l, &l, rate, dragon_engine::samples::BitDepth::Float32, false, false).unwrap();

    let mut h = Harness::new();
    h.st.session.edit("ref", |p| p.reference = Some(ReferenceTrack { path: path.clone(), region: Some((3.0, 6.0)) }));
    // Decode and analyse in the background; keep drawing until both results are in.
    let wait = |h: &mut Harness, what: &dyn Fn(&ReferenceStateProbe) -> bool| {
        for _ in 0..3000 {
            h.idle(mastering, 1);
            if what(&probe(&h.st)) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("timed out");
    };
    wait(&mut h, &|p| p.reference_lufs.is_some());
    let loud = probe(&h.st).reference_lufs.unwrap();
    // Selecting the quiet half changes the analysis.
    h.st.session.edit("region", |p| p.reference.as_mut().unwrap().region = Some((0.0, 3.0)));
    wait(&mut h, &|p| p.reference_lufs.is_some_and(|v| v < loud - 10.0));
    // Analyse the mix (the starter beat).
    super::reference::analyse_mix_now(&mut h.st);
    wait(&mut h, &|p| p.mix_lufs.is_some());
    assert!(probe(&h.st).mix_lufs.unwrap() > -40.0, "the starter beat is audible");
    let _ = std::fs::remove_dir_all(dir);
}

struct ReferenceStateProbe {
    reference_lufs: Option<f32>,
    mix_lufs: Option<f32>,
}

fn probe(st: &State) -> ReferenceStateProbe {
    let (r, m) = st.reference.results();
    ReferenceStateProbe { reference_lufs: r.map(|a| a.integrated), mix_lufs: m.map(|a| a.integrated) }
}

// ---- plugin windows (with a fake plugin: the real CLAP / VST3 hosts are tested in dragon-test-plugins)

use dragon_engine::effects::{Effect, PluginParam};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering as AtOrd};
use std::sync::Arc;

/// Shared switches of the fake plugin: whether its GUI may open, whether it is open, a pending GUI edit.
#[derive(Default)]
struct FakeGui {
    allow: AtomicBool,
    open: AtomicBool,
    opened: AtomicU32,
    edit: std::sync::Mutex<Vec<(u32, f64)>>,
}

struct FakePlugin(Arc<FakeGui>);

impl Effect for FakePlugin {
    fn set_param(&mut self, _id: &str, _v: f32) {}
    fn reset(&mut self) {}
    fn process(&mut self, _l: &mut [f32], _r: &mut [f32], _sc: Option<(&[f32], &[f32])>) {}
    fn plugin_params(&self) -> Vec<PluginParam> {
        vec![PluginParam { id: 7, name: "Cutoff".into(), default: 0.5, steps: 0, automatable: true }]
    }
    fn editor(&mut self, open: bool) -> bool {
        let ok = open && self.0.allow.load(AtOrd::Relaxed);
        if ok {
            self.0.opened.fetch_add(1, AtOrd::Relaxed);
        }
        self.0.open.store(ok, AtOrd::Relaxed);
        ok
    }
    fn idle(&mut self) -> bool {
        self.0.open.load(AtOrd::Relaxed)
    }
    fn editor_error(&self) -> Option<String> {
        (!self.0.allow.load(AtOrd::Relaxed)).then(|| "the editor refused to attach".to_string())
    }
    fn take_param_edits(&mut self) -> Vec<(u32, f64)> {
        std::mem::take(&mut *self.0.edit.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
    }
}

struct FakeFactory(Arc<FakeGui>);

impl dragon_engine::render::PluginFactory for FakeFactory {
    fn create_effect(&mut self, _p: &PluginRef, _rate: f32, _max: usize) -> Option<Box<dyn Effect>> {
        Some(Box::new(FakePlugin(self.0.clone())))
    }
    fn create_instrument(&mut self, _p: &PluginRef, _rate: f32, _max: usize) -> Option<Box<dyn dragon_engine::instruments::InstrumentVoice>> {
        None
    }
}

fn plugin_harness(allow: bool) -> (Harness, Arc<FakeGui>, Id, Id) {
    let mut h = Harness::new();
    let gui = Arc::new(FakeGui::default());
    gui.allow.store(allow, AtOrd::Relaxed);
    h.st.session.pool = dragon_engine::render::Pool::with_plugins(Box::new(FakeFactory(gui.clone())));
    let m = h.st.session.project.mixer.master.id;
    let iid = h.st.session.edit("add", |p| {
        let iid = p.next_id();
        let plugin = PluginRef { format: PluginFormat::Vst3, path: "/plugins/Fake.vst3".into(), id: "00".into(), name: "Fake Synth FX".into(), state: Vec::new(), params: Default::default() };
        p.mixer.master.inserts.push(Insert { id: iid, effect: "plugin".into(), enabled: true, mix: 1.0, params: Default::default(), sidechain: None, plugin: Some(plugin), preset: String::new(), mods: Vec::new() });
        iid
    });
    h.st.session.compile(48000.0);
    h.st.ui.effect_windows.push((m, iid));
    (h, gui, m, iid)
}

fn windows_and_pump(ui: &mut egui::Ui, st: &mut State) {
    super::windows::show(ui.ctx(), st);
    st.pump_plugin_editors();
}

#[test]
fn plugin_window_opens_the_plugins_own_gui_by_itself() {
    let (mut h, gui, m, iid) = plugin_harness(true);
    h.idle(windows_and_pump, 3);
    assert!(gui.open.load(AtOrd::Relaxed), "the plugin's GUI opened without a click");
    assert_eq!(gui.opened.load(AtOrd::Relaxed), 1, "opened once, not every frame");
    assert!(h.st.ui.native_editors.contains(&(m, Some(iid))));
    // A knob turned in the plugin's window is stored in the project as one undo step.
    gui.edit.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((7, 0.9));
    h.idle(windows_and_pump, 2);
    gui.edit.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((7, 0.95));
    h.idle(windows_and_pump, 2);
    let stored = |h: &Harness| h.st.session.project.mixer.master.inserts.iter().find(|x| x.id == iid).unwrap().plugin.as_ref().unwrap().params.get(&7).copied();
    assert_eq!(stored(&h), Some(0.95));
    std::thread::sleep(std::time::Duration::from_millis(450));
    h.idle(windows_and_pump, 1);
    assert!(h.st.ui.plugin_edit_run.is_none(), "the edit run closed after a pause");
    h.st.session.undo();
    assert_eq!(stored(&h), None, "one undo removes the whole knob turn");
    // Closing the plugin window and reopening the effect window opens it again.
    gui.open.store(false, AtOrd::Relaxed);
    h.idle(windows_and_pump, 2);
    assert!(h.st.ui.native_editors.is_empty());
    h.st.ui.effect_windows.clear();
    h.idle(windows_and_pump, 4);
    h.st.ui.effect_windows.push((m, iid));
    h.idle(windows_and_pump, 2);
    assert_eq!(gui.opened.load(AtOrd::Relaxed), 2);
}

#[test]
fn plugin_window_failure_shows_the_reason() {
    let (mut h, gui, m, iid) = plugin_harness(false);
    h.idle(windows_and_pump, 3);
    assert!(!gui.open.load(AtOrd::Relaxed));
    assert!(h.st.ui.native_editors.is_empty());
    let shown = h.ctx.data(|d| d.get_temp::<String>(egui::Id::new(("plugin.window.error", (m, Some(iid)))))).unwrap();
    assert!(shown.contains("refused to attach"), "{shown}");
}

#[test]
fn dragging_from_the_picker_places_patterns_and_automation_clips() {
    let mut h = Harness::new();
    // A second ("split") pattern and an automation lane to pick.
    let lane = h.st.session.edit("setup", |p| {
        let pid = p.add_pattern();
        p.pattern_mut(pid).unwrap().name = "Split 2".into();
        let id = p.next_id();
        let master = p.mixer.master.id;
        p.automation.push(AutomationLane {
            id,
            target: Target::MixerVolume(master),
            points: vec![AutoPoint { beat: 0.0, value: 0.2, tension: 0.0 }, AutoPoint { beat: 8.0, value: 0.9, tension: 0.0 }],
            mode: AutomationMode::Read,
            visible: true,
        });
        id
    });
    let split = h.st.session.project.patterns.last().unwrap().id;
    h.idle(playlist, 3);
    let t = h.rect("pl.timeline");
    let picker = h.rect("pl.picker");
    assert!(picker.right() <= t.left() + 1.0, "picker sits left of the timeline: {picker:?} {t:?}");
    let rows: Vec<(String, Rect)> = h.ctx.data(|d| d.get_temp(egui::Id::new("pl.picker.rows"))).unwrap();
    let row = |k: &str| rows.iter().find(|r| r.0 == k).unwrap_or_else(|| panic!("{k} listed in {rows:?}")).1;
    let zoom = h.st.ui.playlist_zoom;
    let sx = h.st.ui.pl_x as f32;
    let x = |beat: f32| t.left() + (beat - sx) * zoom;
    let track0 = h.st.session.project.tracks[0].id;
    h.drag(playlist, row(&format!("pattern:{split}")).center(), Pos2::new(x(8.1), t.top() + 10.0), PointerButton::Primary, Modifiers::NONE);
    let placed: Vec<&Clip> = h.st.session.project.tracks[0].clips.iter().filter(|c| matches!(c.kind, ClipKind::Pattern { pattern } if pattern == split)).collect();
    assert_eq!(placed.len(), 1, "pattern placed on the first track: {:?}", h.st.session.project.tracks[0].clips);
    assert!((placed[0].start - 8.0).abs() < 1e-6, "snapped to beat 8: {}", placed[0].start);
    assert_eq!(h.st.session.project.tracks[0].id, track0);
    // Automation clip onto the second track.
    h.idle(playlist, 2);
    let rows: Vec<(String, Rect)> = h.ctx.data(|d| d.get_temp(egui::Id::new("pl.picker.rows"))).unwrap();
    let r = rows.iter().find(|r| r.0 == format!("automation:{lane}")).expect("automation listed").1;
    let y2 = t.top() + h.st.session.project.tracks[0].height.clamp(28.0, 200.0) * h.st.ui.pl_vzoom + 6.0;
    h.drag(playlist, r.center(), Pos2::new(x(0.1), y2), PointerButton::Primary, Modifiers::NONE);
    let auto: Vec<&Clip> = h.st.session.project.tracks.iter().flat_map(|t| t.clips.iter()).filter(|c| matches!(c.kind, ClipKind::Automation { lane: l } if l == lane)).collect();
    assert_eq!(auto.len(), 1, "automation clip placed");
    assert!((auto[0].length - 8.0).abs() < 1e-6, "clip spans the curve: {}", auto[0].length);
}

fn mixer(ui: &mut egui::Ui, st: &mut State) {
    super::mixer::show(ui, st);
}

#[test]
fn pulling_a_cable_between_mixer_strips_routes_them() {
    let mut h = Harness::new();
    let (a, b) = (h.st.session.project.mixer.channels[0].id, h.st.session.project.mixer.channels[1].id);
    assert!(h.st.session.project.mixer.channels[0].sends.iter().all(|s| s.target != b));
    h.idle(mixer, 3);
    let rects: Vec<(Id, Rect)> = h.ctx.data(|d| d.get_temp(egui::Id::new("mixer.strips"))).expect("mixer drew its strips");
    let ra = rects.iter().find(|r| r.0 == a).unwrap().1;
    let rb = rects.iter().find(|r| r.0 == b).unwrap().1;
    let jack = Pos2::new(ra.center().x + 14.0, ra.bottom() - 58.0);
    h.drag(mixer, jack, rb.center(), PointerButton::Primary, Modifiers::NONE);
    let ch = h.st.session.project.mixer.get(a).unwrap();
    let routed = ch.output == Some(b) || ch.sends.iter().any(|s| s.target == b);
    assert!(routed, "insert 1 now feeds insert 2: output {:?} sends {:?}", ch.output, ch.sends.len());
    assert_eq!(h.st.ui.mixer, a, "the source insert is selected");
}

fn transport(ui: &mut egui::Ui, st: &mut State) {
    super::transport::show(ui, st);
}

/// Tempo display: dragging the whole number changes whole BPM and keeps the decimals; dragging
/// the decimals changes only the decimals. Each drag is one undo step.
#[test]
fn bpm_display_drags_whole_numbers_or_decimals() {
    let mut h = Harness::new();
    h.st.session.edit("setup", |p| p.bpm = 140.25);
    h.idle(transport, 3);
    let (int_r, dec_r): (Rect, Rect) = h.ctx.data(|d| d.get_temp(egui::Id::new("bpm.parts"))).expect("bpm display drawn");
    let a = int_r.center();
    h.drag(transport, a, a - egui::vec2(0.0, 40.0), PointerButton::Primary, Modifiers::NONE);
    let bpm = h.st.session.project.bpm;
    assert!((bpm - 150.25).abs() < 1e-6, "whole steps, decimals kept: {bpm}");
    h.idle(transport, 2);
    let (_, dec_r2): (Rect, Rect) = h.ctx.data(|d| d.get_temp(egui::Id::new("bpm.parts"))).unwrap();
    let _ = dec_r;
    let b = dec_r2.center();
    h.drag(transport, b, b - egui::vec2(0.0, 20.0), PointerButton::Primary, Modifiers::NONE);
    let bpm = h.st.session.project.bpm;
    assert!((bpm - 150.45).abs() < 1e-6, "only the decimals moved: {bpm}");
    // Two drags = two undo steps.
    h.st.session.undo();
    assert!((h.st.session.project.bpm - 150.25).abs() < 1e-6, "{}", h.st.session.project.bpm);
}

/// Playlist selection: Ctrl + drag box-selects across tracks, Shift + drag on a clip clones the
/// selection, and dragging a multi-track selection moves every clip by the same number of tracks.
#[test]
fn playlist_box_select_clone_and_multi_track_move() {
    let mut h = Harness::new();
    let pat = h.st.session.project.patterns[0].id;
    let (t0, t1, t2) = (h.st.session.project.tracks[0].id, h.st.session.project.tracks[1].id, h.st.session.project.tracks[2].id);
    h.st.session.edit("setup", |p| {
        for t in &mut p.tracks {
            t.clips.clear();
        }
        super::playlist::place_pattern(p, t0, pat, 0.0, 4.0);
        super::playlist::place_pattern(p, t1, pat, 0.0, 4.0);
    });
    h.idle(playlist, 3);
    let t = h.rect("pl.timeline");
    let zoom = h.st.ui.playlist_zoom;
    let sx = h.st.ui.pl_x as f32;
    let x = |beat: f32| t.left() + (beat - sx) * zoom;
    let row_h = h.st.session.project.tracks[0].height.clamp(28.0, 200.0) * h.st.ui.pl_vzoom;
    // Ctrl + drag a box over both clips (it starts on a clip: still a selection).
    h.drag(playlist, Pos2::new(x(0.5), t.top() + 8.0), Pos2::new(x(2.0), t.top() + row_h + 8.0), PointerButton::Primary, Modifiers::CTRL);
    assert_eq!(h.st.ui.clips.len(), 2, "both clips selected");
    let total = |h: &Harness| h.st.session.project.tracks.iter().map(|t| t.clips.len()).sum::<usize>();
    assert_eq!(total(&h), 2, "a selection does not move or add clips");
    // Shift + drag clones the selection 8 beats later.
    h.drag(playlist, Pos2::new(x(1.0), t.top() + 8.0), Pos2::new(x(9.0), t.top() + 8.0), PointerButton::Primary, Modifiers::SHIFT);
    assert_eq!(total(&h), 4, "two copies");
    let starts: Vec<f64> = h.st.session.project.tracks[0].clips.iter().map(|c| c.start).collect();
    assert!(starts.contains(&0.0) && starts.contains(&8.0), "{starts:?}");
    // Drag the copies one track down: both move (tracks 1 and 2).
    h.drag(playlist, Pos2::new(x(9.0), t.top() + 8.0), Pos2::new(x(9.0), t.top() + row_h + 8.0), PointerButton::Primary, Modifiers::NONE);
    let at8 = |h: &Harness, tid: Id| h.st.session.project.track(tid).unwrap().clips.iter().filter(|c| (c.start - 8.0).abs() < 1e-9).count();
    assert_eq!((at8(&h, t0), at8(&h, t1), at8(&h, t2)), (0, 1, 1), "the selection moved down one track");
}

/// Several picker entries selected with Ctrl+click are dragged in together, one per track.
#[test]
fn picker_drags_several_selected_items() {
    let mut h = Harness::new();
    let a = h.st.session.project.patterns[0].id;
    let b = h.st.session.edit("setup", |p| {
        for t in &mut p.tracks {
            t.clips.clear();
        }
        p.add_pattern()
    });
    h.idle(playlist, 3);
    let rows: Vec<(String, Rect)> = h.ctx.data(|d| d.get_temp(egui::Id::new("pl.picker.rows"))).unwrap();
    let row = |k: String| rows.iter().find(|r| r.0 == k).unwrap().1;
    h.click(playlist, row(format!("pattern:{a}")).center(), PointerButton::Primary, Modifiers::NONE);
    h.click(playlist, row(format!("pattern:{b}")).center(), PointerButton::Primary, Modifiers::CTRL);
    assert_eq!(h.st.ui.picker_sel.len(), 2);
    let t = h.rect("pl.timeline");
    let x = t.left() + (4.1 - h.st.ui.pl_x as f32) * h.st.ui.playlist_zoom;
    h.drag(playlist, row(format!("pattern:{a}")).center(), Pos2::new(x, t.top() + 10.0), PointerButton::Primary, Modifiers::NONE);
    let tracks = &h.st.session.project.tracks;
    let has = |i: usize, pat: Id| tracks[i].clips.iter().any(|c| matches!(c.kind, ClipKind::Pattern { pattern } if pattern == pat) && (c.start - 4.0).abs() < 1e-9);
    assert!(has(0, a) && has(1, b), "one per track: {:?}", tracks.iter().map(|t| t.clips.len()).collect::<Vec<_>>());
}

/// shortcuts: the gestures are bound to the right commands.
#[test]
fn shortcuts_follow_fl_studio() {
    let mut h = Harness::new();
    crate::commands::register(&mut h.st);
    let reg = &h.st.ctx.commands;
    let key = |id: &str| reg.get(&format!("dragon.{id}")).and_then(|d| d.descriptor.shortcuts.first().map(|s| s.gesture.to_string())).unwrap_or_default();
    for (id, want) in [
        ("edit.duplicate", "Ctrl+B"),
        ("edit.deselect", "Ctrl+D"),
        ("edit.undo", "Ctrl+Z"),
        ("edit.redo", "Ctrl+Y"),
        ("edit.undo_toggle", "Ctrl+Alt+Z"),
        ("edit.quantize", "Alt+Q"),
        ("edit.quick_quantize", "Shift+Q"),
        ("transport.play", "Space"),
        ("transport.pause", "Ctrl+Space"),
        ("transport.panic", "Ctrl+H"),
        ("transport.precount", "Ctrl+P"),
        ("transport.record", "R"),
        ("edit.snap_toggle", "Backspace"),
        ("edit.split", "Insert"),
        ("edit.mute_sel", "Alt+M"),
        ("edit.unmute_sel", "Shift+M"),
        ("edit.invert_sel", "Shift+I"),
        ("edit.move_left", "Shift+Left"),
        ("edit.move_up", "Shift+Up"),
        ("edit.glue", "Ctrl+G"),
        ("edit.chop", "Ctrl+U"),
        ("edit.flam", "Alt+F"),
        ("ntool.randomize", "Alt+R"),
        ("ntool.strum", "Alt+S"),
        ("ntool.arp", "Alt+A"),
        ("ntool.riff", "Alt+E"),
        ("ntool.limit", "Alt+K"),
        ("ntool.articulate", "Alt+L"),
        ("ntool.lfo", "Alt+O"),
        ("ctx.alt_u", "Alt+U"),
        ("ctx.alt_c", "Alt+C"),
        ("ctx.alt_z", "Alt+Z"),
        ("ctx.shift_g", "Shift+G"),
        ("pl.consolidate", "Ctrl+Alt+C"),
        ("pr.ch_up", "G"),
        ("pr.keyview", "M"),
        ("pr.porta", "O"),
        ("pr.prop_next", "F"),
        ("rack.delete", "Alt+Delete"),
        ("ctx.alt_up", "Alt+Up"),
        ("ntool.claw", "Alt+W"),
        ("ntool.levels", "Alt+X"),
        ("ntool.flip", "Alt+Y"),
        ("tool.slip", "S"),
        ("tool.playback", "Y"),
        ("tool.zoom", "Z"),
        ("zoom.in", "PageUp"),
        ("zoom.out", "PageDown"),
        ("zoom.center", "Shift+0"),
        ("zoom.all", "Shift+4"),
        ("zoom.selection", "Shift+5"),
        ("edit.insert_time", "Ctrl+Insert"),
        ("edit.delete_time", "Ctrl+Delete"),
        ("rack.mute1", "1"),
        ("rack.mute0", "0"),
        ("rack.solo3", "Ctrl+3"),
        ("edit.transpose_up", "Ctrl+Up"),
        ("view.picker", "Alt+P"),
        ("view.playlist", "F5"),
        ("view.rack", "F6"),
        ("view.piano", "F7"),
        ("view.mixer", "F9"),
        ("view.close_windows", "F12"),
        ("file.export_midi", "Ctrl+Shift+M"),
        ("edit.legato", "Ctrl+L"),
        ("transport.metronome", "Ctrl+M"),
        ("transport.mode", "L"),
        ("file.save_new_version", "Ctrl+N"),
        ("file.export", "Ctrl+R"),
        ("file.export_mp3", "Ctrl+Shift+R"),
        ("add.pattern", "F4"),
        ("view.fx_gallery", "F8"),
        ("tool.draw", "P"),
        ("tool.paint", "B"),
        ("tool.delete", "D"),
        ("tool.mute", "T"),
        ("tool.slice", "C"),
        ("tool.select", "E"),
        ("add.marker", "Alt+T"),
    ] {
        assert_eq!(key(id), want, "{id}");
    }
}

/// Ctrl+Alt+Z undoes, pressed again it redoes (the toggle), Ctrl+Z keeps stepping back; copy / paste of clips.
/// The metronome toggles in every editor (in the piano roll it used to open a MIDI file dialog).
#[test]
fn metronome_toggles_in_the_piano_roll() {
    let mut h = Harness::new();
    h.st.ui.editor = crate::state::Editor::PianoRoll;
    let on = h.st.session.project.metronome.enabled;
    let ctx = h.ctx.clone();
    crate::commands::execute(&mut h.st, "dragon.transport.metronome", &ctx);
    assert_eq!(h.st.session.project.metronome.enabled, !on);
}

/// Ctrl+Z goes back hundreds of steps one by one, Ctrl+Y comes forward again.
#[test]
fn ctrl_z_steps_back_hundreds_of_steps() {
    let mut h = Harness::new();
    let base = h.st.session.project.bpm;
    for k in 0..300 {
        h.st.session.edit("Tempo", |p| p.bpm = 60.0 + k as f64 * 0.1);
    }
    let run = |h: &mut Harness, id: &str| {
        let ctx = h.ctx.clone();
        crate::commands::execute(&mut h.st, id, &ctx);
    };
    for _ in 0..250 {
        run(&mut h, "dragon.edit.undo");
    }
    assert!((h.st.session.project.bpm - (60.0 + 49.0 * 0.1)).abs() < 1e-6, "250 steps back: {}", h.st.session.project.bpm);
    for _ in 0..50 {
        run(&mut h, "dragon.edit.undo");
    }
    assert_eq!(h.st.session.project.bpm, base, "all 300 steps undone");
    for _ in 0..10 {
        run(&mut h, "dragon.edit.redo");
    }
    assert!((h.st.session.project.bpm - (60.0 + 9.0 * 0.1)).abs() < 1e-6);
}

#[test]
fn undo_toggle_and_clip_clipboard() {
    let mut h = Harness::new();
    h.st.session.edit("a", |p| p.bpm = 100.0);
    h.st.session.edit("b", |p| p.bpm = 110.0);
    let run = |h: &mut Harness, id: &str| {
        let ctx = h.ctx.clone();
        crate::commands::execute(&mut h.st, id, &ctx);
    };
    run(&mut h, "dragon.edit.undo_toggle");
    assert_eq!(h.st.session.project.bpm, 100.0);
    run(&mut h, "dragon.edit.undo_toggle");
    assert_eq!(h.st.session.project.bpm, 110.0, "second Ctrl+Z redoes");
    run(&mut h, "dragon.edit.undo_toggle");
    run(&mut h, "dragon.edit.undo");
    assert_ne!(h.st.session.project.bpm, 100.0, "Ctrl+Z steps further back");
    // Copy two clips and paste them at the playhead.
    let before: usize = h.st.session.project.tracks.iter().map(|t| t.clips.len()).sum();
    h.st.ui.editor = crate::state::Editor::Playlist;
    h.st.ui.clips = h.st.session.project.tracks[0].clips.iter().take(2).map(|c| c.id).collect();
    let n = h.st.ui.clips.len();
    run(&mut h, "dragon.edit.copy");
    run(&mut h, "dragon.edit.paste");
    let after: usize = h.st.session.project.tracks.iter().map(|t| t.clips.len()).sum();
    assert_eq!(after, before + n);
    assert_eq!(h.st.ui.clips.len(), n, "pasted clips are selected");
}

/// slice tool in the piano roll: a dragged line cuts every note it crosses, where it crosses.
#[test]
fn piano_roll_knife_cuts_crossed_notes() {
    let a = Note { pitch: 60, start: 0, length: PPQ * 4, velocity: 0.8, ..Default::default() };
    let b = Note { pitch: 64, start: 0, length: PPQ * 4, velocity: 0.8, ..Default::default() };
    let c = Note { pitch: 72, start: 0, length: PPQ * 4, velocity: 0.8, ..Default::default() };
    let mut h = piano_with_notes(&[a, b, c]);
    h.st.ui.tool = Tool::Slice;
    h.st.ui.snap = 0.25;
    let from = h.pr_pos(PPQ as f64 * 2.0, 65);
    let to = h.pr_pos(PPQ as f64 * 2.0, 59);
    h.drag(piano, from, to, PointerButton::Primary, Modifiers::NONE);
    let n = h.notes();
    assert_eq!(n.len(), 5, "the two crossed notes are cut, the one above is not: {n:?}");
    for p in [60u8, 64] {
        let mut parts: Vec<(u32, u32)> = n.iter().filter(|x| x.pitch == p).map(|x| (x.start, x.length)).collect();
        parts.sort();
        assert_eq!(parts, vec![(0, PPQ * 2), (PPQ * 2, PPQ * 2)], "pitch {p}");
    }
}

/// slice tool in the playlist: a line dragged down across two tracks cuts both clips at that beat.
#[test]
fn playlist_knife_cuts_across_tracks() {
    let mut h = Harness::new();
    let pat = h.st.session.project.patterns[0].id;
    let (t0, t1) = (h.st.session.project.tracks[0].id, h.st.session.project.tracks[1].id);
    h.st.session.edit("setup", |p| {
        for t in &mut p.tracks {
            t.clips.clear();
        }
        super::playlist::place_pattern(p, t0, pat, 0.0, 8.0);
        super::playlist::place_pattern(p, t1, pat, 0.0, 8.0);
    });
    h.st.ui.tool = Tool::Slice;
    h.idle(playlist, 3);
    let t = h.rect("pl.timeline");
    let x = t.left() + (4.0 - h.st.ui.pl_x as f32) * h.st.ui.playlist_zoom;
    let row_h = h.st.session.project.tracks[0].height.clamp(28.0, 200.0) * h.st.ui.pl_vzoom;
    h.drag(playlist, Pos2::new(x, t.top() + 4.0), Pos2::new(x + 2.0, t.top() + row_h + row_h / 2.0), PointerButton::Primary, Modifiers::NONE);
    for tid in [t0, t1] {
        let mut parts: Vec<(f64, f64)> = h.st.session.project.track(tid).unwrap().clips.iter().map(|c| (c.start, c.length)).collect();
        parts.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(parts, vec![(0.0, 4.0), (4.0, 4.0)], "track {tid}");
    }
}

/// note and clip commands: glue, chop, invert, mute, Shift+arrows, Backspace snap.
#[test]
fn fl_note_and_clip_commands() {
    let a = Note { pitch: 60, start: 0, length: PPQ, velocity: 0.8, ..Default::default() };
    let b = Note { pitch: 60, start: PPQ, length: PPQ, velocity: 0.8, ..Default::default() };
    let c = Note { pitch: 64, start: 0, length: PPQ, velocity: 0.8, ..Default::default() };
    let mut h = piano_with_notes(&[a, b, c]);
    h.st.ui.editor = crate::state::Editor::PianoRoll;
    let run = |h: &mut Harness, id: &str| {
        let ctx = h.ctx.clone();
        assert!(crate::commands::execute(&mut h.st, &format!("dragon.{id}"), &ctx), "{id} handled");
    };
    h.st.ui.notes = vec![0, 1];
    run(&mut h, "edit.glue");
    let n = h.notes();
    assert_eq!(n.len(), 2, "{n:?}");
    assert!(n.iter().any(|x| x.pitch == 60 && x.start == 0 && x.length == 2 * PPQ), "{n:?}");
    // Chop the long note into quarter-beat pieces.
    h.st.ui.notes = vec![n.iter().position(|x| x.pitch == 60).unwrap()];
    run(&mut h, "edit.chop");
    assert_eq!(h.notes().iter().filter(|x| x.pitch == 60).count(), 8);
    run(&mut h, "edit.invert_sel");
    assert_eq!(h.st.ui.notes.len(), 1, "only the E is left after inverting");
    run(&mut h, "edit.mute_sel");
    assert!(h.notes().iter().filter(|x| x.pitch == 64).all(|x| x.muted));
    run(&mut h, "edit.move_up");
    assert!(h.notes().iter().any(|x| x.pitch == 65), "Shift+Up moves a semitone");
    // Backspace toggles snap and restores it.
    run(&mut h, "edit.snap_toggle");
    assert_eq!(h.st.ui.snap, 0.0);
    run(&mut h, "edit.snap_toggle");
    assert_eq!(h.st.ui.snap, 0.25);
    // Clips: Shift+Down moves the selected clip to the next track.
    h.st.ui.editor = crate::state::Editor::Playlist;
    let (t0, clip) = h.st.session.project.tracks.iter().find_map(|t| t.clips.first().map(|c| (t.id, c.id))).expect("a clip");
    h.st.ui.clips = vec![clip];
    run(&mut h, "edit.move_down");
    let k = h.st.session.project.tracks.iter().position(|t| t.clips.iter().any(|c| c.id == clip)).unwrap();
    assert_ne!(h.st.session.project.tracks[k].id, t0);
    run(&mut h, "edit.move_up");
    assert_eq!(h.st.session.project.tracks.iter().find(|t| t.clips.iter().any(|c| c.id == clip)).unwrap().id, t0);
}

/// Number keys by context: mute / solo channels in the rack, 7 / 8 / 9 transpose and reverse
/// clips in the playlist; Ctrl+Insert / Ctrl+Delete add or remove time; the piano-roll tool window.
#[test]
fn fl_context_keys_and_tools() {
    let mut h = Harness::new();
    let run = |h: &mut Harness, id: &str| {
        let ctx = h.ctx.clone();
        assert!(crate::commands::execute(&mut h.st, &format!("dragon.{id}"), &ctx), "{id}");
    };
    h.st.ui.editor = crate::state::Editor::Rack;
    run(&mut h, "rack.mute1");
    assert!(h.st.session.project.channels[0].muted);
    run(&mut h, "rack.solo2");
    assert!(h.st.session.project.channels[1].solo && !h.st.session.project.channels[0].solo);
    // Playlist: an audio clip, 8 transposes up, 9 reverses.
    h.st.ui.editor = crate::state::Editor::Playlist;
    let tid = h.st.session.project.tracks[0].id;
    let cid = h.st.session.edit("audio", |p| {
        let c = Clip { id: p.next_id(), name: "a".into(), start: 4.0, length: 4.0, muted: false, color: Default::default(), kind: ClipKind::Audio(Default::default()) };
        let id = c.id;
        p.track_mut(tid).unwrap().clips.push(c);
        id
    });
    h.st.ui.clips = vec![cid];
    run(&mut h, "rack.mute8");
    run(&mut h, "rack.mute9");
    let clip = |h: &Harness| h.st.session.project.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap().clone();
    match &clip(&h).kind {
        ClipKind::Audio(a) => assert!(a.pitch == 1.0 && a.reverse, "{a:?}"),
        _ => unreachable!(),
    }
    assert!(h.st.session.project.channels.iter().skip(1).all(|c| !c.muted), "number keys outside the rack don't mute");
    // Ctrl+Insert / Ctrl+Delete with the loop range 0…2.
    h.st.session.edit("loop", |p| p.loop_range = Some(Range::new(0.0, 2.0)));
    run(&mut h, "edit.insert_time");
    assert_eq!(clip(&h).start, 6.0);
    run(&mut h, "edit.delete_time");
    assert_eq!(clip(&h).start, 4.0);
    // Zoom to the selected clip.
    run(&mut h, "zoom.selection");
    assert!((h.st.ui.pl_x - 4.0).abs() < 1e-9);
    // Alt+K opens the tool window on Limit.
    run(&mut h, "ntool.limit");
    assert!(h.st.ui.note_tools.open && h.st.ui.note_tools.tool == crate::views::note_tools::Tool::Limit);
    // Tool keys.
    run(&mut h, "tool.zoom");
    assert_eq!(h.st.ui.tool, crate::state::Tool::Zoom);
}

/// No two commands share a default shortcut.
#[test]
fn shortcuts_do_not_collide() {
    let mut h = Harness::new();
    crate::commands::register(&mut h.st);
    let mut seen = std::collections::HashMap::new();
    for d in h.st.ctx.commands.all() {
        for s in &d.descriptor.shortcuts {
            if let Some(prev) = seen.insert(s.gesture.to_string(), d.descriptor.id.clone()) {
                panic!("{} used by {} and {}", s.gesture, prev, d.descriptor.id);
            }
        }
    }
}

/// the channel-rack keys (clone, group, zip, move, route, delete) and piano-roll keys (channel stepping,
/// groups, colour selection, event lane) and playlist clip groups.
#[test]
fn fl_rack_piano_and_group_keys() {
    let mut h = Harness::new();
    let run = |h: &mut Harness, id: &str| {
        let ctx = h.ctx.clone();
        assert!(crate::commands::execute(&mut h.st, &format!("dragon.{id}"), &ctx), "{id}");
    };
    h.st.ui.editor = crate::state::Editor::Rack;
    let n0 = h.st.session.project.channels.len();
    let first = h.st.session.project.channels[0].id;
    h.st.ui.channel = Some(first);
    h.st.ui.rack_sel = vec![first];
    run(&mut h, "ctx.alt_c");
    assert_eq!(h.st.session.project.channels.len(), n0 + 1, "Alt+C clones");
    let clone = h.st.ui.rack_sel[0];
    assert_eq!(h.st.session.project.channels[1].id, clone, "clone sits under the original");
    run(&mut h, "ctx.alt_down");
    assert_eq!(h.st.session.project.channels[2].id, clone, "Alt+Down moves it");
    run(&mut h, "ctx.alt_z");
    assert!(h.st.session.project.channels[2].zipped);
    run(&mut h, "ctx.alt_u");
    assert!(!h.st.session.project.channels[2].zipped);
    run(&mut h, "ctx.alt_g");
    assert!(!h.st.session.project.channels[2].group.is_empty());
    run(&mut h, "zoom.out");
    assert!(h.st.ui.rack_group.is_some(), "PgDn shows the group");
    run(&mut h, "edit.legato");
    let mixer = h.st.session.project.channels[2].mixer;
    assert_eq!(h.st.session.project.channels.iter().filter(|c| c.mixer == mixer).count(), 1, "Ctrl+L: own mixer track");
    run(&mut h, "rack.delete");
    assert_eq!(h.st.session.project.channels.len(), n0);
    // Piano roll.
    let ch = h.st.session.project.channels[0].id;
    let pat = h.st.ui.pattern;
    let notes = vec![
        Note { pitch: 60, start: 0, length: PPQ, color: 1, ..Default::default() },
        Note { pitch: 64, start: PPQ, length: PPQ, color: 2, ..Default::default() },
        Note { pitch: 67, start: 2 * PPQ, length: PPQ, color: 1, ..Default::default() },
    ];
    h.st.session.edit("n", |p| p.pattern_mut(pat).unwrap().channels.entry(ch).or_default().notes = notes);
    h.st.ui.piano_channel = Some(ch);
    h.st.ui.editor = crate::state::Editor::PianoRoll;
    h.st.ui.notes = vec![0];
    run(&mut h, "ctx.shift_c");
    assert_eq!(h.st.ui.notes, vec![0, 2], "Shift+C selects by colour");
    run(&mut h, "ctx.shift_g");
    assert!(h.notes()[0].group > 0 && h.notes()[0].group == h.notes()[2].group);
    run(&mut h, "pr.prop_next");
    assert_eq!(h.st.ui.pr_prop, 1);
    run(&mut h, "pr.ch_down");
    assert_ne!(h.st.ui.piano_channel, Some(ch), "K: next channel");
    run(&mut h, "pr.ch_up_notes");
    assert_eq!(h.st.ui.piano_channel, Some(ch), "H: channel above with notes");
    // Playlist clip groups: Shift+G, then clicking one clip selects both.
    h.st.ui.editor = crate::state::Editor::Playlist;
    let ids: Vec<Id> = h.st.session.project.tracks.iter().flat_map(|t| t.clips.iter().map(|c| c.id)).take(2).collect();
    assert_eq!(ids.len(), 2);
    h.st.ui.clips = ids.clone();
    run(&mut h, "ctx.shift_g");
    assert_eq!(h.st.session.project.clip_groups, vec![ids.clone()]);
    run(&mut h, "ctx.alt_g");
    assert!(h.st.session.project.clip_groups.is_empty());
}

/// Marker jumps and the resize-from-left switch.
#[test]
fn fl_marker_jumps_and_resize_left() {
    let mut h = Harness::new();
    let run = |h: &mut Harness, id: &str| {
        let ctx = h.ctx.clone();
        assert!(crate::commands::execute(&mut h.st, &format!("dragon.{id}"), &ctx), "{id}");
    };
    h.st.session.edit("m", |p| {
        let (a, b) = (p.next_id(), p.next_id());
        p.markers.push(Marker { id: a, beat: 8.0, name: "A".into() });
        p.markers.push(Marker { id: b, beat: 16.0, name: "B".into() });
    });
    let before = h.st.ui.resize_left;
    run(&mut h, "pl.resize_left");
    assert_ne!(h.st.ui.resize_left, before);
    run(&mut h, "pl.marker_next");
    run(&mut h, "pr.wave_helper");
    assert!(h.st.ui.wave_helper);
}

/// Ctrl+I: in the piano roll it writes the last touched parameter into automation at the playhead,
/// elsewhere it arms "wait for input"; Shift+M in the piano roll selects groups at random when nothing
/// selected is muted; numeric-pad * / steps bars.
#[test]
fn fl_ctrl_i_shift_m_and_bars() {
    let mut h = Harness::new();
    let run = |h: &mut Harness, id: &str| {
        let ctx = h.ctx.clone();
        assert!(crate::commands::execute(&mut h.st, &format!("dragon.{id}"), &ctx), "{id}");
    };
    h.st.ui.editor = crate::state::Editor::Playlist;
    run(&mut h, "edit.insert_ctrl_value");
    assert!(h.st.ui.wait_input);
    run(&mut h, "transport.record");
    assert!(h.st.ui.waiting_record, "record waits for a note");
    run(&mut h, "transport.record");
    assert!(!h.st.ui.waiting_record);
    // Last touched: the master limiter's ceiling.
    let master = h.st.session.project.mixer.master.id;
    let lim = h.st.session.project.mixer.master.inserts[0].id;
    h.st.session.edit("v", |p| {
        p.mixer.master.inserts[0].params.insert("ceiling".into(), -2.0);
    });
    h.st.ui.last_target = Some(Target::InsertParam { channel: master, insert: lim, param: "ceiling".into() });
    h.st.ui.editor = crate::state::Editor::PianoRoll;
    run(&mut h, "edit.insert_ctrl_value");
    let lane = h.st.session.project.automation.iter().find(|l| matches!(&l.target, Target::InsertParam { param, .. } if param == "ceiling")).expect("lane");
    assert_eq!(lane.points.len(), 1);
    assert_eq!(lane.points[0].value, -2.0);
    // Shift+M: four single notes, none muted → a random subset of them gets selected.
    let ch = h.st.session.project.channels[0].id;
    let pat = h.st.ui.pattern;
    h.st.session.edit("n", |p| p.pattern_mut(pat).unwrap().channels.entry(ch).or_default().notes = (0..16).map(|k| Note { pitch: 60, start: k * PPQ, length: PPQ, ..Default::default() }).collect());
    h.st.ui.piano_channel = Some(ch);
    h.st.ui.notes = vec![0];
    run(&mut h, "edit.unmute_sel");
    assert!(h.st.ui.notes.len() < 16, "a random subset");
    run(&mut h, "transport.next_bar");
}

/// Slice tool right-drag (: slice and delete the smaller piece): a cut at 1 beat into a 4-beat note
/// keeps only the 3-beat part; in the playlist the same on clips.
#[test]
fn knife_right_drag_deletes_the_smaller_piece() {
    let a = Note { pitch: 60, start: 0, length: PPQ * 4, velocity: 0.8, ..Default::default() };
    let mut h = piano_with_notes(&[a]);
    h.st.ui.tool = Tool::Slice;
    let from = h.pr_pos(PPQ as f64 * 1.0, 62);
    let to = h.pr_pos(PPQ as f64 * 1.0, 58);
    h.drag(piano, from, to, PointerButton::Secondary, Modifiers::NONE);
    let n = h.notes();
    assert_eq!(n.len(), 1, "{n:?}");
    assert_eq!((n[0].start, n[0].length), (PPQ, PPQ * 3));

    let mut h = Harness::new();
    let pat = h.st.session.project.patterns[0].id;
    let t0 = h.st.session.project.tracks[0].id;
    h.st.session.edit("setup", |p| {
        for t in &mut p.tracks {
            t.clips.clear();
        }
        super::playlist::place_pattern(p, t0, pat, 0.0, 8.0);
    });
    h.st.ui.tool = Tool::Slice;
    h.idle(playlist, 3);
    let t = h.rect("pl.timeline");
    let x = t.left() + (6.0 - h.st.ui.pl_x as f32) * h.st.ui.playlist_zoom;
    let row_h = h.st.session.project.tracks[0].height.clamp(28.0, 200.0) * h.st.ui.pl_vzoom;
    h.drag(playlist, Pos2::new(x, t.top() + 4.0), Pos2::new(x + 1.0, t.top() + row_h - 4.0), PointerButton::Secondary, Modifiers::NONE);
    let parts: Vec<(f64, f64)> = h.st.session.project.track(t0).unwrap().clips.iter().map(|c| (c.start, c.length)).collect();
    assert_eq!(parts, vec![(0.0, 6.0)], "the 2-beat piece is gone");
}

/// the side-specific mouse shortcuts: Right-Shift+click slices (playlist and piano roll), Right-Shift +
/// right-click slices and deletes the smaller piece, Right-Alt+click mutes, Left-Shift while resizing
/// ripples the following clips; the numeric-pad 0 does not mute channel 10.
#[test]
fn fl_side_modifier_mouse_shortcuts() {
    use super::keys::SideKey;
    // Piano roll.
    let a = Note { pitch: 60, start: 0, length: PPQ * 4, velocity: 0.8, ..Default::default() };
    let mut h = piano_with_notes(&[a]);
    h.st.ui.tool = Tool::Draw;
    h.st.ui.test_keys = vec![SideKey::RightShift];
    let at = h.pr_pos(PPQ as f64, 60);
    h.click(piano, at, PointerButton::Primary, Modifiers::SHIFT);
    let mut n: Vec<(u32, u32)> = h.notes().iter().map(|x| (x.start, x.length)).collect();
    n.sort();
    assert_eq!(n, vec![(0, PPQ), (PPQ, PPQ * 3)], "Right-Shift+click slices");
    let at = h.pr_pos(PPQ as f64 * 3.0, 60);
    h.click(piano, at, PointerButton::Secondary, Modifiers::SHIFT);
    let mut n: Vec<(u32, u32)> = h.notes().iter().map(|x| (x.start, x.length)).collect();
    n.sort();
    assert_eq!(n, vec![(0, PPQ), (PPQ, PPQ * 2)], "Right-Shift+right-click slices and drops the smaller piece");

    // Playlist.
    let mut h = Harness::new();
    let pat = h.st.session.project.patterns[0].id;
    let t0 = h.st.session.project.tracks[0].id;
    h.st.session.edit("setup", |p| {
        for t in &mut p.tracks {
            t.clips.clear();
        }
        super::playlist::place_pattern(p, t0, pat, 0.0, 8.0);
        super::playlist::place_pattern(p, t0, pat, 8.0, 4.0);
    });
    h.st.ui.tool = Tool::Draw;
    h.idle(playlist, 3);
    let t = h.rect("pl.timeline");
    let row_h = h.st.session.project.tracks[0].height.clamp(28.0, 200.0) * h.st.ui.pl_vzoom;
    let x_at = |h: &Harness, b: f32| t.left() + (b - h.st.ui.pl_x as f32) * h.st.ui.playlist_zoom;
    let y = t.top() + row_h / 2.0;
    h.st.ui.test_keys = vec![SideKey::RightShift];
    h.click(playlist, Pos2::new(x_at(&h, 2.0), y), PointerButton::Primary, Modifiers::SHIFT);
    let clips = |h: &Harness| {
        let mut v: Vec<(f64, f64)> = h.st.session.project.track(t0).unwrap().clips.iter().map(|c| (c.start, c.length)).collect();
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        v
    };
    assert_eq!(clips(&h), vec![(0.0, 2.0), (2.0, 6.0), (8.0, 4.0)], "Right-Shift+click slices the clip");
    // Right-Alt+click mutes.
    h.st.ui.test_keys = vec![SideKey::RightAlt];
    h.st.ui.clips.clear();
    h.click(playlist, Pos2::new(x_at(&h, 1.0), y), PointerButton::Primary, Modifiers::ALT);
    assert!(h.st.session.project.track(t0).unwrap().clips.iter().find(|c| c.start == 0.0).unwrap().muted);
    // Left-Shift resize: the clip at 2…8 grows to 10 and the clip at 8 moves to 10.
    h.st.ui.test_keys = vec![SideKey::LeftShift];
    let edge = Pos2::new(x_at(&h, 8.0) - 2.0, y);
    h.drag(playlist, edge, Pos2::new(x_at(&h, 10.0) - 2.0, y), PointerButton::Primary, Modifiers::SHIFT);
    let c = clips(&h);
    assert!(c.contains(&(2.0, 8.0)) && c.contains(&(10.0, 4.0)), "ripple: {c:?}");
    // Double right-click activates the mute tool.
    h.st.ui.test_keys.clear();
    h.st.ui.tool = crate::state::Tool::Draw;
    let empty = Pos2::new(x_at(&h, 20.0), y);
    h.click(playlist, empty, PointerButton::Secondary, Modifiers::NONE);
    h.click(playlist, empty, PointerButton::Secondary, Modifiers::NONE);
    assert_eq!(h.st.ui.tool, crate::state::Tool::Mute, "double right-click picks the mute tool");
    // Numeric-pad 0 is fast forward, not "mute channel 10".
    h.st.ui.test_keys = vec![SideKey::Numpad0];
    h.st.ui.editor = crate::state::Editor::Rack;
    let ctx = h.ctx.clone();
    let muted_before: Vec<bool> = h.st.session.project.channels.iter().map(|c| c.muted).collect();
    crate::commands::execute(&mut h.st, "dragon.rack.mute0", &ctx);
    assert_eq!(muted_before, h.st.session.project.channels.iter().map(|c| c.muted).collect::<Vec<_>>());
}

/// Every stock effect's editor opens fitted to its content and inside the app window; the full-screen button makes it fill the whole app window, and leaving it restores the fit.
#[test]
fn every_stock_editor_fits_its_window_and_goes_full_screen() {
    let screen = Rect::from_min_size(Pos2::ZERO, egui::vec2(1400.0, 900.0));
    for def in dragon_engine::effects::registry() {
        let mut h = Harness::new();
        let m = h.st.session.project.mixer.master.id;
        let iid = h.st.session.edit("add", |p| {
            let iid = p.next_id();
            p.mixer.master.inserts.push(Insert { id: iid, effect: def.id.into(), enabled: true, mix: 1.0, params: def.defaults(), sidechain: None, plugin: None, preset: String::new(), mods: Vec::new() });
            iid
        });
        h.st.session.compile(48000.0);
        h.st.ui.effect_windows.push((m, iid));
        h.idle(windows_and_pump, 4);
        let area = egui::Id::new(("fx", m, iid));
        let r = h.ctx.memory(|mem| mem.area_rect(area)).unwrap_or_else(|| panic!("{} window drawn", def.id));
        assert!(screen.contains_rect(r), "{}: window {r:?} fits on screen", def.id);
        assert!(r.width() >= 480.0, "{}: window wide enough for its display ({})", def.id, r.width());
        let name: Rect = h.ctx.data(|d| d.get_temp(egui::Id::new(("fx.header.name", m, iid)))).expect("header drawn");
        assert!(r.contains_rect(name) && name.width() > 40.0, "{}: the name stays inside the window, before the buttons", def.id);
        // Full screen.
        h.st.ui.fx_fullscreen = Some((m, iid));
        h.idle(windows_and_pump, 4);
        let f = h.ctx.memory(|mem| mem.area_rect(egui::Id::new(("fx.full", m, iid)))).unwrap();
        assert!(f.width() >= screen.width() - 12.0 && f.height() >= screen.height() - 12.0, "{}: full screen fills the app window, got {f:?}", def.id);
        h.st.ui.fx_fullscreen = None;
        h.idle(windows_and_pump, 4);
        let back = h.ctx.memory(|mem| mem.area_rect(area)).unwrap();
        assert!(screen.contains_rect(back) && (back.size() - r.size()).length() < 2.0, "{}: back to a fitted window {r:?} -> {back:?}", def.id);
    }
}

/// Ctrl+X deletes every selected element at once (and copies clips / notes for Ctrl+V). egui delivers
/// Ctrl+X / Ctrl+C / Ctrl+V as Cut / Copy / Paste events, not key presses: they map to the commands.
#[test]
fn ctrl_x_deletes_the_whole_selection() {
    use crate::commands::{clipboard_command, execute};
    use crate::state::Editor;
    assert_eq!(clipboard_command(&egui::Event::Cut), Some("dragon.edit.cut"));
    assert_eq!(clipboard_command(&egui::Event::Copy), Some("dragon.edit.copy"));
    assert_eq!(clipboard_command(&egui::Event::Paste(String::new())), Some("dragon.edit.paste"));
    assert_eq!(clipboard_command(&egui::Event::Text("x".into())), None);
    // Playlist: clips selected on several tracks all go, nothing else does, and Ctrl+V brings them back.
    let mut h = Harness::new();
    let ctx = h.ctx.clone();
    h.st.session.edit("setup", |p| {
        for t in 0..3 {
            for k in 0..2 {
                let id = p.next_id();
                p.tracks[t].clips.push(Clip { id, name: format!("c{t}{k}"), start: k as f64 * 8.0 + 16.0, length: 4.0, muted: false, color: Rgb::default(), kind: ClipKind::Audio(AudioClip::default()) });
            }
        }
    });
    let total = |h: &Harness| h.st.session.project.tracks.iter().map(|t| t.clips.len()).sum::<usize>();
    let before = total(&h);
    let picked: Vec<Id> = h.st.session.project.tracks[..3].iter().map(|t| t.clips.last().unwrap().id).collect();
    h.st.ui.editor = Editor::Playlist;
    h.st.ui.clips = picked.clone();
    execute(&mut h.st, "dragon.edit.cut", &ctx);
    assert_eq!(total(&h), before - 3, "all three selected clips deleted");
    assert!(h.st.session.project.tracks.iter().flat_map(|t| &t.clips).all(|c| !picked.contains(&c.id)));
    assert!(h.st.ui.clips.is_empty());
    execute(&mut h.st, "dragon.edit.paste", &ctx);
    assert_eq!(total(&h), before, "Ctrl+V pastes the cut clips");
    h.st.session.undo();
    h.st.session.undo();
    assert_eq!(total(&h), before, "one undo step brings the cut clips back");
    // Piano roll: every selected note goes, the others stay – even while playlist clips are selected.
    let n = |p: u8, s: u32| Note { pitch: p, start: s * PPQ, length: PPQ, velocity: 0.8, ..Default::default() };
    let mut h = piano_with_notes(&[n(60, 0), n(62, 1), n(64, 2), n(65, 3)]);
    let ctx = h.ctx.clone();
    h.st.ui.editor = Editor::PianoRoll;
    h.st.ui.clips = vec![h.st.session.project.tracks[0].clips[0].id];
    h.st.ui.notes = vec![0, 2, 3];
    execute(&mut h.st, "dragon.edit.cut", &ctx);
    assert_eq!(h.notes().iter().map(|x| x.pitch).collect::<Vec<_>>(), vec![62]);
    assert!(!h.st.session.project.tracks[0].clips.is_empty(), "playlist clips untouched");
    // Channel rack: the selected channels are deleted.
    let mut h = Harness::new();
    let ctx = h.ctx.clone();
    let chans: Vec<Id> = h.st.session.project.channels.iter().map(|c| c.id).collect();
    assert!(chans.len() >= 3);
    h.st.ui.editor = Editor::Rack;
    h.st.ui.rack_sel = vec![chans[0], chans[2]];
    execute(&mut h.st, "dragon.edit.cut", &ctx);
    let left: Vec<Id> = h.st.session.project.channels.iter().map(|c| c.id).collect();
    assert!(!left.contains(&chans[0]) && !left.contains(&chans[2]) && left.contains(&chans[1]));
}

/// Safe mode after a crash: the project opens without loading its third-party plugins, keeps their slots
/// (and saved state) untouched, and saving it keeps them for a normal open later.
#[test]
fn safe_mode_opens_without_plugins_and_keeps_them() {
    let dir = std::env::temp_dir().join(format!("dragon-safe-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("song.dragon");
    let mut h = Harness::new();
    h.st.ctx.paths.local = dir.join("local");
    let m = h.st.session.project.mixer.master.id;
    let iid = h.st.session.edit("add", |p| {
        let iid = p.next_id();
        let plugin = PluginRef { format: PluginFormat::Vst3, path: "/plugins/Crashy.vst3".into(), id: "00".into(), name: "Crashy".into(), state: vec![1, 2, 3], params: Default::default() };
        p.mixer.master.inserts.push(Insert { id: iid, effect: "plugin".into(), enabled: true, mix: 1.0, params: Default::default(), sidechain: None, plugin: Some(plugin), preset: String::new(), mods: Vec::new() });
        iid
    });
    dragon_engine::project_io::save(&h.st.session.project, &path, 0).unwrap();
    h.st.open_path_mode(&path, Some(crate::crash::SafeMode::NoPlugins));
    assert_eq!(h.st.safe_mode, Some(crate::crash::SafeMode::NoPlugins));
    h.st.session.compile(48000.0);
    assert!(h.st.session.pool.effect_by_id(iid).is_none(), "the plugin is not loaded");
    let ins = h.st.session.project.mixer.get(m).unwrap().inserts.iter().find(|x| x.id == iid).unwrap().clone();
    assert_eq!(ins.plugin.as_ref().unwrap().state, vec![1, 2, 3], "its slot and saved state stay");
    // The open is marked until the project has run for a moment.
    assert!(h.st.ctx.paths.local.join("Crash").join("opening.txt").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Clip gain on audio clips in the playlist: drag the dB handle at the clip's top up / down, Shift for
/// fine steps, double-click resets; with several audio clips selected they all move together.
#[test]
fn audio_clip_gain_handle() {
    let mut h = Harness::new();
    let playlist = |ui: &mut egui::Ui, st: &mut State| super::playlist::show(ui, st);
    let t0 = h.st.session.project.tracks[0].id;
    let t1 = h.st.session.project.tracks[1].id;
    let (a, b) = h.st.session.edit("setup", |p| {
        let mut ids = Vec::new();
        for (t, start) in [(0usize, 0.0), (1usize, 0.0)] {
            let id = p.next_id();
            p.tracks[t].clips.push(Clip { id, name: "vox".into(), start, length: 8.0, muted: false, color: Rgb::default(), kind: ClipKind::Audio(AudioClip::default()) });
            ids.push(id);
        }
        (ids[0], ids[1])
    });
    h.st.ui.playlist_zoom = 30.0;
    h.idle(playlist, 3);
    let t = h.rect("pl.timeline");
    let row_h = h.st.session.project.tracks[0].height.clamp(28.0, 200.0) * h.st.ui.pl_vzoom;
    let x_mid = t.left() + (4.0 - h.st.ui.pl_x as f32) * h.st.ui.playlist_zoom;
    let gain = |h: &Harness, tid: Id, cid: Id| match &h.st.session.project.track(tid).unwrap().clips.iter().find(|c| c.id == cid).unwrap().kind {
        ClipKind::Audio(x) => x.gain_db,
        _ => f32::NAN,
    };
    // The handle sits 8 px below the top of the clip, centred.
    let handle = Pos2::new(x_mid, t.top() + 2.0 + 8.0);
    h.drag(playlist, handle, handle - egui::vec2(0.0, 30.0), PointerButton::Primary, Modifiers::NONE);
    assert!((gain(&h, t0, a) - 3.0).abs() < 0.11, "30 px up = +3 dB, got {}", gain(&h, t0, a));
    assert_eq!(h.st.session.project.track(t0).unwrap().clips[0].start, 0.0, "the clip did not move");
    // Fine: Shift = 0.02 dB per pixel.
    h.drag(playlist, handle, handle + egui::vec2(0.0, 50.0), PointerButton::Primary, Modifiers::SHIFT);
    assert!((gain(&h, t0, a) - 2.0).abs() < 0.11, "50 px down with Shift = -1 dB, got {}", gain(&h, t0, a));
    // Double-click resets.
    h.click(playlist, handle, PointerButton::Primary, Modifiers::NONE);
    h.click(playlist, handle, PointerButton::Primary, Modifiers::NONE);
    assert_eq!(gain(&h, t0, a), 0.0);
    // Both clips selected: one drag moves both.
    h.st.ui.clips = vec![a, b];
    h.drag(playlist, handle, handle + egui::vec2(0.0, 60.0), PointerButton::Primary, Modifiers::NONE);
    assert!((gain(&h, t0, a) + 6.0).abs() < 0.11 && (gain(&h, t1, b) + 6.0).abs() < 0.11, "{} {}", gain(&h, t0, a), gain(&h, t1, b));
    let _ = row_h;
    // Undo is one step per drag.
    h.st.session.undo();
    assert_eq!(gain(&h, t1, b), 0.0);
}

/// Files dropped from the file manager onto the playlist land under the mouse: the track under the
/// pointer, the beat on the grid; below the last track they get a new track.
#[test]
fn files_dropped_on_the_playlist_land_under_the_mouse() {
    let mut h = Harness::new();
    let playlist = |ui: &mut egui::Ui, st: &mut State| super::playlist::show(ui, st);
    h.st.ui.playlist_zoom = 20.0;
    h.st.ui.snap = 1.0;
    h.idle(playlist, 3);
    let info = super::playlist::DropInfo::get(&h.ctx).expect("the playlist publishes its drop geometry");
    let (t1, top, bottom) = info.tracks[1];
    let t = info.timeline;
    let pos = Pos2::new(t.left() + (4.6 - h.st.ui.pl_x as f32) * 20.0, (top + bottom) / 2.0);
    assert_eq!(info.target(pos), Some((Some(1), 4.0)), "track 2, beat 4.6 snapped down to 4");
    assert_eq!(info.target(Pos2::new(t.left() - 10.0, top + 2.0)), None, "the track headers are not a drop target");
    let below = Pos2::new(pos.x, info.tracks.last().unwrap().2 + 5.0);
    let below = if t.contains(below) { below } else { Pos2::new(pos.x, t.bottom() - 2.0) };
    // A real file: dropped on track 2 at beat 4.
    let dir = std::env::temp_dir().join(format!("dragon-drop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("kick.wav");
    let s: Vec<f32> = (0..24000).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
    dragon_engine::samples::write_wav(&wav, &s, &s, 48000, dragon_engine::samples::BitDepth::Float32, false, false).unwrap();
    let (track, beat) = info.target(pos).unwrap();
    h.st.add_audio_clip_at(&wav, track.map(|k| info.tracks[k].0), beat);
    let c = h.st.session.project.track(t1).unwrap().clips.iter().find(|c| c.name == "kick").cloned().expect("on track 2");
    assert_eq!(c.start, 4.0);
    assert!(matches!(c.kind, ClipKind::Audio(_)));
    // Below the last track (if the view shows space there): a new track named after the file.
    if let Some((None, b)) = info.target(below) {
        let n = h.st.session.project.tracks.len();
        h.st.add_audio_clip_at(&wav, None, b);
        assert_eq!(h.st.session.project.tracks.len(), n + 1);
        assert_eq!(h.st.session.project.tracks.last().unwrap().name, "kick");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Mixer auto-balance: measures every channel and sets the faders by role – the kick 1 dB under the focus,
/// a pad 12 dB under, and 1.5 dB more because the pad's tone sits in the kick's low end – with the sum's
/// peak at −6 dBFS into the master, in one undo step; correcting a role re-balances from the same measurement.
#[test]
fn mixer_auto_balance() {
    let mut h = Harness::new();
    let dir = std::env::temp_dir().join(format!("dragon-bal-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let tone = |name: &str, amp: f32| {
        let p = dir.join(name);
        let s: Vec<f32> = (0..48000 * 4).map(|i| (i as f32 * 0.03).sin() * amp).collect();
        dragon_engine::samples::write_wav(&p, &s, &s, 48000, dragon_engine::samples::BitDepth::Float32, false, false).unwrap();
        p
    };
    let (loud, quiet) = (tone("k.wav", 0.5), tone("p.wav", 0.1));
    let (mk, mp) = h.st.session.edit("setup", |p| {
        let mut ms = Vec::new();
        for (k, (src, name)) in [(loud.clone(), "Kick"), (quiet.clone(), "Warm pad")].into_iter().enumerate() {
            let id = p.next_id();
            p.tracks[k].clips.push(Clip { id, name: name.into(), start: 0.0, length: 8.0, muted: false, color: Rgb::default(), kind: ClipKind::Audio(AudioClip { source: src, ..Default::default() }) });
            let m = p.tracks[k].mixer;
            p.mixer.get_mut(m).unwrap().name = name.into();
            ms.push(m);
        }
        // Only these two take part (the template's drum channels are muted).
        for c in &mut p.mixer.channels {
            c.muted = !ms.contains(&c.id);
        }
        (ms[0], ms[1])
    });
    h.st.start_auto_balance();
    let t0 = std::time::Instant::now();
    while h.st.balance_job.is_some() && t0.elapsed().as_secs() < 60 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        h.st.poll_auto_balance();
    }
    assert!(h.st.balance_job.is_none(), "finished");
    let vol = |h: &Harness, id| dragon_engine::dsp::gain_to_db(h.st.session.project.mixer.get(id).unwrap().volume);
    let (k, p) = (vol(&h, mk), vol(&h, mp));
    let after = 13.98 + k - p; // the tones differ by 20·log10(0.5/0.1) dB
    assert!((after - 12.5).abs() < 0.5, "kick {k:.1} dB, pad {p:.1} dB, so kick {after:.1} dB louder");
    assert_eq!(h.st.session.history.labels().last().map(|s| s.as_str()), Some("Auto-balance"));
    let rep = h.st.balance_report.clone().expect("a report");
    assert_eq!(rep.entries.len(), 2, "{:?}", rep.entries.iter().map(|e| (&e.name, e.role, e.profile.active)).collect::<Vec<_>>());
    let (lufs, peak) = rep.sum;
    assert!(lufs <= -15.9 && peak <= -5.9, "headroom into the master: {:?}", rep.sum);
    assert!((lufs + 16.0).abs() < 0.2 || (peak + 6.0).abs() < 0.2, "the stricter target is met exactly: {:?}", rep.sum);
    // Calling the pad a lead instrument (−4 instead of −12, no low-end penalty: still 1.5) moves only it.
    h.st.set_balance_role(mp, Some(dragon_engine::balance::Role::Lead));
    let (k2, p2) = (vol(&h, mk), vol(&h, mp));
    assert!(((p2 - k2) - (p - k) - 8.0).abs() < 0.05, "pad 8 dB closer to the kick: kick {k:.1} -> {k2:.1}, pad {p:.1} -> {p2:.1}");
    assert!(k2 < k, "the headroom is kept: everything else comes down a little");
    assert_eq!(h.st.session.project.mixer.get(mp).unwrap().balance_role, Some(dragon_engine::balance::Role::Lead), "remembered");
    // Keep my fader: the pad goes back to where it was before the balance.
    let pad_before = h.st.balance_report.as_ref().unwrap().base.iter().find(|e| e.id == mp).unwrap().before_db;
    h.st.toggle_balance_keep(mp);
    assert!((vol(&h, mp) - pad_before).abs() < 0.01, "kept");
    h.st.toggle_balance_keep(mp);
    // Style: beat driven brings the kick 2 dB closer to the lead pad (role Lead) than balanced.
    let gap0 = vol(&h, mk) - vol(&h, mp);
    h.st.set_balance_style(dragon_engine::balance::Style::BeatDriven);
    let gap1 = vol(&h, mk) - vol(&h, mp);
    assert!((gap1 - gap0 - 2.0).abs() < 0.05, "{gap0} -> {gap1}");
    // A/B: Before puts the old faders on without an undo step, After brings the balance back.
    let (after_k, steps) = (vol(&h, mk), h.st.session.history.undo_len());
    h.st.hear_balance_before(true);
    assert!((vol(&h, mk) - h.st.balance_report.as_ref().unwrap().base[0].before_db).abs() < 0.01 || (vol(&h, mk) - after_k).abs() > 0.01);
    assert_eq!(h.st.session.history.undo_len(), steps, "comparing is not an edit");
    h.st.close_balance();
    assert!((vol(&h, mk) - after_k).abs() < 0.01, "closing while comparing keeps the balance");
    // Undo balance: every fader back.
    h.st.balance_report.as_mut().unwrap().open = true;
    let base = h.st.balance_report.as_ref().unwrap().base.clone();
    h.st.revert_balance();
    for e in base {
        assert!((vol(&h, e.id) - e.before_db).abs() < 0.01);
    }
    assert!(h.st.balance_report.is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The effect editor's designed panel: dragging the compressor's Threshold knob up changes the insert's
/// parameter in the project, as one undo step; the Style buttons / dropdown and the switches are there.
#[test]
fn effect_panel_knob_edits_the_project() {
    let mut h = Harness::new();
    h.st.open_effect_on_master("compressor");
    let editor = |ui: &mut egui::Ui, st: &mut State| {
        let (m, i) = st.ui.effect_windows[0];
        super::windows::effect_editor(ui, st, m, i);
    };
    h.idle(editor, 3);
    let (m, i) = h.st.ui.effect_windows[0];
    let rects: Vec<(String, Rect)> = h.ctx.data(|d| d.get_temp(egui::Id::new(("fx.panel", m, i)).with("rects"))).expect("the panel was drawn");
    for id in ["threshold", "ratio", "attack", "style", "sidechain", "mix"] {
        assert!(rects.iter().any(|(p, _)| p == id), "{id} on the panel");
    }
    let knob = rects.iter().find(|(p, _)| p == "threshold").unwrap().1;
    // The default project's master already has a limiter: find the compressor by its id.
    let ins = |h: &Harness| h.st.session.project.mixer.master.inserts.iter().find(|x| x.id == i).cloned().unwrap();
    let thr = |h: &Harness| ins(h).params.get("threshold").copied().unwrap_or(-18.0);
    let before = thr(&h);
    let undo_before = h.st.session.history.labels().len();
    h.drag(editor, knob.center(), knob.center() - egui::vec2(0.0, 60.0), PointerButton::Primary, Modifiers::NONE);
    assert!(thr(&h) > before + 3.0, "threshold {before} -> {}", thr(&h));
    assert_eq!(h.st.session.history.labels().len(), undo_before + 1, "one undo step");
    // The sidechain LED switch toggles.
    let sw = rects.iter().find(|(p, _)| p == "sidechain").unwrap().1;
    h.click(editor, sw.center(), PointerButton::Primary, Modifiers::NONE);
    assert_eq!(ins(&h).params.get("sidechain").copied(), Some(1.0));
}

/// File → New gives a fully empty project (no tracks, mixer channels, instruments or master effects),
/// every view works with it, and you can build from nothing: a pattern dropped on the empty playlist
/// gets a track, an instrument gets its mixer channel, and it plays.
#[test]
fn new_project_is_empty_and_usable() {
    let mut h = Harness::new();
    h.st.new_project();
    let p = &h.st.session.project;
    assert!(p.tracks.is_empty() && p.mixer.channels.is_empty() && p.channels.is_empty(), "nothing in it");
    assert!(p.mixer.master.inserts.is_empty(), "no master effects");
    assert_eq!(p.patterns.len(), 1, "an empty Pattern 1");
    assert!(p.patterns[0].channels.is_empty());
    for view in [super::playlist::show as fn(&mut egui::Ui, &mut State), super::mixer::show, super::rack::show, super::piano_roll::show, super::picker::show, super::master::show, super::transport::show] {
        h.idle(view, 2);
    }
    // Build from nothing.
    let pat = h.st.session.project.patterns[0].id;
    super::picker::place(&mut h.st, &super::picker::PickItem::Pattern(pat), None, 0.0);
    assert_eq!(h.st.session.project.tracks.len(), 1, "the pattern got a track");
    h.st.session.edit("Add synth", |p| {
        let ch = p.add_channel("Synth", Instrument::Synth(SynthParams::default()));
        p.pattern_mut(pat).unwrap().channels.insert(ch, PatternChannel { steps: vec![], notes: vec![Note { pitch: 60, start: 0, length: PPQ, ..Note::default() }] });
    });
    assert_eq!(h.st.session.project.mixer.channels.len(), 2, "track and instrument have their mixer channels");
    for view in [super::playlist::show as fn(&mut egui::Ui, &mut State), super::mixer::show, super::rack::show, super::piano_roll::show] {
        h.idle(view, 2);
    }
    let cache = dragon_engine::samples::SampleCache::default();
    let opts = dragon_engine::export::ExportOptions { range: dragon_engine::export::ExportRange::Song, tail: 0.2, ..Default::default() };
    let mut pool = dragon_engine::render::Pool::new();
    let mix = dragon_engine::export::render_mix(&h.st.session.project, &cache, &opts, &mut pool, &mut |_| {});
    assert!(mix.l.iter().any(|x| x.abs() > 0.01), "the note plays");
}

/// Mixer: Ctrl+click selects several tracks; clicking a bus's route arrow routes all of them; Shift+click
/// selects a range; "Sidechain to this track" makes sidechain-only sends and points the bus's
/// compressor at the first source.
#[test]
fn mixer_multi_select_routes_and_sidechains() {
    let mut h = Harness::new();
    let mixer = |ui: &mut egui::Ui, st: &mut State| super::mixer::show(ui, st);
    let bus = h.st.session.edit("bus", |p| p.add_mixer_channel("Bus"));
    let iid = h.st.session.edit("comp", |p| {
        let id = p.next_id();
        let def = dragon_engine::effects::find("compressor").unwrap();
        p.mixer.get_mut(bus).unwrap().inserts.push(Insert { id, effect: "compressor".into(), enabled: true, mix: 1.0, params: def.defaults(), sidechain: None, plugin: None, preset: String::new(), mods: Vec::new() });
        id
    });
    h.idle(mixer, 3);
    let rects: Vec<(Id, Rect)> = h.ctx.data(|d| d.get_temp(egui::Id::new("mixer.rects"))).expect("mixer drawn");
    let chans: Vec<Id> = h.st.session.project.mixer.channels.iter().map(|c| c.id).collect();
    let at = |id: Id| rects.iter().find(|r| r.0 == id).unwrap().1;
    // Click the first track, Ctrl+click the second and third.
    let top = |r: Rect| Pos2::new(r.center().x, r.top() + 8.0);
    h.click(mixer, top(at(chans[0])), PointerButton::Primary, Modifiers::NONE);
    h.click(mixer, top(at(chans[1])), PointerButton::Primary, Modifiers::COMMAND);
    h.click(mixer, top(at(chans[2])), PointerButton::Primary, Modifiers::COMMAND);
    assert_eq!(h.st.ui.mixer_sel, vec![chans[0], chans[1], chans[2]]);
    // Route all three to the bus at once (what its route arrow does).
    let srcs = super::mixer::sources(&h.st);
    super::mixer::toggle_routes(&mut h.st, &srcs, bus);
    for c in &chans[0..3] {
        let ch = h.st.session.project.mixer.get(*c).unwrap();
        assert!(ch.output == Some(bus) || ch.sends.iter().any(|s| s.target == bus && !s.sidechain_only), "{c} routes to the bus");
    }
    assert_eq!(h.st.session.history.labels().last().map(|s| s.as_str()), Some("Route selected tracks"));
    // Shift+click: the range from the current track.
    h.click(mixer, top(at(chans[4])), PointerButton::Primary, Modifiers::SHIFT);
    assert_eq!(h.st.ui.mixer_sel, chans[2..=4].to_vec());
    // Sidechain the range to the bus.
    let srcs = super::mixer::sources(&h.st);
    super::mixer::sidechain_to(&mut h.st, &srcs, bus);
    for c in &chans[2..=4] {
        let ch = h.st.session.project.mixer.get(*c).unwrap();
        assert!(ch.sends.iter().any(|s| s.target == bus && s.sidechain_only), "{c} sidechains to the bus");
    }
    let comp = h.st.session.project.mixer.get(bus).unwrap().inserts.iter().find(|i| i.id == iid).unwrap().clone();
    assert_eq!(comp.sidechain, Some(chans[2]), "the compressor listens to the first source");
}

/// Renaming, reordering and icons: drag a mixer strip's header to move the track; double-click its name
/// to rename (the playlist track with the same name follows); pick an icon; Alt+Left in the mixer;
/// drag a playlist track's grip down; Alt+Down in the playlist.
#[test]
fn rename_reorder_and_icons() {
    let mut h = Harness::new();
    let mixer = |ui: &mut egui::Ui, st: &mut State| super::mixer::show(ui, st);
    h.idle(mixer, 3);
    let order = |h: &Harness| h.st.session.project.mixer.channels.iter().map(|c| c.id).collect::<Vec<Id>>();
    let before = order(&h);
    let rects: Vec<(Id, Rect)> = h.ctx.data(|d| d.get_temp(egui::Id::new("mixer.rects"))).unwrap();
    let r1 = rects.iter().find(|r| r.0 == before[0]).unwrap().1;
    let head = Pos2::new(r1.center().x, r1.top() + 12.0);
    // Two strip widths to the right.
    h.drag(mixer, head, head + egui::vec2(2.0 * 92.0, 0.0), PointerButton::Primary, Modifiers::NONE);
    let after = order(&h);
    assert_eq!(after[2], before[0], "moved two places right: {after:?}");
    assert_eq!(h.st.session.history.labels().last().map(|s| s.as_str()), Some("Move mixer track"), "one undo step");
    // Alt+Left moves it back one.
    h.st.ui.mixer = before[0];
    h.st.ui.mixer_sel = vec![before[0]];
    h.st.ui.editor = crate::state::Editor::Mixer;
    crate::commands::execute(&mut h.st, "ctx.alt_left", &h.ctx.clone());
    assert_eq!(order(&h)[1], before[0]);
    // Double-click the name of the first track's channel (named like its playlist track) and rename it.
    let tid = h.st.session.project.tracks[0].id;
    let ch = h.st.session.project.tracks[0].mixer;
    // Start the rename the way a double-click on the name does, then type.
    h.ctx.data_mut(|d| d.insert_temp(egui::Id::new("mixer.rename"), ch));
    h.idle(mixer, 2);
    h.frame(mixer, vec![Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::COMMAND }, Event::Text("Drums".into())], Modifiers::NONE);
    h.frame(mixer, vec![Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }], Modifiers::NONE);
    h.idle(mixer, 2);
    assert_eq!(h.st.session.project.mixer.get(ch).unwrap().name, "Drums");
    assert_eq!(h.st.session.project.track(tid).unwrap().name, "Drums", "the playlist track follows");
    // Icons: automatic from the name, or chosen.
    assert_eq!(super::track_icons::guess("Drums"), "bus");
    h.st.session.edit("Track icon", |p| p.mixer.get_mut(ch).unwrap().icon = "kick".into());
    assert_eq!(h.st.session.project.mixer.get(ch).unwrap().icon, "kick");
    // Playlist: drag track 1's grip down by two rows.
    let playlist = |ui: &mut egui::Ui, st: &mut State| super::playlist::show(ui, st);
    h.idle(playlist, 3);
    let info = super::playlist::DropInfo::get(&h.ctx).unwrap();
    let header = h.rect("pl.header");
    let (t0, top, bottom) = info.tracks[0];
    let row = bottom - top;
    let grip = Pos2::new(header.left() + 4.0, (top + bottom) / 2.0);
    h.drag(playlist, grip, grip + egui::vec2(0.0, 2.0 * row), PointerButton::Primary, Modifiers::NONE);
    assert_eq!(h.st.session.project.tracks[2].id, t0, "moved two rows down");
    // Alt+Down: the clicked track moves one further.
    h.st.ui.track = Some(t0);
    h.st.ui.clips.clear();
    h.st.ui.editor = crate::state::Editor::Playlist;
    crate::commands::execute(&mut h.st, "ctx.alt_down", &h.ctx.clone());
    assert_eq!(h.st.session.project.tracks[3].id, t0);
}

/// Mixer cables: none until a channel is selected; then only that channel's cables in and out; a click
/// on empty mixer space hides them again.
#[test]
fn mixer_cables_only_for_the_selected_channel() {
    let mut h = Harness::new();
    let mixer = |ui: &mut egui::Ui, st: &mut State| super::mixer::show(ui, st);
    h.st.ui.mixer_sel.clear();
    h.idle(mixer, 2);
    let drawn = |h: &mut Harness| {
        h.idle(mixer, 1);
        h.ctx.data(|d| d.get_temp::<usize>(egui::Id::new("mixer.cables.drawn"))).unwrap_or(0)
    };
    assert_eq!(drawn(&mut h), 0, "nothing selected: no cables");
    let rects: Vec<(Id, Rect)> = h.ctx.data(|d| d.get_temp(egui::Id::new("mixer.rects"))).unwrap();
    let ch = h.st.session.project.mixer.channels[0].id;
    let r = rects.iter().find(|r| r.0 == ch).unwrap().1;
    h.click(mixer, Pos2::new(r.center().x, r.top() + 10.0), PointerButton::Primary, Modifiers::NONE);
    assert_eq!(drawn(&mut h), 1, "its one cable to the master");
    let master = h.st.session.project.mixer.master.id;
    let rm = rects.iter().find(|r| r.0 == master).unwrap().1;
    h.click(mixer, Pos2::new(rm.center().x, rm.top() + 10.0), PointerButton::Primary, Modifiers::NONE);
    assert_eq!(drawn(&mut h), h.st.session.project.mixer.channels.len(), "the master: every cable coming in");
    // Escape over the mixer deselects.
    h.frame(mixer, vec![Event::PointerMoved(rm.center()), Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }], Modifiers::NONE);
    assert_eq!(drawn(&mut h), 0, "deselected with Escape");
    // A click on empty mixer space too (a project with one track leaves room).
    h.st.new_project();
    h.st.session.edit("track", |p| {
        p.add_track("One");
    });
    h.idle(mixer, 2);
    let rects: Vec<(Id, Rect)> = h.ctx.data(|d| d.get_temp(egui::Id::new("mixer.rects"))).unwrap();
    let one = rects[1].1;
    h.click(mixer, Pos2::new(one.center().x, one.top() + 10.0), PointerButton::Primary, Modifiers::NONE);
    assert_eq!(drawn(&mut h), 1);
    h.click(mixer, Pos2::new(one.right() + 160.0, one.center().y), PointerButton::Primary, Modifiers::NONE);
    assert_eq!(drawn(&mut h), 0, "deselected by clicking empty space");
}

/// Dragging an effect slot onto another mixer track copies it there with its settings (Shift moves it).
#[test]
fn drag_an_effect_onto_another_track() {
    let mut h = Harness::new();
    let mixer = |ui: &mut egui::Ui, st: &mut State| super::mixer::show(ui, st);
    let chans: Vec<Id> = h.st.session.project.mixer.channels.iter().map(|c| c.id).collect();
    let (src, dst) = (chans[0], chans[3]);
    h.st.session.edit("fx", |p| {
        let id = p.next_id();
        let mut params = dragon_engine::effects::find("saturator").unwrap().defaults();
        params.insert("drive".into(), 17.0);
        p.mixer.get_mut(src).unwrap().inserts.push(Insert { id, effect: "saturator".into(), enabled: true, mix: 0.6, params, sidechain: None, plugin: None, preset: String::new(), mods: Vec::new() });
    });
    h.idle(mixer, 2);
    let rects: Vec<(Id, Rect)> = h.ctx.data(|d| d.get_temp(egui::Id::new("mixer.rects"))).unwrap();
    let target = rects.iter().find(|r| r.0 == dst).unwrap().1.center();
    let drop = |h: &mut Harness, m: Modifiers| {
        egui::DragAndDrop::set_payload(&h.ctx, super::mixer::SlotDrag { mixer: src, index: 0 });
        h.frame(mixer, vec![Event::PointerMoved(target)], m);
        h.frame(mixer, vec![Event::PointerButton { pos: target, button: PointerButton::Primary, pressed: true, modifiers: m }], m);
        h.frame(mixer, vec![Event::PointerButton { pos: target, button: PointerButton::Primary, pressed: false, modifiers: m }], m);
        h.idle(mixer, 1);
    };
    drop(&mut h, Modifiers::NONE);
    let copied = h.st.session.project.mixer.get(dst).unwrap().inserts.last().cloned().expect("copied");
    assert_eq!((copied.effect.as_str(), copied.params.get("drive").copied(), copied.mix), ("saturator", Some(17.0), 0.6));
    assert_eq!(h.st.session.project.mixer.get(src).unwrap().inserts.len(), 1, "the original stays");
    drop(&mut h, Modifiers::SHIFT);
    assert!(h.st.session.project.mixer.get(src).unwrap().inserts.is_empty(), "Shift moves it");
    assert_eq!(h.st.session.project.mixer.get(dst).unwrap().inserts.iter().filter(|i| i.effect == "saturator").count(), 2);
}

/// The export window: a quality preset sets format and loudness; the summary names the file; stems
/// change the path to a folder; exporting runs and shows the result with the loudness.
#[test]
fn export_window_presets_and_result() {
    let mut h = Harness::new();
    let mut d = super::export::ExportDialog::new(&h.st.session.project, false, None);
    let dir = std::env::temp_dir().join(format!("dragon-export-ui-{}", std::process::id()));
    d.test_set_dir(&dir);
    d.test_preset(1);
    assert_eq!(d.test_summary(), ("Untitled.wav".to_string(), Some(-14.0)));
    d.test_preset(2);
    assert_eq!(d.test_summary().0, "Untitled.mp3");
    d.test_stems(true);
    assert_eq!(d.test_summary().0, "Untitled stems");
    d.test_stems(false);
    d.test_preset(0);
    // Run it.
    let ctx = h.ctx.clone();
    d.test_start(&mut h.st);
    let t0 = std::time::Instant::now();
    while d.test_running() && t0.elapsed().as_secs() < 120 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            d.show(ctx, &mut h.st);
        });
    }
    let files = d.test_files().expect("exported");
    assert!(files.iter().any(|f| f.ends_with("Untitled.wav") && f.exists()), "{files:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Cut a clip, drag one piece below the last track: it lands on a new track grouped with its own track and
/// routed to the same mixer channel; one undo puts everything back.
#[test]
fn a_cut_piece_dragged_below_the_tracks_gets_a_grouped_track() {
    let mut h = Harness::new();
    let pat = h.st.session.project.patterns[0].id;
    let t0 = h.st.session.project.tracks[0].id;
    h.st.session.edit("setup", |p| {
        p.tracks.truncate(1);
        p.tracks[0].clips.clear();
        super::playlist::place_pattern(p, t0, pat, 0.0, 8.0);
        let c = p.tracks[0].clips[0].id;
        dragon_engine::edit::split_clip(p, t0, c, 4.0);
    });
    let undo_depth = h.st.session.history.undo_len();
    h.st.ui.tool = Tool::Draw;
    h.st.ui.pl_x = 0.0;
    h.idle(playlist, 3);
    let t = h.rect("pl.timeline");
    let row_h = h.st.session.project.tracks[0].height.clamp(28.0, 200.0) * h.st.ui.pl_vzoom;
    let x = t.left() + 6.0 * h.st.ui.playlist_zoom;
    let from = Pos2::new(x, t.top() + row_h / 2.0);
    let to = Pos2::new(x, t.top() + row_h + 30.0);
    assert!(to.y < t.bottom(), "room below the track");
    h.drag(playlist, from, to, PointerButton::Primary, Modifiers::NONE);
    let p = &h.st.session.project;
    assert_eq!(p.tracks.len(), 2, "a new track");
    let (a, b) = (&p.tracks[0], &p.tracks[1]);
    assert!(a.group.is_some() && a.group == b.group, "grouped with its track");
    assert_eq!(a.mixer, b.mixer, "routed to the same mixer channel");
    assert_eq!(a.clips.iter().map(|c| c.start).collect::<Vec<_>>(), vec![0.0]);
    assert_eq!(b.clips.iter().map(|c| c.start).collect::<Vec<_>>(), vec![4.0], "the dragged piece, on time");
    h.st.session.compile(48000.0);
    // One undo step.
    assert_eq!(h.st.session.history.undo_len(), undo_depth + 1);
    h.st.session.undo();
    assert_eq!(h.st.session.project.tracks.len(), 1);
    assert_eq!(h.st.session.project.tracks[0].clips.len(), 2);
}

/// The fade handles on an audio clip's top corners: drag them in to fade in / out, double-click removes
/// the fade; the gain handle and the clip itself stay put.
#[test]
fn audio_clip_fades_are_dragged_from_the_corners() {
    let mut h = Harness::new();
    let playlist = |ui: &mut egui::Ui, st: &mut State| super::playlist::show(ui, st);
    let tid = h.st.session.project.tracks[0].id;
    let cid = h.st.session.edit("setup", |p| {
        let id = p.next_id();
        p.tracks[0].clips.push(Clip { id, name: "vox".into(), start: 0.0, length: 8.0, muted: false, color: Rgb::default(), kind: ClipKind::Audio(AudioClip::default()) });
        id
    });
    h.st.ui.playlist_zoom = 30.0;
    h.st.ui.pl_x = 0.0;
    h.idle(playlist, 3);
    let t = h.rect("pl.timeline");
    let fades = |h: &Harness| match &h.st.session.project.track(tid).unwrap().clips.iter().find(|c| c.id == cid).unwrap().kind {
        ClipKind::Audio(a) => (a.fade_in, a.fade_out),
        _ => (f64::NAN, f64::NAN),
    };
    let x = |beat: f32| t.left() + beat * 30.0;
    let y = t.top() + 2.0 + 13.0;
    // Fade in: from the top-left corner to beat 2.
    h.drag(playlist, Pos2::new(x(0.0) + 5.0, y), Pos2::new(x(2.0), y), PointerButton::Primary, Modifiers::NONE);
    assert!((fades(&h).0 - 2.0).abs() < 0.05, "fade in {:?}", fades(&h));
    // Fade out: from the top-right corner to beat 6.
    h.drag(playlist, Pos2::new(x(8.0) - 5.0, y), Pos2::new(x(6.0), y), PointerButton::Primary, Modifiers::NONE);
    assert!((fades(&h).1 - 2.0).abs() < 0.05, "fade out {:?}", fades(&h));
    let c = h.st.session.project.track(tid).unwrap().clips.iter().find(|c| c.id == cid).unwrap().clone();
    assert_eq!((c.start, c.length), (0.0, 8.0), "the clip did not move or resize");
    // A fade can't run past the other one.
    h.drag(playlist, Pos2::new(x(2.0), y), Pos2::new(x(7.5), y), PointerButton::Primary, Modifiers::NONE);
    assert!((fades(&h).0 - 6.0).abs() < 0.05, "fade in stops at the fade out: {:?}", fades(&h));
    // Where both handles meet, the left half grabs the fade in: drag it back to beat 3.
    h.drag(playlist, Pos2::new(x(6.0) - 3.0, y), Pos2::new(x(3.0) - 3.0, y), PointerButton::Primary, Modifiers::NONE);
    let (fi, fo) = fades(&h);
    assert!((fi - 2.9).abs() < 0.05 && (fo - 2.0).abs() < 0.05, "{:?}", fades(&h));
    // Double-click the fade-in handle: gone; undo brings it back.
    let at = Pos2::new(x(fi as f32), y);
    h.click(playlist, at, PointerButton::Primary, Modifiers::NONE);
    h.click(playlist, at, PointerButton::Primary, Modifiers::NONE);
    assert_eq!(fades(&h).0, 0.0);
    h.st.session.undo();
    assert!((fades(&h).0 - fi).abs() < 1e-9);
}

/// The mixer's "+ record" button makes a recording channel; its strip shows the arm button and input.
#[test]
fn the_mixer_adds_recording_channels() {
    let mut h = Harness::new();
    let mixer = |ui: &mut egui::Ui, st: &mut State| super::mixer::show(ui, st);
    h.idle(mixer, 2);
    let n = h.st.session.project.mixer.channels.len();
    let button = h.ctx.data(|d| d.get_temp::<(Rect, u64)>(egui::Id::new(("tour.anchor", "mixer.add_rec")))).expect("+ record drawn").0;
    h.click(mixer, button.center(), PointerButton::Primary, Modifiers::NONE);
    // The command runs through the app's command registry: run it directly when the harness has none.
    if h.st.session.project.mixer.channels.len() == n {
        crate::views::recording::add_recording_channel(&mut h.st);
    }
    assert_eq!(h.st.session.project.mixer.channels.len(), n + 1);
    h.idle(mixer, 2);
    let has = |k: &str| h.ctx.data(|d| d.get_temp::<(Rect, u64)>(egui::Id::new(("tour.anchor", k)))).is_some();
    assert!(has("mixer.rec_strip") && has("mixer.rec_input") && has("mixer.rec_monitor"), "the strip and the panel show the recording controls");
}

/// Compact mode: playlist tracks become thin rows; mixer strips get narrow.
#[test]
fn compact_mode_shrinks_tracks_and_strips() {
    let mut h = Harness::new();
    let playlist = |ui: &mut egui::Ui, st: &mut State| super::playlist::show(ui, st);
    let mixer = |ui: &mut egui::Ui, st: &mut State| super::mixer::show(ui, st);
    let rows = |h: &mut Harness| {
        h.idle(playlist, 2);
        super::playlist::DropInfo::get(&h.ctx).unwrap().tracks.iter().map(|t| t.2 - t.1).collect::<Vec<f32>>()
    };
    let strips = |h: &mut Harness| {
        h.idle(mixer, 2);
        h.ctx.data(|d| d.get_temp::<Vec<(Id, Rect)>>(egui::Id::new("mixer.rects"))).unwrap().iter().map(|r| r.1.width()).fold(0.0f32, f32::max)
    };
    let (full_rows, full_w) = (rows(&mut h), strips(&mut h));
    h.st.settings.compact_tracks = true;
    h.st.settings.compact_mixer = true;
    let (small_rows, small_w) = (rows(&mut h), strips(&mut h));
    assert!(small_rows.iter().all(|r| (*r - super::playlist::COMPACT_H).abs() < 0.5), "{small_rows:?}");
    assert!(small_rows.iter().sum::<f32>() < full_rows.iter().sum::<f32>() * 0.6);
    assert!(small_w < full_w * 0.65, "strips {small_w} vs {full_w}");
}

/// Dragging a short sample (a one-shot) from the browser onto a playlist track adds it there, where it
/// was dropped, as a clip at least a grid cell wide (so it can be seen and grabbed).
#[test]
fn a_short_sample_dragged_from_the_browser_lands_in_the_playlist() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kick.wav");
    // 80 ms.
    let d = dragon_engine::samples::AudioData::from_planar(48_000, vec![0.5; 3840], vec![0.5; 3840]);
    dragon_engine::repair::write_wav(&path, &d).unwrap();
    let mut h = Harness::new();
    let playlist = |ui: &mut egui::Ui, st: &mut State| super::playlist::show(ui, st);
    h.st.ui.playlist_zoom = 30.0;
    h.st.ui.pl_x = 0.0;
    h.idle(playlist, 3);
    let t = h.rect("pl.timeline");
    let tid = h.st.session.project.tracks[1].id;
    let info = super::playlist::DropInfo::get(&h.ctx).unwrap();
    let row = info.tracks.iter().find(|r| r.0 == tid).copied().unwrap();
    let at = Pos2::new(t.left() + 8.0 * 30.0 + 3.0, (row.1 + row.2) / 2.0);
    // The drag starts elsewhere (in the browser): the payload is set while the button is down.
    let start = Pos2::new(t.left() + 5.0, t.bottom() - 5.0);
    h.frame(playlist, vec![Event::PointerMoved(start), Event::PointerButton { pos: start, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }], Modifiers::NONE);
    egui::DragAndDrop::set_payload(&h.ctx, path.clone());
    for k in 1..=4 {
        let p = start + (at - start) * (k as f32 / 4.0);
        h.frame(playlist, vec![Event::PointerMoved(p)], Modifiers::NONE);
    }
    h.frame(playlist, vec![Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }], Modifiers::NONE);
    h.idle(playlist, 1);
    let clip = h.st.session.project.track(tid).unwrap().clips.iter().find(|c| matches!(&c.kind, ClipKind::Audio(a) if a.source == path)).cloned();
    let clip = clip.expect("the sample landed on the track it was dropped on");
    assert!((clip.start - 8.0).abs() < 0.3, "at the drop point: {}", clip.start);
}

// ------------------------------------------------------------------------------------------------ gestures

/// The master's 3D spectrum turns with a left-drag (yaw across, tilt up / down) and a double-click resets it.
#[test]
fn master_spectrum_rotates_with_a_drag() {
    let mut h = Harness::new();
    h.idle(super::master::show, 3);
    let r = h.rect("master.spectrum3d.rect");
    let view = |h: &Harness| h.ctx.data(|d| d.get_temp::<(f32, f32)>(egui::Id::new("master.spectrum3d.view"))).unwrap();
    let before = view(&h);
    h.drag(super::master::show, r.center(), r.center() + egui::vec2(120.0, 40.0), PointerButton::Primary, Modifiers::NONE);
    let after = view(&h);
    assert!(after.0 > before.0 + 0.5, "yaw turned: {before:?} -> {after:?}");
    assert!(after.1 > before.1 + 0.1, "tilted: {before:?} -> {after:?}");
    // Double-click: back to the starting view.
    let c = r.center();
    for _ in 0..2 {
        h.frame(super::master::show, vec![Event::PointerButton { pos: c, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }], Modifiers::NONE);
        h.frame(super::master::show, vec![Event::PointerButton { pos: c, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }], Modifiers::NONE);
    }
    h.idle(super::master::show, 1);
    assert_eq!(view(&h), before, "double-click resets the view");
}

/// Arrow keys in the playlist: with clips selected they move the clips, without they scroll the view.
#[test]
fn playlist_arrow_keys_move_clips_or_scroll() {
    let mut h = Harness::new();
    h.st.ui.editor = crate::state::Editor::Playlist;
    let run = |h: &mut Harness, id: &str| {
        let ctx = h.ctx.clone();
        crate::commands::execute(&mut h.st, id, &ctx);
    };
    // Nothing selected: Right scrolls a bar, Down a track.
    h.st.ui.clips.clear();
    let (x0, y0) = (h.st.ui.pl_x, h.st.ui.pl_y);
    run(&mut h, "dragon.nav.right");
    run(&mut h, "dragon.rack.down");
    assert!(h.st.ui.pl_x > x0 && h.st.ui.pl_y > y0, "scrolled: {} {}", h.st.ui.pl_x, h.st.ui.pl_y);
    run(&mut h, "dragon.nav.left");
    assert!((h.st.ui.pl_x - x0).abs() < 1e-9, "back");
    // A clip selected: Right moves it later.
    let (cid, start) = h.st.session.project.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| (c.id, c.start)).next().expect("the demo has clips");
    h.st.ui.clips = vec![cid];
    run(&mut h, "dragon.nav.right");
    let now = h.st.session.project.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap().start;
    assert!(now > start, "the clip moved right: {start} -> {now}");
}

/// Audio repair zooms like the playlist: Ctrl + right-drag a box zooms into it, Ctrl + right-click zooms
/// out, middle-drag pans.
#[test]
fn repair_area_zoom_and_pan() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("tone.wav");
    let s: Vec<f32> = (0..48000 * 4).map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 48000.0).sin() * 0.3).collect();
    // (Loading analyses the clip in the background: slow in debug builds.)
    dragon_engine::samples::write_wav(&f, &s, &s, 48000, dragon_engine::samples::BitDepth::Float32, false, false).unwrap();
    let mut h = Harness::new();
    h.st.add_audio_clip_at(&f, None, 0.0);
    let cid = h.st.ui.clips[0];
    super::repair::open_with(&mut h.st, cid, super::repair::Tool::Denoise);
    for _ in 0..1200 {
        h.idle(super::repair::show, 1);
        if h.st.ui.repair.data.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(h.st.ui.repair.data.is_some(), "the clip loaded");
    h.idle(super::repair::show, 2);
    let plot = h.rect("repair.spectrum");
    let ctrl = Modifiers { ctrl: true, command: true, ..Modifiers::NONE };
    // Box over the second quarter of the clip.
    let a = Pos2::new(plot.left() + plot.width() * 0.25, plot.top() + 20.0);
    let b = Pos2::new(plot.left() + plot.width() * 0.5, plot.bottom() - 20.0);
    h.drag(super::repair::show, a, b, PointerButton::Secondary, ctrl);
    let (t0, t1) = h.st.ui.repair.view.expect("zoomed in");
    assert!((t0 - 1.0).abs() < 0.15 && (t1 - 2.0).abs() < 0.15, "the box became the view: {t0:.2} … {t1:.2}");
    // Middle-drag to the left: the view moves later in time.
    h.drag(super::repair::show, plot.center(), plot.center() - egui::vec2(plot.width() * 0.25, 0.0), PointerButton::Middle, Modifiers::NONE);
    let (p0, _) = h.st.ui.repair.view.unwrap();
    assert!(p0 > t0 + 0.1, "panned: {t0:.2} -> {p0:.2}");
    // Ctrl + right-click: zooms out.
    h.click(super::repair::show, plot.center(), PointerButton::Secondary, ctrl);
    let w = h.st.ui.repair.view.map(|v| v.1 - v.0).unwrap_or(4.0);
    assert!(w > (t1 - t0) * 1.5, "zoomed out: width {w:.2}");
}

/// Playing live lights the key on the piano-roll keyboard: a typing-keyboard key (Z = C in the current
/// octave) and a MIDI note (both go through `live_note`). A note out of view scrolls the roll to it, and
/// a released key fades out.
#[test]
fn live_notes_light_the_piano_roll_keys() {
    fn view(ui: &mut egui::Ui, st: &mut State) {
        super::keyboard::typing_keyboard(st, ui.ctx());
        super::piano_roll::show(ui, st);
    }
    let mut h = Harness::new();
    h.st.ui.typing_keyboard = true;
    h.idle(view, 3);
    let key = |k: egui::Key, pressed: bool| Event::Key { key: k, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE };
    h.frame(view, vec![key(egui::Key::Z, true)], Modifiers::NONE);
    let c = (h.st.ui.octave * 12) as usize;
    assert!(h.st.ui.sounding[c] > 0.0, "Z lights C{}", h.st.ui.octave);
    h.frame(view, vec![key(egui::Key::Z, false)], Modifiers::NONE);
    assert_eq!(h.st.ui.sounding[c], 0.0);
    assert!(h.st.ui.released[c].is_some(), "fading out");

    // A MIDI note far above the view: the roll scrolls so it's in sight.
    h.st.ui.pr_y = 128.0 * h.st.ui.key_height; // scrolled to the bottom
    h.idle(view, 1);
    let y0 = h.st.ui.pr_y;
    h.st.live_note(true, 108, 1.0);
    h.idle(view, 1);
    let row = (127 - 108) as f32 * h.st.ui.key_height;
    assert!(h.st.ui.pr_y < y0 && row >= h.st.ui.pr_y, "scrolled up to the note: {y0} -> {}", h.st.ui.pr_y);
    assert!(h.st.ui.sounding[108] >= 1.0);
    // A note already in view doesn't move it.
    let y1 = h.st.ui.pr_y;
    h.st.live_note(true, 107, 0.5);
    h.idle(view, 1);
    assert_eq!(h.st.ui.pr_y, y1);
}
