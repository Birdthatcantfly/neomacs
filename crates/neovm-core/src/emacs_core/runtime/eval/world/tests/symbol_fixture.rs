//! A real retained World with one scalar global and no Context/bootstrap/GC.
//! This fixture tests C2 authority handoff, not collection or evaluator setup.
use super::*;

impl Context {
    pub(crate) fn with_test_symbol_era<R>(
        body: impl for<'heap> FnOnce(crate::tagged::gc::ParallelHeapEra<'heap>, SymId) -> R,
    ) -> R {
        Self::with_test_symbol_backing_era(crate::tagged::gc::PlainSymbolBacking::Unprojected, body)
    }

    /// C3 setup only: the same small World, optionally projected before the
    /// era exists. No production cell accessor changes and no setup collection.
    pub(crate) fn with_test_symbol_backing_era<R>(
        kind: crate::tagged::gc::PlainSymbolBacking,
        body: impl for<'heap> FnOnce(crate::tagged::gc::ParallelHeapEra<'heap>, SymId) -> R,
    ) -> R {
        use crate::tagged::gc::{AliasableBox, ParallelHeapEra, TaggedHeap};

        let heap = AliasableBox::new(TaggedHeap::new());
        // Manager construction can allocate; install only the owning root.
        crate::tagged::gc::set_tagged_heap_root(heap.root());
        let mut obarray = Obarray::new();
        let id = crate::emacs_core::intern::intern("p3sym-c2-global");
        obarray.seed_value_id(id, TaggedValue::fixnum(73));
        let mut ready = WorldStorageBuilder {
            owned_roots: Default::default(),
            tagged_heap: heap,
            obarray,
            features: Vec::new(),
            watchers: VariableWatcherList::new(),
            active_variable_watchers: HashSet::new(),
            autoloads: AutoloadManager::new(),
            treesit: crate::emacs_core::treesit::TreeSitterManager::new(),
            frames: FrameManager::default(),
            coding_systems: CodingSystemManager::new(),
            macro_perf_stats: MacroPerfStats::default(),
            buffers: BufferManager::new_world_parts(),
            global: crate::emacs_core::eval::global_state::GlobalState::fresh(
                crate::keyboard::CommandLoop::new(),
            ),
        }
        .build();
        let world = Arc::clone(&ready.obarray.lease.0);
        if kind != crate::tagged::gc::PlainSymbolBacking::Unprojected {
            let mut table = world.thread_projections.borrow_mut();
            ready.obarray.with_dispatch_write(|obarray| {
                if kind == crate::tagged::gc::PlainSymbolBacking::VoidProjection {
                    obarray.seed_value_id(id, TaggedValue::UNBOUND);
                }
                // Use admission's actual plan: projecting an owned UNBOUND
                // word would issue a private descriptor instead of shared VOID.
                let plan = obarray.admission_plan(id).unwrap();
                obarray.project_admitted_cell(id, plan, &mut table);
            });
        }
        let heap = world.tagged_heap.root().as_ptr();
        // SAFETY: freshly constructed, sole serial owner, no marker or GC was
        // started; no Lisp/Context root source exists and no collection runs.
        let collector = unsafe { (*heap).drain_to_quiescent(|_| {}) }.unwrap();
        let exclusion = collector.exclude_concurrent_mark().unwrap();
        let retention = exclusion.retain().unwrap();
        drop(exclusion);
        world.baton_holder.set(Some(std::thread::current().id()));
        let keeper = Baton {
            _retention: retention,
            world,
        };
        // SAFETY: same original heap root, no collector/reference remains
        // from the preceding short exclusion extent.
        let collector = unsafe { (*heap).drain_to_quiescent(|_| {}) }.unwrap();
        // No Context exists in this fixture, hence no Context-private roots.
        // World storage is retained by the actual Baton, exactly as in C2.
        let frozen = FrozenLegacyRoots {
            heap: collector.heap_identity(),
            words: Vec::new(),
        };
        let era = ParallelHeapEra::admit_rooted(collector, keeper, frozen)
            .unwrap_or_else(|e| panic!("minimal symbol fixture admission: {:?}", e.reason));
        body(era, id)
        // ready owns the same World until after the body and keeper teardown.
    }
}
