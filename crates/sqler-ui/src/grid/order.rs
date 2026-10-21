// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! `GridOrder`: the sorted and filtered view of a grid's loaded rows.
//!
//! The order is a published list of model-row indices (`Vec<u32>`) plus its inverse, and it
//! implements [`RowOrder`] through [`DisplayRows`], so selection, scrolling and rendering see
//! display rows while the model keeps its rows untouched. The model only appends, so the indices
//! stay valid while it grows.
//!
//! The work runs on the caller's thread as a resumable job: [`GridOrder::step`] does a slice of
//! it and returns, so the UI can keep drawing between slices. Changing the spec drops the running
//! job (that is the cancel); the previously published order stays on screen until the new one is
//! published. Rows that arrive later are scanned, sorted and merged in by an extend job.

use std::cmp::Ordering;
use std::time::{Duration, Instant};

use super::filter::{CompiledFilter, FilterSpec};
use super::model::{GridModel, IngestState};
use super::selection::RowOrder;
use super::sort::{Collation, NULL_TAG, SortDirection, SortSpec, cmp_keys, compare_cells, encode};
use super::store::CellRef;

/// The most rows sorted as one unit.
const RUN_MAX: usize = 16_384;
/// The fewest rows of an adaptive run (but for the last run).
const RUN_MIN: usize = 256;
/// The size of the first run; the next ones follow the measured speed.
const RUN_FIRST: usize = 1_024;
/// The time one run should take; a step does runs until its own budget is spent.
const RUN_TARGET: Duration = Duration::from_millis(2);
/// How long arriving rows may wait, while the result streams, before they are merged in.
const EXTEND_AFTER: Duration = Duration::from_millis(500);
/// Rows scanned, elements merged or inverse entries written per unit of work.
const UNIT: usize = 1_024;
/// Inverse entries written per unit.
const INVERSE_UNIT: usize = 4_096;
/// New rows needed, while the result still streams, before an extend job starts.
const EXTEND_FLOOR: usize = 4_096;
/// The marker of a hidden row in the inverse.
const HIDDEN: u32 = u32::MAX;

/// Where a [`GridOrder::step`] call left the job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderProgress {
    /// Nothing to do.
    Idle,
    /// The job is not finished; `done` of `total` units are complete.
    Working {
        /// Units done.
        done: usize,
        /// Units in all.
        total: usize,
    },
    /// The job finished and its order is now the published one.
    Published,
}

/// The order of one grid: a sort spec, a filter spec and the published result.
#[derive(Default)]
pub struct GridOrder {
    sort: SortSpec,
    filter: FilterSpec,
    collation: Collation,
    /// Whether `order` holds a published arrangement; otherwise rows show in model order.
    published: bool,
    order: Vec<u32>,
    inverse: Vec<u32>,
    covered: usize,
    /// One cache per sort key, dense by model row, valid for `0..cache_rows`.
    keys: Vec<KeyCache>,
    job: Option<Box<Job>>,
    /// When the order was last published (the clock of the time trigger of `start_extend`).
    published_at: Option<Instant>,
    /// Test overrides: a fixed run length, the extend floor, the extend delay.
    run_len: Option<usize>,
    extend_floor: Option<usize>,
    extend_after: Option<Duration>,
}

/// Counts and flags only: no cell value and no filter text reaches a log.
impl std::fmt::Debug for GridOrder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GridOrder")
            .field("sort_keys", &self.sort.keys.len())
            .field("filter_conditions", &self.filter.conditions.len())
            .field("quick_filter", &self.filter.quick.is_some())
            .field("collation", &self.collation)
            .field("published", &self.published)
            .field("displayed_rows", &self.order.len())
            .field("covered", &self.covered)
            .field("key_caches", &self.keys.len())
            .field("job", &self.job)
            .finish_non_exhaustive()
    }
}

impl GridOrder {
    /// The sort spec.
    #[must_use]
    pub fn sort(&self) -> &SortSpec {
        &self.sort
    }

    /// The filter spec.
    #[must_use]
    pub fn filter(&self) -> &FilterSpec {
        &self.filter
    }

    /// The text collation of sort and filter.
    #[must_use]
    pub fn collation(&self) -> Collation {
        self.collation
    }

    /// Sets the sort spec and starts a full job. The published order stays until it is replaced.
    pub fn set_sort(&mut self, s: SortSpec) {
        if s != self.sort {
            self.sort = s;
            self.respec();
        }
    }

    /// Sets the filter spec and starts a full job.
    pub fn set_filter(&mut self, f: FilterSpec) {
        if f != self.filter {
            self.filter = f;
            self.respec();
        }
    }

    /// Sets the collation and starts a full job.
    pub fn set_collation(&mut self, c: Collation) {
        if c != self.collation {
            self.collation = c;
            self.respec();
        }
    }

    /// The column set changed: clears the sort and the filter (the collation is a setting and
    /// stays) and returns to model order at once.
    pub fn reset(&mut self) {
        self.sort = SortSpec::default();
        self.filter = FilterSpec::default();
        self.respec();
    }

    /// Whether a sort or a filter is set.
    #[must_use]
    pub fn is_active(&self) -> bool {
        !self.sort.is_empty() || !self.filter.is_empty()
    }

    /// Whether a job is running.
    #[must_use]
    pub fn is_working(&self) -> bool {
        self.job.is_some()
    }

    /// The progress of the running job; `Idle` without one.
    #[must_use]
    pub fn progress(&self) -> OrderProgress {
        self.job.as_ref().map_or(OrderProgress::Idle, |j| {
            let (done, total) = j.progress();
            OrderProgress::Working { done, total }
        })
    }

    /// The model rows the published order covers.
    #[must_use]
    pub fn covered(&self) -> usize {
        if self.published { self.covered } else { 0 }
    }

    /// An estimate of the heap bytes held by the order, its inverse and the key caches.
    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        let keys: usize = self
            .keys
            .iter()
            .map(|k| k.tags.capacity() + k.bits.capacity() * 8)
            .sum();
        let job = self.job.as_ref().map_or(0, |j| j.heap_bytes());
        (self.order.capacity() + self.inverse.capacity()) * 4 + keys + job
    }

    /// The rows as displayed: the published order, or the model order when none is published
    /// (`model_rows` rows).
    #[must_use]
    pub fn rows(&self, model_rows: usize) -> DisplayRows<'_> {
        DisplayRows {
            order: self
                .published
                .then_some((self.order.as_slice(), self.inverse.as_slice())),
            model_rows,
        }
    }

    /// Drops the running job and starts a full one for the new spec, or returns to model order
    /// when nothing is set.
    fn respec(&mut self) {
        if self.is_active() {
            self.job = Some(Box::new(Job::new(false, 0, self.compile())));
            return;
        }
        self.job = None;
        self.published = false;
        self.order = Vec::new();
        self.inverse = Vec::new();
        self.covered = 0;
        self.keys = Vec::new();
    }

    fn compile(&self) -> Option<CompiledFilter> {
        (!self.filter.is_empty()).then(|| self.filter.compile(self.collation))
    }

    /// Starts an extend job when rows have arrived that the order does not cover. Returns whether
    /// a job runs now.
    fn start_extend(&mut self, model: &GridModel) -> bool {
        if !self.published || !self.is_active() {
            return false;
        }
        let rows = model.row_count().min(u32::MAX as usize);
        if rows <= self.covered {
            return false;
        }
        let fresh = rows - self.covered;
        let floor = self.extend_floor.unwrap_or(EXTEND_FLOOR);
        let after = self.extend_after.unwrap_or(EXTEND_AFTER);
        // Enough rows, or some rows that have waited long enough: a slow result is not frozen.
        let enough = fresh >= floor.max(self.covered / 8)
            || self.published_at.is_none_or(|t| t.elapsed() >= after);
        if !enough && matches!(model.state(), IngestState::Streaming) {
            return false;
        }
        let caches_ok = self.keys.len() == self.sort.keys.len()
            && self.keys.iter().all(|k| k.tags.len() == self.covered);
        let extend = caches_ok;
        let from = if extend { self.covered } else { 0 };
        self.job = Some(Box::new(Job::new(extend, from, self.compile())));
        true
    }

    /// Does up to about `budget` of work (`None`: until the job ends) and says where the job is.
    /// At least one unit of work is done per call. Without a job, starts an extend job if rows
    /// arrived that the published order does not cover (see the ADR for the coalescing rule).
    pub fn step(&mut self, model: &GridModel, budget: Option<Duration>) -> OrderProgress {
        if self.job.is_none() && !self.start_extend(model) {
            return OrderProgress::Idle;
        }
        let deadline = budget.map(|b| Instant::now() + b);
        loop {
            let Some(job) = self.job.as_mut() else {
                return OrderProgress::Idle;
            };
            if job.to.is_none() {
                let to = model.row_count().min(u32::MAX as usize);
                job.to = Some(to);
                if !job.extend {
                    self.keys = self
                        .sort
                        .keys
                        .iter()
                        .map(|_| KeyCache::with_capacity(to))
                        .collect();
                }
            }
            let ctx = Ctx {
                model,
                sort: &self.sort,
                collation: self.collation,
                run_len: self.run_len,
                published: &self.order,
            };
            if job.advance(&ctx, &mut self.keys) {
                self.publish();
                return OrderProgress::Published;
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return self.progress();
            }
        }
    }

    fn publish(&mut self) {
        let Some(job) = self.job.take() else {
            return;
        };
        self.covered = job.to.unwrap_or(0);
        self.order = job.result;
        self.inverse = job.inverse;
        self.published = true;
        self.published_at = Some(Instant::now());
    }
}

/// The display order of a grid at one moment: identity, or a published order.
///
/// Rows the published order does not cover yet (they arrived after it was built) are not shown.
#[derive(Debug, Clone, Copy)]
pub struct DisplayRows<'a> {
    order: Option<(&'a [u32], &'a [u32])>,
    model_rows: usize,
}

impl RowOrder for DisplayRows<'_> {
    fn len(&self) -> usize {
        self.order.map_or(self.model_rows, |(o, _)| o.len())
    }

    fn model_row(&self, display: usize) -> Option<usize> {
        match self.order {
            Some((o, _)) => o.get(display).map(|&m| m as usize),
            None => (display < self.model_rows).then_some(display),
        }
    }

    fn display_row(&self, model: usize) -> Option<usize> {
        match self.order {
            Some((_, inv)) => inv
                .get(model)
                .filter(|&&d| d != HIDDEN)
                .map(|&d| d as usize),
            None => (model < self.model_rows).then_some(model),
        }
    }
}

/// The 9 bytes per row of one sort key: tag and bits (see `sort::encode`).
#[derive(Default)]
struct KeyCache {
    tags: Vec<u8>,
    bits: Vec<u64>,
}

impl std::fmt::Debug for KeyCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyCache")
            .field("rows", &self.tags.len())
            .finish_non_exhaustive()
    }
}

impl KeyCache {
    fn with_capacity(n: usize) -> Self {
        Self {
            tags: Vec::with_capacity(n),
            bits: Vec::with_capacity(n),
        }
    }
}

/// What a job reads while it advances.
struct Ctx<'a> {
    model: &'a GridModel,
    sort: &'a SortSpec,
    collation: Collation,
    /// A fixed run length (tests); `None`: runs sized from the measured speed.
    run_len: Option<usize>,
    published: &'a [u32],
}

/// The comparator of the sort: key caches first, cells when the keys tie, the model row last.
struct Cmp<'a> {
    ctx: &'a Ctx<'a>,
    keys: &'a [KeyCache],
}

impl Cmp<'_> {
    fn cell(&self, row: u32, column: usize) -> CellRef<'_> {
        self.ctx
            .model
            .row(row as usize)
            .and_then(|r| r.get(column))
            .unwrap_or(CellRef::Null)
    }

    fn compare(&self, a: u32, b: u32) -> Ordering {
        for (key, cache) in self.ctx.sort.keys.iter().zip(self.keys) {
            let (ia, ib) = (a as usize, b as usize);
            let (ta, ba) = (cache.tags[ia], cache.bits[ia]);
            let (tb, bb) = (cache.tags[ib], cache.bits[ib]);
            let o = cmp_keys(ta, ba, tb, bb).unwrap_or_else(|| {
                compare_cells(
                    self.cell(a, key.column),
                    self.cell(b, key.column),
                    self.ctx.collation,
                )
            });
            let o =
                if key.direction == SortDirection::Descending && ta != NULL_TAG && tb != NULL_TAG {
                    o.reverse()
                } else {
                    o
                };
            if o != Ordering::Equal {
                return o;
            }
        }
        a.cmp(&b)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Scan,
    Runs,
    MergeNew,
    MergeOld,
    Inverse,
}

/// A resumable sort/filter pass over model rows `from..to`.
struct Job {
    /// Adds rows to the published order instead of replacing it.
    extend: bool,
    from: usize,
    /// Fixed at the first step: the model rows at that moment.
    to: Option<usize>,
    filter: Option<CompiledFilter>,
    next: usize,
    kept: Vec<u32>,
    scratch: String,
    phase: Phase,
    run_next: usize,
    /// The size of the next run when the sizes adapt.
    run_size: usize,
    /// The end of each sorted run so far.
    run_ends: Vec<usize>,
    merger: Option<Merger>,
    result: Vec<u32>,
    inverse: Vec<u32>,
    inv_pos: usize,
}

/// No cell data: the phase and the sizes only.
impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job")
            .field("extend", &self.extend)
            .field("from", &self.from)
            .field("to", &self.to)
            .field("next", &self.next)
            .field("phase", &self.phase)
            .field("kept", &self.kept.len())
            .field("runs", &self.run_ends.len())
            .finish_non_exhaustive()
    }
}

impl Job {
    fn new(extend: bool, from: usize, filter: Option<CompiledFilter>) -> Self {
        Self {
            extend,
            from,
            to: None,
            filter,
            next: from,
            kept: Vec::new(),
            scratch: String::new(),
            phase: Phase::Scan,
            run_next: 0,
            run_size: RUN_FIRST,
            run_ends: Vec::new(),
            merger: None,
            result: Vec::new(),
            inverse: Vec::new(),
            inv_pos: 0,
        }
    }

    fn heap_bytes(&self) -> usize {
        self.kept.capacity() * 4
            + self.result.capacity() * 4
            + self.inverse.capacity() * 4
            + self.merger.as_ref().map_or(0, Merger::heap_bytes)
    }

    /// `(done, total)`: four equal shares for scanning, sorting runs, merging and the inverse.
    fn progress(&self) -> (usize, usize) {
        let n = self.to.map_or(0, |t| t.saturating_sub(self.from));
        let share = |part: usize, whole: usize| n * part / whole.max(1);
        let done = match self.phase {
            Phase::Scan => self.next - self.from,
            Phase::Runs => n + share(self.run_next.min(self.kept.len()), self.kept.len()),
            Phase::MergeNew | Phase::MergeOld => {
                let f = self.merger.as_ref().map_or(0.0, Merger::fraction);
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    clippy::cast_precision_loss
                )] // a progress estimate
                let part = (n as f64 * f) as usize;
                // An extend job merges twice: the new rows, then into the published order.
                match (self.phase, self.extend) {
                    (Phase::MergeNew, true) => 2 * n + part / 2,
                    (Phase::MergeOld, _) => 2 * n + n / 2 + part / 2,
                    _ => 2 * n + part,
                }
            }
            Phase::Inverse => 3 * n + share(self.inv_pos, self.result.len()),
        };
        (done, (4 * n).max(1))
    }

    /// Does one unit of work. Returns whether the job is finished (the result is ready).
    #[allow(clippy::cast_possible_truncation)] // rows are capped at u32::MAX
    fn advance(&mut self, ctx: &Ctx<'_>, keys: &mut [KeyCache]) -> bool {
        let to = self.to.unwrap_or(0);
        match self.phase {
            Phase::Scan => {
                let count = (to - self.next).min(UNIT);
                let vp = ctx.model.viewport(self.next, count);
                if vp.is_empty() {
                    self.next = to;
                }
                let mut index = self.next;
                for row in vp.iter() {
                    let keep = self
                        .filter
                        .as_ref()
                        .is_none_or(|f| f.matches(row, &mut self.scratch));
                    for (key, cache) in ctx.sort.keys.iter().zip(keys.iter_mut()) {
                        let (tag, bits) = match row.get(key.column) {
                            Some(c) if keep => encode(c, ctx.collation),
                            _ => (NULL_TAG, 0),
                        };
                        cache.tags.push(tag);
                        cache.bits.push(bits);
                    }
                    if keep {
                        self.kept.push(index as u32);
                    }
                    index += 1;
                }
                self.next = index.max(self.next);
                if self.next >= to {
                    if ctx.sort.is_empty() {
                        self.result = std::mem::take(&mut self.kept);
                        if self.extend {
                            let mut all = ctx.published.to_vec();
                            all.append(&mut self.result);
                            self.result = all;
                        }
                        self.phase = Phase::Inverse;
                    } else {
                        self.phase = Phase::Runs;
                    }
                }
            }
            Phase::Runs => {
                let cmp = Cmp { ctx, keys };
                let len = ctx.run_len.unwrap_or(self.run_size).max(1);
                let end = (self.run_next + len).min(self.kept.len());
                let started = Instant::now();
                self.kept[self.run_next..end].sort_unstable_by(|&a, &b| cmp.compare(a, b));
                if ctx.run_len.is_none() {
                    self.run_size = next_run_size(end - self.run_next, started.elapsed());
                }
                self.run_next = end;
                self.run_ends.push(end);
                if end >= self.kept.len() {
                    let kept = std::mem::take(&mut self.kept);
                    self.merger = Some(Merger::new(kept, std::mem::take(&mut self.run_ends)));
                    self.phase = Phase::MergeNew;
                }
            }
            Phase::MergeNew | Phase::MergeOld => {
                let cmp = Cmp { ctx, keys };
                let Some(m) = self.merger.as_mut() else {
                    self.phase = Phase::Inverse;
                    return false;
                };
                m.run(UNIT, |a, b| cmp.compare(a, b));
                if m.done() {
                    let sorted = self
                        .merger
                        .take()
                        .map(Merger::into_sorted)
                        .unwrap_or_default();
                    if self.phase == Phase::MergeNew && self.extend && !ctx.published.is_empty() {
                        let mut all = Vec::with_capacity(ctx.published.len() + sorted.len());
                        all.extend_from_slice(ctx.published);
                        all.extend_from_slice(&sorted);
                        let ends = vec![ctx.published.len(), all.len()];
                        self.merger = Some(Merger::new(all, ends));
                        self.phase = Phase::MergeOld;
                    } else {
                        self.result = sorted;
                        self.phase = Phase::Inverse;
                    }
                }
            }
            Phase::Inverse => {
                if self.inverse.len() != to {
                    self.inverse = vec![HIDDEN; to];
                }
                let end = (self.inv_pos + INVERSE_UNIT).min(self.result.len());
                for p in self.inv_pos..end {
                    if let Some(slot) = self.inverse.get_mut(self.result[p] as usize) {
                        *slot = p as u32;
                    }
                }
                self.inv_pos = end;
                return end >= self.result.len();
            }
        }
        false
    }
}

/// The size of the next run: what takes about `RUN_TARGET` at the speed just measured (a sort
/// of `n` rows costs about `n log2 n` comparisons), at most double, within the run limits.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)] // an estimate
fn next_run_size(rows: usize, took: Duration) -> usize {
    if rows < RUN_MIN {
        return RUN_MIN;
    }
    let n = rows as f64;
    let per_unit = (took.as_secs_f64() / (n * n.log2())).max(1e-10);
    let fit = RUN_TARGET.as_secs_f64() / per_unit;
    // `fit` is about n' log2 n'; dividing by the log of the last size is close enough.
    let size = (fit / n.log2()) as usize;
    size.clamp(RUN_MIN, (rows * 2).min(RUN_MAX))
}

/// A resumable bottom-up merge sort over sorted runs that end at `ends`.
struct Merger {
    src: Vec<u32>,
    dst: Vec<u32>,
    /// The ends of the runs of `src` in this pass.
    ends: Vec<usize>,
    /// The ends of the runs `dst` gets in this pass.
    next_ends: Vec<usize>,
    /// The pair being merged (it joins runs `2 * pair` and `2 * pair + 1`).
    pair: usize,
    /// Start of the pair being merged.
    lo: usize,
    in_pair: bool,
    i: usize,
    j: usize,
    mid: usize,
    hi: usize,
    k: usize,
    pass: usize,
    passes: usize,
}

impl Merger {
    fn new(src: Vec<u32>, ends: Vec<usize>) -> Self {
        let len = src.len();
        let passes = ends.len().max(1).next_power_of_two().trailing_zeros() as usize;
        Self {
            dst: vec![0; len],
            src,
            next_ends: Vec::with_capacity(ends.len() / 2 + 1),
            ends,
            pair: 0,
            lo: 0,
            in_pair: false,
            i: 0,
            j: 0,
            mid: 0,
            hi: 0,
            k: 0,
            pass: 0,
            passes,
        }
    }

    fn heap_bytes(&self) -> usize {
        (self.src.capacity() + self.dst.capacity()) * 4
            + (self.ends.capacity() + self.next_ends.capacity()) * 8
    }

    fn done(&self) -> bool {
        self.ends.len() <= 1
    }

    #[allow(clippy::cast_precision_loss)] // a progress estimate
    fn fraction(&self) -> f64 {
        if self.passes == 0 || self.src.is_empty() {
            return 1.0;
        }
        let len = self.src.len() as f64;
        let at = if self.in_pair { self.k } else { self.lo };
        ((self.pass as f64 * len + at as f64) / (self.passes as f64 * len)).min(1.0)
    }

    fn into_sorted(self) -> Vec<u32> {
        self.src
    }

    /// Writes at most `units` merged elements.
    fn run(&mut self, units: usize, cmp: impl Fn(u32, u32) -> Ordering) {
        let mut left = units;
        while left > 0 && !self.done() {
            if !self.in_pair {
                let first = self.pair * 2;
                if first >= self.ends.len() {
                    std::mem::swap(&mut self.src, &mut self.dst);
                    self.ends = std::mem::take(&mut self.next_ends);
                    self.pair = 0;
                    self.lo = 0;
                    self.pass += 1;
                    continue;
                }
                self.mid = self.ends[first];
                self.hi = self.ends.get(first + 1).copied().unwrap_or(self.mid);
                self.i = self.lo;
                self.j = self.mid;
                self.k = self.lo;
                self.next_ends.push(self.hi);
                self.in_pair = true;
            }
            let take_left = if self.i >= self.mid {
                false
            } else if self.j >= self.hi {
                true
            } else {
                cmp(self.src[self.i], self.src[self.j]) != Ordering::Greater
            };
            if take_left {
                self.dst[self.k] = self.src[self.i];
                self.i += 1;
            } else {
                self.dst[self.k] = self.src[self.j];
                self.j += 1;
            }
            self.k += 1;
            left -= 1;
            if self.k == self.hi {
                self.in_pair = false;
                self.lo = self.hi;
                self.pair += 1;
            }
        }
    }
}

/// Models for tests: columns, then rows pushed in batches.
#[cfg(test)]
pub(super) mod testing {
    use sqler_driver_api::sqler_core::{Column, DataType, Row, Value};

    use super::super::model::{GridModel, GridSink, Msg, grid_channel_with};
    use super::super::store::Chunk;
    use crate::grid::GridLimits;

    /// A streaming model with `width` text columns and no rows.
    pub fn channel(width: usize) -> (GridModel, GridSink) {
        let (mut model, sink) = grid_channel_with(GridLimits::UNBOUNDED);
        let cols = (0..width)
            .map(|i| Column::new(format!("c{i}"), DataType::Text))
            .collect();
        assert!(sink.send(Msg::Columns(cols)));
        model.poll(usize::MAX);
        (model, sink)
    }

    /// Appends rows to the model.
    pub fn push(model: &mut GridModel, sink: &GridSink, rows: Vec<Vec<Value>>) {
        let width = model.columns().len();
        let rows = rows.into_iter().map(Row).collect();
        assert!(sink.send(Msg::Rows(Chunk::from_rows(rows, width))));
        model.poll(usize::MAX);
    }

    /// Ends the result.
    pub fn finish(model: &mut GridModel, sink: &GridSink) {
        assert!(sink.send(Msg::Done(None)));
        model.poll(usize::MAX);
    }
}

#[cfg(test)]
mod tests {
    use sqler_driver_api::sqler_core::Value;

    use super::super::filter::{Condition, FilterOp, QuickFilter};
    use super::super::sort::{SortKey, compare_cells};
    use super::*;

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    const WORDS: [&str; 9] = [
        "alpha", "Alpha", "ALPHA", "beta", "Äpfel", "äpfel", "", "z", "zz",
    ];

    fn random_rows(n: usize, seed: u64) -> Vec<Vec<Value>> {
        let mut r = Rng(seed);
        (0..n)
            .map(|_| {
                let int = match r.below(10) {
                    0 => Value::Null,
                    _ => Value::Int(i64::try_from(r.below(50)).unwrap() - 25),
                };
                let text = match r.below(12) {
                    0 => Value::Null,
                    _ => Value::Text(format!(
                        "{}{}",
                        WORDS[usize::try_from(r.below(9)).unwrap()],
                        r.below(4)
                    )),
                };
                let float = match r.below(15) {
                    0 => Value::Null,
                    1 => Value::Float(f64::NAN),
                    2 => Value::Float(f64::INFINITY),
                    _ => Value::Float(f64::from(i32::try_from(r.below(20)).unwrap()) / 2.0),
                };
                let boolean = Value::Bool(r.below(2) == 0);
                vec![int, text, float, boolean]
            })
            .collect()
    }

    fn loaded(rows: Vec<Vec<Value>>) -> GridModel {
        let (mut model, sink) = testing::channel(4);
        testing::push(&mut model, &sink, rows);
        testing::finish(&mut model, &sink);
        model
    }

    fn key(column: usize, direction: SortDirection) -> SortKey {
        SortKey { column, direction }
    }

    fn sort_of(keys: &[(usize, SortDirection)]) -> SortSpec {
        SortSpec {
            keys: keys.iter().map(|&(c, d)| key(c, d)).collect(),
        }
    }

    fn shown(order: &GridOrder, model: &GridModel) -> Vec<usize> {
        let rows = order.rows(model.row_count());
        (0..rows.len())
            .map(|d| rows.model_row(d).unwrap())
            .collect()
    }

    fn run(order: &mut GridOrder, model: &GridModel) {
        assert_eq!(order.step(model, None), OrderProgress::Published);
    }

    fn run_sliced(order: &mut GridOrder, model: &GridModel) -> usize {
        let mut steps = 1;
        loop {
            match order.step(model, Some(Duration::ZERO)) {
                OrderProgress::Published => return steps,
                OrderProgress::Working { done, total } => assert!(done <= total),
                OrderProgress::Idle => panic!("idle before publishing"),
            }
            steps += 1;
            assert!(steps < 1_000_000, "stuck");
        }
    }

    /// The reference: filter, then a stable sort over `compare_cells` with the direction applied
    /// to non-NULL pairs.
    fn reference(model: &GridModel, order: &GridOrder) -> Vec<usize> {
        let filter = order.filter().compile(order.collation());
        let mut scratch = String::new();
        let mut rows: Vec<usize> = (0..model.row_count())
            .filter(|&i| filter.matches(model.row(i).unwrap(), &mut scratch))
            .collect();
        let coll = order.collation();
        rows.sort_by(|&a, &b| {
            for k in &order.sort().keys {
                let left = model.row(a).unwrap().get(k.column).unwrap();
                let right = model.row(b).unwrap().get(k.column).unwrap();
                let mut ord = compare_cells(left, right, coll);
                if k.direction == SortDirection::Descending && !left.is_null() && !right.is_null() {
                    ord = ord.reverse();
                }
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            Ordering::Equal
        });
        rows
    }

    fn quick(text: &str, column: Option<usize>) -> FilterSpec {
        FilterSpec {
            quick: Some(QuickFilter {
                text: text.into(),
                column,
            }),
            conditions: vec![],
        }
    }

    use SortDirection::{Ascending as Asc, Descending as Desc};

    #[test]
    fn default_is_the_identity() {
        let model = loaded(random_rows(10, 1));
        let order = GridOrder::default();
        assert!(!order.is_active() && !order.is_working());
        assert_eq!(order.covered(), 0);
        let rows = order.rows(10);
        assert_eq!(
            (rows.len(), rows.model_row(9), rows.model_row(10)),
            (10, Some(9), None)
        );
        assert_eq!((rows.display_row(3), rows.display_row(10)), (Some(3), None));
        let mut order = order;
        assert_eq!(order.step(&model, None), OrderProgress::Idle);
        assert_eq!(order.progress(), OrderProgress::Idle);
    }

    #[test]
    fn sliced_and_complete_runs_match_the_reference() {
        let specs = [
            (sort_of(&[(0, Asc)]), FilterSpec::default()),
            (sort_of(&[(0, Desc)]), FilterSpec::default()),
            (sort_of(&[(1, Asc), (0, Desc)]), FilterSpec::default()),
            (
                sort_of(&[(2, Desc), (3, Asc), (1, Asc)]),
                FilterSpec::default(),
            ),
            (SortSpec::default(), quick("alp", None)),
            (sort_of(&[(1, Desc)]), quick("1", Some(1))),
            (
                sort_of(&[(0, Asc)]),
                FilterSpec {
                    quick: None,
                    conditions: vec![
                        Condition {
                            column: 0,
                            op: FilterOp::Greater,
                            operand: "-5".into(),
                        },
                        Condition {
                            column: 2,
                            op: FilterOp::NotNull,
                            operand: String::new(),
                        },
                    ],
                },
            ),
        ];
        let model = loaded(random_rows(3_000, 0x9e37_79b9));
        for coll in [Collation::CaseInsensitive, Collation::CodePoint] {
            for (sort, filter) in &specs {
                for run_len in [None, Some(7), Some(1)] {
                    let mut a = GridOrder {
                        run_len,
                        ..GridOrder::default()
                    };
                    a.set_collation(coll);
                    a.set_sort(sort.clone());
                    a.set_filter(filter.clone());
                    run(&mut a, &model);
                    let mut b = GridOrder {
                        run_len,
                        ..GridOrder::default()
                    };
                    b.set_collation(coll);
                    b.set_sort(sort.clone());
                    b.set_filter(filter.clone());
                    assert!(run_sliced(&mut b, &model) > 3);
                    let want = reference(&model, &a);
                    assert_eq!(shown(&a, &model), want, "{sort:?} {filter:?} {coll:?}");
                    assert_eq!(shown(&b, &model), want, "sliced {sort:?} {filter:?}");
                    assert_eq!(a.covered(), 3_000);
                }
            }
        }
    }

    #[test]
    fn nulls_are_last_both_ways_and_ties_keep_model_order() {
        let rows = vec![
            vec![Value::Int(2)],
            vec![Value::Null],
            vec![Value::Int(1)],
            vec![Value::Int(2)],
            vec![Value::Null],
            vec![Value::Int(1)],
        ];
        let (mut model, sink) = testing::channel(1);
        testing::push(&mut model, &sink, rows);
        let mut order = GridOrder::default();
        order.set_sort(sort_of(&[(0, Asc)]));
        run(&mut order, &model);
        assert_eq!(shown(&order, &model), [2, 5, 0, 3, 1, 4]);
        order.set_sort(sort_of(&[(0, Desc)]));
        run(&mut order, &model);
        assert_eq!(shown(&order, &model), [0, 3, 2, 5, 1, 4]);
    }

    #[test]
    fn display_rows_round_trip_and_hide_filtered_rows() {
        let model = loaded(random_rows(500, 7));
        let mut order = GridOrder::default();
        order.set_filter(quick("alpha", Some(1)));
        order.set_sort(sort_of(&[(0, Asc)]));
        run(&mut order, &model);
        let rows = order.rows(model.row_count());
        let mut seen = 0;
        for m in 0..model.row_count() {
            if let Some(d) = rows.display_row(m) {
                assert_eq!(rows.model_row(d), Some(m));
                seen += 1;
            }
        }
        assert_eq!(seen, rows.len());
        assert!(!rows.is_empty() && rows.len() < 500);
        assert_eq!(rows.model_row(rows.len()), None);
        assert_eq!(rows.display_row(500), None);
        assert!(order.heap_bytes() >= rows.len() * 8);
    }

    #[test]
    fn the_published_order_stays_until_the_new_one_is_ready() {
        let model = loaded(random_rows(2_000, 3));
        let mut order = GridOrder::default();
        order.set_sort(sort_of(&[(0, Asc)]));
        run(&mut order, &model);
        let before = shown(&order, &model);
        order.set_sort(sort_of(&[(0, Desc)]));
        assert!(order.is_working());
        assert!(matches!(
            order.step(&model, Some(Duration::ZERO)),
            OrderProgress::Working { .. }
        ));
        assert_eq!(shown(&order, &model), before, "old order still shown");
        // Superseded mid-job: only the last spec is published.
        order.set_sort(sort_of(&[(1, Asc)]));
        run(&mut order, &model);
        assert_eq!(shown(&order, &model), reference(&model, &order));
        assert_eq!(order.progress(), OrderProgress::Idle);
        assert_eq!(order.step(&model, None), OrderProgress::Idle);
        // Setting the same spec again changes nothing.
        order.set_sort(sort_of(&[(1, Asc)]));
        assert!(!order.is_working());
    }

    #[test]
    fn clearing_the_specs_returns_to_model_order_at_once() {
        let model = loaded(random_rows(100, 5));
        let mut order = GridOrder::default();
        order.set_sort(sort_of(&[(0, Asc)]));
        order.set_filter(quick("a", None));
        run(&mut order, &model);
        assert!(order.covered() == 100 && order.is_active());
        order.set_sort(SortSpec::default());
        run(&mut order, &model);
        assert_eq!(shown(&order, &model), reference(&model, &order));
        order.set_filter(FilterSpec::default());
        assert!(!order.is_active() && !order.is_working());
        assert_eq!(shown(&order, &model), (0..100).collect::<Vec<_>>());
        assert_eq!(order.covered(), 0);
        assert!(order.heap_bytes() < 64);
        // reset() clears the specs and keeps the collation.
        order.set_collation(Collation::CodePoint);
        order.set_sort(sort_of(&[(0, Asc)]));
        run(&mut order, &model);
        order.reset();
        assert!(!order.is_active() && !order.is_working());
        assert_eq!(order.collation(), Collation::CodePoint);
        assert_eq!(shown(&order, &model).len(), 100);
    }

    #[test]
    fn before_the_first_publish_rows_show_in_model_order() {
        let model = loaded(random_rows(50, 9));
        let mut order = GridOrder::default();
        order.set_sort(sort_of(&[(0, Asc)]));
        assert!(order.is_active() && order.is_working());
        assert_eq!(
            order.progress(),
            OrderProgress::Working { done: 0, total: 1 }
        );
        assert_eq!(order.rows(50).len(), 50);
        assert_eq!(order.rows(50).model_row(3), Some(3));
        order.step(&model, Some(Duration::ZERO));
        assert_eq!(order.rows(50).model_row(3), Some(3));
    }

    #[test]
    fn progress_is_monotonic_and_ends_below_total() {
        let model = loaded(random_rows(5_000, 11));
        let mut order = GridOrder {
            run_len: Some(512),
            ..GridOrder::default()
        };
        order.set_sort(sort_of(&[(1, Asc)]));
        let mut last = 0;
        loop {
            match order.step(&model, Some(Duration::ZERO)) {
                OrderProgress::Working { done, total } => {
                    assert!(done >= last, "{done} < {last}");
                    assert!(done <= total && total == 4 * 5_000);
                    last = done;
                }
                OrderProgress::Published => break,
                OrderProgress::Idle => panic!(),
            }
        }
        assert!(last > 0);
    }

    #[test]
    fn extending_gives_the_same_result_as_a_full_job() {
        let all = random_rows(6_000, 21);
        for spec in [
            (sort_of(&[(1, Asc), (0, Desc)]), FilterSpec::default()),
            (sort_of(&[(0, Desc)]), quick("a", None)),
            (SortSpec::default(), quick("beta", Some(1))),
        ] {
            let (mut model, sink) = testing::channel(4);
            let mut order = GridOrder {
                run_len: Some(100),
                extend_floor: Some(10),
                ..GridOrder::default()
            };
            order.set_sort(spec.0.clone());
            order.set_filter(spec.1.clone());
            for batch in all.chunks(1_500) {
                testing::push(&mut model, &sink, batch.to_vec());
                // Not Streaming-quiet: the batch is at least 8 % of what is covered or large
                // enough, so the next step starts a job.
                while order.is_working() || order.covered() < model.row_count() {
                    let p = order.step(&model, None);
                    assert!(p != OrderProgress::Idle || order.covered() == model.row_count());
                    if p == OrderProgress::Idle {
                        break;
                    }
                }
                assert_eq!(shown(&order, &model), reference(&model, &order), "{spec:?}");
                assert_eq!(order.covered(), model.row_count());
            }
            let mut full = GridOrder::default();
            full.set_sort(spec.0.clone());
            full.set_filter(spec.1.clone());
            run(&mut full, &model);
            assert_eq!(shown(&order, &model), shown(&full, &model));
        }
    }

    #[test]
    fn small_batches_while_streaming_are_coalesced() {
        let (mut model, sink) = testing::channel(4);
        let mut order = GridOrder {
            extend_after: Some(Duration::from_secs(3600)),
            ..GridOrder::default()
        };
        order.set_sort(sort_of(&[(0, Asc)]));
        testing::push(&mut model, &sink, random_rows(10_000, 1));
        run(&mut order, &model);
        // 100 new rows: far below max(4096, covered / 8). Nothing starts while streaming.
        testing::push(&mut model, &sink, random_rows(100, 2));
        assert_eq!(order.step(&model, None), OrderProgress::Idle);
        assert_eq!(order.rows(model.row_count()).len(), 10_000);
        testing::push(&mut model, &sink, random_rows(3_900, 3));
        assert_eq!(
            order.step(&model, None),
            OrderProgress::Idle,
            "4,000 < 4,096"
        );
        // Reaching the floor starts the job.
        testing::push(&mut model, &sink, random_rows(1_000, 4));
        assert_eq!(order.step(&model, None), OrderProgress::Published);
        assert_eq!(order.covered(), 15_000);
        // At the end of the result the rest is merged in whatever its size.
        testing::push(&mut model, &sink, random_rows(3, 5));
        assert_eq!(order.step(&model, None), OrderProgress::Idle);
        testing::finish(&mut model, &sink);
        assert_eq!(order.step(&model, None), OrderProgress::Published);
        assert_eq!(order.covered(), 15_003);
        assert_eq!(shown(&order, &model), reference(&model, &order));
        assert_eq!(order.step(&model, None), OrderProgress::Idle);
    }

    #[test]
    fn waiting_rows_are_merged_after_the_delay_even_when_few() {
        let (mut model, sink) = testing::channel(4);
        let mut order = GridOrder {
            extend_after: Some(Duration::ZERO),
            ..GridOrder::default()
        };
        order.set_sort(sort_of(&[(0, Asc)]));
        testing::push(&mut model, &sink, random_rows(10_000, 1));
        run(&mut order, &model);
        // One row, far below the floor, but the delay is over: it shows up while streaming.
        testing::push(&mut model, &sink, random_rows(1, 2));
        assert_eq!(order.step(&model, None), OrderProgress::Published);
        assert_eq!(order.covered(), 10_001);
        assert_eq!(shown(&order, &model), reference(&model, &order));
        // Nothing waits: nothing to do.
        assert_eq!(order.step(&model, None), OrderProgress::Idle);
        // With a long delay the same single row stays hidden.
        let mut slow = GridOrder {
            extend_after: Some(Duration::from_secs(3600)),
            ..GridOrder::default()
        };
        slow.set_sort(sort_of(&[(0, Asc)]));
        run(&mut slow, &model);
        testing::push(&mut model, &sink, random_rows(1, 3));
        assert_eq!(slow.step(&model, None), OrderProgress::Idle);
    }

    #[test]
    fn run_sizes_follow_the_measured_speed() {
        // Fast: doubles up to the cap.
        assert_eq!(next_run_size(1_024, Duration::from_micros(20)), 2_048);
        assert_eq!(next_run_size(RUN_MAX, Duration::from_micros(20)), RUN_MAX);
        // Slow (a run of 1,024 took 10 ms, 5x the target): shrinks.
        let slow = next_run_size(1_024, Duration::from_millis(10));
        assert!((RUN_MIN..1_024 / 2).contains(&slow), "{slow}");
        // Very slow: the floor. A short last run does not distort the next size.
        assert_eq!(next_run_size(4_096, Duration::from_secs(1)), RUN_MIN);
        assert_eq!(next_run_size(10, Duration::from_secs(1)), RUN_MIN);
    }

    #[test]
    fn debug_output_holds_no_cell_data() {
        let secret = "SECRET-CELL-VALUE";
        let rows: Vec<Vec<Value>> = (0..50)
            .map(|i| {
                vec![
                    Value::Text(format!("{secret}{i}")),
                    Value::Text(secret.to_owned()),
                    Value::Int(i),
                    Value::Null,
                ]
            })
            .collect();
        let model = loaded(rows);
        let mut order = GridOrder::default();
        order.set_sort(sort_of(&[(0, Asc)]));
        order.set_filter(quick("zzz-needle", None));
        assert!(!format!("{order:?}").contains("needle"));
        // Mid-job: step once with a zero budget, so the job holds keys and scratch.
        assert!(matches!(
            order.step(&model, Some(Duration::ZERO)),
            OrderProgress::Working { .. }
        ));
        assert!(format!("{order:?}").contains("GridOrder"));
        run(&mut order, &model);
        for text in [format!("{order:?}"), format!("{:?}", order.rows(50))] {
            assert!(!text.contains(secret), "{text}");
        }
    }

    #[test]
    fn the_threshold_grows_with_the_covered_rows() {
        let (mut model, sink) = testing::channel(4);
        let mut order = GridOrder {
            extend_floor: Some(10),
            extend_after: Some(Duration::from_secs(3600)),
            ..GridOrder::default()
        };
        order.set_sort(sort_of(&[(0, Asc)]));
        testing::push(&mut model, &sink, random_rows(1_000, 1));
        run(&mut order, &model);
        testing::push(&mut model, &sink, random_rows(100, 2));
        assert_eq!(
            order.step(&model, None),
            OrderProgress::Idle,
            "100 < 1,000 / 8"
        );
        testing::push(&mut model, &sink, random_rows(30, 3));
        assert_eq!(order.step(&model, None), OrderProgress::Published);
        assert_eq!(order.covered(), 1_130);
    }

    #[test]
    fn rows_not_yet_covered_are_hidden() {
        let (mut model, sink) = testing::channel(4);
        let mut order = GridOrder::default();
        testing::push(&mut model, &sink, random_rows(20, 1));
        order.set_filter(quick("", None));
        assert!(!order.is_active());
        order.set_sort(sort_of(&[(0, Asc)]));
        run(&mut order, &model);
        testing::push(&mut model, &sink, random_rows(5, 2));
        let rows = order.rows(model.row_count());
        assert_eq!(rows.len(), 20);
        assert_eq!(rows.display_row(22), None);
    }

    #[test]
    fn a_spec_set_before_the_columns_arrive_is_applied_later() {
        let (mut model, sink) = testing::channel(4);
        let mut order = GridOrder::default();
        order.set_sort(sort_of(&[(0, Asc)]));
        run(&mut order, &model);
        assert_eq!(order.covered(), 0);
        testing::push(&mut model, &sink, random_rows(30, 2));
        testing::finish(&mut model, &sink);
        assert_eq!(order.step(&model, None), OrderProgress::Published);
        assert_eq!(shown(&order, &model), reference(&model, &order));
    }

    #[test]
    fn a_sort_column_past_the_row_width_sorts_as_null() {
        let model = loaded(random_rows(40, 8));
        let mut order = GridOrder::default();
        order.set_sort(sort_of(&[(9, Desc)]));
        run(&mut order, &model);
        assert_eq!(shown(&order, &model), (0..40).collect::<Vec<_>>());
    }
}
