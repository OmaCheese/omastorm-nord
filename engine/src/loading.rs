//! Loading progress (S31, `docs/protocol.md`, loading progress): what
//! `state.loading` says while a client waits for a station, a product or a
//! set, and the rules that keep it honest. The engine counts what it
//! fetches and builds anyway; nothing here asks for anything.
//!
//! A load has up to three stages (S35). `first` lasts until the volumes
//! that load's first frame needs are all in; `build` until that frame is
//! assembled, sent and on screen (`Tracker::shown`); `history` until the
//! frames behind it are in the timeline. Within a stage the percentage
//! never goes down, and a stage that is over stays full: `wire` sends the
//! whole load as `stages`, one `Segment` per stage, so a client draws one
//! segmented bar rather than a number that starts again three times. At
//! 100 a stage stays for `LINGER`, then gives way to the next stage or to
//! `null`. Counts reach the wire at most once a second (`THROTTLE`); a
//! stage's start, its 100 and its end go at once.
//!
//! Before S35 the first stage was held at 99 until the frame was on
//! screen, so the last radars' volumes, the build, the ~1 MB frame and the
//! draw all hid behind one number. `first` now reaches a true 100 when
//! every counted radar is in, and the wait that follows is `build`'s.
//!
//! Who reports what:
//! - a radar's or a provider composite's poller: its live frame (`shown`),
//!   its backfill's plan and end (`plan`, `history_end`, from
//!   `Event::HistoryPlan`/`HistoryEnd`), each backfilled frame
//!   (`backfilled`);
//! - My mosaic and a composite's product (`mosaic::run`): a `Progress`
//!   while the fill lasts, `None` once it is over, and the build's start
//!   and end (`building`, from `Event::Building`).

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
    /// S35: the frame assembled, sent and drawn. Before S35 this wait had
    /// no stage of its own and hid inside `First`'s last percent.
    Build,
    History,
}

impl Stage {
    /// Where the stage comes in a load, so `wire` can tell a segment that
    /// is over from one still to come.
    fn order(self) -> usize {
        match self {
            Stage::First => 0,
            Stage::Build => 1,
            Stage::History => 2,
        }
    }
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Unit {
    Volumes,
    Frames,
    /// S35, the build stage: 0 building, 1 built and sent, 2 on screen.
    Steps,
}

/// The build's two steps (`Unit::Steps`).
const BUILD_STEPS: u32 = 2;

/// How far a load's whole build is: 0 building, 1 sent, 2 on screen.
type Built = u32;

/// S35: what a client draws segments of. `state` says whether the segment
/// is over (and so stays full), running, or still to come.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum SegmentState {
    Done,
    Active,
    Waiting,
}

/// One stage of the load as the bar draws it (docs/protocol.md, Loading
/// progress). `share` is the segment's width; a load's shares sum to 100.
#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct Segment {
    pub stage: Stage,
    pub share: u32,
    pub percent: u32,
    pub done: u32,
    pub total: u32,
    pub unit: Unit,
    pub state: SegmentState,
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
    /// S35: the whole load in order, the fields above being exactly the
    /// `active` segment's. Filled by `wire`, empty everywhere else.
    pub stages: Vec<Segment>,
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
            stages: Vec::new(),
        }
    }
}

/// The stages a load of `kind` goes through, in order, each with its share
/// of the bar. Fixed per kind rather than guessed from how long a stage
/// may take, so the segment boundaries never move while the load runs.
fn plan_of(kind: Kind) -> [(Stage, u32); 3] {
    match kind {
        Kind::Radar => [
            (Stage::First, 40),
            (Stage::Build, 10),
            (Stage::History, 50),
        ],
        Kind::Made => [
            (Stage::First, 55),
            (Stage::Build, 20),
            (Stage::History, 25),
        ],
    }
}

/// What a stage of a load of `kind` counts in.
fn unit_of(kind: Kind, stage: Stage) -> Unit {
    match (kind, stage) {
        (_, Stage::Build) => Unit::Steps,
        (Kind::Radar, _) => Unit::Frames,
        (Kind::Made, _) => Unit::Volumes,
    }
}

/// `weights` as whole shares of 100 that sum to exactly 100, by largest
/// remainder, none of them 0 (a segment a client could not draw).
fn shares(weights: &[u32]) -> Vec<u32> {
    let n = weights.len();
    if n == 0 {
        return Vec::new();
    }
    let sum: u64 = weights.iter().map(|w| u64::from((*w).max(1))).sum();
    let mut out = Vec::with_capacity(n);
    let mut rest: Vec<(u64, usize)> = Vec::with_capacity(n);
    for (i, w) in weights.iter().enumerate() {
        let scaled = u64::from((*w).max(1)) * 100;
        out.push(((scaled / sum) as u32).max(1));
        rest.push((scaled % sum, i));
    }
    let mut total: u32 = out.iter().sum();
    // The largest remainders take what rounding down left over.
    rest.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut k = 0;
    while total < 100 {
        out[rest[k % n].1] += 1;
        total += 1;
        k += 1;
    }
    // The floor of 1 can overshoot when a load has many tiny stages; the
    // widest segment gives the excess back.
    while total > 100 {
        let Some(i) = (0..n).filter(|i| out[*i] > 1).max_by_key(|i| out[*i]) else {
            break;
        };
        out[i] -= 1;
        total -= 1;
    }
    out
}

/// What a finished stage keeps for its segment: it draws full, with the
/// counts it ended on.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Final {
    done: u32,
    total: u32,
    unit: Unit,
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
    /// S35: the load in words, for the build stage's own label.
    name: String,
    /// S35: the frame time being built, `14:25Z` (`building`); empty until
    /// the builder says.
    frame_time: String,
    /// S35: how far the whole build is (0 building, 1 sent, 2 on screen).
    built: Built,
    /// S35: the stages published as the current one. A stage whose work
    /// ended before it was ever published was never drawn, and is left out
    /// of `stages` rather than flashing past (an instant build).
    seen: Vec<Stage>,
    /// S35: the counts each finished stage ended on, for its full segment.
    finals: Vec<(Stage, Final)>,
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
            name: name.to_owned(),
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
            name: std::mem::take(&mut self.name),
            ..Tracker::default()
        };
    }

    /// S35: the stage showing is over. Its segment keeps the counts it
    /// ended on and draws full from here on.
    fn retire(&mut self) {
        let Some(current) = &self.current else {
            return;
        };
        let (stage, ended) = (
            current.stage,
            Final {
                done: current.done,
                total: current.total,
                unit: current.unit,
            },
        );
        match self.finals.iter_mut().find(|(s, _)| *s == stage) {
            Some((_, f)) => *f = ended,
            None => self.finals.push((stage, ended)),
        }
    }

    /// Show `stage` now, or after the stage at 100 has lingered.
    fn set_stage(&mut self, stage: Option<Loading>, now: Instant) {
        self.urgent = true;
        if self.finished_at.is_some() {
            self.next = stage;
            return;
        }
        self.retire();
        self.finished_at = stage.as_ref().filter(|s| s.percent >= 100).map(|_| now);
        self.current = stage;
    }

    /// S35: the stage that just reached 100. The first stage's end is the
    /// build's start, so the bar moves on to the build rather than holding
    /// at 99 until the frame is on screen.
    fn stage_finished(&mut self, stage: Stage, now: Instant) {
        if stage == Stage::First {
            self.queue_build(now);
        }
    }

    /// S35: the build stage, after the first. Nothing is queued once the
    /// frame is already on screen (a radar's decode and send, or a frame
    /// the tilt store made at once): the segment is then never drawn.
    fn queue_build(&mut self, now: Instant) {
        if self.built >= BUILD_STEPS {
            return;
        }
        let label = self.build_label();
        let stage = Loading::new(Stage::Build, self.built, BUILD_STEPS, Unit::Steps, label);
        self.set_stage(Some(stage), now);
    }

    /// `Nordic Rain mass: building 14:25Z`, then `… drawing 14:25Z`: the
    /// build stage is honest about which of the two it waits on (S35).
    fn build_label(&self) -> String {
        let what = if self.frame_time.is_empty() {
            "the frame"
        } else {
            &self.frame_time
        };
        let verb = if self.built >= 1 { "drawing" } else { "building" };
        format!("{}: {verb} {what}", self.name)
    }

    /// S35: a made frame's build began (`started`), or the built frame was
    /// sent (`started` false). The first stage's work ends when the build
    /// begins; `shown` still ends the build.
    pub fn building(&mut self, time: &str, started: bool, now: Instant) {
        if !time.is_empty() {
            self.frame_time = time.to_owned();
        }
        if !started {
            self.built = self.built.max(1);
        }
        match self.current.as_ref().map(|c| c.stage) {
            // Still on the first stage (or its 100 lingering): it is over,
            // and the build follows it.
            Some(Stage::First) => {
                self.finish(now);
                self.queue_build(now);
            }
            Some(Stage::Build) => {
                let counts = Loading::new(
                    Stage::Build,
                    self.built,
                    BUILD_STEPS,
                    Unit::Steps,
                    self.build_label(),
                );
                self.update(counts, now);
            }
            // Past it (a history frame's build), or no load: nothing.
            _ => {}
        }
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
        let reached = counts.percent >= 100;
        let stage = counts.stage;
        if reached {
            self.finished_at = Some(now);
            self.urgent = true;
        }
        let under = current.under.clone();
        self.current = Some(Loading { under, ..counts });
        if reached {
            self.stage_finished(stage, now);
        }
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
            let stage = current.stage;
            self.stage_finished(stage, now);
        }
    }

    /// The load's frame reached the client's timeline: the first stage and
    /// the build are both done. S35: it no longer depends on the client
    /// following the newest frame (`main.rs`), so a timeline scrubbed back
    /// into the past does not freeze the bar.
    pub fn shown(&mut self, now: Instant) {
        self.first_shown = true;
        self.built = BUILD_STEPS;
        match self.current.as_ref().map(|c| c.stage) {
            Some(Stage::First) => {
                self.finish(now);
                // The build was over before it was ever drawn: no segment
                // for it (a radar's decode and send, a frame from the
                // tilt store).
                self.next = self.next.take().filter(|n| n.stage != Stage::Build);
            }
            Some(Stage::Build) => {
                let counts = Loading::new(
                    Stage::Build,
                    BUILD_STEPS,
                    BUILD_STEPS,
                    Unit::Steps,
                    self.build_label(),
                );
                self.update(counts, now);
                self.finish(now);
            }
            _ => {}
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
            // S35: the first stage counts on and reaches a true 100 when
            // every counted radar is in. What follows it is the build's
            // own segment, not the last percent of this one.
            (Some(Stage::First), Stage::First) => self.update(counts, now),
            (Some(Stage::History), Stage::History) => self.update(counts, now),
            // Never back to an earlier stage once past it.
            (Some(Stage::Build) | Some(Stage::History), Stage::First) => {}
            (Some(Stage::History), Stage::Build) | (Some(Stage::First), Stage::Build) => {}
            (Some(Stage::Build), Stage::Build) => self.update(counts, now),
            (None, Stage::First) if self.first_shown => {}
            (None, Stage::First) => {
                self.set_stage(
                    Some(Loading {
                        under: self.under.clone(),
                        ..counts
                    }),
                    now,
                );
            }
            (None, Stage::Build) => {}
            // The fill has moved on to the backfill, so whatever the build
            // was waiting on is over.
            (Some(Stage::Build), Stage::History) => {
                self.finish(now);
                self.set_stage(Some(counts), now);
            }
            (Some(Stage::First) | None, Stage::History) => {
                if self.finished_at.is_some() || self.current.is_none() {
                    self.set_stage(Some(counts), now);
                } else {
                    // Past the first frame without seeing it: the frame
                    // went to history (a newer one was catalogued).
                    self.retire();
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
            self.retire();
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
            if let Some(stage) = self.published.as_ref().map(|p| p.stage)
                && !self.seen.contains(&stage)
            {
                self.seen.push(stage);
            }
        }
        if due {
            self.urgent = false;
        }
        let mut wire = self.published.clone()?;
        wire.stages = self.segments(&wire);
        Some(wire)
    }

    /// S35: the whole load as segments, in order, for one bar. The stages
    /// before the one showing are full, those after it are dim, and a
    /// stage that ended before it was ever published is left out, so the
    /// number of segments is what the load really went through. The shares
    /// sum to 100 over the segments drawn.
    fn segments(&self, showing: &Loading) -> Vec<Segment> {
        let here = showing.stage.order();
        let drawn: Vec<(Stage, u32)> = plan_of(self.kind)
            .into_iter()
            .filter(|(stage, _)| stage.order() >= here || self.seen.contains(stage))
            .collect();
        let widths = shares(&drawn.iter().map(|(_, w)| *w).collect::<Vec<u32>>());
        drawn
            .iter()
            .zip(widths)
            .map(|(&(stage, _), share)| match stage.order().cmp(&here) {
                std::cmp::Ordering::Less => {
                    let ended = self
                        .finals
                        .iter()
                        .find(|(s, _)| *s == stage)
                        .map(|(_, f)| *f)
                        .unwrap_or(Final {
                            done: 0,
                            total: 0,
                            unit: unit_of(self.kind, stage),
                        });
                    Segment {
                        stage,
                        share,
                        percent: 100,
                        done: ended.done,
                        total: ended.total,
                        unit: ended.unit,
                        state: SegmentState::Done,
                    }
                }
                std::cmp::Ordering::Equal => Segment {
                    stage,
                    share,
                    percent: showing.percent,
                    done: showing.done,
                    total: showing.total,
                    unit: showing.unit,
                    state: if showing.percent >= 100 {
                        SegmentState::Done
                    } else {
                        SegmentState::Active
                    },
                },
                std::cmp::Ordering::Greater => Segment {
                    stage,
                    share,
                    percent: 0,
                    done: 0,
                    total: 0,
                    unit: unit_of(self.kind, stage),
                    state: SegmentState::Waiting,
                },
            })
            .collect()
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

    /// The shape of a segment, for the assertions below.
    fn seg(s: &Segment) -> (Stage, u32, u32, SegmentState) {
        (s.stage, s.share, s.percent, s.state)
    }

    /// S35, the stream's reason for being: the first stage reaches a true
    /// 100 when every counted radar is in, and the wait that follows —
    /// the build, the frame's send, the draw — is its own segment instead
    /// of hiding behind 99 %.
    #[test]
    fn a_made_first_stage_reaches_100_and_the_build_is_its_own_segment() {
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
        // Every volume in: a true 100, not 99 (S31's cap, gone).
        t.progress(report(Stage::First, 82, 82), at(t0, 3000));
        let full = t.wire(at(t0, 3000)).unwrap();
        assert_eq!((full.stage, full.percent), (Stage::First, 100));
        assert_eq!(
            full.stages.iter().map(seg).collect::<Vec<_>>(),
            vec![
                (Stage::First, 55, 100, SegmentState::Done),
                (Stage::Build, 20, 0, SegmentState::Waiting),
                (Stage::History, 25, 0, SegmentState::Waiting),
            ]
        );
        // The build says so, and takes over once the first has lingered.
        t.building("14:25Z", true, at(t0, 3100));
        let building = t.wire(at(t0, 4100)).unwrap();
        assert_eq!(
            (building.stage, building.percent, building.unit),
            (Stage::Build, 0, Unit::Steps)
        );
        assert_eq!(building.label, "Nordic Rain mass: building 14:25Z");
        assert_eq!(
            building.stages.iter().map(seg).collect::<Vec<_>>(),
            vec![
                (Stage::First, 55, 100, SegmentState::Done),
                (Stage::Build, 20, 0, SegmentState::Active),
                (Stage::History, 25, 0, SegmentState::Waiting),
            ],
            "the stage behind it stays full"
        );
        // Built and sent: half the segment, and the label says which of
        // the two it is waiting on.
        t.building("14:25Z", false, at(t0, 4200));
        let drawing = t.wire(at(t0, 5200)).unwrap();
        assert_eq!((drawing.percent, drawing.done), (50, 1));
        assert_eq!(drawing.label, "Nordic Rain mass: drawing 14:25Z");
        t.shown(at(t0, 5300));
        let drawn = t.wire(at(t0, 5300)).unwrap();
        assert_eq!((drawn.stage, drawn.percent, drawn.done), (Stage::Build, 100, 2));
        t.progress(report(Stage::History, 41, 123), at(t0, 5400));
        assert_eq!(t.wire(at(t0, 5400)).unwrap().stage, Stage::Build, "lingers");
        let history = t.wire(at(t0, 6400)).unwrap();
        assert_eq!((history.stage, history.percent), (Stage::History, 33));
        assert_eq!(
            history.stages.iter().map(seg).collect::<Vec<_>>(),
            vec![
                (Stage::First, 55, 100, SegmentState::Done),
                (Stage::Build, 20, 100, SegmentState::Done),
                (Stage::History, 25, 33, SegmentState::Active),
            ]
        );
        // Never back to an earlier stage.
        t.progress(report(Stage::First, 1, 2), at(t0, 7500));
        assert_eq!(t.wire(at(t0, 7500)).unwrap().stage, Stage::History);
        t.progress(None, at(t0, 8500));
        assert_eq!(t.wire(at(t0, 8500)).unwrap().percent, 100);
        assert_eq!(t.wire(at(t0, 9600)), None);
        // Over: a later report starts nothing.
        t.progress(report(Stage::History, 1, 41), at(t0, 10_500));
        assert_eq!(t.wire(at(t0, 11_500)), None);
    }

    /// S35: a build over before it was ever published was never drawn, so
    /// it gets no segment, and the shares of the ones drawn still sum to
    /// 100. A radar's build is a decode and a send its poller already did.
    #[test]
    fn an_instant_build_gets_no_segment_and_the_shares_still_sum_to_100() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.begin(Kind::Radar, "Vara Reflectivity 0.5°", false, None);
        let first = t.wire(t0).unwrap();
        assert_eq!(
            first.stages.iter().map(seg).collect::<Vec<_>>(),
            vec![
                (Stage::First, 40, 0, SegmentState::Active),
                (Stage::Build, 10, 0, SegmentState::Waiting),
                (Stage::History, 50, 0, SegmentState::Waiting),
            ]
        );
        // The frame arrives and is on screen in one go.
        t.shown(at(t0, 200));
        let shown = t.wire(at(t0, 200)).unwrap();
        assert_eq!(shown.percent, 100);
        t.plan("Vara Reflectivity 0.5°", 11, at(t0, 300));
        let history = t.wire(at(t0, 1400)).unwrap();
        assert_eq!(history.stage, Stage::History);
        assert_eq!(
            history.stages.iter().map(seg).collect::<Vec<_>>(),
            vec![
                (Stage::First, 44, 100, SegmentState::Done),
                (Stage::History, 56, 0, SegmentState::Active),
            ],
            "no build segment, and 44 + 56 = 100"
        );
        assert_eq!(
            history.stages.iter().map(|s| s.share).sum::<u32>(),
            100,
            "a client lays the bar out from the shares alone"
        );
    }

    /// S35: a load that opened on a frame already on screen has no first
    /// stage and no build, so its bar is the history alone, full width.
    #[test]
    fn a_history_only_load_is_one_full_width_segment() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.begin(Kind::Radar, "Vara Rain mass", true, None);
        t.plan("Vara Rain mass", 4, at(t0, 100));
        let only = t.wire(at(t0, 100)).unwrap();
        assert_eq!(only.stages.iter().map(seg).collect::<Vec<_>>(), vec![
            (Stage::History, 100, 0, SegmentState::Active)
        ]);
    }

    /// S35: the segments are the load, not the client's view of it. The
    /// build ends when the frame reaches the timeline, which `main.rs`
    /// reports whether or not the timeline is following the newest frame,
    /// so a viewer scrubbed back still sees the bar advance.
    #[test]
    fn the_build_ends_without_the_client_looking_at_the_frame() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.begin(Kind::Made, "Nordic Rain mass", false, None);
        t.progress(report(Stage::First, 82, 82), at(t0, 1000));
        t.building("14:25Z", true, at(t0, 1100));
        assert_eq!(t.wire(at(t0, 2100)).unwrap().stage, Stage::Build);
        t.building("14:25Z", false, at(t0, 2200));
        // `shown` is the only signal, and it no longer waits on following.
        t.shown(at(t0, 2300));
        assert_eq!(t.wire(at(t0, 2300)).unwrap().percent, 100);
        t.progress(report(Stage::History, 2, 4), at(t0, 2400));
        assert_eq!(t.wire(at(t0, 3400)).unwrap().stage, Stage::History);
    }

    #[test]
    fn the_shares_sum_to_100_whatever_the_weights() {
        assert_eq!(shares(&[]), Vec::<u32>::new());
        assert_eq!(shares(&[7]), vec![100]);
        assert_eq!(shares(&[55, 20, 25]), vec![55, 20, 25]);
        assert_eq!(shares(&[40, 50]), vec![44, 56]);
        assert_eq!(shares(&[1, 1, 1]), vec![34, 33, 33]);
        for weights in [
            vec![40u32, 10, 50],
            vec![55, 20, 25],
            vec![10, 50],
            vec![0, 0, 1],
            vec![1; 7],
        ] {
            let got = shares(&weights);
            assert_eq!(got.iter().sum::<u32>(), 100, "{weights:?} -> {got:?}");
            assert!(got.iter().all(|s| *s >= 1), "{weights:?} -> {got:?}");
        }
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
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.begin(Kind::Made, "Nordic Rain mass", false, None);
        t.progress(
            Some(Progress {
                stage: Stage::First,
                done: 52,
                total: 82,
                label: "Nordic Rain mass: 26 of 40 radars in for 14:25Z (1 silent)".into(),
            }),
            at(t0, 1000),
        );
        assert_eq!(
            serde_json::to_string(&t.wire(at(t0, 1000)).unwrap()).unwrap(),
            r#"{"stage":"first","percent":63,"done":52,"total":82,"unit":"volumes","label":"Nordic Rain mass: 26 of 40 radars in for 14:25Z (1 silent)","stages":[{"stage":"first","share":55,"percent":63,"done":52,"total":82,"unit":"volumes","state":"active"},{"stage":"build","share":20,"percent":0,"done":0,"total":0,"unit":"steps","state":"waiting"},{"stage":"history","share":25,"percent":0,"done":0,"total":0,"unit":"volumes","state":"waiting"}]}"#
        );
    }
}
