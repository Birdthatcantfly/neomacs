//! Whole text-run rendering for buffer source items.

use crate::buffer_source::item_append::BufferSourceRowAppendContext;
use crate::buffer_source::item_render::BufferSourceItemRenderOutcome;
use crate::display_cursor::{
    CapturedCursorInfo, CapturedCursorPlacement, CapturedCursorSlotWidth, CursorCaptureState,
    capture_cursor_info,
};
use crate::display_item::DisplaySourcePosition;
use crate::display_row::append_context::DisplayRowAppendKind;
use crate::display_row::builder::{
    DisplayRowAppendProgress, DisplayRowAppendStatus, DisplayRowPosition,
};
use crate::display_row::face_state::DisplayRowActiveFaceState;
use crate::display_row::face_state::DisplayRowExtendFace;
use crate::display_row::geometry::DisplayRowGeometryState;
use crate::display_row::source_render::TextRowSourceRenderState;
use crate::display_row::walk_state::{TrailingWhitespaceRenderState, WordWrapRenderState};
use crate::display_source::{DisplaySourceStepItem, DisplaySourceTextOrigin};
use crate::display_source_append_plan::DisplaySourceAppendRenderPolicy;
use crate::display_source_progress::DisplaySourceProgressState;
use crate::neovm_bridge::LayoutBufferView;

#[derive(Clone, Copy)]
pub(crate) struct BufferSourceTextRunRenderRequest {
    text_origin: DisplaySourceTextOrigin,
    point_charpos: i64,
    position: DisplayRowPosition,
    geometry: DisplayRowGeometryState,
}

/// Where a clipped whole-run append stopped consuming the run: the position of
/// the first character the row's right edge refused. `charpos` is
/// buffer-absolute and `byte_idx` is relative to the walk's text origin — the
/// [`crate::display_source::DisplaySourceTextPosition`] split the walk's own
/// progress state uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClippedRunResume {
    charpos: i64,
    byte_idx: usize,
}

impl ClippedRunResume {
    fn charpos(self) -> i64 {
        self.charpos
    }

    fn byte_idx(self) -> usize {
        self.byte_idx
    }
}

/// What the row's right edge did to a whole-run append.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WholeRunClip {
    /// The append drew through the run's end.
    Complete,
    /// The append stopped inside the run; everything before `resume` is on
    /// the row and the producer continues at `resume`.
    Clipped(ClippedRunResume),
    /// The row refused even the run's first glyph: it is already full. The
    /// run consumed nothing, so the caller must NOT reseat (an empty prefix
    /// would re-produce this run forever) — it drops to character
    /// granularity so the refused character's own overflow decision ends
    /// the row, the same element arm the former measure-then-split paths
    /// deferred to.
    RefusedAtEdge,
}

impl BufferSourceTextRunRenderRequest {
    pub(crate) fn new(
        text_start_byte: usize,
        point_charpos: i64,
        position: DisplayRowPosition,
        geometry: DisplayRowGeometryState,
    ) -> Self {
        Self {
            text_origin: DisplaySourceTextOrigin::new(text_start_byte),
            point_charpos,
            position,
            geometry,
        }
    }

    /// Whether `source_item` is a run the single-pass whole-run append can
    /// take: a multi-character non-composed `TextRun` whose source ends are
    /// known. The row-edge decision is NOT part of this gate — the append
    /// pipeline's writer itself stops at the right edge and reports the
    /// refusal, the rule GNU's `move_it_in_display_line_to` applies
    /// (it rejects a character once
    /// `it->current_x + pixel_width > it->last_visible_x`,
    /// src/xdisp.c:10422-10451).
    pub(crate) fn admits_whole_run_render(source_item: &DisplaySourceStepItem) -> bool {
        source_item.text_run().is_some()
            && source_item.source_end_charpos().is_some()
            && source_item.source_end_byte_idx().is_some()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_and_apply<
        B: LayoutBufferView + ?Sized,
        FS: crate::neovm_bridge::FaceServices,
    >(
        self,
        source_item: DisplaySourceStepItem,
        active_face_state: &DisplayRowActiveFaceState,
        append_context: &BufferSourceRowAppendContext<'_, '_, B>,
        cursor_info: &mut CursorCaptureState,
        trailing_whitespace: &mut TrailingWhitespaceRenderState,
        word_wrap: &mut WordWrapRenderState,
        predecessor_row_extend: Option<DisplayRowExtendFace>,
        source_render: &mut TextRowSourceRenderState<'_, FS>,
        progress: &mut DisplaySourceProgressState<'_>,
    ) -> BufferSourceItemRenderOutcome {
        let output_display_point_start = source_render.output_emitter().display_point_len();
        let output_row_positions_start = source_render
            .output_emitter()
            .current_row_display_positions();
        // Snapshot the row's drawn-glyph counts BEFORE this run is appended. The
        // whole-run word-wrap path records candidates after the append, so each
        // candidate's boundary adds the writer's primitive offset, which
        // includes wide-character padding and accounts for composition.
        let row_glyph_checkpoint_start = source_render.capture_glyph_checkpoint();
        let source_end_charpos = source_item.source_end_charpos();
        let source_end_byte_idx = source_item.source_end_byte_idx();
        let source_text = source_item.raw_text_run().unwrap_or_default().to_owned();
        let (_source_step_char, _, _, source_item) = source_item.into_render_parts();
        let mut render_policy = DisplaySourceAppendRenderPolicy::natural();
        let Some(append_progress) = append_context.append_source_display_item_to_text_row(
            &self.geometry,
            source_render,
            source_item,
            self.position,
            DisplayRowAppendKind::SourceText,
            &mut render_policy,
        ) else {
            return BufferSourceItemRenderOutcome::Stop;
        };
        let clip = whole_run_clip(&append_progress, &source_text, self.text_origin);
        let row_glyph_checkpoint_after_append = source_render.capture_glyph_checkpoint();
        capture_whole_text_run_cursor_if_point(
            cursor_info,
            active_face_state,
            &self.geometry,
            self.point_charpos,
            &append_progress,
        );
        // The slot zip below covers exactly the characters the row kept, so a
        // clipped append tracks (and records word-wrap candidates for) its
        // fitting prefix only, like the split prefix this path replaced.
        apply_whole_text_run_trailing_whitespace_state(
            &source_text,
            trailing_whitespace,
            &self.geometry,
            &append_progress,
        );
        crate::display_row::word_wrap::record_text_progress(
            &source_text,
            self.text_origin,
            word_wrap,
            output_display_point_start,
            output_row_positions_start,
            row_glyph_checkpoint_start,
            row_glyph_checkpoint_after_append,
            &append_progress,
            predecessor_row_extend,
            active_face_state.row_extend_fill(),
        );
        progress.apply_row_position(append_progress.end());
        match clip {
            WholeRunClip::Complete => {
                if let Some(end_charpos) = source_end_charpos {
                    progress.max_charpos(end_charpos);
                }
                if let Some(end_byte_idx) = source_end_byte_idx {
                    progress.set_byte_idx(end_byte_idx);
                }
                BufferSourceItemRenderOutcome::Rendered
            }
            WholeRunClip::Clipped(resume) => {
                // Only the fitting prefix was consumed: the walk position and
                // (via the caller's reseat) the producer both move to the
                // first unfitting character, whose own element arm runs the
                // row's overflow lifecycle from there.
                progress.max_charpos(resume.charpos());
                progress.set_byte_idx(resume.byte_idx());
                BufferSourceItemRenderOutcome::RenderedToRowEdge {
                    resume_charpos: resume.charpos(),
                }
            }
            WholeRunClip::RefusedAtEdge => BufferSourceItemRenderOutcome::RunRefusedAtRowEdge,
        }
    }
}

/// Classify the append against the row's right edge. The writer pushes one
/// source slot per run character it keeps, so the last emitted slot names the
/// last character on the row and the resume position is the one after it.
///
/// The resume follows the walk's coordinate split
/// ([`DisplaySourceTextPosition`]): `charpos` buffer-absolute, `byte_idx`
/// relative to the walk's text origin — the same conversion
/// [`DisplaySourceStepItem::source_end_byte_idx`] applies to run ends.
fn whole_run_clip(
    append_progress: &DisplayRowAppendProgress,
    source_text: &str,
    text_origin: DisplaySourceTextOrigin,
) -> WholeRunClip {
    if append_progress.status() != DisplayRowAppendStatus::Clipped {
        return WholeRunClip::Complete;
    }
    let Some(last) = append_progress.slots().last() else {
        return WholeRunClip::RefusedAtEdge;
    };
    let DisplaySourcePosition::Buffer {
        char_pos, byte_pos, ..
    } = last.source()
    else {
        return WholeRunClip::RefusedAtEdge;
    };
    let emitted = append_progress.emitted_glyphs();
    let (_, last_ch) = source_text
        .char_indices()
        .nth(emitted - 1)
        .expect("a text run append emits at most one slot per run character");
    // One past the last kept character, in buffer bytes; `position_from_buffer`
    // maps it into the walk's slice the way every other resume position is
    // mapped. A slot at or before the origin cannot happen for a kept prefix,
    // but a None still degrades safely to the element arm.
    match text_origin.position_from_buffer(
        neovm_core::buffer::EmacsBytePos::new(byte_pos.get() + last_ch.len_utf8()),
        neovm_core::buffer::CharPos0::new(char_pos.get() + 1),
    ) {
        Some(resume) => WholeRunClip::Clipped(ClippedRunResume {
            charpos: resume.charpos(),
            byte_idx: resume.byte_idx(),
        }),
        None => WholeRunClip::RefusedAtEdge,
    }
}

fn capture_whole_text_run_cursor_if_point(
    cursor_info: &mut CursorCaptureState,
    active_face_state: &DisplayRowActiveFaceState,
    geometry: &DisplayRowGeometryState,
    point_charpos: i64,
    append_progress: &DisplayRowAppendProgress,
) {
    let slot = append_progress.slots().iter().find(|slot| {
        let DisplaySourcePosition::Buffer { char_pos, .. } = slot.source() else {
            return false;
        };
        cursor_info.should_capture_visible_glyph_at(char_pos.get() as i64, point_charpos)
    });
    let Some(slot) = slot else {
        return;
    };
    let DisplaySourcePosition::Buffer { byte_pos, .. } = slot.source() else {
        return;
    };
    capture_cursor_info(
        cursor_info,
        CapturedCursorInfo::from_active_face_state(
            active_face_state,
            CapturedCursorPlacement::from_row_text_position(
                geometry.text_position(slot.x_px(), byte_pos.get(), slot.col()),
                CapturedCursorSlotWidth::Explicit(slot.width_px()),
                false,
            ),
        ),
    );
}

fn apply_whole_text_run_trailing_whitespace_state(
    text: &str,
    trailing_whitespace: &mut TrailingWhitespaceRenderState,
    geometry: &DisplayRowGeometryState,
    append_progress: &DisplayRowAppendProgress,
) {
    if !trailing_whitespace.is_enabled() {
        return;
    }
    for (ch, slot) in text.chars().zip(append_progress.slots()) {
        trailing_whitespace.track_rendered_char(ch, geometry.start_marker_at_x(slot.x_px()));
    }
}
