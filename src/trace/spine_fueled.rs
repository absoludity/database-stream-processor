//! An append-only collection of update batches.
//!
//! The `Spine` is a general-purpose trace implementation based on collection
//! and merging immutable batches of updates. It is generic with respect to the
//! batch type, and can be instantiated for any implementor of `trace::Batch`.
//!
//! ## Design
//!
//! This spine is represented as a list of layers, where each element in the
//! list is either
//!
//!   1. MergeState::Vacant  empty
//!   2. MergeState::Single  a single batch
//!   3. MergeState::Double  a pair of batches
//!
//! Each "batch" has the option to be `None`, indicating a non-batch that
//! nonetheless acts as a number of updates proportionate to the level at which
//! it exists (for bookkeeping).
//!
//! Each of the batches at layer i contains at most 2^i elements.
//!
//! Each batch at layer i is treated as if it contains exactly 2^i elements,
//! even though it may actually contain fewer elements. This allows us to
//! decouple the physical representation from logical amounts of effort invested
//! in each batch. It allows us to begin compaction and to reduce the number of
//! updates, without compromising our ability to continue to move updates along
//! the spine. We are explicitly making the trade-off that while some batches
//! might compact at lower levels, we want to treat them as if they contained
//! their full set of updates for accounting reasons (to apply work to higher
//! levels).
//!
//! We maintain the invariant that for any in-progress merge at level k there
//! should be fewer than 2^k records at levels lower than k. That is, even if we
//! were to apply an unbounded amount of effort to those records, we would not
//! have enough records to prompt a merge into the in-progress merge. Ideally,
//! we maintain the extended invariant that for any in-progress merge at level
//! k, the remaining effort required (number of records minus applied effort) is
//! less than the number of records that would need to be added to reach 2^k
//! records in layers below.
//!
//! ## Mathematics
//!
//! When a merge is initiated, there should be a non-negative *deficit* of
//! updates before the layers below could plausibly produce a new batch for the
//! currently merging layer. We must determine a factor of proportionality, so
//! that newly arrived updates provide at least that amount of "fuel"
//! towards the merging layer, so that the merge completes before lower levels
//! invade.
//!
//! ### Deficit:
//!
//! A new merge is initiated only in response to the completion of a prior
//! merge, or the introduction of new records from outside. The latter case is
//! special, and will maintain our invariant trivially, so we will focus on the
//! former case.
//!
//! When a merge at level k completes, assuming we have maintained our invariant
//! then there should be fewer than 2^k records at lower levels. The newly
//! created merge at level k+1 will require up to 2^k+2 units of work, and
//! should not expect a new batch until strictly more than 2^k records are
//! added. This means that a factor of proportionality of four should be
//! sufficient to ensure that the merge completes before a new merge is
//! initiated.
//!
//! When new records get introduced, we will need to roll up any batches at
//! lower levels, which we treat as the introduction of records. Each of these
//! virtual records introduced should either be accounted for the fuel it should
//! contribute, as it results in the promotion of batches closer to in-progress
//! merges.
//!
//! ### Fuel sharing
//!
//! We like the idea of applying fuel preferentially to merges at *lower*
//! levels, under the idea that they are easier to complete, and we benefit from
//! fewer total merges in progress. This does delay the completion of merges at
//! higher levels, and may not obviously be a total win. If we choose to
//! do this, we should make sure that we correctly account for completed merges
//! at low layers: they should still extract fuel from new updates even though
//! they have completed, at least until they have paid back any "debt" to higher
//! layers by continuing to provide fuel as updates arrive.

use std::{
    cell::RefCell,
    fmt::{Display, Formatter},
    mem::replace,
};

use crate::{
    lattice::Lattice,
    time::Timestamp,
    trace::{
        cursor::{Cursor, CursorList},
        Antichain, Batch, BatchReader, Merger, Trace, TraceReader,
    },
    NumEntries,
};
use deepsize::DeepSizeOf;
use textwrap::indent;

/// An append-only collection of update tuples.
///
/// A spine maintains a small number of immutable collections of update tuples,
/// merging the collections when two have similar sizes. In this way, it allows
/// the addition of more tuples, which may then be merged with other immutable
/// collections.
pub struct Spine<B>
where
    B: Batch,
{
    merging: Vec<MergeState<B>>,
    lower: Antichain<B::Time>,
    upper: Antichain<B::Time>,
    // Batches from `merging` stored in a flat array, for use by `SpineCursor`.
    // Any operation that modifies spine invalidates this vector (and the associated
    // cursor, if any).
    cursor_storage: RefCell<Vec<B>>,
    effort: usize,
    activator: Option<timely::scheduling::activate::Activator>,
    dirty: bool,
}

impl<B> Display for Spine<B>
where
    B: Batch + Display + Clone + 'static,
    B::Key: Ord,
    B::Val: Ord,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), std::fmt::Error> {
        let mut res = Vec::new();
        self.map_batches(|batch| {
            res.push(writeln!(
                f,
                "batch:\n{}",
                indent(&batch.to_string(), "    ")
            ))
        });
        res.into_iter().collect()
    }
}

// TODO.
impl<B> Clone for Spine<B>
where
    B: Batch,
{
    fn clone(&self) -> Self {
        unimplemented!()
    }
}

impl<B> DeepSizeOf for Spine<B>
where
    B: Batch + DeepSizeOf + Clone + 'static,
    B::Key: Ord,
    B::Val: Ord,
{
    fn deep_size_of_children(&self, _context: &mut deepsize::Context) -> usize {
        let mut result = 0;
        self.map_batches(|batch| result += batch.deep_size_of());
        result
    }
}

impl<B> NumEntries for Spine<B>
where
    B: Batch + DeepSizeOf + Clone + 'static,
    B::Key: Ord,
    B::Val: Ord,
{
    fn num_entries_shallow(&self) -> usize {
        let mut total = 0;
        self.map_batches(|batch| total += batch.len());
        total
    }

    fn num_entries_deep(&self) -> usize {
        self.num_entries_shallow()
    }

    const CONST_NUM_ENTRIES: Option<usize> = None;
}

impl<B> BatchReader for Spine<B>
where
    B: Batch + Clone + 'static,
    B::Key: Ord,
    B::Val: Ord,
{
    type Key = B::Key;
    type Val = B::Val;
    type Time = B::Time;
    type R = B::R;

    type Cursor = SpineCursor<B>;

    fn len(&self) -> usize {
        let mut result = 0;
        self.map_batches(|b| result += b.len());
        result
    }

    fn lower(&self) -> &Antichain<Self::Time> {
        &self.lower
    }

    fn upper(&self) -> &Antichain<Self::Time> {
        &self.upper
    }

    fn cursor(&self) -> Self::Cursor {
        let mut cursors = Vec::new();
        let mut storage = Vec::new();

        for merge_state in self.merging.iter().rev() {
            match merge_state {
                MergeState::Double(variant) => match variant {
                    MergeVariant::InProgress(batch1, batch2, _) => {
                        if !batch1.is_empty() {
                            cursors.push(batch1.cursor());
                            storage.push(batch1.clone());
                        }
                        if !batch2.is_empty() {
                            cursors.push(batch2.cursor());
                            storage.push(batch2.clone());
                        }
                    }
                    MergeVariant::Complete(Some(batch)) => {
                        if !batch.is_empty() {
                            cursors.push(batch.cursor());
                            storage.push(batch.clone());
                        }
                    }
                    MergeVariant::Complete(None) => {}
                },
                MergeState::Single(Some(batch)) => {
                    if !batch.is_empty() {
                        cursors.push(batch.cursor());
                        storage.push(batch.clone());
                    }
                }
                MergeState::Single(None) => {}
                MergeState::Vacant => {}
            }
        }

        *self.cursor_storage.borrow_mut() = storage;
        SpineCursor::new(cursors, self)
    }
}

impl<B> TraceReader for Spine<B>
where
    B: Batch + Clone + 'static,
    B::Key: Ord,
    B::Val: Ord,
{
    type Batch = B;

    fn map_batches<F: FnMut(&Self::Batch)>(&self, mut f: F) {
        for batch in self.merging.iter().rev() {
            match batch {
                MergeState::Double(MergeVariant::InProgress(batch1, batch2, _)) => {
                    f(batch1);
                    f(batch2);
                }
                MergeState::Double(MergeVariant::Complete(Some(batch))) => f(batch),
                MergeState::Single(Some(batch)) => f(batch),
                _ => {}
            }
        }
    }
}

impl<B: Batch> Spine<B> {
    fn cursor_storage_unchecked(&self) -> &Vec<B> {
        // Safety: references returned by this method should never escape this module
        // and should only ne used in non-reentrant code.
        unsafe { &*self.cursor_storage.as_ptr() }
    }
}

pub struct SpineCursor<B: Batch> {
    #[allow(clippy::type_complexity)]
    cursor: CursorList<B::Key, B::Val, B::Time, B::R, B::Cursor>,
}

impl<B: Batch> SpineCursor<B>
where
    B::Key: Ord,
    B::Val: Ord,
{
    fn new(cursors: Vec<B::Cursor>, spine: &Spine<B>) -> Self {
        Self {
            cursor: CursorList::new(cursors, spine.cursor_storage_unchecked()),
        }
    }
}

impl<B: Batch> Cursor<B::Key, B::Val, B::Time, B::R> for SpineCursor<B>
where
    B::Key: Ord,
    B::Val: Ord,
{
    type Storage = Spine<B>;

    #[inline]
    fn key_valid(&self, spine: &Self::Storage) -> bool {
        self.cursor.key_valid(spine.cursor_storage_unchecked())
    }
    #[inline]
    fn val_valid(&self, spine: &Self::Storage) -> bool {
        self.cursor.val_valid(spine.cursor_storage_unchecked())
    }

    #[inline]
    fn key<'a>(&self, spine: &'a Self::Storage) -> &'a B::Key {
        self.cursor.key(spine.cursor_storage_unchecked())
    }
    #[inline]
    fn val<'a>(&self, spine: &'a Self::Storage) -> &'a B::Val {
        self.cursor.val(spine.cursor_storage_unchecked())
    }
    #[inline]
    fn map_times<L: FnMut(&B::Time, &B::R)>(&mut self, spine: &Self::Storage, logic: L) {
        self.cursor
            .map_times(spine.cursor_storage_unchecked(), logic);
    }

    #[inline]
    fn weight(&mut self, spine: &Self::Storage) -> B::R
    where
        B::Time: PartialEq<()>,
    {
        self.cursor.weight(spine.cursor_storage_unchecked())
    }

    #[inline]
    fn step_key(&mut self, spine: &Self::Storage) {
        self.cursor.step_key(spine.cursor_storage_unchecked());
    }

    #[inline]
    fn seek_key(&mut self, spine: &Self::Storage, key: &B::Key) {
        self.cursor.seek_key(spine.cursor_storage_unchecked(), key);
    }

    #[inline]
    fn step_val(&mut self, spine: &Self::Storage) {
        self.cursor.step_val(spine.cursor_storage_unchecked());
    }

    #[inline]
    fn seek_val(&mut self, spine: &Self::Storage, val: &B::Val) {
        self.cursor.seek_val(spine.cursor_storage_unchecked(), val);
    }

    #[inline]
    fn rewind_keys(&mut self, spine: &Self::Storage) {
        self.cursor.rewind_keys(spine.cursor_storage_unchecked());
    }

    #[inline]
    fn rewind_vals(&mut self, spine: &Self::Storage) {
        self.cursor.rewind_vals(spine.cursor_storage_unchecked());
    }
}

impl<B> Trace for Spine<B>
where
    B: Batch + Clone + 'static,
    B::Key: Ord,
    B::Val: Ord,
{
    fn new(activator: Option<timely::scheduling::activate::Activator>) -> Self {
        Self::with_effort(1, activator)
    }

    fn recede_to(&mut self, frontier: &B::Time) {
        self.cursor_storage.borrow_mut().clear();

        // Complete all in-progress merges, as we don't have an easy way to update
        // timestamps in an ongoing merge.
        self.complete_merges();

        self.map_batches_mut(|b| b.recede_to(frontier));
    }

    /// Apply some amount of effort to trace maintenance.
    ///
    /// The units of effort are updates, and the method should be
    /// thought of as analogous to inserting as many empty updates,
    /// where the trace is permitted to perform proportionate work.
    fn exert(&mut self, effort: &mut isize) {
        self.cursor_storage.borrow_mut().clear();

        // If there is work to be done, ...
        self.tidy_layers();
        if !self.reduced() {
            // If any merges exist, we can directly call `apply_fuel`.
            if self.merging.iter().any(|b| b.is_double()) {
                self.apply_fuel(effort);
            }
            // Otherwise, we'll need to introduce fake updates to move merges along.
            else {
                // Introduce an empty batch with roughly *effort number of virtual updates.
                let level = (*effort as usize).next_power_of_two().trailing_zeros() as usize;
                self.introduce_batch(None, level);
            }
            // We were not in reduced form, so let's check again in the future.
            if let Some(activator) = &self.activator {
                activator.activate();
            }
        }
    }

    fn consolidate(mut self) -> Option<Self::Batch> {
        self.cursor_storage.borrow_mut().clear();

        // Merge batches until there is nothing left to merge.
        let mut fuel = isize::max_value();
        while !self.reduced() {
            self.exert(&mut fuel);
        }
        // Return the sole remaining batch (if one exists).
        for merging in self.merging.into_iter() {
            if let MergeState::Single(Some(batch)) = merging {
                if !batch.is_empty() {
                    return Some(batch);
                }
            }
        }

        // Consolidated trace is empty.
        None
    }

    // Ideally, this method acts as insertion of `batch`, even if we are not yet
    // able to begin merging the batch. This means it is a good time to perform
    // amortized work proportional to the size of batch.
    fn insert(&mut self, batch: Self::Batch) {
        assert!(batch.lower() != batch.upper());

        self.cursor_storage.borrow_mut().clear();

        // Ignore empty batches.
        // Note: we may want to use empty batches to artificially force compaction.
        if batch.is_empty() {
            return;
        }

        self.dirty = true;
        self.lower = self.lower.meet(batch.lower());
        self.upper = self.upper.join(batch.upper());

        // Leonid: we do not require batch bounds to grow monotonically.
        //assert_eq!(batch.lower(), &self.upper);

        let index = batch.len().next_power_of_two();
        self.introduce_batch(Some(batch), index.trailing_zeros() as usize);

        // If more than one batch remains reschedule ourself.
        if !self.reduced() {
            if let Some(activator) = &self.activator {
                activator.activate();
            }
        }
    }

    fn clear_dirty_flag(&mut self) {
        self.dirty = false;
    }

    fn dirty(&self) -> bool {
        self.dirty
    }
}

impl<B> Spine<B>
where
    B: Batch + Clone + 'static,
    B::Key: Ord,
    B::Val: Ord,
{
    /// True iff there is at most one non-empty batch in `self.merging`.
    ///
    /// When true, there is no maintenance work to perform in the trace, other
    /// than compaction. We do not yet have logic in place to determine if
    /// compaction would improve a trace, so for now we are ignoring that.
    fn reduced(&self) -> bool {
        let mut non_empty = 0;
        for index in 0..self.merging.len() {
            if self.merging[index].is_double() {
                return false;
            }
            if self.merging[index].len() > 0 {
                non_empty += 1;
            }
            if non_empty > 1 {
                return false;
            }
        }
        true
    }

    /// Describes the merge progress of layers in the trace.
    ///
    /// Intended for diagnostics rather than public consumption.
    #[allow(dead_code)]
    fn describe(&self) -> Vec<(usize, usize)> {
        self.merging
            .iter()
            .map(|b| match b {
                MergeState::Vacant => (0, 0),
                x @ MergeState::Single(_) => (1, x.len()),
                x @ MergeState::Double(_) => (2, x.len()),
            })
            .collect()
    }

    /// Allocates a fueled `Spine` with a specified effort multiplier.
    ///
    /// This trace will merge batches progressively, with each inserted batch
    /// applying a multiple of the batch's length in effort to each merge.
    /// The `effort` parameter is that multiplier. This value should be at
    /// least one for the merging to happen; a value of zero is not helpful.
    pub fn with_effort(
        mut effort: usize,
        activator: Option<timely::scheduling::activate::Activator>,
    ) -> Self {
        // Zero effort is .. not smart.
        if effort == 0 {
            effort = 1;
        }

        Spine {
            cursor_storage: RefCell::new(Vec::new()),
            lower: Antichain::from_elem(B::Time::minimum()),
            upper: Antichain::new(),
            merging: Vec::new(),
            effort,
            activator,
            dirty: false,
        }
    }

    /// Introduces a batch at an indicated level.
    ///
    /// The level indication is often related to the size of the batch, but
    /// it can also be used to artificially fuel the computation by supplying
    /// empty batches at non-trivial indices, to move merges along.
    pub fn introduce_batch(&mut self, batch: Option<B>, batch_index: usize) {
        // Step 0.  Determine an amount of fuel to use for the computation.
        //
        //          Fuel is used to drive maintenance of the data structure,
        //          and in particular are used to make progress through merges
        //          that are in progress. The amount of fuel to use should be
        //          proportional to the number of records introduced, so that
        //          we are guaranteed to complete all merges before they are
        //          required as arguments to merges again.
        //
        //          The fuel use policy is negotiable, in that we might aim
        //          to use relatively less when we can, so that we return
        //          control promptly, or we might account more work to larger
        //          batches. Not clear to me which are best, of if there
        //          should be a configuration knob controlling this.

        // The amount of fuel to use is proportional to 2^batch_index, scaled
        // by a factor of self.effort which determines how eager we are in
        // performing maintenance work. We need to ensure that each merge in
        // progress receives fuel for each introduced batch, and so multiply
        // by that as well.
        // Leonid: We deliberately hit this path in `consolidate`.
        /*
        if batch_index > 32 {
            println!("Large batch index: {}", batch_index);
        }
        */

        // We believe that eight units of fuel is sufficient for each introduced
        // record, accounted as four for each record, and a potential four more
        // for each virtual record associated with promoting existing smaller
        // batches. We could try and make this be less, or be scaled to merges
        // based on their deficit at time of instantiation. For now, we remain
        // conservative.
        let mut fuel = 8 << batch_index;
        // Scale up by the effort parameter, which is calibrated to one as the
        // minimum amount of effort.
        fuel *= self.effort;
        // Convert to an `isize` so we can observe any fuel shortfall.
        let mut fuel = fuel as isize;

        // Step 1.  Apply fuel to each in-progress merge.
        //
        //          Before we can introduce new updates, we must apply any
        //          fuel to in-progress merges, as this fuel is what ensures
        //          that the merges will be complete by the time we insert
        //          the updates.
        self.apply_fuel(&mut fuel);

        // Step 2.  We must ensure the invariant that adjacent layers do not
        //          contain two batches will be satisfied when we insert the
        //          batch. We forcibly completing all merges at layers lower
        //          than and including `batch_index`, so that the new batch
        //          is inserted into an empty layer.
        //
        //          We could relax this to "strictly less than `batch_index`"
        //          if the layer above has only a single batch in it, which
        //          seems not implausible if it has been the focus of effort.
        //
        //          This should be interpreted as the introduction of some
        //          volume of fake updates, and we will need to fuel merges
        //          by a proportional amount to ensure that they are not
        //          surprised later on. The number of fake updates should
        //          correspond to the deficit for the layer, which perhaps
        //          we should track explicitly.
        self.roll_up(batch_index);

        // Step 3. This insertion should be into an empty layer. It is a
        //         logical error otherwise, as we may be violating our
        //         invariant, from which all wonderment derives.
        self.insert_at(batch, batch_index);

        // Step 4. Tidy the largest layers.
        //
        //         It is important that we not tidy only smaller layers,
        //         as their ascension is what ensures the merging and
        //         eventual compaction of the largest layers.
        self.tidy_layers();
    }

    /// Ensures that an insertion at layer `index` will succeed.
    ///
    /// This method is subject to the constraint that all existing batches
    /// should occur at higher levels, which requires it to "roll up" batches
    /// present at lower levels before the method is called. In doing this,
    /// we should not introduce more virtual records than 2^index, as that
    /// is the amount of excess fuel we have budgeted for completing merges.
    fn roll_up(&mut self, index: usize) {
        // Ensure entries sufficient for `index`.
        while self.merging.len() <= index {
            self.merging.push(MergeState::Vacant);
        }

        // We only need to roll up if there are non-vacant layers.
        if self.merging[..index].iter().any(|m| !m.is_vacant()) {
            // Collect and merge all batches at layers up to but not including `index`.
            let mut merged = None;
            for i in 0..index {
                self.insert_at(merged, i);
                merged = self.complete_at(i);
            }

            // The merged results should be introduced at level `index`, which should
            // be ready to absorb them (possibly creating a new merge at the time).
            self.insert_at(merged, index);

            // If the insertion results in a merge, we should complete it to ensure
            // the upcoming insertion at `index` does not panic.
            if self.merging[index].is_double() {
                let merged = self.complete_at(index);
                self.insert_at(merged, index + 1);
            }
        }
    }

    /// Applies an amount of fuel to merges in progress.
    ///
    /// The supplied `fuel` is for each in progress merge, and if we want to
    /// spend the fuel non-uniformly (e.g. prioritizing merges at low
    /// layers) we could do so in order to maintain fewer batches on average
    /// (at the risk of completing merges of large batches later, but tbh
    /// probably not much later).
    pub fn apply_fuel(&mut self, fuel: &mut isize) {
        // For the moment our strategy is to apply fuel independently to each merge
        // in progress, rather than prioritizing small merges. This sounds like a
        // great idea, but we need better accounting in place to ensure that merges
        // that borrow against later layers but then complete still "acquire" fuel
        // to pay back their debts.
        for index in 0..self.merging.len() {
            // Give each level independent fuel, for now.
            let mut fuel = *fuel;
            // Pass along various logging stuffs, in case we need to report success.
            self.merging[index].work(&mut fuel);
            // `fuel` could have a deficit at this point, meaning we over-spent when
            // we took a merge step. We could ignore this, or maintain the deficit
            // and account future fuel against it before spending again. It isn't
            // clear why that would be especially helpful to do; we might want to
            // avoid overspends at multiple layers in the same invocation (to limit
            // latencies), but there is probably a rich policy space here.

            // If a merge completes, we can immediately merge it in to the next
            // level, which is "guaranteed" to be complete at this point, by our
            // fueling discipline.
            if self.merging[index].is_complete() {
                let complete = self.complete_at(index);
                self.insert_at(complete, index + 1);
            }
        }
    }

    /// Inserts a batch at a specific location.
    ///
    /// This is a non-public internal method that can panic if we try and insert
    /// into a layer which already contains two batches (and is still in the
    /// process of merging).
    fn insert_at(&mut self, batch: Option<B>, index: usize) {
        // Ensure the spine is large enough.
        while self.merging.len() <= index {
            self.merging.push(MergeState::Vacant);
        }

        // Insert the batch at the location.
        match self.merging[index].take() {
            MergeState::Vacant => {
                self.merging[index] = MergeState::Single(batch);
            }
            MergeState::Single(old) => {
                self.merging[index] = MergeState::begin_merge(old, batch);
            }
            MergeState::Double(_) => {
                panic!("Attempted to insert batch into incomplete merge!")
            }
        };
    }

    /// Completes and extracts what ever is at layer `index`.
    fn complete_at(&mut self, index: usize) -> Option<B> {
        self.merging[index].complete()
    }

    /// Attempts to draw down large layers to size appropriate layers.
    fn tidy_layers(&mut self) {
        // If the largest layer is complete (not merging), we can attempt
        // to draw it down to the next layer. This is permitted if we can
        // maintain our invariant that below each merge there are at most
        // half the records that would be required to invade the merge.
        if !self.merging.is_empty() {
            let mut length = self.merging.len();
            if self.merging[length - 1].is_single() {
                // To move a batch down, we require that it contain few
                // enough records that the lower level is appropriate,
                // and that moving the batch would not create a merge
                // violating our invariant.

                let appropriate_level = self.merging[length - 1]
                    .len()
                    .next_power_of_two()
                    .trailing_zeros() as usize;

                // Continue only as far as is appropriate
                while appropriate_level < length - 1 {
                    match self.merging[length - 2].take() {
                        // Vacant or structurally empty batches can be absorbed.
                        MergeState::Vacant | MergeState::Single(None) => {
                            self.merging.remove(length - 2);
                            length = self.merging.len();
                        }
                        // Single batches may initiate a merge, if sizes are
                        // within bounds, but terminate the loop either way.
                        MergeState::Single(Some(batch)) => {
                            // Determine the number of records that might lead
                            // to a merge. Importantly, this is not the number
                            // of actual records, but the sum of upper bounds
                            // based on indices.
                            let mut smaller = 0;
                            for (index, batch) in self.merging[..(length - 2)].iter().enumerate() {
                                match batch {
                                    MergeState::Vacant => {}
                                    MergeState::Single(_) => {
                                        smaller += 1 << index;
                                    }
                                    MergeState::Double(_) => {
                                        smaller += 2 << index;
                                    }
                                }
                            }

                            if smaller <= (1 << length) / 8 {
                                self.merging.remove(length - 2);
                                self.insert_at(Some(batch), length - 2);
                            } else {
                                self.merging[length - 2] = MergeState::Single(Some(batch));
                            }
                            return;
                        }
                        // If a merge is in progress there is nothing to do.
                        MergeState::Double(state) => {
                            self.merging[length - 2] = MergeState::Double(state);
                            return;
                        }
                    }
                }
            }
        }
    }

    /// Complete all in-progress merges (without starting any new ones).
    fn complete_merges(&mut self) {
        for merge_state in self.merging.iter_mut() {
            if merge_state.is_inprogress() {
                let mut fuel = isize::max_value();
                merge_state.work(&mut fuel);
            }
        }
        assert!(self.merging.iter().all(|m| !m.is_inprogress()));
    }

    /// Mutate all batches.  Can only be invoked when there are no in-progress
    /// matches in the trait.
    fn map_batches_mut<F: FnMut(&mut <Self as TraceReader>::Batch)>(&mut self, mut f: F) {
        for batch in self.merging.iter_mut().rev() {
            match batch {
                MergeState::Double(MergeVariant::InProgress(_batch1, _batch2, _)) => {
                    panic!("map_batches_mut called on an in-progress batch")
                }
                MergeState::Double(MergeVariant::Complete(Some(batch))) => f(batch),
                MergeState::Single(Some(batch)) => f(batch),
                _ => {}
            }
        }
    }
}

/// Describes the state of a layer.
///
/// A layer can be empty, contain a single batch, or contain a pair of batches
/// that are in the process of merging into a batch for the next layer.
enum MergeState<B: Batch> {
    /// An empty layer, containing no updates.
    Vacant,
    /// A layer containing a single batch.
    ///
    /// The `None` variant is used to represent a structurally empty batch
    /// present to ensure the progress of maintenance work.
    Single(Option<B>),
    /// A layer containing two batches, in the process of merging.
    Double(MergeVariant<B>),
}

impl<B: Batch> MergeState<B> {
    /// The number of actual updates contained in the level.
    fn len(&self) -> usize {
        match self {
            MergeState::Single(Some(b)) => b.len(),
            MergeState::Double(MergeVariant::InProgress(b1, b2, _)) => b1.len() + b2.len(),
            MergeState::Double(MergeVariant::Complete(Some(b))) => b.len(),
            _ => 0,
        }
    }

    /// True only for the MergeState::Vacant variant.
    fn is_vacant(&self) -> bool {
        matches!(self, MergeState::Vacant)
    }

    /// True only for the MergeState::Single variant.
    fn is_single(&self) -> bool {
        matches!(self, MergeState::Single(_))
    }

    /// True only for the MergeState::Double variant.
    fn is_double(&self) -> bool {
        matches!(self, MergeState::Double(_))
    }

    /// Immediately complete any merge.
    ///
    /// The result is either a batch, if there is a non-trivial batch to return
    /// or `None` if there is no meaningful batch to return. This does not
    /// distinguish between Vacant entries and structurally empty batches,
    /// which should be done with the `is_complete()` method.
    ///
    /// There is the addional option of input batches.
    fn complete(&mut self) -> Option<B> {
        match replace(self, MergeState::Vacant) {
            MergeState::Vacant => None,
            MergeState::Single(batch) => batch,
            MergeState::Double(variant) => variant.complete(),
        }
    }

    /// True iff the layer is a complete merge, ready for extraction.
    fn is_complete(&self) -> bool {
        matches!(self, MergeState::Double(MergeVariant::Complete(_)))
    }

    /// True iff the layer is an in-progress merge.
    fn is_inprogress(&self) -> bool {
        matches!(self, MergeState::Double(MergeVariant::InProgress(..)))
    }

    /// Performs a bounded amount of work towards a merge.
    ///
    /// If the merge completes, the resulting batch is returned.
    /// If a batch is returned, it is the obligation of the caller
    /// to correctly install the result.
    fn work(&mut self, fuel: &mut isize) {
        // We only perform work for merges in progress.
        if let MergeState::Double(layer) = self {
            layer.work(fuel)
        }
    }

    /// Extract the merge state, typically temporarily.
    fn take(&mut self) -> Self {
        replace(self, MergeState::Vacant)
    }

    /// Initiates the merge of an "old" batch with a "new" batch.
    ///
    /// The upper frontier of the old batch should match the lower
    /// frontier of the new batch, with the resulting batch describing
    /// their composed interval, from the lower frontier of the old
    /// batch to the upper frontier of the new batch.
    ///
    /// Either batch may be `None` which corresponds to a structurally
    /// empty batch whose upper and lower froniers are equal. This
    /// option exists purely for bookkeeping purposes, and no computation
    /// is performed to merge the two batches.
    fn begin_merge(batch1: Option<B>, batch2: Option<B>) -> MergeState<B> {
        let variant = match (batch1, batch2) {
            (Some(batch1), Some(batch2)) => {
                // Leonid: we do not require batch bounds to grow monotonically.
                //assert!(batch1.upper() == batch2.lower());

                let begin_merge = <B as Batch>::begin_merge(&batch1, &batch2);
                MergeVariant::InProgress(batch1, batch2, begin_merge)
            }
            (None, Some(x)) => MergeVariant::Complete(Some(x)),
            (Some(x), None) => MergeVariant::Complete(Some(x)),
            (None, None) => MergeVariant::Complete(None),
        };

        MergeState::Double(variant)
    }
}

enum MergeVariant<B: Batch> {
    /// Describes an actual in-progress merge between two non-trivial batches.
    InProgress(B, B, <B as Batch>::Merger),
    /// A merge that requires no further work. May or may not represent a
    /// non-trivial batch.
    Complete(Option<B>),
}

impl<B: Batch> MergeVariant<B> {
    /// Completes and extracts the batch, unless structurally empty.
    ///
    /// The result is either `None`, for structurally empty batches,
    /// or a batch and optionally input batches from which it derived.
    fn complete(mut self) -> Option<B> {
        let mut fuel = isize::max_value();
        self.work(&mut fuel);
        if let MergeVariant::Complete(batch) = self {
            batch
        } else {
            panic!("Failed to complete a merge!");
        }
    }

    /// Applies some amount of work, potentially completing the merge.
    ///
    /// In case the work completes, the source batches are returned.
    /// This allows the caller to manage the released resources.
    fn work(&mut self, fuel: &mut isize) {
        let variant = replace(self, MergeVariant::Complete(None));
        if let MergeVariant::InProgress(b1, b2, mut merge) = variant {
            merge.work(&b1, &b2, fuel);
            if *fuel > 0 {
                *self = MergeVariant::Complete(Some(merge.done()));
            } else {
                *self = MergeVariant::InProgress(b1, b2, merge);
            }
        } else {
            *self = variant;
        }
    }
}
