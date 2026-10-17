//! Route-coverage telemetry: the refusal taxonomy's reported buckets, the
//! process-wide counters behind NEOMACS_LAYOUT_STATS_FILE, and the test-only
//! engagement counters each routed capability class is proven by.
//!
//! Counting only. Nothing here decides whether a row routes.

use super::*;

/// The reported refusal buckets.
///
/// P4.8(d): the 21-reason histogram existed to RANK the next migration
/// increment — it answered "which refusal should the route learn to handle
/// next?". The migration is over, so what the counters are for has changed:
/// they now report standing coverage, and the question is whether a workload's
/// misses are the expected capability ones. Four buckets carry that (they are
/// the only reasons above 3% of corpus attempts) and everything else is
/// `Other`. The full [`RouteRefusal`] vocabulary survives unchanged for
/// control flow and for the per-reason assertions in row_route_test.rs; only
/// the REPORT collapses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RouteRefusalClass {
    /// The buffer has an active display table — the largest single class, and
    /// a whole-buffer capability fact rather than a per-row one.
    DisplayTable,
    /// Point sits inside the routed coverage. Not a capability limit: it
    /// tracks point, so it moves with the workload.
    PointInRow,
    /// A box face the route cannot reproduce.
    ProbeBoxFace,
    /// A hazard text property in range (display / mouse-face / line-height).
    HazardProp,
    /// Everything else: window policy, scan-ladder misses, overlays, elision,
    /// replacements, seams and the remaining probe refusals.
    Other,
}

impl RouteRefusalClass {
    const COUNT: usize = 5;

    const ALL: [RouteRefusalClass; Self::COUNT] = [
        RouteRefusalClass::DisplayTable,
        RouteRefusalClass::PointInRow,
        RouteRefusalClass::ProbeBoxFace,
        RouteRefusalClass::HazardProp,
        RouteRefusalClass::Other,
    ];

    fn index(self) -> usize {
        match self {
            RouteRefusalClass::DisplayTable => 0,
            RouteRefusalClass::PointInRow => 1,
            RouteRefusalClass::ProbeBoxFace => 2,
            RouteRefusalClass::HazardProp => 3,
            RouteRefusalClass::Other => 4,
        }
    }

    fn label(self) -> &'static str {
        match self {
            RouteRefusalClass::DisplayTable => "display_table",
            RouteRefusalClass::PointInRow => "point_in_row",
            RouteRefusalClass::ProbeBoxFace => "probe_box_face",
            RouteRefusalClass::HazardProp => "hazard_prop",
            RouteRefusalClass::Other => "other",
        }
    }
}

impl RouteRefusal {
    fn class(self) -> RouteRefusalClass {
        match self {
            RouteRefusal::DisplayTable => RouteRefusalClass::DisplayTable,
            RouteRefusal::PointInRow => RouteRefusalClass::PointInRow,
            RouteRefusal::ProbeBoxFace => RouteRefusalClass::ProbeBoxFace,
            RouteRefusal::HazardProp => RouteRefusalClass::HazardProp,
            RouteRefusal::PolicyHscroll
            | RouteRefusal::PolicySelectiveDisplay
            | RouteRefusal::PolicyWordWrap
            | RouteRefusal::PolicyTrailingWhitespace
            | RouteRefusal::ScanNoFitFirstChar
            | RouteRefusal::ScanExactFill
            | RouteRefusal::ScanChar
            | RouteRefusal::ScanCompose
            | RouteRefusal::ScanEob
            | RouteRefusal::ScanHorizon
            | RouteRefusal::Overlay
            | RouteRefusal::Elision
            | RouteRefusal::OverflowElision
            | RouteRefusal::Replacement
            | RouteRefusal::Boundary
            | RouteRefusal::ComposedSeam
            | RouteRefusal::ProbeFaceDiverges
            | RouteRefusal::UnrealizedFace
            | RouteRefusal::CommitRefused => RouteRefusalClass::Other,
        }
    }
}

/// Route-coverage telemetry, mirroring the NEOMACS_LAYOUT_STATS_FILE
/// pattern: when `NEOMACS_ROW_ROUTE_STATS_FILE` names a path, the counters
/// below accumulate (relaxed atomics, only touched when the file env is set
/// — a single cached-bool branch otherwise) and `engine.rs` appends one
/// CUMULATIVE line per accepted frame. Aggregation takes the LAST line per
/// pid, so multi-process suite runs sum cleanly.
pub(super) fn route_stats_file() -> Option<&'static str> {
    static FILE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    FILE.get_or_init(|| std::env::var("NEOMACS_ROW_ROUTE_STATS_FILE").ok())
        .as_deref()
        .filter(|path| !path.is_empty())
}

pub(super) static ROUTE_STAT_ATTEMPTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

pub(super) static ROUTE_STAT_SKIPPED: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

pub(super) static ROUTE_STAT_ROUTED: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

pub(super) static ROUTE_STAT_REFUSALS: [std::sync::atomic::AtomicUsize; RouteRefusalClass::COUNT] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; RouteRefusalClass::COUNT];

pub(super) fn note_route_attempt() {
    if route_stats_file().is_some() {
        ROUTE_STAT_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

pub(super) fn note_route_refusal(reason: RouteRefusal) {
    if route_stats_file().is_some() {
        ROUTE_STAT_REFUSALS[reason.class().index()]
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

pub(super) fn note_route_skipped() {
    if route_stats_file().is_some() {
        ROUTE_STAT_SKIPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    #[cfg(test)]
    ROUTE_SKIPPED_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Test-only engagement proof for the P4.8(b) refusal window: walk positions
/// the route was NOT re-attempted at because an earlier position on the same
/// line had already proven the range unroutable.
#[cfg(test)]
pub(crate) static ROUTE_SKIPPED_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// The cumulative telemetry line for this process, or `None` when the stats
/// file env is unset. Appended by the engine once per accepted frame.
pub(crate) fn route_stats_append_report() {
    use std::io::Write as _;
    let Some(path) = route_stats_file() else {
        return;
    };
    // `skipped` is reported alongside `attempts`, not inside it: a skipped
    // position never became an attempt (the refusal window answered for it).
    // An attempt now ends either routed or as a noted refusal: there is no
    // separate "stopped the walk" outcome any more, because a refused row is
    // rewound and left to the buffer pipeline.
    let mut line = format!(
        "row_route pid={} attempts={} skipped={} routed={}",
        std::process::id(),
        ROUTE_STAT_ATTEMPTS.load(std::sync::atomic::Ordering::Relaxed),
        ROUTE_STAT_SKIPPED.load(std::sync::atomic::Ordering::Relaxed),
        ROUTE_STAT_ROUTED.load(std::sync::atomic::Ordering::Relaxed),
    );
    for class in RouteRefusalClass::ALL {
        let count = ROUTE_STAT_REFUSALS[class.index()].load(std::sync::atomic::Ordering::Relaxed);
        line.push_str(&format!(" refuse_{}={}", class.label(), count));
    }
    {
        let (label, counter) = ("routed_mid_line", &ROUTE_STAT_ROUTED_MID_LINE);
        line.push_str(&format!(
            " {label}={}",
            counter.load(std::sync::atomic::Ordering::Relaxed)
        ));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Test-only engagement proof: rows actually rendered through the routed
/// item-renderer acquisition in this process. Lets the flag-on suite run
/// assert the route is exercised rather than silently unreachable.
#[cfg(test)]
pub(crate) static ROUTED_ROW_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Test-only engagement proof for the rewind: routed-row attempts that
/// appended at least one part and then restored their checkpoint.
#[cfg(test)]
pub(crate) static ROUTE_REWIND_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Test-only fault injection: attempts armed to refuse AFTER appending their
/// first part, which is what exercises the rewind with real glyphs already in
/// the row. `cargo nextest` runs one test per process, so this process-wide
/// counter cannot leak between tests.
#[cfg(test)]
static ROUTE_FORCED_COMMIT_REFUSALS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Arms [`ROUTE_FORCED_COMMIT_REFUSALS`] for `count` route attempts. The guard
/// disarms on drop, so a panicking test cannot leak the injection into a
/// later layout in the same process.
#[cfg(test)]
#[must_use = "dropping the guard disarms the injection"]
pub(crate) struct RoutedRowRefusalInjection(());

#[cfg(test)]
impl RoutedRowRefusalInjection {
    pub(crate) fn arm(count: usize) -> Self {
        ROUTE_FORCED_COMMIT_REFUSALS.store(count, std::sync::atomic::Ordering::Relaxed);
        Self(())
    }
}

#[cfg(test)]
impl Drop for RoutedRowRefusalInjection {
    fn drop(&mut self) {
        ROUTE_FORCED_COMMIT_REFUSALS.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Consume one armed forced refusal, if any. Called only from the route's
/// single render pass.
#[cfg(test)]
pub(crate) fn take_forced_commit_refusal() -> bool {
    let armed = ROUTE_FORCED_COMMIT_REFUSALS.load(std::sync::atomic::Ordering::Relaxed);
    if armed == 0 {
        return false;
    }
    ROUTE_FORCED_COMMIT_REFUSALS.store(armed - 1, std::sync::atomic::Ordering::Relaxed);
    true
}

/// Test-only engagement proof for the multi-face extension: routed rows that
/// rendered as MORE than one face segment.
#[cfg(test)]
pub(crate) static ROUTED_SEGMENTED_ROW_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Test-only engagement proof for the tab extension: routed rows containing
/// at least one TAB.
#[cfg(test)]
pub(crate) static ROUTED_TAB_ROW_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Test-only engagement proof for the wide-char extension: routed rows
/// containing at least one 2-column char.
#[cfg(test)]
pub(crate) static ROUTED_WIDE_ROW_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Test-only engagement proof for the P4.6 rung-4 un-refusal: routed rows
/// that carried at least one overlay-string anchor.
#[cfg(test)]
pub(crate) static ROUTED_OVERLAY_STRING_ROW_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Test-only engagement proof for the overlay-face extension: routed rows
/// intersected by at least one (face-only) overlay.
#[cfg(test)]
pub(crate) static ROUTED_OVERLAY_ROW_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Test-only engagement proof for the invisible-elision extension: routed
/// rows that elide at least one plain-invisible span.
#[cfg(test)]
pub(crate) static ROUTED_ELIDED_ROW_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Test-only engagement proof for the composed-cluster extension: routed
/// rows containing at least one merged zero-width extender.
#[cfg(test)]
pub(crate) static ROUTED_COMPOSED_ROW_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Test-only engagement proof for the phase-2f truncation extension: routed
/// overflow-prefix rows in a truncating window (the pipeline truncates at
/// the handoff).
#[cfg(test)]
pub(crate) static ROUTED_TRUNCATION_PREFIX_ROW_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Test-only engagement proof for the phase-2f continuation extension:
/// routed overflow-prefix rows in a wrapping window (the pipeline continues
/// the line at the handoff).
#[cfg(test)]
pub(crate) static ROUTED_WRAP_PREFIX_ROW_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Test-only engagement proof for the phase-2h EOB-tail extension: routed
/// newline-less tail rows ending at the source end.
#[cfg(test)]
pub(crate) static ROUTED_EOB_TAIL_ROW_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Test-only engagement proof for the increment 2i display-replacement
/// extension: routed rows containing at least one display-string replacement
/// rendered through the pipeline's replacement session.
#[cfg(test)]
pub(crate) static ROUTED_REPLACEMENT_ROW_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Test-only engagement proof for the P4.8(a) entry unification: rows routed
/// from a MID-LINE position (a wrapped line's continuation row, or a resume
/// after a display element), which the entry taxonomy used to gate.
#[cfg(test)]
pub(crate) static ROUTED_MID_LINE_START_ROW_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Telemetry twin of the test-only mid-line counter, reported on the stats
/// line as `routed_mid_line` so real workloads show what entry unification
/// contributes.
pub(super) static ROUTE_STAT_ROUTED_MID_LINE: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

pub(super) fn note_routed_row(plan: &PlainRowPlan, wrap_mode: LineWrapMode, mid_line_start: bool) {
    if route_stats_file().is_some() {
        ROUTE_STAT_ROUTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if mid_line_start {
            ROUTE_STAT_ROUTED_MID_LINE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    #[cfg(not(test))]
    let _ = wrap_mode;
    #[cfg(test)]
    {
        ROUTED_ROW_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if mid_line_start {
            ROUTED_MID_LINE_START_ROW_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if plan.is_segmented() {
            ROUTED_SEGMENTED_ROW_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if plan.has_tab {
            ROUTED_TAB_ROW_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if plan.has_wide {
            ROUTED_WIDE_ROW_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if plan.has_overlay {
            ROUTED_OVERLAY_ROW_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if plan.has_overlay_strings() {
            ROUTED_OVERLAY_STRING_ROW_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if plan.has_elision() {
            ROUTED_ELIDED_ROW_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if plan.has_composed() {
            ROUTED_COMPOSED_ROW_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if plan.is_overflow_handoff() {
            match wrap_mode {
                LineWrapMode::Truncate => {
                    ROUTED_TRUNCATION_PREFIX_ROW_COUNT
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                LineWrapMode::Wrap => {
                    ROUTED_WRAP_PREFIX_ROW_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
        if plan.is_end_of_source() {
            ROUTED_EOB_TAIL_ROW_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if plan.has_replacement() {
            ROUTED_REPLACEMENT_ROW_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    #[cfg(not(test))]
    let _ = plan;
}
