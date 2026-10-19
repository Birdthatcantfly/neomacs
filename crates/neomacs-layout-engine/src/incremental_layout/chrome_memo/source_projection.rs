//! One render-call's source aliases, shared by the plain fingerprint and the
//! fresh transport bindings. Equality deliberately remains Lisp `equal`, as in
//! both old consumers; changing source identity is a separate semantic change.

//! | Knob | Default | Values | Gate |
//! | --- | --- | --- | --- |
//! | `NEOMACS_CHROME_SOURCE_PROJECTION` | `on` | `on`/`1`/`true`/`yes`; other values opt out | Share source alias classification only after plain fingerprint eligibility. |

use std::marker::PhantomData;
use std::rc::Rc;

use neomacs_display_protocol::glyph_matrix::GlyphStringId;
use neovm_core::emacs_core::xdisp::ModeLineDisplayOutput;
use neovm_core::emacs_core::{Context, Value};
use neovm_core::window::{PresentedWindowChromeArea, PresentedWindowChromeString};

use super::ChromeSpanShape;

/// Immutable process policy. Invalid values opt out; tests select it explicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChromeSourceProjectionMode {
    Off,
    On,
}

impl ChromeSourceProjectionMode {
    fn parse(value: Option<&std::ffi::OsStr>) -> Self {
        match value {
            None => Self::On,
            Some(value) => match value
                .to_str()
                .map(str::trim)
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("on" | "1" | "true" | "yes") => Self::On,
                _ => Self::Off,
            },
        }
    }
}

/// Read once; no retained Lisp state, cache, or mutable per-thread policy.
pub(crate) fn chrome_source_projection_mode() -> ChromeSourceProjectionMode {
    static MODE: std::sync::OnceLock<ChromeSourceProjectionMode> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| {
        ChromeSourceProjectionMode::parse(
            std::env::var_os("NEOMACS_CHROME_SOURCE_PROJECTION").as_deref(),
        )
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChromeSourceSpan {
    output_start: usize,
    output_end: usize,
    source_start: usize,
    string_id: GlyphStringId,
}

impl ChromeSourceSpan {
    #[inline]
    pub(crate) fn new(
        output_start: usize,
        output_end: usize,
        source_start: usize,
        string_id: GlyphStringId,
    ) -> Self {
        Self {
            output_start,
            output_end,
            source_start,
            string_id,
        }
    }

    #[inline]
    pub(crate) fn source_position(self, output_position: usize) -> Option<(GlyphStringId, usize)> {
        if output_position < self.output_start || output_position >= self.output_end {
            return None;
        }
        Some((
            self.string_id,
            self.source_start
                .saturating_add(output_position - self.output_start),
        ))
    }
}

/// Unrooted Values belong only to this synchronous render attempt. The caller
/// already scratch-roots the formatted root and every source across this span;
/// no Lisp/GC occurs between classification and `into_rooted`. Rc's zero-sized marker
/// prevents sending or sharing the plan across mutators. No Value enters the
/// retained fingerprint, and this type has no Clone implementation.
#[must_use]
#[derive(Debug)]
pub(crate) struct ChromeSourceBindings {
    values: Vec<Value>,
    string_ids: Vec<GlyphStringId>,
    spans: Vec<ChromeSourceSpan>,
    _mutator: PhantomData<Rc<()>>,
}

static_assertions::assert_not_impl_any!(ChromeSourceBindings: Send, Sync, Clone);

impl ChromeSourceBindings {
    /// Called at the original fresh-source installation site, after the memo
    /// decision and before scratch roots are restored or row callbacks run.
    pub(crate) fn into_rooted(
        self,
        area: PresentedWindowChromeArea,
        evaluator: &Context,
    ) -> Result<
        (Vec<PresentedWindowChromeString>, Vec<ChromeSourceSpan>),
        neovm_core::tagged::transport::SharedRootError,
    > {
        // SAFETY: the caller's installed mutator owns these scratch-rooted
        // formatter Values. Sharing allocates no Lisp and invokes no collection.
        let roots = unsafe { evaluator.share_values(&self.values) }?;
        let presented = self
            .string_ids
            .into_iter()
            .zip(roots)
            .map(|(id, root)| PresentedWindowChromeString::new(area, id, root))
            .collect();
        Ok((presented, self.spans))
    }
}

#[derive(Debug)]
pub(super) struct ChromeSourceProjection {
    pub(super) fingerprint_spans: Box<[ChromeSpanShape]>,
    pub(super) root_index: Option<usize>,
    pub(super) bindings: ChromeSourceBindings,
}

impl ChromeSourceProjection {
    pub(super) fn capture(formatted: &ModeLineDisplayOutput) -> Self {
        Self::classify(
            formatted.value(),
            formatted.source_spans().iter().map(|span| {
                (
                    span.source(),
                    span.output_start(),
                    span.output_end(),
                    span.source_start(),
                )
            }),
        )
    }

    fn classify(
        root: Value,
        spans: impl ExactSizeIterator<Item = (Value, usize, usize, usize)>,
    ) -> Self {
        // Reuse the two consumers' output vectors. The single temporary alias
        // vector replaces their independent source/source-id vectors; no extra
        // span clone or Arc is needed.
        let mut sources: Vec<(Value, GlyphStringId)> = Vec::new();
        let mut values = vec![root];
        let root_id = crate::display_row::root_lisp_string_id();
        let mut string_ids = vec![root_id];
        let mut fingerprint_spans = Vec::with_capacity(spans.len());
        let mut transport_spans = Vec::with_capacity(spans.len());
        let mut root_index = None;
        let mut non_root_sources = 0usize;
        for (source, output_start, output_end, source_start) in spans {
            let (index, string_id) = match sources
                .iter()
                .enumerate()
                .find(|(_, (seen, _))| *seen == source)
            {
                Some((index, (_, id))) => (index, *id),
                None => {
                    let index = sources.len();
                    let id = if source == root {
                        root_index = Some(index);
                        root_id
                    } else {
                        let id = GlyphStringId::new(non_root_sources.saturating_add(2) as u64);
                        non_root_sources = non_root_sources.saturating_add(1);
                        values.push(source);
                        string_ids.push(id);
                        id
                    };
                    sources.push((source, id));
                    (index, id)
                }
            };
            fingerprint_spans.push(ChromeSpanShape {
                output_start,
                output_end,
                source_start,
                source_index: u32::try_from(index).unwrap_or(u32::MAX),
            });
            transport_spans.push(ChromeSourceSpan::new(
                output_start,
                output_end,
                source_start,
                string_id,
            ));
        }
        Self {
            fingerprint_spans: fingerprint_spans.into_boxed_slice(),
            root_index,
            bindings: ChromeSourceBindings {
                values,
                string_ids,
                spans: transport_spans,
                _mutator: PhantomData,
            },
        }
    }
}

#[cfg(test)]
#[path = "../tests/chrome_source_projection_test.rs"]
mod tests;
