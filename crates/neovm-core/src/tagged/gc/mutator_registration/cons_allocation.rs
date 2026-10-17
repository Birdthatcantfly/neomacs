//! Rooted single-cons allocation for admitted consumers; no Lisp or heap TLS.
use super::super::cons_storage::CatalogSource;
use super::super::*;
use super::*;
use crate::tagged::transport::{ConsRootSeal, RootedConsInputs, RootedSlot, SharedRoot};

/// Only a completely initialized exclusive cell can create this handoff.
/// The run loan remains active until the transport bridge installs its root.
pub(crate) struct InitializedCons<'run> {
    heap: HeapIdentity,
    value: TaggedValue,
    _run: PhantomData<&'run mut ()>,
    _thread: PhantomData<Rc<()>>,
}
impl InitializedCons<'_> {
    pub(crate) fn into_root_parts(self, _: ConsRootSeal) -> (HeapIdentity, TaggedValue) {
        (self.heap, self.value)
    }
}
static_assertions::assert_not_impl_any!(InitializedCons<'static>: Send, Sync, Clone, Copy);

impl RunAccess<'_, '_> {
    pub fn heap_identity(&self) -> HeapIdentity {
        self.owner.registry.heap
    }

    /// Allocate exactly one initialized cons and install its SharedRoot before
    /// returning. Operands remain rooted throughout; this invokes no Lisp,
    /// collection, Context accessor, payload reader or installed-heap lookup.
    #[cold]
    #[inline(never)]
    pub fn cons(
        &mut self,
        car: &SharedRoot,
        cdr: &SharedRoot,
    ) -> Result<SharedRoot, RegistrationError> {
        SharedRoot::cons_from_run(self, car, cdr)
    }

    /// The lasting single-cons primitive for rooted VM/frame or transport homes.
    /// All three homes are validated before any reservation or mutation.
    #[inline]
    pub fn cons_into<C: RootedSlot, D: RootedSlot, R: RootedSlot>(
        &mut self,
        car: &C,
        cdr: &D,
        result: &mut R,
    ) -> Result<(), RegistrationError> {
        crate::tagged::transport::cons_into(self, car, cdr, result)
    }

    #[inline]
    pub(crate) fn allocate_rooted_cons(
        &mut self,
        inputs: &RootedConsInputs<'_>,
    ) -> Result<InitializedCons<'_>, RegistrationError> {
        // run_access checked the thread before lending the unique !Send/!Sync
        // guard. Nothing can change its owner or release its loan while this
        // borrow exists. Keep asynchronous poison refusal without acquiring a
        // temporary std::thread::Thread/Arc on each cons.
        if self.owner.registry.poisoned.load(Ordering::Acquire) {
            return Err(RegistrationError::Poisoned);
        }
        if inputs.heap_identity() != self.heap_identity() {
            return Err(RegistrationError::WrongHeap);
        }
        let registry = self.owner.registry;
        if !self.owner.entry.live_run_loan.load(Ordering::Acquire) {
            return Err(RegistrationError::OutstandingLoan);
        }
        let needs_refill = self
            .owner
            .local
            .as_ref()
            .expect("owner storage")
            .catalog_region
            .as_ref()
            .is_none_or(|region| region.empty());
        if needs_refill {
            self.refill_rooted_cons_region()?;
        }
        let local = self.owner.local.as_mut().expect("owner storage");
        // SAFETY: this canonical RunAccess uniquely owns the stamped region.
        // Its storage stays in the registry catalog; only this operation takes
        // this cell. The rooted operands outlive initialization/registration.
        let cell = unsafe {
            local
                .catalog_region
                .as_mut()
                .expect("reserved region")
                .take_cell()
        }
        .ok_or(RegistrationError::Poisoned)?;
        let (car, cdr) = inputs.words();
        // SAFETY: the cell is exclusive and in bounds. Full initialization is
        // one raw write; no reference covers another owner's disjoint cells.
        unsafe {
            cell.write(ConsCell {
                car: TaggedValue::from_bits(car),
                cdr_or_next: crate::tagged::header::ConsCdrOrNext {
                    cdr: TaggedValue::from_bits(cdr),
                },
            });
        }
        Ok(InitializedCons {
            heap: registry.heap,
            // SAFETY: the preceding write initialized this exact owned cell.
            value: unsafe { TaggedValue::from_cons_ptr(cell) },
            _run: PhantomData,
            _thread: PhantomData,
        })
    }

    #[cold]
    #[inline(never)]
    fn refill_rooted_cons_region(&mut self) -> Result<(), RegistrationError> {
        let registry = self.owner.registry;
        let key = self.owner.entry.key;
        self.close_cons_region()?;
        let local = self.owner.local.as_mut().expect("owner storage");
        const MAX: usize = super::super::alloc_region::CONS_REGION_MAX_CELLS;
        // Refuse arithmetic overflow before reserving or charging storage.
        local
            .hot
            .allocated_count
            .checked_add(MAX)
            .ok_or(RegistrationError::Capacity)?;
        local
            .hot
            .bytes_since_gc
            .checked_add(MAX * size_of::<ConsCell>())
            .ok_or(RegistrationError::Capacity)?;
        if local.cold.young.id() != self.owner.entry.young {
            return Err(RegistrationError::StaleRegistration);
        }
        let young = local.cold.young.resident_mut()?;
        let region = registry.cons_catalog.grant(key, young, MAX)?;
        let n = region.total();
        local.hot.allocated_count += n;
        let slot = MemoryUseCountSlot::ConsCells.index();
        local.hot.memory_use_counts[slot] =
            local.hot.memory_use_counts[slot].wrapping_add(n as u64);
        local.hot.bytes_since_gc += n * size_of::<ConsCell>();
        local.region_stats.cons_refills += 1;
        local.region_stats.cons_cells_granted += n as u64;
        local.region_stats.cons_short_runs += u64::from(n < 8);
        match region.source() {
            CatalogSource::FreeList => local.region_stats.cons_from_free_list += 1,
            CatalogSource::BlockTail => local.region_stats.cons_from_block_tail += 1,
            CatalogSource::FreshBlock => local.region_stats.cons_from_fresh_block += 1,
        }
        local.catalog_region = Some(region);
        Ok(())
    }

    /// Explicitly return unhanded capacity before relinquishing the run or
    /// publishing an ACK. Drop never performs this potentially contended work.
    #[cold]
    #[inline(never)]
    pub fn close_cons_region(&mut self) -> Result<(), RegistrationError> {
        self.owner.check_thread()?;
        let local = self.owner.local.as_mut().expect("owner storage");
        let Some(region) = local.catalog_region.take() else {
            return Ok(());
        };
        let result = self
            .owner
            .registry
            .cons_catalog
            .close(self.owner.entry.key, region);
        let unused = match result {
            Ok(unused) => unused,
            Err((error, region)) => {
                local.catalog_region = Some(region);
                // close released every catalog/allocator guard first.
                self.owner.registry.poison();
                return Err(error);
            }
        };
        local.hot.allocated_count -= unused;
        local.hot.bytes_since_gc -= unused * size_of::<ConsCell>();
        let slot = MemoryUseCountSlot::ConsCells.index();
        local.hot.memory_use_counts[slot] =
            local.hot.memory_use_counts[slot].wrapping_sub(unused as u64);
        local.region_stats.cons_tail_closes += u64::from(unused != 0);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn cons_region_for_test(&self) -> Option<(usize, usize, usize)> {
        self.owner
            .local
            .as_ref()?
            .catalog_region
            .as_ref()
            .map(|r| r.bounds_for_test())
    }
}
