//! Loading progress (S31, `docs/protocol.md`, loading progress): what
//! `state.loading` says while a client waits for a station, a product or a
//! set, and the rules that keep it honest. The engine counts what it
//! fetches and builds anyway; nothing here asks for anything.
//!
//! A load has up to two stages. `first` lasts until the load's first frame
//! is on screen; `history` until the frames behind it are in the timeline.
//! Within a stage the percentage never goes down, and it reaches 100 only
//! when the stage's frame is shown (`Tracker::shown`) or its last frame is
//! in (`history_end`, a made fill's end). At 100 a stage stays for
//! `LINGER`, then gives way to the next stage or to `null`. Counts reach
//! the wire at most once a second (`THROTTLE`); a stage's start, its 100
//! and its end go at once.
//!
//! Who reports what:
//! - a radar's or a provider composite's poller: its live frame (`shown`),
//!   its backfill's plan and end (`plan`, `history_end`, from
//!   `Event::HistoryPlan`/`HistoryEnd`), each backfilled frame
//!   (`backfilled`);
//! - My mosaic and a composite's product (`mosaic::run`): a `Progress`
//!   while the fill lasts, `None` once it is over.

use crate::products::Want;
use crate::protocol::{Frame, SiteKind, Station};
use serde::Serialize;
use std::time::{Duration, Instant};

/// Between two count updates on the wire.
pub const THROTTLE: Duration = Duration::from_secs(1);
/// How long a finished stage stays at 100 before the next one or `null`.
pub const LINGER: Duration = Duration::from_secs(1);

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    First,
    History,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Unit {
    Volumes,
    Frames,
}

/// `state.loading` as sent.
#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct Loading {
    pub stage: Stage,
    pub percent: u32,
    pub done: u32,
    pub total: u32,
    pub unit: Unit,
    pub label: String,
    /// A composite's own newest frame, drawn under its product's first
    /// stage (docs/protocol.md, `under`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub under: Option<Frame>,
}

impl Loading {
    fn new(stage: Stage, done: u32, total: u32, unit: Unit, label: String) -> Loading {
        Loading {
            stage,
            percent: percent(done, total),
            done: done.min(total),
            total,
            unit,
            label,
            under: None,
        }
    }
}

/// `done` of `total` as a whole percentage, rounded down; 0 of nothing.
pub fn percent(done: u32, total: u32) -> u32 {
    if total == 0 {
        0
    } else {
        (u64::from(done.min(total)) * 100 / u64::from(total)) as u32
    }
}

/// What a made fill reports (`mosaic::run`): its stage, volumes in of
/// volumes wanted, and the words for it.
#[derive(Clone, PartialEq, Debug)]
pub struct Progress {
    pub stage: Stage,
    pub done: u32,
    pub total: u32,
    pub label: String,
}

/// Where a load's counts come from.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Kind {
    /// A radar or a provider's composite: its poller's frames.
    #[default]
    Radar,
    /// My mosaic or a composite's product: `mosaic::run`'s `Progress`.
    Made,
}

/// The load a station shows under `want`: made by `mosaic.rs` for My
/// mosaic and a composite's product (S24b), else its poller's frames.
pub fn kind_of(station: &Station, want: Want) -> Kind {
    let mine = station.provider == crate::providers::ProviderId::Mosaic;
    if mine || (station.kind == SiteKind::Grid && !want.is_lowest()) {
        Kind::Made
    } else {
        Kind::Radar
    }
}

/// A radar's or a provider composite's load in words: `Vara Reflectivity
/// 0.5°`, `Vara Rain mass`, `Sweden composite`.
pub fn radar_name(station: &Station, want: Want) -> String {
    if station.kind == SiteKind::Grid {
        return format!("{} composite", station.name);
    }
    let angle = match want {
        Want::Lowest => crate::products::nominal_angles(station).first().copied(),
        Want::Angle(deg) => Some(deg),
        _ => None,
    };
    match angle {
        Some(deg) => format!("{} Reflectivity {deg:.1}°", station.name),
        None => format!("{} {}", station.name, want.product().1),
    }
}

/// The engine's load and what `state.loading` last said.
#[derive(Default)]
pub struct Tracker {
    kind: Kind,
    /// Whether a load is running: made progress is taken only then.
    active: bool,
    /// The stage showing, before throttling.
    current: Option<Loading>,
    /// The stage that follows once `current` has stayed at 100 for `LINGER`.
    next: Option<Loading>,
    /// When `current` reached 100.
    finished_at: Option<Instant>,
    /// Whether the first stage's frame reached the screen.
    first_shown: bool,
    /// A radar's frames backfilled since the load began, and its plan.
    backfilled: u32,
    planned: Option<u32>,
    /// The composite under a made first stage, for that stage shown again
    /// after an offline spell (review NIT5).
    under: Option<Frame>,
    /// A radar's backfill ended (review NIT4): its history stage finishes
    /// as soon as it shows, even if that is after the first stage's linger.
    ended: bool,
    /// What the wire said, and when it last changed.
    published: Option<Loading>,
    published_at: Option<Instant>,
    /// A stage started, finished or ended: sent with the next broadcast.
    urgent: bool,
}

impl Tracker {
    /// A new load (`select_site`, `set_product`, `set_mosaic`): of `kind`,
    /// named `name`. With no frame on screen (`has_frame` false) its first
    /// stage shows at once: a radar's first frame, or a made fill that has
    /// not reported yet; `under` is drawn beneath a composite's product.
    pub fn begin(&mut self, kind: Kind, name: &str, has_frame: bool, under: Option<Frame>) {
        *self = Tracker {
            kind,
            active: true,
            published: self.published.take(),
            published_at: self.published_at,
            urgent: true,
            ..Tracker::default()
        };
        if has_frame {
            return;
        }
        let mut first = match kind {
            Kind::Radar => Loading::new(
                Stage::First,
                0,
                1,
                Unit::Frames,
                format!("{name}: first frame"),
            ),
            Kind::Made => Loading::new(
                Stage::First,
                0,
                0,
                Unit::Volumes,
                format!("{name}: placing the radars"),
            ),
        };
        first.under = under.clone();
        self.under = under;
        self.current = Some(first);
    }

    /// No load: a poller the engine restarted on its own. Its backfill may
    /// still start a history stage (`plan`).
    pub fn reset(&mut self) {
        *self = Tracker {
            published: self.published.take(),
            published_at: self.published_at,
            urgent: true,
            ..Tracker::default()
        };
    }

    /// Show `stage` now, or after the stage at 100 has lingered.
    fn set_stage(&mut self, stage: Option<Loading>, now: Instant) {
        self.urgent = true;
        if self.finished_at.is_some() {
            self.next = stage;
            return;
        }
        self.finished_at = stage.as_ref().filter(|s| s.percent >= 100).map(|_| now);
        self.current = stage;
    }

    /// New counts for the stage showing: taken when they say as much as or
    /// more than the wire's (the percentage never goes down); at 100 the
    /// stage is finished.
    fn update(&mut self, counts: Loading, now: Instant) {
        let Some(current) = &self.current else {
            return;
        };
        if current.stage != counts.stage
            || self.finished_at.is_some()
            || counts.percent < current.percent
        {
            return;
        }
        if counts.percent >= 100 {
            self.finished_at = Some(now);
            self.urgent = true;
        }
        let under = current.under.clone();
        self.current = Some(Loading { under, ..counts });
    }

    fn finish(&mut self, now: Instant) {
        if let Some(current) = &mut self.current
            && self.finished_at.is_none()
        {
            current.total = current.done.max(1);
            current.done = current.total;
            current.percent = 100;
            current.under = None;
            self.finished_at = Some(now);
            self.urgent = true;
        }
    }

    /// A frame of the load reached the screen: the first stage is done.
    pub fn shown(&mut self, now: Instant) {
        self.first_shown = true;
        if self
            .current
            .as_ref()
            .is_some_and(|c| c.stage == Stage::First)
        {
            self.finish(now);
        }
    }

    /// A radar's history stage that waited behind the first stage's linger
    /// (review NIT4): its counts as they are now, and finished if its
    /// backfill already ended.
    fn refresh_history(&mut self, now: Instant) {
        let (Kind::Radar, Some(total), Some(current)) = (self.kind, self.planned, &self.current)
        else {
            return;
        };
        if current.stage != Stage::History || total == 0 {
            return;
        }
        let label = history_label(&current.label, self.backfilled, total);
        self.current = Some(Loading::new(
            Stage::History,
            self.backfilled,
            total,
            Unit::Frames,
            label,
        ));
        if self.ended {
            self.finish(now);
        }
    }

    /// A radar's backfilled frame joined the timeline.
    pub fn backfilled(&mut self, now: Instant) {
        if self.kind != Kind::Radar {
            return;
        }
        self.backfilled += 1;
        if let (Some(total), Some(current)) = (self.planned, &self.current)
            && current.stage == Stage::History
        {
            let counts = Loading::new(
                Stage::History,
                self.backfilled,
                total,
                Unit::Frames,
                history_label(&current.label, self.backfilled, total),
            );
            self.update(counts, now);
        }
    }

    /// A radar's backfill will bring `frames` earlier frames of the load
    /// named `name`, the tilt store's (already counted by `backfilled`)
    /// and those it fetches: its history stage. None: nothing to wait for.
    pub fn plan(&mut self, name: &str, frames: usize, now: Instant) {
        if self.kind != Kind::Radar {
            return;
        }
        let total = u32::try_from(frames).unwrap_or(u32::MAX);
        self.active = true;
        self.planned = Some(total);
        if total == 0 {
            // The first stage, if it still shows, ends here: no frame came
            // for it (a volume too old or undecodable), and none will.
            self.set_stage(None, now);
            return;
        }
        let done = self.backfilled.min(total);
        let label = history_label(&format!("{name}:"), done, total);
        let stage = Loading::new(Stage::History, done, total, Unit::Frames, label);
        self.set_stage(Some(stage), now);
    }

    /// A radar's backfill ended; a short one ends its stage at 100 with
    /// what came.
    pub fn history_end(&mut self, now: Instant) {
        if self.kind != Kind::Radar {
            return;
        }
        self.ended = true;
        if self
            .current
            .as_ref()
            .is_some_and(|c| c.stage == Stage::History)
        {
            self.finish(now);
        }
    }

    /// A made fill's report: counts for its stage, the next stage, or
    /// (`None`) the end of the fill.
    pub fn progress(&mut self, report: Option<Progress>, now: Instant) {
        if self.kind != Kind::Made || !self.active {
            return;
        }
        let Some(report) = report else {
            self.active = false;
            if self.current.is_some() {
                self.finish(now);
            }
            self.next = None;
            return;
        };
        let counts = Loading::new(
            report.stage,
            report.done,
            report.total,
            Unit::Volumes,
            report.label,
        );
        let showing = self.current.as_ref().map(|c| c.stage);
        match (showing, report.stage) {
            // The first stage counts on, and stays short of 100 until its
            // frame is on screen.
            (Some(Stage::First), Stage::First) => {
                let capped = counts.percent.min(99);
                self.update(
                    Loading {
                        percent: capped,
                        ..counts
                    },
                    now,
                );
            }
            (Some(Stage::History), Stage::History) => self.update(counts, now),
            // Never back to the first stage once past it.
            (Some(Stage::History), Stage::First) => {}
            (None, Stage::First) if self.first_shown => {}
            (None, Stage::First) => {
                let capped = counts.percent.min(99);
                self.set_stage(
                    Some(Loading {
                        percent: capped,
                        under: self.under.clone(),
                        ..counts
                    }),
                    now,
                );
            }
            (Some(Stage::First) | None, Stage::History) => {
                if self.finished_at.is_some() || self.current.is_none() {
                    self.set_stage(Some(counts), now);
                } else {
                    // Past the first frame without seeing it: the frame
                    // went to history (a newer one was catalogued).
                    self.current = None;
                    self.set_stage(Some(counts), now);
                }
            }
        }
    }

    /// The feed went silent before the first frame: nothing to wait for in
    /// that stage (a history under way goes on). Offline (review NIT5): no
    /// stage stays while no request can succeed; one starts again with the
    /// feed (a backfill's plan, a made fill's next report).
    pub fn quiet(&mut self, now: Instant, offline: bool) {
        if offline {
            if self.current.is_some() || self.next.is_some() {
                self.current = None;
                self.next = None;
                self.finished_at = None;
                self.urgent = true;
            }
            return;
        }
        if self
            .current
            .as_ref()
            .is_some_and(|c| c.stage == Stage::First && self.finished_at.is_none())
        {
            self.current = None;
            self.set_stage(None, now);
            if self.kind == Kind::Made {
                self.active = false;
            }
        }
    }

    /// What `state.loading` says now: counts at most once per `THROTTLE`,
    /// a stage's start, 100 and end at once; a finished stage gives way
    /// after `LINGER`.
    pub fn wire(&mut self, now: Instant) -> Option<Loading> {
        if let Some(at) = self.finished_at
            && now.duration_since(at) >= LINGER
        {
            self.current = self.next.take();
            self.finished_at = None;
            self.refresh_history(now);
            if self.current.as_ref().is_some_and(|s| s.percent >= 100) {
                self.finished_at.get_or_insert(now);
            }
            self.urgent = true;
        }
        let due = self.urgent
            || self
                .published_at
                .is_none_or(|at| now.duration_since(at) >= THROTTLE);
        if due && self.published != self.current {
            self.published = self.current.clone();
            self.published_at = Some(now);
        }
        if due {
            self.urgent = false;
        }
        self.published.clone()
    }
}

/// `Vara Reflectivity 0.5°: 7 of 12 frames` from a stage's label.
fn history_label(label: &str, done: u32, total: u32) -> String {
    let name = label.split_once(':').map_or(label, |(name, _)| name);
    format!("{name}: {done} of {total} frames")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    fn report(stage: Stage, done: u32, total: u32) -> Option<Progress> {
        Some(Progress {
            stage,
            done,
            total,
            label: format!("Nordic Rain mass: {done} of {total}"),
        })
    }

    #[test]
    fn the_percentage_rounds_down_and_never_divides_by_nothing() {
        assert_eq!(percent(0, 0), 0);
        assert_eq!(percent(1, 3), 33);
        assert_eq!(percent(2, 3), 66);
        assert_eq!(percent(40, 41), 97);
        assert_eq!(percent(5, 4), 100);
    }

    #[test]
    fn a_radars_first_frame_then_its_history_then_nothing() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.begin(Kind::Radar, "Vara Reflectivity 0.5°", false, None);
        let first = t.wire(t0).unwrap();
        assert_eq!(
            (
                first.stage,
                first.percent,
                first.done,
                first.total,
                first.unit
            ),
            (Stage::First, 0, 0, 1, Unit::Frames)
        );
        assert_eq!(first.label, "Vara Reflectivity 0.5°: first frame");
        // The live frame: 100 at once, for a second.
        t.shown(at(t0, 200));
        assert_eq!(t.wire(at(t0, 200)).unwrap().percent, 100);
        // The tilt store's frames come before the plan and count.
        t.backfilled(at(t0, 300));
        t.backfilled(at(t0, 400));
        t.plan("Vara Reflectivity 0.5°", 11, at(t0, 500));
        assert_eq!(t.wire(at(t0, 900)).unwrap().percent, 100, "lingers");
        let history = t.wire(at(t0, 1300)).unwrap();
        assert_eq!(
            (history.stage, history.done, history.total, history.percent),
            (Stage::History, 2, 11, 18)
        );
        assert_eq!(history.label, "Vara Reflectivity 0.5°: 2 of 11 frames");
        for i in 0..8 {
            t.backfilled(at(t0, 1400 + i * 10));
        }
        // Counts wait for the throttle...
        assert_eq!(t.wire(at(t0, 1500)).unwrap().done, 2);
        assert_eq!(t.wire(at(t0, 2300)).unwrap().done, 10);
        // ...and the last frame is sent at once.
        t.backfilled(at(t0, 2350));
        assert_eq!(t.wire(at(t0, 2360)).unwrap().percent, 100);
        t.history_end(at(t0, 2400));
        assert_eq!(t.wire(at(t0, 2400)).unwrap().percent, 100);
        assert_eq!(t.wire(at(t0, 3500)), None);
    }

    /// Review NIT4: the whole backfill (plan, frames, end) lands while the
    /// first frame's 100 lingers (a tilt store with every frame, no
    /// backfill delay): the history stage then shows its real count and
    /// finishes, rather than a stale count that never ends.
    /// Review NIT5: offline, no stage stays (a history no request can
    /// finish); the feed back, the next plan or report starts it again.
    #[test]
    fn offline_clears_any_stage_and_the_feed_starts_it_again() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.begin(Kind::Radar, "Vara Reflectivity 0.5°", true, None);
        t.plan("Vara Reflectivity 0.5°", 11, at(t0, 0));
        assert_eq!(t.wire(at(t0, 0)).unwrap().stage, Stage::History);
        t.quiet(at(t0, 100), false);
        assert!(t.wire(at(t0, 100)).is_some(), "silence leaves a history");
        t.quiet(at(t0, 200), true);
        assert_eq!(t.wire(at(t0, 200)), None, "offline clears it");
        t.plan("Vara Reflectivity 0.5°", 4, at(t0, 300));
        assert_eq!(t.wire(at(t0, 300)).unwrap().total, 4);
        let frame: Frame = serde_json::from_str(include_str!("../data/fixture.json")).unwrap();
        t.begin(Kind::Made, "Nordic Rain mass", false, Some(frame));
        t.quiet(at(t0, 400), true);
        assert_eq!(t.wire(at(t0, 400)), None);
        t.progress(report(Stage::First, 5, 10), at(t0, 500));
        let back = t.wire(at(t0, 500)).unwrap();
        assert_eq!(
            (back.stage, back.percent, back.under.is_some()),
            (Stage::First, 50, true)
        );
    }

    #[test]
    fn a_backfill_inside_the_linger_shows_and_finishes() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.begin(Kind::Radar, "Vara Rain mass", false, None);
        t.shown(at(t0, 0));
        assert_eq!(t.wire(at(t0, 0)).unwrap().percent, 100);
        t.plan("Vara Rain mass", 11, at(t0, 100));
        for i in 0..11 {
            t.backfilled(at(t0, 200 + i * 10));
        }
        t.history_end(at(t0, 500));
        assert_eq!(t.wire(at(t0, 600)).unwrap().stage, Stage::First, "lingers");
        let history = t.wire(at(t0, 1100)).unwrap();
        assert_eq!(
            (history.stage, history.done, history.total, history.percent),
            (Stage::History, 11, 11, 100)
        );
        assert_eq!(history.label, "Vara Rain mass: 11 of 11 frames");
        assert_eq!(t.wire(at(t0, 2200)), None, "and ends");
    }

    #[test]
    fn a_backfill_that_stops_early_ends_at_100_with_what_came() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.begin(Kind::Radar, "Vara Rain mass", true, None);
        assert_eq!(t.wire(t0), None, "a frame on screen: no first stage");
        t.plan("Vara Rain mass", 11, at(t0, 100));
        t.backfilled(at(t0, 200));
        t.backfilled(at(t0, 1300));
        assert_eq!(t.wire(at(t0, 1300)).unwrap().done, 2);
        t.history_end(at(t0, 1400));
        let end = t.wire(at(t0, 1400)).unwrap();
        assert_eq!((end.percent, end.done, end.total), (100, 2, 2));
        assert_eq!(t.wire(at(t0, 2500)), None);
    }

    #[test]
    fn a_plan_of_nothing_and_a_quiet_feed_end_the_load() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.begin(Kind::Radar, "Kiruna Reflectivity 0.5°", false, None);
        assert!(t.wire(t0).is_some());
        t.quiet(at(t0, 100), false);
        assert_eq!(t.wire(at(t0, 100)), None);
        t.begin(Kind::Radar, "Vara Reflectivity 0.5°", true, None);
        t.plan("Vara Reflectivity 0.5°", 0, at(t0, 200));
        assert_eq!(t.wire(at(t0, 200)), None);
    }

    #[test]
    fn a_made_first_stage_never_goes_down_and_waits_for_its_frame() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.begin(Kind::Made, "Nordic Rain mass", false, None);
        assert_eq!(
            t.wire(t0).unwrap().label,
            "Nordic Rain mass: placing the radars"
        );
        t.progress(report(Stage::First, 30, 82), at(t0, 1000));
        assert_eq!(t.wire(at(t0, 1000)).unwrap().percent, 36);
        // A lower count (the target time moved) does not show.
        t.progress(report(Stage::First, 20, 80), at(t0, 2000));
        assert_eq!(t.wire(at(t0, 2000)).unwrap().percent, 36);
        // Every volume in: 99 until the frame is on screen.
        t.progress(report(Stage::First, 82, 82), at(t0, 3000));
        assert_eq!(t.wire(at(t0, 3000)).unwrap().percent, 99);
        t.shown(at(t0, 3500));
        assert_eq!(t.wire(at(t0, 3500)).unwrap().percent, 100);
        t.progress(report(Stage::History, 41, 123), at(t0, 3600));
        assert_eq!(t.wire(at(t0, 4000)).unwrap().stage, Stage::First);
        let history = t.wire(at(t0, 4600)).unwrap();
        assert_eq!((history.stage, history.percent), (Stage::History, 33));
        // Never back to the first stage.
        t.progress(report(Stage::First, 1, 2), at(t0, 6000));
        assert_eq!(t.wire(at(t0, 6000)).unwrap().stage, Stage::History);
        t.progress(None, at(t0, 7000));
        assert_eq!(t.wire(at(t0, 7000)).unwrap().percent, 100);
        assert_eq!(t.wire(at(t0, 8100)), None);
        // Over: a later report starts nothing.
        t.progress(report(Stage::History, 1, 41), at(t0, 9000));
        assert_eq!(t.wire(at(t0, 10_000)), None);
    }

    #[test]
    fn under_goes_with_the_first_stage() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        let frame: Frame = serde_json::from_str(include_str!("../data/fixture.json")).unwrap();
        t.begin(Kind::Made, "Nordic Column max", false, Some(frame));
        t.progress(report(Stage::First, 3, 10), at(t0, 1000));
        assert!(t.wire(at(t0, 1000)).unwrap().under.is_some());
        t.shown(at(t0, 1100));
        assert!(t.wire(at(t0, 1100)).unwrap().under.is_none());
    }

    #[test]
    fn a_radar_ignores_made_reports_and_a_made_load_ignores_backfills() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.begin(Kind::Radar, "Nordic composite", false, None);
        t.progress(report(Stage::First, 5, 10), at(t0, 1000));
        assert_eq!(t.wire(at(t0, 1000)).unwrap().percent, 0);
        t.begin(Kind::Made, "Nordic Column max", true, None);
        t.plan("Nordic Column max", 5, at(t0, 1100));
        t.backfilled(at(t0, 1200));
        assert_eq!(t.wire(at(t0, 2300)), None);
    }

    #[test]
    fn the_wire_serializes_as_documented() {
        let l = Loading::new(
            Stage::First,
            52,
            82,
            Unit::Volumes,
            "Nordic Rain mass: 26 of 40 radars in for 14:25Z (1 silent)".into(),
        );
        assert_eq!(
            serde_json::to_string(&l).unwrap(),
            r#"{"stage":"first","percent":63,"done":52,"total":82,"unit":"volumes","label":"Nordic Rain mass: 26 of 40 radars in for 14:25Z (1 silent)"}"#
        );
    }
}
