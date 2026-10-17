//! GNU's basic/demand face lifecycle. Definition materialization never calls
//! Lisp; only creating a realized attribute combination crosses that boundary.

use super::*;
use crate::emacs_core::error::Flow;
use crate::face::{Color, Face, FaceDecoration, SpecifiedColor, UnderlineColorOperation};
use crate::gc_trace::GcTrace;
use strum::IntoEnumIterator;

/// GNU xfaces.c:5952-5983, deliberately independent of Lisp face IDs and the
/// renderer's older basic-face ID enum. Enum iteration derives count and order.
#[derive(Clone, Copy, Debug, strum::EnumIter, strum::IntoStaticStr)]
#[strum(serialize_all = "kebab-case")]
enum BasicFace {
    Default,
    ModeLineActive,
    ModeLineInactive,
    ToolBar,
    Fringe,
    HeaderLineActive,
    HeaderLineInactive,
    ScrollBar,
    Border,
    Cursor,
    Mouse,
    Menu,
    VerticalBorder,
    WindowDivider,
    WindowDividerFirstPixel,
    WindowDividerLastPixel,
    InternalBorder,
    ChildFrameBorder,
    TabBar,
    TabLineActive,
    TabLineInactive,
    Margin,
}

/// Temporary roots use the existing current-mutator root stack, rather than
/// storing Lisp state in a new TLS cache. Drop restores it on every exit.
#[must_use]
#[derive(Debug)]
struct RealizationRoots {
    saved_len: usize,
    _mutator: std::marker::PhantomData<std::rc::Rc<()>>,
}
static_assertions::assert_not_impl_any!(RealizationRoots: Send, Sync);
impl RealizationRoots {
    fn new(attributes: &Face) -> Self {
        let mut roots = Vec::new();
        attributes.trace_roots(&mut roots);
        Self::from_values(roots)
    }

    fn from_values(values: impl IntoIterator<Item = Value>) -> Self {
        let scope = Self {
            saved_len: crate::emacs_core::eval::save_scratch_gc_roots(),
            _mutator: Default::default(),
        };
        for value in values {
            crate::emacs_core::eval::push_scratch_gc_root(value);
        }
        scope
    }
}
impl Drop for RealizationRoots {
    fn drop(&mut self) {
        crate::emacs_core::eval::restore_scratch_gc_roots(self.saved_len);
    }
}

fn descriptor_color(desc: Value, color_cells: i64) -> Option<Color> {
    let items = list_to_vec(&desc)?;
    let index = items.get(1)?.as_fixnum()?;
    let terminal = TerminalColor::from_tty_color_desc(index, color_cells)?;
    let component = |at: usize| {
        items
            .get(at)?
            .as_fixnum()
            .map(|value| (value.clamp(0, 65535) / 257) as u8)
    };
    Some(Color::rgb(component(2)?, component(3)?, component(4)?).with_terminal(terminal))
}

/// map_tty_color (GNU xfaces.c:6639-6654) first uses an exact association in
/// tty-color-alist; only a missing association calls Lisp tty-color-desc.
/// No native palette approximation is permitted at this boundary.
fn map_color(
    eval: &mut super::super::eval::Context,
    frame: FrameId,
    spec: &SpecifiedColor,
) -> Result<Option<Color>, Flow> {
    let Some(name) = spec.spec_string() else {
        return Ok(match spec {
            SpecifiedColor::Rgb(r, g, b) => Some(Color::rgb(*r, *g, *b)),
            _ => None,
        });
    };
    let cells = crate::emacs_core::terminal::pure::builtin_tty_display_color_cells(
        eval,
        vec![Value::make_frame(frame.0)],
    )?
    .as_fixnum()
    .unwrap_or(0);
    if !name.is_empty()
        && eval
            .variable_value_by_name("tty-defined-color-alist")
            .is_some_and(|value| value.is_cons())
        && eval.obarray.fboundp("tty-color-alist")
    {
        let alist = eval.funcall_general(
            Value::symbol("tty-color-alist"),
            vec![Value::make_frame(frame.0)],
        )?;
        if let Some(entries) = list_to_vec(&alist)
            && let Some(desc) = entries
                .into_iter()
                .find(|entry| entry.is_cons() && entry.cons_car().as_utf8_str() == Some(name))
        {
            // Exact associations need only NAME/INDEX in GNU map_tty_color;
            // tty-color-define permits omitted RGB. Keep that terminal identity
            // even when there are no render-preview RGB components.
            let terminal = list_to_vec(&desc)
                .and_then(|items| items.get(1).and_then(|value| value.as_fixnum()))
                .and_then(|index| TerminalColor::from_tty_color_desc(index, cells));
            return Ok(terminal.map(|terminal| {
                descriptor_color(desc, cells).unwrap_or_else(|| {
                    Color::from_name(name)
                        .or_else(|| Color::from_hex(name))
                        .unwrap_or(Color::rgb(0, 0, 0))
                        .with_terminal(terminal)
                })
            }));
        }
    }
    if name.is_empty() || !eval.obarray.fboundp("tty-color-desc") {
        return Ok(None);
    }
    let descriptor = eval.funcall_general(
        Value::symbol("tty-color-desc"),
        vec![Value::string(name), Value::make_frame(frame.0)],
    )?;
    Ok(descriptor_color(descriptor, cells))
}

/// One dynamic binding in the frame's exclusive mutator. Normal Lisp exits
/// execute unbind watchers; Rust unwinding restores native storage without
/// invoking more Lisp. The borrow prevents migration or another mutator from
/// observing this temporary binding.
#[must_use = "finish the binding to propagate Lisp unwind conditions"]
struct RealizationBinding<'a> {
    context: &'a mut super::super::eval::Context,
    depth: Option<usize>,
}

impl std::fmt::Debug for RealizationBinding<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RealizationBinding")
            .field("depth", &self.depth)
            .finish_non_exhaustive()
    }
}
static_assertions::assert_not_impl_any!(RealizationBinding<'static>: Send, Sync, Copy);

impl<'a> RealizationBinding<'a> {
    fn enter(
        context: &'a mut super::super::eval::Context,
        variable: &str,
        value: Value,
    ) -> Result<Self, Flow> {
        let depth = context.specpdl.len();
        let mut binding = Self {
            context,
            depth: Some(depth),
        };
        binding.context().try_specbind_or_unwind_to(
            depth,
            crate::emacs_core::intern::intern(variable),
            value,
        )?;
        Ok(binding)
    }

    fn context(&mut self) -> &mut super::super::eval::Context {
        self.context
    }

    fn finish<T>(mut self, result: Result<T, Flow>) -> Result<T, Flow> {
        let cleanup_input = match &result {
            Ok(_) => Ok(Value::NIL),
            Err(flow) => Err(flow.clone()),
        };
        let cleanup = match self.depth {
            Some(depth) => self.context.unbind_to_with_result(depth, cleanup_input),
            None => cleanup_input,
        };
        self.depth = None;
        cleanup?;
        result
    }
}

impl Drop for RealizationBinding<'_> {
    fn drop(&mut self) {
        if let Some(depth) = self.depth.take() {
            self.context.discard_specpdl_to(depth);
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum RealizationLookup {
    Demand,
    Basic,
}

impl super::super::eval::Context {
    /// Initialize the frame cache and basic faces at the creation boundary.
    pub fn initialize_display_faces_for_frame(&mut self, frame: FrameId) {
        initialize_frame_faces(self, frame);
    }

    /// Read a complete cached realization without evaluating Lisp.
    pub fn cached_display_face_for_frame(&self, frame: FrameId, attributes: &Face) -> Option<Face> {
        let cache = &self.frames.get(frame)?.face_realization;
        cache
            .generation_matches(self.face_change_count)
            .then(|| cache.lookup(attributes))
            .flatten()
            .cloned()
    }

    /// Realize fully merged attributes on FRAME, caching the complete source
    /// attribute combination exactly like GNU lookup_face. The owning frame's
    /// exclusive mutator performs Lisp callbacks; no cross-mutator state exists.
    /// Initial placeholder frames use dummy faces (GNU xfaces.c:6205-6209).
    pub fn realize_display_face_for_frame(&mut self, frame: FrameId, attributes: &Face) -> Face {
        if self.redisplay_hook_flow_pending() {
            return attributes.clone();
        }
        match self.realize_display_face_with_lookup(frame, attributes, RealizationLookup::Demand) {
            Ok(realized) => realized,
            Err(flow) => {
                self.defer_redisplay_hook_flow(flow);
                attributes.clone()
            }
        }
    }

    fn realize_display_face_with_lookup(
        &mut self,
        frame: FrameId,
        attributes: &Face,
        lookup: RealizationLookup,
    ) -> Result<Face, Flow> {
        if !crate::face::realization::enabled()
            || self
                .frames
                .get(frame)
                .is_none_or(|frame| frame.initial || frame.effective_window_system().is_some())
        {
            return Ok(attributes.clone());
        }
        if matches!(lookup, RealizationLookup::Demand)
            && let Some(realized) = self.frames.get(frame).and_then(|frame| {
                frame
                    .face_realization
                    .generation_matches(self.face_change_count)
                    .then(|| frame.face_realization.lookup(attributes))
                    .flatten()
            })
        {
            return Ok(realized.clone());
        }
        let _roots = RealizationRoots::new(attributes);
        let generation = self.face_change_count;
        let mut realized = attributes.clone();
        // GNU realize_tty_face:6731-6803: underline BEFORE foreground/background.
        if let FaceDecoration::Enabled(underline) = &mut realized.underline {
            if let Some(operations) = &attributes.color_specifications.underline_operations {
                for operation in operations.iter() {
                    underline.color = match operation {
                        UnderlineColorOperation::Lookup(specification) => {
                            map_color(self, frame, specification)?
                        }
                        UnderlineColorOperation::Foreground => None,
                    };
                }
            } else if let Some(spec) = &attributes.color_specifications.underline {
                underline.color = map_color(self, frame, spec)?;
            }
        }
        if let Some(spec) = &attributes.color_specifications.foreground {
            realized.foreground = map_color(self, frame, spec)?;
        }
        if let Some(spec) = &attributes.color_specifications.background {
            realized.background = map_color(self, frame, spec)?;
        }
        if self.face_change_count == generation
            && let Some(frame) = self.frames.get_mut(frame)
        {
            frame
                .face_realization
                .insert(attributes.clone(), realized.clone());
        }
        Ok(realized)
    }
}

pub(crate) fn prepare_runtime_faces(
    eval: &mut super::super::eval::Context,
    frame: FrameId,
) -> bool {
    if eval.redisplay_hook_flow_pending() {
        return false;
    }
    match prepare_runtime_faces_result(eval, frame) {
        Ok(changed) => changed,
        Err(flow) => {
            eval.defer_redisplay_hook_flow(flow);
            false
        }
    }
}

/// Ordinary Lisp frame creation and parameter updates propagate realization
/// errors directly; only the display transaction wrapper defers their flow.
pub(crate) fn prepare_runtime_faces_result(
    eval: &mut super::super::eval::Context,
    frame: FrameId,
) -> Result<bool, Flow> {
    let generation = eval.face_change_count;
    if let Some(table) = eval
        .frames
        .get(frame)
        .and_then(|frame| frame.face_realization.table(generation))
        .cloned()
    {
        *eval.face_table = table;
        return Ok(false);
    }
    realize_default_lisp_face_for_frame(eval, frame);
    let table = runtime_face_table_from_frame_lisp_faces(eval, frame, false);
    if let Some(owner) = eval.frames.get_mut(frame) {
        owner.face_realization.prepare(generation, table.clone());
    }
    *eval.face_table = table;
    if let Err(flow) = realize_basic_faces(eval, frame) {
        if let Some(owner) = eval.frames.get_mut(frame) {
            owner.face_realization.invalidate();
        }
        return Err(flow);
    }
    Ok(true)
}

pub(crate) fn realize_basic_faces(
    eval: &mut super::super::eval::Context,
    frame: FrameId,
) -> Result<(), Flow> {
    let table = eval.face_table.clone();
    // Advice can invalidate or replace both runtime owners. Keep every source
    // in this local snapshot alive until all later basic faces have been read.
    let mut table_roots = Vec::new();
    table.trace_roots(&mut table_roots);
    let _table_roots = RealizationRoots::from_values(table_roots);
    let mut default = table.resolve("default");
    default.inherit = None;
    default.doc = None;
    // GNU realize_default_face inhibits redisplay while the default cache slot
    // is incomplete (xfaces.c:6105-6111). Advice may request redisplay here.
    let mut scope = RealizationBinding::enter(eval, "inhibit-redisplay", Value::T)?;
    let result =
        scope
            .context()
            .realize_display_face_with_lookup(frame, &default, RealizationLookup::Basic);
    scope.finish(result)?;

    // GNU's remaining basics are canonical even when the current displayed
    // buffer remaps faces, and callbacks observe that dynamic binding too.
    let mut scope = RealizationBinding::enter(eval, "face-remapping-alist", Value::NIL)?;
    let result: Result<(), Flow> = (|| {
        for basic in BasicFace::iter().filter(|basic| !matches!(basic, BasicFace::Default)) {
            let name: &'static str = basic.into();
            let mut attributes = default.merge(&table.resolve(name));
            attributes.inherit = None;
            attributes.doc = None;
            // Basic IDs always re-realize (realize_face rather than lookup).
            scope.context().realize_display_face_with_lookup(
                frame,
                &attributes,
                RealizationLookup::Basic,
            )?;
        }
        Ok(())
    })();
    scope.finish(result)
}

/// Eager frame-creation boundary (GNU init_frame_faces). Batch creation leaves
/// the cache uninitialized until the first iterator, frame.c:1822-1823.
pub(crate) fn initialize_frame_faces(eval: &mut super::super::eval::Context, frame: FrameId) {
    if crate::face::realization::enabled() && !eval.noninteractive() {
        prepare_runtime_faces(eval, frame);
    }
}

pub(crate) fn basic_face_names() -> impl Iterator<Item = &'static str> {
    BasicFace::iter().map(Into::into)
}

/// GNU creates basic faces before copying the selected frame's lface table
/// into a newly created terminal frame (frame.c:1822-1823,1886-1897).
/// This guard temporarily exposes that empty definition table and restores
/// both owners on every Rust exit. The exclusively borrowed Context prevents
/// another mutator from observing the temporary table.
#[must_use]
struct InitialTerminalFaces<'a> {
    context: &'a mut super::super::eval::Context,
    frame: FrameId,
    definitions: Value,
    foreground: Option<Value>,
    background: Option<Value>,
    runtime: crate::face::FaceTable,
    _roots: RealizationRoots,
}

impl std::fmt::Debug for InitialTerminalFaces<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InitialTerminalFaces")
            .field("frame", &self.frame)
            .field("runtime_faces", &self.runtime.len())
            .finish_non_exhaustive()
    }
}
static_assertions::assert_not_impl_any!(InitialTerminalFaces<'static>: Send, Sync, Copy);

impl<'a> InitialTerminalFaces<'a> {
    fn new(context: &'a mut super::super::eval::Context, frame: FrameId) -> Option<Self> {
        let definitions = context.frames.get(frame)?.face_hash_table;
        let runtime = (*context.face_table).clone();
        let owner = context.frames.get(frame)?;
        let foreground = owner.known_parameter(FrameParam::ForegroundColor);
        let background = owner.known_parameter(FrameParam::BackgroundColor);
        let mut roots = vec![definitions];
        roots.extend([foreground, background].into_iter().flatten());
        runtime.trace_roots(&mut roots);
        let roots = RealizationRoots::from_values(roots);
        let empty = Value::hash_table(HashTableTest::Eq);
        let owner = context.frames.get_mut(frame)?;
        owner.face_hash_table = empty;
        // GNU's first basic pass precedes Fmodify_frame_parameters. The plain
        // Rust constructor has stored these parameters already; hide them until
        // that pass finishes, then replay their actual parameter handlers.
        owner.remove_known_parameter(FrameParam::ForegroundColor);
        owner.remove_known_parameter(FrameParam::BackgroundColor);
        Some(Self {
            context,
            frame,
            definitions,
            foreground,
            background,
            runtime,
            _roots: roots,
        })
    }
}

impl Drop for InitialTerminalFaces<'_> {
    fn drop(&mut self) {
        if let Some(frame) = self.context.frames.get_mut(self.frame) {
            frame.face_hash_table = self.definitions;
            for (parameter, value) in [
                (FrameParam::ForegroundColor, self.foreground),
                (FrameParam::BackgroundColor, self.background),
            ] {
                match value {
                    Some(value) => {
                        frame.set_known_parameter(parameter, value);
                    }
                    None => {
                        frame.remove_known_parameter(parameter);
                    }
                }
            }
            frame.face_realization.invalidate();
        }
        *self.context.face_table = self.runtime.clone();
    }
}

pub(crate) fn initialize_new_terminal_frame_faces(
    context: &mut super::super::eval::Context,
    frame: FrameId,
    parameters: Value,
) -> Result<(), Flow> {
    if !crate::face::realization::enabled() {
        return Ok(());
    }
    let _roots = RealizationRoots::from_values([parameters]);
    if !context.noninteractive() {
        let Some(initial) = InitialTerminalFaces::new(context, frame) else {
            return Ok(());
        };
        for name in basic_face_names() {
            let _ = ensure_frame_lisp_face_vector(
                initial.context,
                frame,
                name,
                FrameFaceInitial::Empty,
            );
        }
        prepare_runtime_faces_result(initial.context, frame)?;
    }
    // Frame creation applies its color parameters only after the initial basic
    // faces and selected-frame face definitions exist (GNU frame.c:1886-1905).
    // Other parameter handlers already ran in the plain constructor.
    let colors: Vec<Value> = list_to_vec(&parameters)
        .unwrap_or_default()
        .into_iter()
        .filter(|pair| {
            pair.is_cons()
                && matches!(
                    pair.cons_car().as_bare_symbol_name(),
                    Some("foreground-color" | "background-color")
                )
        })
        .collect();
    if !colors.is_empty() {
        let color_parameters = Value::list(colors);
        let _replay_roots = RealizationRoots::from_values([color_parameters]);
        super::super::frame::builtin_modify_frame_parameters(
            context,
            vec![Value::make_frame(frame.0), color_parameters],
        )?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/realization_boundaries_test.rs"]
mod boundary_tests;
