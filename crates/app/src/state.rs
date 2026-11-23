//! Everything the views share: framework context, the open session, the audio backend, settings and
//! UI selection state – plus the actions that touch several of them (files, recording, import).

use crate::audio::Audio;
use crate::session::Session;
use dragon_engine::device::{Command, DeviceConfig, MidiInputs, RecordTarget};
use dragon_engine::midi::RetroBuffer;
use dragon_engine::model::*;
use dragon_engine::plugins::PluginInfo;
use dragon_engine::project_io::{self, Autosave};
use dragon_engine::render::{LiveEvent, PlayMode};
use dragon_engine::time::TempoMap;
use redhive_client_framework::AppContext;
use redhive_contracts::ui::Severity;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub device: DeviceConfig,
    pub plugin_folders: Vec<PathBuf>,
    pub plugins: Vec<PluginInfo>,
    pub recent: Vec<PathBuf>,
    pub browser_folders: Vec<PathBuf>,
    pub autosave_minutes: u32,
    pub undo_limit: usize,
    pub ui_scale: f32,
    pub show_splash: bool,
    /// Shows the start screen ("What do you want to do?") when the app opens.
    pub show_start: bool,
    /// Plays the Dragon startup sound when the app opens.
    pub startup_sound: bool,
    /// Compact playlist tracks: one thin row each (name, mute, solo, arm).
    pub compact_tracks: bool,
    /// Compact mixer strips: narrow, without the pan / separation knobs and the dB scale.
    pub compact_mixer: bool,
    /// Looks for a new Dragon DAW version when the app opens.
    pub check_updates: bool,
    /// A version the user chose to skip (not offered again at start).
    pub skipped_update: Option<String>,
    pub reduced_motion: bool,
    pub follow_playhead: bool,
    /// MCU control surface: MIDI input and (optional) output for motor faders and LEDs.
    #[serde(alias = "mackie_input")]
    pub mcu_input: Option<String>,
    #[serde(alias = "mackie_output")]
    pub mcu_output: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            device: DeviceConfig::default(),
            plugin_folders: Vec::new(),
            plugins: Vec::new(),
            recent: Vec::new(),
            browser_folders: Vec::new(),
            autosave_minutes: 3,
            undo_limit: dragon_engine::history::UNDO_STEPS,
            ui_scale: 1.0,
            show_splash: true,
            show_start: true,
            startup_sound: true,
            compact_tracks: false,
            compact_mixer: false,
            check_updates: true,
            skipped_update: None,
            reduced_motion: false,
            follow_playhead: true,
            mcu_input: None,
            mcu_output: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Draw,
    Paint,
    Select,
    Slice,
    Erase,
    Mute,
    /// "Playback" (Y): hold the mouse on the timeline to play from there; dragging scrubs.
    Playback,
    /// "Zoom" (Z): drag a box to zoom into it, right-click zooms out.
    Zoom,
    /// "Slip edit" (S, playlist): drag inside an audio clip to move the audio, the clip stays.
    Slip,
}

impl Tool {
    pub const ALL: [(Tool, &'static str, &'static str); 9] = [
        (Tool::Draw, "✏", "Draw (P)"),
        (Tool::Paint, "🎨", "Paint (B)"),
        (Tool::Select, "⬛", "Select (E)"),
        (Tool::Slice, "✂", "Slice (C)"),
        (Tool::Erase, "🗙", "Delete (D)"),
        (Tool::Mute, "🔇", "Mute (T)"),
        (Tool::Slip, "↔", "Slip edit (S)"),
        (Tool::Playback, "▶", "Playback (Y)"),
        (Tool::Zoom, "🔍", "Zoom (Z)"),
    ];

    /// Its short name and what it does (for the tool buttons).
    pub fn describe(self) -> (&'static str, &'static str) {
        match self {
            Tool::Draw => ("Draw", "Click to add a clip or note, drag it to move, drag its edge to resize."),
            Tool::Paint => ("Paint", "Drag across the grid to paint many clips or notes in one go."),
            Tool::Select => ("Select", "Drag a box to select several things (Ctrl+drag works with any tool)."),
            Tool::Slice => ("Slice", "Drag a line across clips or notes to cut them where it crosses."),
            Tool::Erase => ("Delete", "Click or drag over clips or notes to delete them (right-click deletes too)."),
            Tool::Mute => ("Mute", "Click clips or notes to mute / unmute them."),
            Tool::Slip => ("Slip", "Drag inside an audio clip to move its audio while the clip stays put."),
            Tool::Playback => ("Play", "Hold the mouse on the timeline to play from there; drag to scrub."),
            Tool::Zoom => ("Zoom", "Drag a box to zoom into it; right-click zooms out."),
        }
    }
}

/// The editor the user last worked in (decides what Select all / Copy / Paste act on).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Editor {
    #[default]
    Playlist,
    PianoRoll,
    /// The channel rack (number keys mute / solo channels there).
    Rack,
    /// The mixer (Alt+Left / Right move the selected tracks there).
    Mixer,
    /// Audio Repair (Ctrl+A selects all notes / the whole clip, arrows move notes there).
    Repair,
}

/// Copied clips (with their track index) or notes.
#[derive(Debug, Clone)]
pub enum Clipboard {
    Clips(Vec<(usize, Clip)>),
    Notes(Vec<Note>),
}

/// An ffmpeg download's outcome (the folder it went to, or why not).
pub type FfmpegJob = std::sync::Arc<std::sync::Mutex<Option<Result<PathBuf, String>>>>;

pub struct UiState {
    pub pattern: Id,
    pub channel: Option<Id>,
    pub piano_channel: Option<Id>,
    pub mixer: Id,
    /// Mixer tracks selected together with `mixer` (Ctrl / Shift + click); routing applies to all of them.
    pub mixer_sel: Vec<Id>,
    /// The playlist track last clicked (its header).
    pub track: Option<Id>,
    pub clips: Vec<Id>,
    pub notes: Vec<usize>,
    pub tool: Tool,
    pub snap: f64,
    /// The snap restored when Backspace turns snapping back on.
    pub snap_saved: f64,
    pub playlist_zoom: f32,
    pub piano_zoom: f32,
    pub key_height: f32,
    pub scale_root: u8,
    pub scale: Option<usize>,
    pub chord: usize,
    pub typing_keyboard: bool,
    pub octave: i32,
    pub held_keys: Vec<(egui::Key, u8)>,
    /// Notes sounding live (typing keyboard or MIDI controller): velocity per pitch, 0 = not held.
    pub sounding: Vec<f32>,
    /// When each pitch was let go (the key's light fades out).
    pub released: Vec<Option<Instant>>,
    /// The last note played live: the piano roll scrolls to it if it's out of view.
    pub follow_note: Option<u8>,
    pub effect_windows: Vec<(Id, Id)>,
    pub instrument_windows: Vec<Id>,
    pub palette: Option<String>,
    pub pl_x: f64,
    pub pl_y: f32,
    pub pr_x: f64,
    pub pr_y: f32,
    /// Playlist vertical zoom (track heights are multiplied by it).
    pub pl_vzoom: f32,
    /// The picker panel (patterns, audio and automation clips) left of the playlist is shown.
    pub pl_picker: bool,
    /// Entries selected in the picker (dragged together).
    pub picker_sel: Vec<crate::views::picker::PickItem>,
    pub editor: Editor,
    /// The file selected in the browser (shown in its waveform preview) and when its preview started.
    pub browser_sel: Option<PathBuf>,
    /// The start screen is showing.
    pub start_screen: bool,
    /// The album songs' last measured loudness (mixer channel, LUFS).
    pub album_lufs: Vec<(Id, f32)>,
    /// The Audio Repair tab.
    pub repair: crate::views::repair::RepairUi,
    /// The update check and its bar.
    pub update: crate::update::UpdateUi,
    /// The click-through tour.
    pub tour: crate::tutorial::Tour,
    /// Transmuter's preset browser.
    pub wave: crate::views::wave_ui::Browser,
    /// Drakestring' preset browser.
    pub guitar: crate::views::guitar_ui::Browser,
    /// Emberkeys' preset browser.
    pub piano: crate::views::piano_ui::Browser,
    /// ffmpeg can be run (`None` = not checked yet).
    pub ffmpeg_ok: Option<bool>,
    /// Why ffmpeg can't be used (the places tried and what went wrong).
    pub ffmpeg_why: Option<String>,
    /// An ffmpeg download running in the background.
    pub ffmpeg_job: Option<FfmpegJob>,
    /// The control being touched (hovered or turned) and its value, for the hint at the top left:
    /// (where, what, value, when).
    pub hint: Option<(String, String, String, Instant)>,
    pub preview_started: Option<Instant>,
    /// Where in the file the running preview started (seconds).
    pub preview_offset: f64,
    pub clipboard: Option<Clipboard>,
    /// History revision right after the last Ctrl+Z (the undo/redo toggle).
    pub last_undo: Option<u64>,
    /// Running zoom animations: target (zoom, x, vertical zoom / key height, y).
    pub pl_anim: Option<crate::views::nav::ViewTarget>,
    pub pr_anim: Option<crate::views::nav::ViewTarget>,
    pub pl_drag: Option<crate::views::playlist::Drag>,
    /// Clip-gain drag: (track, clip, gain at the start) of every clip being changed.
    pub gain_drag: Vec<(Id, Id, f32)>,
    pub pr_drag: Option<crate::views::piano_roll::Drag>,
    pub waveforms: std::collections::HashMap<PathBuf, std::sync::Arc<Vec<(f32, f32)>>>,
    pub clip_window: Option<Id>,
    /// Open native plugin editors: (mixer channel or rack channel, insert).
    pub native_editors: Vec<(Id, Option<Id>)>,
    /// A run of parameter edits from a plugin's own window (one knob turn = one undo step): when the last
    /// edit arrived and the history revision after it. The run ends after a short pause or when anything
    /// else edits the project.
    pub plugin_edit_run: Option<(std::time::Instant, u64)>,
    pub video: crate::views::video::VideoState,
    /// Running stem separation: (source clip start beat, track name) → stem files.
    pub stem_job: Option<redhive_client_framework::runtime::Promise<StemResult>>,
    pub pattern_mode: bool,
    pub export: Option<crate::views::export::ExportDialog>,
    pub recovery: Option<Vec<(PathBuf, std::time::SystemTime)>>,
    pub about: bool,
    /// The effect gallery window is open.
    pub fx_gallery: bool,
    /// Selected rack channels (Ctrl+click adds; empty = the current channel).
    pub rack_sel: Vec<Id>,
    /// The rack shows only this channel group (PgUp / PgDn cycle through groups).
    pub rack_group: Option<String>,
    /// Rack: mixer-track selectors shown (Alt+M).
    pub rack_mixer_sel: bool,
    /// Piano roll event lane: 0 velocity, 1 pan, 2 release (F / Shift+F).
    pub pr_prop: usize,
    /// Piano roll: note names on every key (M).
    pub pr_key_names: bool,
    /// Piano roll: new notes slide (portamento, O).
    pub porta: bool,
    /// Piano roll: ghost notes of the other channels (Alt+V).
    pub ghosts: bool,
    /// Piano roll: scale helpers (Alt+B).
    pub helpers: bool,
    /// Clips can be resized from their left edge (Ctrl+Alt+Home).
    pub resize_left: bool,
    /// Piano roll: the song's audio drawn under the notes (Alt+N).
    pub wave_helper: bool,
    /// The parameter touched last (Ctrl+I inserts its value into automation).
    pub last_target: Option<dragon_engine::model::Target>,
    /// Ctrl+I (outside the piano roll): recording waits for the first played note.
    pub wait_input: bool,
    /// Record was pressed with wait-for-input on and no note has come yet.
    pub waiting_record: bool,
    /// Side-specific keys held, for tests (the real state comes from the OS).
    pub test_keys: Vec<crate::views::keys::SideKey>,
    /// Right-button press on empty space (time, position) – held long enough it becomes the slice tool.
    pub hold_slice: Option<(f64, egui::Pos2)>,
    /// The context tool menu (middle + right click), where it is open.
    pub tool_menu: Option<egui::Pos2>,
    /// the piano-roll tool windows (Alt+A, Alt+E, Alt+K …).
    pub note_tools: crate::views::note_tools::NoteTools,
    /// The stock effect editor shown full screen (mixer insert, effect).
    pub fx_fullscreen: Option<(Id, Id)>,
    pub midi_learn: Option<Target>,
    pub last_note_len: u32,
    pub tap_times: Vec<Instant>,
    pub record_started: Option<Instant>,
    /// Where an audio recording started (beat), for the live take drawn in the playlist.
    pub record_beat: Option<f64>,
    pub status: String,
    pub plugin_scan: Option<redhive_client_framework::runtime::Promise<(Vec<PluginInfo>, Vec<String>)>>,
}

impl UiState {
    fn new(p: &Project) -> Self {
        UiState {
            pattern: p.patterns.first().map(|x| x.id).unwrap_or(0),
            channel: p.channels.first().map(|c| c.id),
            piano_channel: p.channels.first().map(|c| c.id),
            mixer: p.mixer.master.id,
            mixer_sel: Vec::new(),
            track: None,
            clips: Vec::new(),
            notes: Vec::new(),
            tool: Tool::Draw,
            snap: 0.25,
            snap_saved: 0.25,
            playlist_zoom: 28.0,
            piano_zoom: 96.0,
            key_height: 14.0,
            scale_root: 0,
            scale: None,
            chord: 0,
            typing_keyboard: false,
            octave: 5,
            held_keys: Vec::new(),
            sounding: vec![0.0; 128],
            released: vec![None; 128],
            follow_note: None,
            effect_windows: Vec::new(),
            instrument_windows: Vec::new(),
            palette: None,
            pl_x: 0.0,
            pl_y: 0.0,
            pr_x: 0.0,
            pr_y: 60.0 * 14.0,
            pl_vzoom: 1.0,
            pl_picker: true,
            picker_sel: Vec::new(),
            editor: Editor::Playlist,
            browser_sel: None,
            start_screen: false,
            album_lufs: Vec::new(),
            repair: Default::default(),
            update: Default::default(),
            tour: Default::default(),
            ffmpeg_ok: None,
            ffmpeg_why: None,
            wave: Default::default(),
            guitar: Default::default(),
            piano: Default::default(),
            ffmpeg_job: None,
            hint: None,
            preview_started: None,
            preview_offset: 0.0,
            clipboard: None,
            last_undo: None,
            pl_anim: None,
            pr_anim: None,
            pl_drag: None,
            gain_drag: Vec::new(),
            pr_drag: None,
            waveforms: Default::default(),
            clip_window: None,
            native_editors: Vec::new(),
            plugin_edit_run: None,
            video: Default::default(),
            stem_job: None,
            pattern_mode: false,
            export: None,
            recovery: None,
            about: false,
            fx_gallery: false,
            rack_sel: Vec::new(),
            rack_group: None,
            rack_mixer_sel: true,
            pr_prop: 0,
            pr_key_names: false,
            porta: false,
            ghosts: true,
            helpers: true,
            resize_left: true,
            wave_helper: false,
            last_target: None,
            wait_input: false,
            waiting_record: false,
            test_keys: Vec::new(),
            hold_slice: None,
            tool_menu: None,
            note_tools: Default::default(),
            fx_fullscreen: None,
            midi_learn: None,
            last_note_len: PPQ,
            tap_times: Vec::new(),
            record_started: None,
            record_beat: None,
            status: String::new(),
            plugin_scan: None,
        }
    }
}

/// Measured pre-fader loudness per mixer channel (LUFS).
pub type Loudness = Vec<(Id, f32)>;

/// A running album loudness match.
pub struct BalanceJob {
    pub progress: Arc<std::sync::atomic::AtomicU32>,
    pub result: Arc<Mutex<Option<Loudness>>>,
}

/// What auto-balance measured and decided, kept for its report (and to re-balance when a role is changed).
#[derive(Clone)]
pub struct BalanceReport {
    pub profiles: Vec<(Id, dragon_engine::balance::Profile)>,
    pub entries: Vec<dragon_engine::balance::Entry>,
    /// The headroom offset every fader got (dB).
    pub offset: f32,
    /// The balanced sum's loudness and peak into the master chain (after the offset).
    pub sum: (f32, f32),
    pub open: bool,
    /// The plan the headroom was measured for (later changes carry the offset over from it).
    pub base: Vec<dragon_engine::balance::Entry>,
    pub style: dragon_engine::balance::Style,
    /// Channels whose fader the user keeps.
    pub keep: Vec<Id>,
    /// A/B: the old faders are on right now (to compare).
    pub hearing_before: bool,
}

/// A running mixer auto-balance (see [`State::start_auto_balance`]).
pub struct AutoBalanceJob {
    /// The style and kept channels of the previous report, carried into the new one.
    pub carry: (dragon_engine::balance::Style, Vec<Id>),
    /// The user closed the progress window (the result still opens when it's done).
    pub hidden: bool,
    pub progress: Arc<std::sync::atomic::AtomicU32>,
    pub result: Arc<Mutex<Option<BalanceReport>>>,
}

pub struct State {
    pub ctx: AppContext,
    pub session: Session,
    pub audio: Audio,
    pub ui: UiState,
    pub settings: Settings,
    pub midi: Option<MidiInputs>,
    pub retro: RetroBuffer,
    pub autosave: Autosave,
    pub last_autosave: Instant,
    pub crashed_last_time: bool,
    pub kit: Vec<(String, u8, PathBuf)>,
    pub mcu: Mcu,
    /// Mastering reference track: loaded audio, analyses and background jobs.
    pub reference: crate::views::reference::ReferenceState,
    /// Dragon Quests (gamification).
    pub quests: crate::views::quests::Quests,
    /// What the last run was doing when it crashed (shown once at startup).
    pub crash: Option<crate::crash::CrashInfo>,
    /// The open project runs in safe mode.
    pub safe_mode: Option<crate::crash::SafeMode>,
    /// Frames left until a just-opened project counts as opened fine (its crash marker is cleared then).
    pub open_guard: Option<u32>,
    /// Mixer auto-balance running in the background: progress (‰) and the result when done.
    pub balance_job: Option<AutoBalanceJob>,
    /// The last auto-balance (its report window).
    pub balance_report: Option<BalanceReport>,
    /// Album loudness matching running in the background.
    pub album_job: Option<BalanceJob>,
    /// Audio watchdog: the callback count last seen, when it last moved, the last restart, and whether the
    /// device was lost (then it is retried until it comes back).
    pub audio_watch: (u64, Instant, Option<Instant>, bool),
}

/// MCU state: bank offset, the output connection and what was last sent (feedback only on change).
#[derive(Default)]
pub struct Mcu {
    pub bank: usize,
    pub out: Option<dragon_engine::device::MidiOutput>,
    sent_faders: [f32; 9],
    sent_leds: std::collections::HashMap<u8, bool>,
}

/// The built-in drum kit (synthesised on first run into the app's data folder).
pub fn default_kit(paths: &redhive_client_framework::context::Paths) -> Vec<(String, u8, PathBuf)> {
    dragon_engine::instruments::kit::ensure_default_kit(&paths.local.join("Dragon Kit")).unwrap_or_default()
}

/// Drum pads loaded with the built-in kit.
pub fn kit_pads(kit: &[(String, u8, PathBuf)]) -> Vec<Pad> {
    let mut pads = dragon_engine::instruments::default_pads();
    for pad in pads.iter_mut() {
        if let Some((_, _, path)) = kit.iter().find(|k| k.1 == pad.note) {
            pad.layers = vec![PadLayer { path: path.clone(), velocity_low: 0.0, velocity_high: 1.0 }];
        }
    }
    pads
}

/// A starter project: kick / clap / hat / snare samplers with a first beat, a bass and a lead synth, a
/// reverb bus and a limiter on the master – press play and it sounds.
pub fn starter_project(kit: &[(String, u8, PathBuf)]) -> Project {
    let mut p = Project::new_default();
    p.meta.name = "Untitled".into();
    let pat = p.patterns[0].id;
    let beat: &[(&str, &[usize])] = &[("Kick", &[0, 4, 8, 12]), ("Clap", &[4, 12]), ("Closed hat", &[2, 6, 10, 14]), ("Snare", &[])];
    for (name, steps) in beat {
        let Some((_, _, path)) = kit.iter().find(|k| k.0 == *name) else { continue };
        let ch = p.add_channel(name, Instrument::Sampler(SamplerParams { path: path.clone(), ..Default::default() }));
        let mut st = vec![Step::default(); 16];
        for s in *steps {
            st[*s].on = true;
        }
        if let Some(pt) = p.pattern_mut(pat) {
            pt.channels.insert(ch, PatternChannel { steps: st, notes: Vec::new() });
        }
    }
    let bass = SynthParams {
        osc: [Oscillator { wave: Waveform::Saw, level: 0.8, ..Default::default() }, Oscillator { wave: Waveform::Square, level: 0.4, coarse: -12, ..Default::default() }, Oscillator { level: 0.0, ..Default::default() }],
        cutoff: 900.0,
        polyphony: 1,
        ..Default::default()
    };
    p.add_channel("Bass", Instrument::Synth(bass));
    p.add_channel("Lead", Instrument::Synth(SynthParams { unison: 3, detune: 0.15, ..Default::default() }));
    let bus = p.add_mixer_channel("Reverb");
    let ins = p.next_id();
    if let Some(m) = p.mixer.get_mut(bus) {
        let def = dragon_engine::effects::find("reverb").unwrap();
        m.inserts.push(Insert { id: ins, effect: "reverb".into(), enabled: true, mix: 1.0, params: def.defaults(), sidechain: None, plugin: None, preset: String::new(), mods: Vec::new() });
    }
    let lim = p.next_id();
    p.mixer.master.inserts.push(Insert {
        id: lim,
        effect: "limiter".into(),
        enabled: true,
        mix: 1.0,
        params: dragon_engine::effects::find("limiter").unwrap().defaults(),
        sidechain: None,
        plugin: None,
        preset: String::new(),
        mods: Vec::new(),
    });
    // The beat on the first track, four bars.
    if let Some(t) = p.tracks.first().map(|t| t.id) {
        if let Some(tr) = p.track_mut(t) {
            tr.name = "Beat".into();
        }
        let mixer = p.track(t).map(|tr| tr.mixer);
        if let Some(m) = mixer.and_then(|m| p.mixer.get_mut(m)) {
            m.name = "Beat".into();
        }
        for bar in 0..4 {
            let id = p.next_id();
            let color = p.patterns[0].color;
            if let Some(tr) = p.track_mut(t) {
                tr.clips.push(Clip { id, name: "Pattern 1".into(), start: bar as f64 * 4.0, length: 4.0, muted: false, color, kind: ClipKind::Pattern { pattern: pat } });
            }
        }
    }
    p
}

impl State {
    pub fn new(ctx: AppContext, mut settings: Settings) -> Self {
        let kit = default_kit(&ctx.paths);
        let mut session = Session::new(Project::empty(), None);
        // 200 was the old default: lift it to the new one (500); anything below 200 is raised to 200.
        if settings.undo_limit == 200 {
            settings.undo_limit = dragon_engine::history::UNDO_STEPS;
        }
        settings.undo_limit = settings.undo_limit.max(200);
        session.history.limit = settings.undo_limit;
        let song = session.compile(48000.0);
        let audio = Audio::start(&settings.device, song);
        let midi = None;
        let autosave = Autosave::new(ctx.paths.autosave.clone());
        let crashed = !autosave.begin_session();
        let ui = UiState::new(&session.project);
        let quests = crate::views::quests::Quests::load(&ctx.paths);
        let mut st = State {
            ctx,
            session,
            audio,
            ui,
            settings,
            midi,
            retro: RetroBuffer::new(120.0),
            autosave,
            last_autosave: Instant::now(),
            crashed_last_time: crashed,
            kit,
            mcu: Mcu::default(),
            reference: Default::default(),
            quests,
            crash: None,
            safe_mode: None,
            open_guard: None,
            audio_watch: (0, Instant::now(), None, false),
            balance_job: None,
            balance_report: None,
            album_job: None,
        };
        st.open_midi();
        st.session.force_recompile();
        if st.crashed_last_time {
            let list = st.autosave.list();
            if !list.is_empty() {
                st.ui.recovery = Some(list);
            }
        }
        st
    }

    /// Keeps the audio output alive: when the stream reports an error, or its callback stops being called
    /// for 2 s (a driver hang, the device unplugged or switched), the engine is restarted on the configured
    /// (or default) device and playback continues where it was. A lost device is retried every 5 s.
    pub fn audio_watchdog(&mut self) {
        // Effects whose output blew up (NaN / infinity) were silenced and reset by the engine: say which.
        let nan = dragon_engine::fault::take_nan();
        if !nan.is_empty() {
            let p = &self.session.project;
            let names: Vec<String> = nan
                .iter()
                .filter_map(|(id, _)| {
                    std::iter::once(&p.mixer.master)
                        .chain(p.mixer.channels.iter())
                        .find_map(|c| c.inserts.iter().find(|x| x.id == *id).map(|x| format!("{} on {}", dragon_engine::effects::find(&x.effect).map(|d| d.name).unwrap_or(x.effect.as_str()), c.name)))
                })
                .collect();
            if !names.is_empty() {
                self.toast(format!("{} produced invalid audio and was reset", names.join(", ")), Severity::Warning);
            }
        }
        let now = Instant::now();
        let (count, failed) = match self.audio.health() {
            Some(h) => (h.callbacks.load(std::sync::atomic::Ordering::Relaxed), h.failed.load(std::sync::atomic::Ordering::Relaxed)),
            None => (0, false),
        };
        let device = self.audio.health().is_some();
        if count != self.audio_watch.0 {
            self.audio_watch.0 = count;
            self.audio_watch.1 = now;
        }
        let stalled = device && now.duration_since(self.audio_watch.1) > std::time::Duration::from_secs(2);
        let lost_retry = !device && self.audio_watch.3;
        let cooled = self.audio_watch.2.is_none_or(|t| now.duration_since(t) > std::time::Duration::from_secs(5));
        // A fault caught in the engine is handled at once (the effect is bypassed before the restart).
        let faulted = dragon_engine::fault::pending();
        if !((failed || stalled || lost_retry) && cooled) && !faulted {
            return;
        }
        let t = self.audio.transport().state;
        // A caught fault in an effect: bypass that insert so it cannot fail again on every restart.
        let fault = dragon_engine::fault::take();
        if let Some((id, msg)) = &fault {
            let found = id.and_then(|id| {
                let p = &self.session.project;
                std::iter::once(&p.mixer.master).chain(p.mixer.channels.iter()).find_map(|c| c.inserts.iter().find(|x| x.id == id).map(|x| (c.name.clone(), x.effect.clone(), x.plugin.is_some())))
            });
            match (id, found) {
                (Some(iid), Some((chan, effect, _))) => {
                    let iid = *iid;
                    self.session.edit("Bypass failing effect", |p| {
                        for c in std::iter::once(&mut p.mixer.master).chain(p.mixer.channels.iter_mut()) {
                            for x in &mut c.inserts {
                                if x.id == iid {
                                    x.enabled = false;
                                }
                            }
                        }
                    });
                    let name = dragon_engine::effects::find(&effect).map(|d| d.name.to_string()).unwrap_or(effect);
                    self.toast(format!("{name} on {chan} failed ({msg}) – bypassed it and restarted the audio. Details: audio-faults.txt"), Severity::Error);
                }
                _ => self.toast(format!("The audio engine hit an error ({msg}) and was restarted. Details: audio-faults.txt"), Severity::Error),
            }
        }
        let rate = self.audio.rate() as f32;
        let song = self.session.compile(rate);
        // Close the old stream before opening the device again.
        self.audio = crate::audio::Audio::Null(crate::audio::NullEngine::start(song.clone()), "restarting".into());
        self.audio = crate::audio::Audio::start(&self.settings.device, song);
        self.session.force_recompile();
        self.audio_watch = (0, now, Some(now), self.audio.health().is_none());
        if self.audio.health().is_some() {
            self.audio.send(dragon_engine::device::Command::Seek(t.beat.max(0.0)));
            if t.playing && !t.recording {
                self.audio.send(dragon_engine::device::Command::Play(t.mode));
            }
            if fault.is_none() {
                self.toast(format!("Audio output stopped responding – restarted ({})", self.audio.describe()), Severity::Warning);
            }
        } else if !lost_retry {
            self.toast("Audio output lost – retrying every few seconds", Severity::Error);
        }
    }

    /// Starts the mixer's auto-balance in the background: a first offline render measures every channel
    /// (loudness while playing, peak, spectrum), the balance is planned by role, a second render sums the
    /// planned channels (master effects bypassed) to set the headroom, and then the faders move as one undo
    /// step. See `dragon_engine::balance`.
    pub fn start_auto_balance(&mut self) {
        if self.balance_job.is_some() {
            return;
        }
        let project = self.session.project.clone();
        let cache = self.session.cache.clone();
        let rate = self.audio.rate() as f32;
        let progress = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let result = Arc::new(Mutex::new(None));
        let (p2, r2) = (progress.clone(), result.clone());
        std::thread::spawn(move || {
            use dragon_engine::balance;
            use std::sync::atomic::Ordering::Relaxed;
            let mut pool = dragon_engine::render::Pool::with_plugins(Box::new(dragon_engine::plugins::Host));
            // At most 4 minutes are measured (enough for a balance; long songs stay quick).
            let profiles = dragon_engine::render::measure_strip_profiles(&project, &cache, rate, &mut pool, 240.0, &mut |x| p2.store((x * 650.0) as u32, Relaxed));
            let mut entries = balance::plan(&project, &profiles, &[]);
            let (mut offset, mut sum) = (0.0, (-70.0, -120.0));
            if !entries.is_empty() {
                let trial = balance::trial_project(&project, &entries);
                let mut pool = dragon_engine::render::Pool::with_plugins(Box::new(dragon_engine::plugins::Host));
                let sums = dragon_engine::render::measure_strip_profiles(&trial, &cache, rate, &mut pool, 240.0, &mut |x| p2.store(650 + (x * 350.0) as u32, Relaxed));
                if let Some((_, m)) = sums.iter().find(|(id, _)| *id == dragon_engine::model::MASTER) {
                    offset = balance::headroom_offset(m);
                    sum = (m.active + offset, m.peak_db + offset);
                }
            }
            balance::finish(&mut entries, offset);
            if let Ok(mut r) = r2.lock() {
                let base = entries.clone();
                *r = Some(BalanceReport { profiles, entries, offset, sum, open: true, base, style: Default::default(), keep: Vec::new(), hearing_before: false });
            }
            redhive_client_framework::runtime::request_repaint();
        });
        let carry = self.balance_report.take().map(|r| (r.style, r.keep)).unwrap_or_default();
        self.balance_job = Some(AutoBalanceJob { carry, hidden: false, progress, result });
    }

    /// Applies a finished auto-balance and opens its report.
    pub fn poll_auto_balance(&mut self) {
        let Some(job) = &self.balance_job else { return };
        let Some(mut report) = job.result.lock().ok().and_then(|mut r| r.take()) else { return };
        let (style, keep) = job.carry.clone();
        self.balance_job = None;
        if report.entries.is_empty() {
            self.toast("Auto-balance: no unmuted channel with audio to balance", Severity::Warning);
            return;
        }
        self.apply_balance(&report.entries, "Auto-balance", None);
        let n = report.entries.len();
        let short = report.entries.iter().filter(|e| e.short_db.abs() > 0.5).count();
        let extra = if short > 0 { format!(" – {short} need gain at the source, see the report") } else { String::new() };
        self.ui.status = format!("Auto-balance: {n} channels set – report in Mixer › Tools");
        self.toast(format!("Auto-balanced {n} channel{}{extra} – Ctrl+Z to undo", if n == 1 { "" } else { "s" }), Severity::Info);
        report.style = style;
        report.keep = keep;
        let carried = report.style != Default::default() || !report.keep.is_empty();
        self.balance_report = Some(report);
        if carried {
            self.rebalance("Auto-balance: style", None);
        }
    }

    fn apply_balance(&mut self, entries: &[dragon_engine::balance::Entry], label: &str, role: Option<(Id, Option<dragon_engine::balance::Role>)>) {
        let faders: Vec<(Id, f32)> = entries.iter().map(|e| (e.id, dragon_engine::dsp::db_to_gain(e.fader_db))).collect();
        self.session.edit(label, |p| {
            if let Some((id, r)) = role {
                if let Some(c) = p.mixer.get_mut(id) {
                    c.balance_role = r;
                }
            }
            for (id, v) in &faders {
                if let Some(c) = p.mixer.get_mut(*id) {
                    c.volume = *v;
                }
            }
        });
    }

    /// Re-plans the balance from the last measurement (no new render) with the report's style and kept
    /// channels, `role` changed on channel `id` first if given, and applies it as one undo step.
    fn rebalance(&mut self, label: &str, role: Option<(Id, Option<dragon_engine::balance::Role>)>) {
        use dragon_engine::balance;
        let Some(rep) = &self.balance_report else { return };
        let mut p = self.session.project.clone();
        if let Some((id, r)) = role {
            if let Some(c) = p.mixer.get_mut(id) {
                c.balance_role = r;
            }
        }
        // The faders from before the balance stay the reference for "before" and for kept channels.
        for e in &rep.base {
            if let Some(c) = p.mixer.get_mut(e.id) {
                c.volume = dragon_engine::dsp::db_to_gain(e.before_db);
            }
        }
        let mut entries = balance::plan_with(&p, &rep.profiles, &[], rep.style, &rep.keep);
        let offset = balance::carry_offset(&rep.base, rep.offset, &entries);
        balance::finish(&mut entries, offset);
        self.apply_balance(&entries, label, role);
        if let Some(r) = &mut self.balance_report {
            r.entries = entries;
            r.hearing_before = false;
        }
    }

    /// Changes what a channel is in the mix (`None` = from its names / sound again) and re-balances from
    /// the last measurement (no new render): the role is remembered on the channel.
    pub fn set_balance_role(&mut self, id: Id, role: Option<dragon_engine::balance::Role>) {
        self.rebalance("Auto-balance: role", Some((id, role)));
    }

    /// The overall character of the balance (vocal up front, beat driven, …).
    pub fn set_balance_style(&mut self, style: dragon_engine::balance::Style) {
        if let Some(r) = &mut self.balance_report {
            r.style = style;
        }
        self.rebalance("Auto-balance: style", None);
    }

    /// Keeps a channel's own fader (or lets auto-balance set it again).
    pub fn toggle_balance_keep(&mut self, id: Id) {
        if let Some(r) = &mut self.balance_report {
            if let Some(k) = r.keep.iter().position(|x| *x == id) {
                r.keep.remove(k);
            } else {
                r.keep.push(id);
            }
        }
        self.rebalance("Auto-balance: keep fader", None);
    }

    /// A/B: puts the old faders on (`true`) or the balanced ones back, to compare by ear. Not an undo
    /// step: the balance itself is one.
    pub fn hear_balance_before(&mut self, before: bool) {
        let Some(rep) = &mut self.balance_report else { return };
        rep.hearing_before = before;
        for e in &rep.entries {
            if let Some(c) = self.session.project.mixer.get_mut(e.id) {
                c.volume = dragon_engine::dsp::db_to_gain(if before { e.before_db } else { e.fader_db });
            }
        }
        self.session.history.touch();
    }

    /// Puts every fader back where it was before the balance (one undo step) and closes the report.
    pub fn revert_balance(&mut self) {
        let Some(rep) = self.balance_report.take() else { return };
        let back: Vec<(Id, f32)> = rep.base.iter().map(|e| (e.id, dragon_engine::dsp::db_to_gain(e.before_db))).collect();
        self.session.edit("Auto-balance undone", |p| {
            for (id, v) in &back {
                if let Some(c) = p.mixer.get_mut(*id) {
                    c.volume = *v;
                }
            }
        });
        self.toast("Faders are back where they were", Severity::Info);
    }

    /// Closes the report, keeping what is on (the balance, unless the old faders were being compared:
    /// then the balance goes back on).
    pub fn close_balance(&mut self) {
        if self.balance_report.as_ref().is_some_and(|r| r.hearing_before) {
            self.hear_balance_before(false);
        }
        if let Some(r) = &mut self.balance_report {
            r.open = false;
        }
    }

    pub fn save_settings(&self) {
        self.ctx.paths.save_json("settings.json", &self.settings);
    }

    pub fn toast(&mut self, text: impl Into<String>, s: Severity) {
        self.ctx.toast(text, s);
    }

    // ------------------------------------------------------------------------------------ transport

    pub fn transport(&self) -> dragon_engine::render::TransportState {
        self.audio.transport().state
    }

    pub fn beat_now(&self) -> f64 {
        self.audio.transport().beat_now()
    }

    pub fn play_mode(&self) -> PlayMode {
        if self.ui.song_mode() {
            PlayMode::Song
        } else {
            PlayMode::Pattern(self.ui.pattern)
        }
    }

    pub fn toggle_play(&mut self) {
        let t = self.transport();
        if self.audio.is_recording() {
            self.stop_recording();
        } else if t.playing {
            self.audio.send(Command::Pause);
        } else {
            // The engine cuts the browser preview when playback starts.
            self.ui.preview_started = None;
            self.audio.send(Command::Play(self.play_mode()));
        }
    }

    pub fn stop(&mut self) {
        if self.audio.is_recording() {
            self.stop_recording();
        }
        self.audio.send(Command::Stop);
        self.audio.send(Command::Live(LiveEvent::AllNotesOff));
    }

    pub fn seek(&mut self, beat: f64) {
        self.audio.send(Command::Seek(beat.max(0.0)));
    }

    pub fn live_note(&mut self, on: bool, note: u8, velocity: f32) {
        // Light the key on the piano-roll keyboard (whatever channel the note goes to).
        if let Some(v) = self.ui.sounding.get_mut(note as usize) {
            *v = if on { velocity.max(0.05) } else { 0.0 };
            self.ui.released[note as usize] = if on { None } else { Some(Instant::now()) };
            if on {
                self.ui.follow_note = Some(note);
            }
        }
        // "wait for input": the first note played starts the armed recording.
        if on && self.ui.waiting_record {
            self.ui.waiting_record = false;
            self.start_recording();
        }
        let Some(ch) = self.ui.channel.or(self.ui.piano_channel) else { return };
        self.audio.send(Command::Live(if on { LiveEvent::NoteOn { channel: ch, note, velocity } } else { LiveEvent::NoteOff { channel: ch, note } }));
        let t = self.audio.transport();
        let secs = t.state.seconds + if t.state.playing { t.at.elapsed().as_secs_f64() } else { 0.0 };
        self.retro.push(secs.max(0.0) + self.ui.record_started.map(|_| 0.0).unwrap_or(0.0), note, velocity, on);
    }

    // ------------------------------------------------------------------------------------ recording

    pub fn start_recording(&mut self) {
        let armed: Vec<(Id, InputSource, String)> = self.session.project.tracks.iter().filter(|t| t.armed).map(|t| (t.id, t.input.unwrap_or(InputSource { left: 0, right: Some(1) }), t.name.clone())).collect();
        if armed.is_empty() {
            // No armed audio track: record notes into the pattern while the song plays (live MIDI recording).
            self.ui.record_started = Some(Instant::now());
            self.retro = RetroBuffer::new(600.0);
            let count_in = self.session.project.metronome.count_in_bars;
            self.audio.send(Command::Record { count_in_bars: count_in });
            self.toast("Recording notes – play the typing keyboard or a MIDI controller", Severity::Info);
            return;
        }
        if !self.audio.can_record() {
            self.toast("No audio input device is open – choose one in Options › Audio", Severity::Warning);
            return;
        }
        // Audio is recorded into the arrangement: switch to song mode so the take lands in the playlist.
        if self.ui.pattern_mode {
            self.ui.pattern_mode = false;
        }
        let dir = self.recordings_dir();
        let _ = std::fs::create_dir_all(&dir);
        let stamp = chrono_like_stamp();
        let targets: Vec<RecordTarget> = armed.iter().map(|(id, src, name)| RecordTarget { track: *id, source: *src, path: dir.join(format!("{} {stamp}.wav", sanitize(name))) }).collect();
        let count_in = self.session.project.metronome.count_in_bars;
        self.ui.record_beat = Some(self.beat_now().max(0.0));
        self.audio.start_recording(targets, count_in);
        self.ui.record_started = Some(Instant::now());
    }

    pub fn stop_recording(&mut self) {
        self.ui.record_started = None;
        self.ui.record_beat = None;
        let takes = self.audio.stop_recording();
        if !takes.is_empty() {
            crate::views::quests::event(self, crate::views::quests::Event::Recorded);
        }
        if takes.is_empty() {
            // Note recording: capture what was played into the selected pattern.
            let bpm = self.session.project.bpm;
            let notes = self.retro.capture(bpm);
            if let (false, Some(ch)) = (notes.is_empty(), self.ui.piano_channel.or(self.ui.channel)) {
                let pat = self.ui.pattern;
                self.session.edit("Record notes", |p| {
                    if let Some(pt) = p.pattern_mut(pat) {
                        pt.channels.entry(ch).or_default().notes.extend(notes);
                    }
                });
                self.toast("Recorded notes into the pattern", Severity::Success);
            }
            return;
        }
        let n = takes.len();
        self.session.edit("Record audio", |p| {
            let tm = TempoMap::from_project(p);
            let punch = p.punch_enabled && p.punch.is_some_and(|r| r.len() > 0.01);
            for t in takes {
                let secs = t.frames as f64 / t.rate as f64 - t.latency;
                let s0 = tm.beat_to_seconds(t.start_beat);
                let len = tm.seconds_to_beat(s0 + secs.max(0.0)) - t.start_beat;
                let end = t.start_beat + len.max(0.01);
                let overlaps = p.track(t.track).map(|tr| tr.clips.iter().any(|c| c.end() > t.start_beat + 1e-6 && c.start < end - 1e-6)).unwrap_or(false);
                // Punch recording replaces (the old clips stay as a take to comp); otherwise recording over
                // existing audio never touches it: the new take goes onto a new track right below, grouped
                // with the armed one and on the same mixer channel.
                if punch && overlaps {
                    let k = p.track(t.track).map(|tr| tr.takes.len() + 1).unwrap_or(1);
                    dragon_engine::edit::push_take(p, t.track, &format!("Take {k}"));
                }
                let id = p.next_id();
                let name = t.path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                if let Some(tr) = p.track_mut(t.track) {
                    tr.audio = true;
                    tr.clips.push(Clip {
                        id,
                        name,
                        start: t.start_beat,
                        length: len.max(0.01),
                        muted: false,
                        color: tr.color,
                        kind: ClipKind::Audio(AudioClip { source: t.path, offset: t.latency, ..Default::default() }),
                    });
                }
                if !punch && overlaps {
                    dragon_engine::edit::clips_to_grouped_track(p, t.track, &[id]);
                }
            }
        });
        self.toast(format!("Recorded {n} take(s)"), Severity::Success);
    }

    pub fn recordings_dir(&self) -> PathBuf {
        match &self.session.path {
            Some(p) => p.parent().unwrap_or(Path::new(".")).join("Recordings"),
            None => self.ctx.paths.projects.join("Recordings"),
        }
    }

    // ------------------------------------------------------------------------------------ files

    /// File → New: a fully empty project.
    pub fn new_project(&mut self) {
        self.replace_session(Session::new(Project::empty(), None));
    }

    /// File → New with demo beat: drums, bass, lead, a reverb bus and a limiter on the master.
    pub fn new_demo_project(&mut self) {
        self.replace_session(Session::new(starter_project(&self.kit), None));
    }

    pub fn replace_session(&mut self, mut s: Session) {
        self.stop();
        s.history.limit = self.settings.undo_limit;
        self.ui = UiState::new(&s.project);
        self.session = s;
        self.session.force_recompile();
    }

    pub fn open_path(&mut self, path: &Path) {
        self.open_path_mode(path, None);
    }

    /// Opens a project, optionally in safe mode. While it opens (until it has run for a moment) a marker
    /// names it, so if it crashes the app the next start offers safe mode.
    pub fn open_path_mode(&mut self, path: &Path, safe: Option<crate::crash::SafeMode>) {
        use crate::crash::SafeMode;
        crate::crash::mark(&self.ctx.paths.local, "opening", path);
        match project_io::load(path) {
            Ok(p) => {
                let missing = project_io::missing_files(&p);
                let pool = match safe {
                    None => dragon_engine::render::Pool::with_plugins(Box::new(dragon_engine::plugins::Host)),
                    Some(SafeMode::NoPlugins) => dragon_engine::render::Pool::new(),
                    Some(SafeMode::NoPluginStates) => dragon_engine::render::Pool::with_plugins(Box::new(dragon_engine::plugins::HostWithoutStates)),
                };
                self.replace_session(Session::with_pool(p, Some(path.to_path_buf()), pool));
                self.safe_mode = safe;
                self.open_guard = Some(90);
                if let Some(m) = safe {
                    self.toast(format!("Safe mode: {} – save, fix or remove the plugin that crashed, then open the project normally", m.label()), Severity::Warning);
                }
                self.add_recent(path);
                if !missing.is_empty() {
                    self.toast(format!("{} audio file(s) are missing – see Project › Missing files", missing.len()), Severity::Warning);
                }
            }
            Err(e) => {
                crate::crash::clear(&self.ctx.paths.local, "opening");
                self.toast(format!("Cannot open {}: {e}", path.display()), Severity::Error)
            }
        }
    }

    pub fn open_dialog(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("Dragon project", &["dragon"]).set_directory(&self.ctx.paths.projects).pick_file() {
            self.open_path(&p);
        }
    }

    /// the "Save new version": saves a copy next to the project as `<Name>_2`, `<Name>_3`, … and
    /// continues in it.
    pub fn save_new_version(&mut self) -> bool {
        let Some(cur) = self.session.path.clone() else { return self.save(true) };
        let dir = cur.parent().map(|d| d.to_path_buf()).unwrap_or_default();
        let stem = cur.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Untitled".into());
        let (base, mut n) = match stem.rsplit_once('_') {
            Some((b, k)) if k.parse::<u32>().is_ok() => (b.to_string(), k.parse::<u32>().unwrap() + 1),
            _ => (stem.clone(), 2),
        };
        let mut path = dir.join(format!("{base}_{n}.dragon"));
        while path.exists() {
            n += 1;
            path = dir.join(format!("{base}_{n}.dragon"));
        }
        self.session.end_gesture();
        self.capture_plugin_states();
        self.save_to(&path)
    }

    pub fn save(&mut self, save_as: bool) -> bool {
        let path = match (&self.session.path, save_as) {
            (Some(p), false) => p.clone(),
            _ => {
                let name = format!("{}.dragon", if self.session.project.meta.name.is_empty() { "Untitled" } else { &self.session.project.meta.name });
                match rfd::FileDialog::new().add_filter("Dragon project", &["dragon"]).set_directory(&self.ctx.paths.projects).set_file_name(name).save_file() {
                    // Every project gets its own folder: <chosen dir>/<Name>/<Name>.dragon.
                    Some(p) => project_io::project_folder_path(&p.with_extension("dragon")),
                    None => return false,
                }
            }
        };
        self.session.end_gesture();
        self.capture_plugin_states();
        self.save_to(&path)
    }

    /// Saves into the project folder of `path` (creating `Samples`, `Recordings`, `Renders`, `Backup` and
    /// copying outside files into `Samples`). The copies replace the originals in the project without an
    /// undo step: saving is not an edit.
    pub fn save_to(&mut self, path: &Path) -> bool {
        let mut p = self.session.project.clone();
        match project_io::save_in_folder(&mut p, path, 5) {
            Ok(copied) => {
                let path = path.to_path_buf();
                self.session.project = p;
                self.session.history.touch();
                self.session.set_path(path.clone());
                self.session.history.mark_saved();
                self.add_recent(&path);
                self.ui.status = if copied > 0 { format!("Saved {} ({copied} file(s) copied into Samples)", path.display()) } else { format!("Saved {}", path.display()) };
                crate::views::quests::event(self, crate::views::quests::Event::Saved);
                true
            }
            Err(e) => {
                self.toast(format!("Save failed: {e}"), Severity::Error);
                false
            }
        }
    }

    /// (Re)opens MIDI inputs (the configured ones plus the control surface) and the control-surface output.
    pub fn open_midi(&mut self) {
        let mut names = self.settings.device.midi_inputs.clone();
        if let (false, Some(m)) = (names.is_empty(), &self.settings.mcu_input) {
            if !names.contains(m) {
                names.push(m.clone());
            }
        }
        self.midi = None;
        self.midi = Some(MidiInputs::open(&names, Some(self.audio.shared_transport()), Some(Arc::new(redhive_client_framework::runtime::request_repaint))));
        self.mcu.out = None;
        self.mcu.sent_faders = [-1.0; 9];
        self.mcu.sent_leds.clear();
        if let Some(name) = self.settings.mcu_output.clone() {
            match dragon_engine::device::MidiOutput::open(&name) {
                Ok(o) => self.mcu.out = Some(o),
                Err(e) => self.ctx.log.warn("mcu", e.to_string()),
            }
        }
    }

    fn mcu_track(&self, strip: u8) -> Option<Id> {
        self.session.project.tracks.get(self.mcu.bank + strip as usize).map(|t| t.id)
    }

    fn mcu_mixer(&self, strip: u8) -> Option<Id> {
        if strip == 8 {
            Some(self.session.project.mixer.master.id)
        } else {
            self.mcu_track(strip).and_then(|t| self.session.project.track(t)).map(|t| t.mixer)
        }
    }

    /// Applies one control-surface action; returns `true` when it was a continuous gesture.
    pub fn apply_mcu(&mut self, a: dragon_engine::midi::McuAction) -> bool {
        use dragon_engine::midi::McuAction as A;
        let n_tracks = self.session.project.tracks.len();
        match a {
            A::Fader { strip, value } => {
                if let Some(m) = self.mcu_mixer(strip) {
                    let g = dragon_engine::midi::mcu_fader_to_gain(value);
                    self.session.gesture("Fader", |p| p.mixer.get_mut(m).iter_mut().for_each(|c| c.volume = g));
                    self.mcu.sent_faders[strip as usize] = value;
                    return true;
                }
            }
            A::Pan { strip, delta } => {
                if let Some(m) = self.mcu_mixer(strip) {
                    self.session.gesture("Pan", |p| p.mixer.get_mut(m).iter_mut().for_each(|c| c.pan = (c.pan + delta as f32 * 0.02).clamp(-1.0, 1.0)));
                    return true;
                }
            }
            A::Mute(s) | A::Solo(s) | A::Arm(s) => {
                if let Some(t) = self.mcu_track(s) {
                    self.session.edit("Control surface", |p| {
                        if let Some(tr) = p.track_mut(t) {
                            match a {
                                A::Mute(_) => tr.muted = !tr.muted,
                                A::Solo(_) => tr.solo = !tr.solo,
                                _ => tr.armed = !tr.armed,
                            }
                        }
                    });
                }
            }
            A::Select(s) => {
                if let Some(m) = self.mcu_mixer(s) {
                    self.ui.mixer = m;
                }
            }
            A::Play => self.toggle_play(),
            A::Stop => self.stop(),
            A::Record => self.ctx.run("dragon.transport.record", serde_json::Value::Null),
            A::Rewind => {
                let b = self.beat_now();
                self.seek((b - 4.0).max(0.0));
            }
            A::Forward => {
                let b = self.beat_now();
                self.seek(b + 4.0);
            }
            A::Loop => self.ctx.run("dragon.transport.loop", serde_json::Value::Null),
            A::Click => self.ctx.run("dragon.transport.metronome", serde_json::Value::Null),
            A::BankLeft => self.mcu.bank = self.mcu.bank.saturating_sub(8),
            A::BankRight => self.mcu.bank = (self.mcu.bank + 8).min(n_tracks.saturating_sub(1) / 8 * 8),
            A::ChannelLeft => self.mcu.bank = self.mcu.bank.saturating_sub(1),
            A::ChannelRight => self.mcu.bank = (self.mcu.bank + 1).min(n_tracks.saturating_sub(1)),
        }
        false
    }

    /// Sends motor-fader positions and button LEDs that changed to the control surface.
    pub fn mcu_feedback(&mut self) {
        if self.mcu.out.is_none() {
            return;
        }
        let playing = self.transport().playing;
        let recording = self.audio.is_recording() || self.ui.record_started.is_some();
        let mut msgs: Vec<[u8; 3]> = Vec::new();
        for strip in 0..9u8 {
            let vol = self.mcu_mixer(strip).and_then(|m| self.session.project.mixer.get(m)).map(|c| c.volume).unwrap_or(0.0);
            let f = dragon_engine::midi::mcu_gain_to_fader(vol);
            if (f - self.mcu.sent_faders[strip as usize]).abs() > 0.002 {
                self.mcu.sent_faders[strip as usize] = f;
                msgs.push(dragon_engine::midi::mcu_fader_message(strip, f));
            }
        }
        let mut leds: Vec<(u8, bool)> = vec![(0x5E, playing), (0x5F, recording), (0x56, self.session.project.loop_enabled), (0x59, self.session.project.metronome.enabled)];
        for strip in 0..8u8 {
            let t = self.mcu_track(strip).and_then(|t| self.session.project.track(t));
            leds.push((strip, t.map(|t| t.armed).unwrap_or(false)));
            leds.push((0x08 + strip, t.map(|t| t.solo).unwrap_or(false)));
            leds.push((0x10 + strip, t.map(|t| t.muted).unwrap_or(false)));
            leds.push((0x18 + strip, t.map(|t| t.mixer == self.ui.mixer).unwrap_or(false)));
        }
        for (note, on) in leds {
            if self.mcu.sent_leds.get(&note) != Some(&on) {
                self.mcu.sent_leds.insert(note, on);
                msgs.push(dragon_engine::midi::mcu_led_message(note, on));
            }
        }
        if let Some(out) = &mut self.mcu.out {
            for m in msgs {
                out.send(&m);
            }
        }
    }

    /// Copies every loaded plugin's internal state into the project (so it is saved with it).
    pub fn capture_plugin_states(&mut self) {
        let mut states: Vec<(Id, Option<Id>, Vec<u8>)> = Vec::new();
        for m in self.session.project.mixer.all() {
            for i in m.inserts.iter().filter(|i| i.plugin.is_some()) {
                if let Some(s) = self.session.pool.effect_by_id(i.id).and_then(|e| e.lock().ok().and_then(|mut g| g.save_state())) {
                    states.push((m.id, Some(i.id), s));
                }
            }
        }
        for c in self.session.project.channels.iter().filter(|c| matches!(c.instrument, Instrument::Plugin(_))) {
            if let Some(s) = self.session.pool.instrument_by_id(c.id).and_then(|e| e.lock().ok().and_then(|mut g| g.save_state())) {
                states.push((c.id, None, s));
            }
        }
        if states.is_empty() {
            return;
        }
        // Not an undo step: state capture does not change what the user edited.
        let p = &mut self.session.project;
        for (owner, ins, data) in states {
            match ins {
                Some(i) => {
                    if let Some(r) = p.mixer.get_mut(owner).and_then(|m| m.inserts.iter_mut().find(|x| x.id == i)).and_then(|x| x.plugin.as_mut()) {
                        r.state = data;
                    }
                }
                None => {
                    if let Some(Instrument::Plugin(r)) = p.channel_mut(owner).map(|c| &mut c.instrument) {
                        r.state = data;
                    }
                }
            }
        }
    }

    pub fn add_recent(&mut self, p: &Path) {
        self.settings.recent.retain(|x| x != p);
        self.settings.recent.insert(0, p.to_path_buf());
        self.settings.recent.truncate(12);
        self.save_settings();
    }

    pub fn autosave_tick(&mut self) {
        let mins = self.settings.autosave_minutes;
        if mins == 0 || !self.session.history.is_dirty() || self.session.in_gesture() {
            return;
        }
        if self.last_autosave.elapsed().as_secs() >= mins as u64 * 60 {
            self.last_autosave = Instant::now();
            let name = self.session.path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Untitled".into());
            match self.autosave.save(&self.session.project, &name) {
                Ok(_) => self.ui.status = "Autosaved".into(),
                Err(e) => self.ctx.log.warn("autosave", e.to_string()),
            }
        }
    }

    // ------------------------------------------------------------------------------------ import

    /// Puts an audio file on a track at `beat` (the first empty track when `track` is `None`).
    /// Adds an audio clip at `beat` on `track`, or on a new track named after the file when `track` is
    /// `None` (a file dropped below the last track).
    pub fn add_audio_clip_at(&mut self, path: &Path, track: Option<Id>, beat: f64) {
        match track {
            Some(t) => self.add_audio_clip(path, Some(t), beat),
            None => {
                let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Audio".into());
                let tid = self.session.edit("Add track", |p| p.add_track(&name));
                self.add_audio_clip(path, Some(tid), beat);
            }
        }
    }

    pub fn add_audio_clip(&mut self, path: &Path, track: Option<Id>, beat: f64) {
        let data = match self.session.cache.get(path, self.audio.rate()) {
            Ok(d) => d,
            Err(e) => {
                self.toast(format!("Cannot read {}: {e}", path.display()), Severity::Error);
                return;
            }
        };
        let secs = data.duration();
        let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let src = path.to_path_buf();
        let sel = self.session.edit("Add audio clip", |p| {
            let tid = track.or_else(|| p.tracks.iter().find(|t| t.clips.is_empty()).map(|t| t.id)).unwrap_or_else(|| p.add_track(&name));
            let tm = TempoMap::from_project(p);
            let s0 = tm.beat_to_seconds(beat);
            let len = tm.seconds_to_beat(s0 + secs) - beat;
            let id = p.next_id();
            if let Some(t) = p.track_mut(tid) {
                t.clips.push(Clip { id, name, start: beat, length: len.max(0.01), muted: false, color: t.color, kind: ClipKind::Audio(AudioClip { source: src, ..Default::default() }) });
            }
            id
        });
        self.ui.clips = vec![sel];
    }

    /// Loads a sample into a new sampler channel of the rack.
    pub fn add_sampler(&mut self, path: &Path) {
        let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let src = path.to_path_buf();
        let id = self.session.edit("Add sampler", |p| p.add_channel(&name, Instrument::Sampler(SamplerParams { path: src, ..Default::default() })));
        self.ui.channel = Some(id);
    }

    /// Replaces a rack channel's sound with an audio file (drop on a channel).
    pub fn replace_sample(&mut self, channel: Id, path: &Path) {
        let path = path.to_path_buf();
        if self.session.edit("Replace sound", |p| dragon_engine::edit::replace_channel_sample(p, channel, &path)) {
            self.ui.channel = Some(channel);
            let name = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            self.ui.status = format!("Replaced the sound with {name}");
        }
    }

    /// Starts separating the selected audio clip into drums / harmonic / centre stems (background).
    pub fn separate_selected(&mut self) {
        let clip = self.session.project.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| self.ui.clips.contains(&c.id) && c.audio().is_some()).cloned();
        let Some(clip) = clip else {
            self.toast("Select an audio clip first", Severity::Info);
            return;
        };
        if self.ui.stem_job.is_some() {
            return;
        }
        let a = clip.audio().cloned().unwrap();
        let cache = self.session.cache.clone();
        let rate = self.audio.rate();
        let dir = self.recordings_dir();
        let name = clip.name.clone();
        let start = clip.start;
        self.ui.status = format!("Separating {name}…");
        self.ui.stem_job = Some(redhive_client_framework::runtime::Promise::spawn(async move {
            tokio::task::spawn_blocking(move || {
                let d = cache.get(&a.source, rate).map_err(|e| e.to_string())?;
                let off = ((a.offset * d.rate as f64) as usize).min(d.frames());
                let part = dragon_engine::samples::AudioData { rate: d.rate, l: d.l[off..].to_vec(), r: d.r[off..].to_vec(), ..Default::default() };
                let s = dragon_engine::analysis::separate_stems(&part);
                let mut files = Vec::new();
                for (label, data) in [("Drums", &s.drums), ("Harmonic", &s.harmonic), ("Centre", &s.center)] {
                    let f = dir.join(format!("{} – {label}.wav", sanitize(&name)));
                    dragon_engine::samples::write_wav(&f, &data.l, &data.r, data.rate, dragon_engine::samples::BitDepth::Float32, false, false).map_err(|e| e.to_string())?;
                    files.push((label.to_string(), f));
                }
                Ok((start, name, files))
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        }));
    }

    pub fn poll_stem_job(&mut self) {
        let Some(r) = redhive_client_framework::runtime::poll(&mut self.ui.stem_job) else { return };
        match r {
            Ok((start, name, files)) => {
                for (label, f) in files {
                    let t = self.session.edit("Add stem track", |p| p.add_track(&format!("{name} {label}")));
                    self.add_audio_clip(&f, Some(t), start);
                }
                let ids = self.ui.clips.clone();
                self.session.edit("Mute source of stems", |p| {
                    for t in p.tracks.iter_mut() {
                        for c in t.clips.iter_mut().filter(|c| ids.contains(&c.id)) {
                            c.muted = true;
                        }
                    }
                });
                self.toast("Separated into drums, harmonic and centre stems (source clip muted)", Severity::Success);
            }
            Err(e) => self.toast(format!("Stem separation failed: {e}"), Severity::Error),
        }
        self.ui.status.clear();
    }

    /// Puts a video on the timeline at beat 0 and its soundtrack on a new audio track.
    pub fn import_video(&mut self, path: &Path) {
        // Without ffmpeg, say so and show where to get it (instead of a "not found" error).
        if !self.ffmpeg_ready() {
            self.toast("Video needs ffmpeg – the Video view can get it for you", Severity::Warning);
            self.ctx.open("dragon.video", None);
            return;
        }
        let v = match dragon_engine::video::probe(path) {
            Ok(v) => v,
            Err(e) => {
                self.toast(format!("Cannot open video: {e}"), Severity::Error);
                return;
            }
        };
        let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let wav = self.recordings_dir().join(format!("{} (video audio).wav", sanitize(&stem)));
        let has_audio = match dragon_engine::video::extract_audio(path, &wav) {
            Ok(b) => b,
            Err(e) => {
                self.toast(format!("Video audio: {e}"), Severity::Warning);
                false
            }
        };
        self.session.edit("Import video", |p| p.video = Some(v));
        if has_audio {
            let t = self.session.edit("Video audio track", |p| p.add_track(&format!("{stem} (audio)")));
            self.add_audio_clip(&wav, Some(t), 0.0);
        }
        self.ctx.open("dragon.video", None);
    }

    pub fn import_midi(&mut self, path: &Path) {
        match dragon_engine::midi::import(path) {
            Ok(m) => {
                let n = m.tracks.len();
                let pat = self.session.edit("Import MIDI", |p| {
                    let pat = p.add_pattern();
                    if let Some(b) = m.bpm {
                        p.bpm = b;
                    }
                    for (name, notes) in m.tracks {
                        let ch = p.add_channel(if name.is_empty() { "MIDI" } else { &name }, Instrument::Synth(SynthParams::default()));
                        if let Some(pt) = p.pattern_mut(pat) {
                            pt.channels.entry(ch).or_default().notes = notes;
                        }
                    }
                    pat
                });
                self.ui.pattern = pat;
                self.toast(format!("Imported {n} MIDI track(s) into a new pattern"), Severity::Success);
            }
            Err(e) => self.toast(format!("MIDI import failed: {e}"), Severity::Error),
        }
    }

    pub fn preview(&mut self, path: &Path) {
        self.ui.browser_sel = Some(path.to_path_buf());
        match self.session.cache.get(path, self.audio.rate()) {
            Ok(d) => {
                self.audio.send(Command::Live(LiveEvent::Preview(d)));
                self.ui.preview_started = Some(Instant::now());
                self.ui.preview_offset = 0.0;
            }
            Err(e) => self.ui.status = format!("Cannot preview: {e}"),
        }
    }

    /// Plays `path` from `seconds` on (the clip window's preview; `preview_offset` keeps where it started).
    pub fn preview_from(&mut self, path: &Path, seconds: f64) {
        match self.session.cache.get(path, self.audio.rate()) {
            Ok(d) => {
                let at = (seconds.max(0.0) * d.rate as f64) as usize;
                self.audio.send(Command::Live(LiveEvent::PreviewAt(d, at)));
                self.ui.browser_sel = Some(path.to_path_buf());
                self.ui.preview_started = Some(Instant::now());
                self.ui.preview_offset = seconds.max(0.0);
            }
            Err(e) => self.ui.status = format!("Cannot preview: {e}"),
        }
    }

    pub fn stop_preview(&mut self) {
        self.audio.send(Command::Live(LiveEvent::StopPreview));
        self.ui.preview_started = None;
    }

    /// Applies an incoming MIDI message: notes to the selected channel, CC to mappings / MIDI learn.
    pub fn handle_midi(&mut self) {
        let Some(m) = &self.midi else { return };
        let msgs: Vec<_> = m.rx.try_iter().map(|t| (t, m.all_ports.get(t.port).cloned())).collect();
        let mut mapped = false;
        for (t, port) in msgs {
            use dragon_engine::midi::MidiMessage as M;
            if port.is_some() && port == self.settings.mcu_input {
                if let Some(a) = dragon_engine::midi::mcu(t.message) {
                    mapped |= self.apply_mcu(a);
                }
                continue;
            }
            match t.message {
                M::NoteOn { note, velocity, .. } if velocity > 0 => self.live_note(true, note, velocity as f32 / 127.0),
                M::NoteOn { note, .. } | M::NoteOff { note, .. } => self.live_note(false, note, 0.0),
                // Pedals go straight to the instrument being played (unless MIDI learn or a mapping takes them).
                M::Control { controller: c @ (64 | 66 | 67), value, channel } if self.ui.midi_learn.is_none() && !self.session.project.midi_mappings.iter().any(|x| x.channel == channel && x.controller == c) => {
                    if let Some(ch) = self.ui.channel.or(self.ui.piano_channel) {
                        let param = match c {
                            64 => "__sustain",
                            66 => "__sostenuto",
                            _ => "__soft",
                        };
                        self.audio.send(Command::Live(LiveEvent::Param { channel: ch, param, value: value as f32 / 127.0 }));
                    }
                }
                M::Control { channel, controller, value } => {
                    if let Some(target) = self.ui.midi_learn.take() {
                        let (min, max) = dragon_engine::midi::target_range(&target);
                        self.session.edit("MIDI learn", |p| {
                            p.midi_mappings.retain(|x| !(x.channel == channel && x.controller == controller));
                            p.midi_mappings.push(MidiMapping { channel, controller, target, min, max });
                        });
                        self.toast(format!("Mapped CC {controller}"), Severity::Success);
                        continue;
                    }
                    let maps: Vec<MidiMapping> = self.session.project.midi_mappings.iter().filter(|x| x.channel == channel && x.controller == controller).cloned().collect();
                    for mp in maps {
                        mapped = true;
                        let v = dragon_engine::midi::map_cc(value, mp.min, mp.max);
                        self.session.gesture("MIDI control", |p| set_target(p, &mp.target, v));
                    }
                }
                M::Start | M::Continue => self.audio.send(Command::Play(self.play_mode())),
                M::Stop => self.audio.send(Command::Pause),
                _ => {}
            }
        }
        if mapped {
            self.session.end_gesture();
        }
    }
}

impl State {
    /// Whether ffmpeg / ffprobe can be run (checked once, again after an install).
    pub fn ffmpeg_ready(&mut self) -> bool {
        if self.ui.ffmpeg_ok.is_none() {
            let r = dragon_engine::video::check();
            self.ui.ffmpeg_why = r.as_ref().err().cloned();
            self.ui.ffmpeg_ok = Some(r.is_ok());
        }
        self.ui.ffmpeg_ok == Some(true)
    }

    /// Looks for ffmpeg again (after installing it).
    pub fn recheck_ffmpeg(&mut self) {
        dragon_engine::video::recheck();
        self.ui.ffmpeg_ok = None;
    }

    /// Downloads ffmpeg in the background (Windows, Linux); `poll_ffmpeg_install` finishes it.
    pub fn install_ffmpeg(&mut self) {
        if self.ui.ffmpeg_job.is_some() {
            return;
        }
        let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
        let out = slot.clone();
        std::thread::spawn(move || {
            let r = dragon_engine::video::install_ffmpeg().map_err(|e| e.to_string());
            if let Ok(mut g) = out.lock() {
                *g = Some(r);
            }
            redhive_client_framework::runtime::request_repaint();
        });
        self.ui.ffmpeg_job = Some(slot);
    }

    pub fn poll_ffmpeg_install(&mut self) {
        let Some(job) = &self.ui.ffmpeg_job else { return };
        let Some(r) = job.lock().ok().and_then(|mut g| g.take()) else { return };
        self.ui.ffmpeg_job = None;
        self.recheck_ffmpeg();
        match r {
            Ok(_) if self.ffmpeg_ready() => self.toast("ffmpeg is ready – you can import video now", Severity::Success),
            Ok(dir) => self.toast(format!("ffmpeg was downloaded to {} but does not run: {}", dir.display(), self.ui.ffmpeg_why.clone().unwrap_or_default()), Severity::Error),
            Err(e) => self.toast(format!("Could not get ffmpeg: {e}"), Severity::Error),
        }
    }

    /// Shows `name: value` of `source` in the hint at the top left (any control, stock or plugin).
    pub fn hint(&mut self, source: &str, name: &str, value: String) {
        self.ui.hint = Some((source.to_string(), name.to_string(), value, Instant::now()));
    }

    /// Orderly teardown on close, while the main window still exists: stop playback, close every plugin
    /// window (they are owned by the main window; Windows would destroy them under the plugins), then
    /// stop the audio device and join the engine threads, so nothing runs plugin or driver code while
    /// the process exits.
    pub fn shutdown(&mut self) {
        self.stop();
        self.session.pool.close_all_editors();
        self.ui.native_editors.clear();
        self.audio.shutdown();
    }

    /// Services open native plugin editors (window events, plugin timers); drops the closed ones.
    pub fn pump_plugin_editors(&mut self) -> bool {
        let pool = &self.session.pool;
        // Parameter edits made in the plugins' own windows, to store in the project.
        let mut edits: Vec<(EditorKey, Vec<(u32, f64)>)> = Vec::new();
        // The last parameter turned in a plugin's own window (for the hint).
        let mut touched: Option<(EditorKey, String, f64)> = None;
        let last = &mut None::<(String, f64)>;
        self.ui.native_editors.retain(|key @ (owner, ins)| {
            let (open, e) = match ins {
                // try_lock: while the audio thread is processing the plugin, its GUI idles next frame
                // (the UI never makes the audio thread wait).
                // Busy (locked) counts as still open; a processor that is gone closes its editor.
                Some(i) => pool
                    .effect_by_id(*i)
                    .map(|e| {
                        e.try_lock()
                            .map(|mut g| {
                                let ed = g.take_param_edits();
                                if let Some((id, v)) = ed.last() {
                                    *last = g.plugin_params().into_iter().find(|p| p.id == *id).map(|p| (p.name, *v));
                                }
                                (g.idle(), ed)
                            })
                            .unwrap_or((true, Vec::new()))
                    })
                    .unwrap_or((false, Vec::new())),
                None => pool
                    .instrument_by_id(*owner)
                    .map(|e| {
                        e.try_lock()
                            .map(|mut g| {
                                let ed = g.take_param_edits();
                                if let Some((id, v)) = ed.last() {
                                    *last = g.plugin_params().into_iter().find(|p| p.id == *id).map(|p| (p.name, *v));
                                }
                                (g.idle(), ed)
                            })
                            .unwrap_or((true, Vec::new()))
                    })
                    .unwrap_or((false, Vec::new())),
            };
            if !e.is_empty() {
                edits.push((*key, e));
                if let Some((n, v)) = last.take() {
                    touched = Some((*key, n, v));
                }
            }
            open
        });
        if let Some(((owner, ins), name, v)) = touched {
            let source = match ins {
                Some(i) => self.session.project.mixer.get(owner).and_then(|c| c.inserts.iter().find(|x| x.id == i)).and_then(|x| x.plugin.as_ref()).map(|p| p.name.clone()),
                None => self.session.project.channel(owner).map(|c| c.name.clone()),
            }
            .unwrap_or_default();
            self.hint(&source, &name, format!("{:.0} %", v * 100.0));
        }
        if edits.is_empty() {
            if self.ui.plugin_edit_run.is_some_and(|(t, _)| t.elapsed() > std::time::Duration::from_millis(400)) {
                self.ui.plugin_edit_run = None;
            }
            return !self.ui.native_editors.is_empty();
        }
        let apply = |p: &mut Project| {
            for ((owner, ins), list) in &edits {
                let r = match ins {
                    Some(i) => p.mixer.get_mut(*owner).and_then(|c| c.inserts.iter_mut().find(|x| x.id == *i)).and_then(|x| x.plugin.as_mut()),
                    None => match p.channel_mut(*owner).map(|c| &mut c.instrument) {
                        Some(Instrument::Plugin(r)) => Some(r),
                        _ => None,
                    },
                };
                if let Some(r) = r {
                    for (id, v) in list {
                        r.params.insert(*id, *v);
                    }
                }
            }
        };
        // Continue the current run (no new undo step) unless it paused or something else edited meanwhile.
        let continuing = self.ui.plugin_edit_run.is_some_and(|(t, rev)| t.elapsed() <= std::time::Duration::from_millis(400) && rev == self.session.history.revision);
        if continuing {
            apply(&mut self.session.project);
            self.session.history.touch();
        } else {
            self.session.edit("Plugin parameter", apply);
        }
        self.ui.plugin_edit_run = Some((std::time::Instant::now(), self.session.history.revision));
        !self.ui.native_editors.is_empty()
    }
}

/// An open plugin editor: owner (mixer channel or rack channel) and, for effects, the insert.
type EditorKey = (Id, Option<Id>);

/// Result of a stem-separation job: source clip start, clip name and the stem files.
pub type StemResult = Result<(f64, String, Vec<(String, PathBuf)>), String>;

/// Frames per waveform overview bucket.
pub const WAVE_BUCKET: usize = 256;

impl State {
    /// Waveform overview of a file (min/max per [`WAVE_BUCKET`] frames, mono), cached.
    pub fn waveform(&mut self, path: &Path) -> Option<std::sync::Arc<Vec<(f32, f32)>>> {
        if let Some(w) = self.ui.waveforms.get(path) {
            return Some(w.clone());
        }
        let d = self.session.cache.get(path, self.audio.rate()).ok()?;
        let n = d.frames().div_ceil(WAVE_BUCKET);
        let mut v = Vec::with_capacity(n);
        for b in 0..n {
            let (s, e) = (b * WAVE_BUCKET, ((b + 1) * WAVE_BUCKET).min(d.frames()));
            let (mut lo, mut hi) = (0.0f32, 0.0f32);
            for i in s..e {
                let x = (d.l[i] + d.r[i]) * 0.5;
                lo = lo.min(x);
                hi = hi.max(x);
            }
            v.push((lo, hi));
        }
        let w = std::sync::Arc::new(v);
        self.ui.waveforms.insert(path.to_path_buf(), w.clone());
        Some(w)
    }
}

impl UiState {
    pub fn song_mode(&self) -> bool {
        !self.pattern_mode
    }
}

/// Writes a value to whatever an automation / MIDI target points at.
pub fn set_target(p: &mut Project, t: &Target, v: f32) {
    match t {
        Target::MixerVolume(id) => {
            if let Some(m) = p.mixer.get_mut(*id) {
                m.volume = v;
            }
        }
        Target::MixerPan(id) => {
            if let Some(m) = p.mixer.get_mut(*id) {
                m.pan = v;
            }
        }
        Target::MixerWidth(id) => {
            if let Some(m) = p.mixer.get_mut(*id) {
                m.width = v;
            }
        }
        Target::MixerMute(id) => {
            if let Some(m) = p.mixer.get_mut(*id) {
                m.muted = v > 0.5;
            }
        }
        Target::SendLevel { channel, target } => {
            if let Some(s) = p.mixer.get_mut(*channel).and_then(|m| m.sends.iter_mut().find(|s| s.target == *target)) {
                s.level = v;
            }
        }
        Target::InsertParam { channel, insert, param } => {
            if let Some(i) = p.mixer.get_mut(*channel).and_then(|m| m.inserts.iter_mut().find(|i| i.id == *insert)) {
                i.params.insert(param.clone(), v);
            }
        }
        Target::InsertMix { channel, insert } => {
            if let Some(i) = p.mixer.get_mut(*channel).and_then(|m| m.inserts.iter_mut().find(|i| i.id == *insert)) {
                i.mix = v;
            }
        }
        Target::ChannelVolume(id) => {
            if let Some(c) = p.channel_mut(*id) {
                c.volume = v;
            }
        }
        Target::ChannelPan(id) => {
            if let Some(c) = p.channel_mut(*id) {
                c.pan = v;
            }
        }
        Target::ChannelPitch(id) => {
            if let Some(c) = p.channel_mut(*id) {
                c.pitch = v;
            }
        }
        Target::SynthCutoff(id) => match p.channel_mut(*id).map(|c| &mut c.instrument) {
            Some(Instrument::Synth(s)) => s.cutoff = v,
            Some(Instrument::Wavetable(w)) => w.filters[0].cutoff = v,
            _ => {}
        },
        Target::SynthResonance(id) => match p.channel_mut(*id).map(|c| &mut c.instrument) {
            Some(Instrument::Synth(s)) => s.resonance = v,
            Some(Instrument::Wavetable(w)) => w.filters[0].resonance = v,
            _ => {}
        },
        Target::InstrumentParam { channel, param } if matches!(p.channel(*channel).map(|c| &c.instrument), Some(Instrument::Piano(_))) => {
            if let Some(Instrument::Piano(g)) = p.channel_mut(*channel).map(|c| &mut c.instrument) {
                match param.as_str() {
                    "tone" => g.tone = v,
                    "body" => g.body = v,
                    "high_damp" => g.high_damp = v,
                    "resonance" => g.resonance = v,
                    "master" => g.master = v,
                    _ => {}
                }
            }
        }
        Target::InstrumentParam { channel, param } if matches!(p.channel(*channel).map(|c| &c.instrument), Some(Instrument::Guitar(_))) => {
            if let Some(Instrument::Guitar(g)) = p.channel_mut(*channel).map(|c| &mut c.instrument) {
                match param.as_str() {
                    "brightness" => g.brightness = v,
                    "body" => g.body = v,
                    "drive" => g.drive = v,
                    "tone" => g.tone = v,
                    "strum" => g.strum_time = v,
                    "master" => g.master = v,
                    _ => {}
                }
            }
        }
        Target::InstrumentParam { channel, param } => {
            if let Some(Instrument::Wavetable(w)) = p.channel_mut(*channel).map(|c| &mut c.instrument) {
                let n = |pre: &str| param.strip_prefix(pre).and_then(|d| d.parse::<usize>().ok()).filter(|d| *d >= 1).map(|d| d - 1);
                if param == "master" {
                    w.master = v;
                } else if let Some(k) = n("macro").filter(|k| *k < 4) {
                    w.macros[k] = v;
                } else if let Some(k) = n("pos").filter(|k| *k < 3) {
                    w.osc[k].pos = v;
                } else if let Some(k) = n("cutoff").filter(|k| *k < 2) {
                    w.filters[k].cutoff = v;
                }
            }
        }
        Target::PluginParam { channel, insert, param } => {
            let r = match insert {
                Some(i) => p.mixer.get_mut(*channel).and_then(|m| m.inserts.iter_mut().find(|x| x.id == *i)).and_then(|x| x.plugin.as_mut()),
                None => match p.channel_mut(*channel).map(|c| &mut c.instrument) {
                    Some(Instrument::Plugin(r)) => Some(r),
                    _ => None,
                },
            };
            if let Some(r) = r {
                r.params.insert(*param, v as f64);
            }
        }
        Target::Tempo => p.bpm = v as f64,
    }
}

pub fn target_name(p: &Project, t: &Target) -> String {
    let mx = |id: &Id| p.mixer.get(*id).map(|m| m.name.clone()).unwrap_or_else(|| "?".into());
    let ch = |id: &Id| p.channel(*id).map(|c| c.name.clone()).unwrap_or_else(|| "?".into());
    match t {
        Target::MixerVolume(id) => format!("{} volume", mx(id)),
        Target::MixerPan(id) => format!("{} pan", mx(id)),
        Target::MixerWidth(id) => format!("{} width", mx(id)),
        Target::MixerMute(id) => format!("{} mute", mx(id)),
        Target::SendLevel { channel, target } => format!("{} ➡ {} send", mx(channel), mx(target)),
        Target::InsertParam { channel, param, .. } => format!("{} {param}", mx(channel)),
        Target::InsertMix { channel, .. } => format!("{} effect mix", mx(channel)),
        Target::ChannelVolume(id) => format!("{} volume", ch(id)),
        Target::ChannelPan(id) => format!("{} pan", ch(id)),
        Target::ChannelPitch(id) => format!("{} pitch", ch(id)),
        Target::SynthCutoff(id) => format!("{} cutoff", ch(id)),
        Target::SynthResonance(id) => format!("{} resonance", ch(id)),
        Target::PluginParam { channel, param, .. } => format!("{} param {param}", ch(channel)),
        Target::Tempo => "Tempo".into(),
        Target::InstrumentParam { channel, param } => {
            let what = match p.channel(*channel).map(|c| &c.instrument) {
                Some(Instrument::Wavetable(w)) => param.strip_prefix("macro").and_then(|d| d.parse::<usize>().ok()).and_then(|d| w.macro_names.get(d.wrapping_sub(1)).cloned()).unwrap_or_else(|| param.clone()),
                _ => param.clone(),
            };
            format!("{} {what}", ch(channel))
        }
    }
}

pub fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' }).collect()
}

fn chrono_like_stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("{secs}")
}

impl State {
    /// Adds a stock effect to the master and opens its editor.
    /// Adds the first effect of plugin file `path` to the master and opens its window (scripted tests).
    pub fn open_plugin_on_master(&mut self, path: &Path) {
        let format = if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("clap")) { dragon_engine::model::PluginFormat::Clap } else { dragon_engine::model::PluginFormat::Vst3 };
        let infos = match dragon_engine::plugins::scan_file(format, path) {
            Ok(i) => i,
            Err(e) => {
                tracing::error!("smoke plugin {}: {e}", path.display());
                return;
            }
        };
        let Some(info) = infos.iter().find(|i| !i.instrument).or(infos.first()) else { return };
        let r = info.to_ref();
        let m = self.session.project.mixer.master.id;
        let iid = self.session.edit("Add plugin", |p| {
            let iid = p.next_id();
            p.mixer.master.inserts.push(Insert { id: iid, effect: "plugin".into(), enabled: true, mix: 1.0, params: Default::default(), sidechain: None, plugin: Some(r), preset: String::new(), mods: Vec::new() });
            iid
        });
        self.ui.effect_windows.push((m, iid));
    }

    pub fn open_effect_on_master(&mut self, id: &str) {
        let Some(def) = dragon_engine::effects::find(id) else { return };
        let m = self.session.project.mixer.master.id;
        let iid = self.session.edit("Add effect", |p| {
            let iid = p.next_id();
            p.mixer.master.inserts.push(Insert { id: iid, effect: def.id.into(), enabled: true, mix: 1.0, params: def.defaults(), sidechain: None, plugin: None, preset: String::new(), mods: Vec::new() });
            iid
        });
        self.ui.effect_windows.push((m, iid));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_project_makes_a_beat() {
        let dir = std::env::temp_dir().join(format!("dragon-kit-{}", std::process::id()));
        let kit = dragon_engine::instruments::kit::ensure_default_kit(&dir).unwrap();
        let p = starter_project(&kit);
        assert!(p.channels.len() >= 6, "kick, clap, hat, snare, bass, lead");
        let cache = dragon_engine::samples::SampleCache::default();
        let opts = dragon_engine::export::ExportOptions { range: dragon_engine::export::ExportRange::Song, tail: 0.5, ..Default::default() };
        let mut pool = dragon_engine::render::Pool::new();
        let mix = dragon_engine::export::render_mix(&p, &cache, &opts, &mut pool, &mut |_| {});
        // Four bars at 140 BPM ≈ 6.9 s.
        assert!(mix.duration() > 6.5, "{}", mix.duration());
        assert!(mix.peak() > 0.2, "the beat should be audible, peak {}", mix.peak());
        // The first kick is at 0 s, the clap on beat 2 (≈ 0.43 s).
        let rate = mix.rate as f64;
        let clap_at = (60.0 / p.bpm * rate) as usize;
        let window = |s: usize| mix.l[s..s + 2000].iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(window(0) > 0.2 && window(clap_at) > 0.1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn control_surface_moves_faders_and_toggles_tracks() {
        use dragon_engine::midi::McuAction;
        let mut st = State::new(redhive_client_framework::AppContext::new("en"), Settings { show_splash: false, ..Settings::default() });
        st.new_demo_project();
        let t0 = st.session.project.tracks[0].clone();
        assert!(st.apply_mcu(McuAction::Fader { strip: 0, value: 0.75 }));
        st.session.end_gesture();
        let vol = st.session.project.mixer.get(t0.mixer).unwrap().volume;
        assert!((vol - 1.0).abs() < 1e-3, "unity at 75 %: {vol}");
        st.apply_mcu(McuAction::Mute(0));
        assert!(st.session.project.track(t0.id).unwrap().muted);
        st.apply_mcu(McuAction::Fader { strip: 8, value: 0.0 });
        st.session.end_gesture();
        assert_eq!(st.session.project.mixer.master.volume, 0.0);
        // Banking past the last track is clamped.
        st.apply_mcu(McuAction::BankRight);
        assert_eq!(st.mcu.bank, 0, "four tracks fit in the first bank");
    }
}
