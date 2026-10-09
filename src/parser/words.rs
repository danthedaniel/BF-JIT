//! Lifting of multi-byte arithmetic to `Word` nodes.
//!
//! Compilers targeting brainfuck keep numbers in several cells and do
//! arithmetic on them a byte at a time, propagating carries with comparisons.
//! Even optimized, copying a 32-bit number takes eight nodes and negating one
//! more than a dozen.
//!
//! In each run of straight-line nodes, the nodes computing the final value of
//! a cell are sliced out, along with the nodes they depend on, the nodes using
//! values they compute and the nodes changing what they read before they're
//! done. If every cell the slice leaves changed and which is used later ends
//! up holding a constant, its old value or part of a number which is a sum of
//! numbers (or a product of two) in the cells it reads, the slice is replaced
//! by `Word` and `Set` nodes. Sums are guessed by running the slice on random
//! values, then proven for all values by symbolically executing it on
//! polynomials, or failing that on binary decision diagrams.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::ops::Range;

use super::accelerate::{self, Bits, Diagrams, FALSE, Symbolic, TRUE, constant};
use super::poly::{Algebra, Poly};
use super::{AstNode, WordTerm};

/// The widest number a `Word` node writes, in bytes.
const MAX_LEN: u8 = 8;
/// Slices with more nodes than this aren't considered.
const MAX_SLICE: usize = 256;
/// Slices reading more cells than this aren't considered.
const MAX_INPUTS: usize = 24;
/// The number of random cell values slices are run on.
const SAMPLES: usize = 32;

/// A fast hasher for the small keys used here.
#[derive(Default)]
struct FastHasher(u64);

impl Hasher for FastHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.write_u64(u64::from(byte));
        }
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = (self.0.rotate_left(5) ^ value).wrapping_mul(0x517c_c1b7_2722_0a95);
    }

    #[allow(clippy::cast_sign_loss)]
    fn write_i32(&mut self, value: i32) {
        self.write_u64(u64::from(value as u32));
    }

    fn write_usize(&mut self, value: usize) {
        self.write_u64(value as u64);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<FastHasher>>;
type FastSet<T> = HashSet<T, BuildHasherDefault<FastHasher>>;

/// Lift multi-byte arithmetic in an optimized program to `Word` nodes.
/// Unless `keep_tape` is set, cells are only correct as far as the program
/// observes them.
pub fn lift(nodes: Vec<AstNode>, keep_tape: bool) -> Vec<AstNode> {
    let live = if keep_tape {
        Live::All
    } else {
        Live::Cells(BTreeSet::new(), 0)
    };
    lift_block(nodes, live)
}

/// Lift the runs in a block, after which the cells in `live` are used.
/// Runs are lifted from last to first, so what's used after each is known.
fn lift_block(nodes: Vec<AstNode>, mut live: Live) -> Vec<AstNode> {
    let mut output = Vec::with_capacity(nodes.len());
    // The current run, reversed
    let mut run = Vec::new();
    let flush = |run: &mut Vec<AstNode>, live: &mut Live, output: &mut Vec<AstNode>| {
        run.reverse();
        for node in lift_run(std::mem::take(run), live).into_iter().rev() {
            live.before(&node);
            output.push(node);
        }
    };
    for node in nodes.into_iter().rev() {
        match node {
            AstNode::Add(..)
            | AstNode::Set(..)
            | AstNode::MulAdd { .. }
            | AstNode::CondAdd { .. }
            | AstNode::ProductAdd { .. } => run.push(node),
            AstNode::Loop(body) => {
                flush(&mut run, &mut live, &mut output);
                // The cells used after each iteration are those used after
                // the loop or by another iteration.
                live.before_loop(&body);
                output.push(AstNode::Loop(lift_block(body, live.clone())));
            }
            node => {
                flush(&mut run, &mut live, &mut output);
                live.before(&node);
                output.push(node);
            }
        }
    }
    flush(&mut run, &mut live, &mut output);
    output.reverse();
    output
}

/// The cells which may be used later, relative to the data pointer.
#[derive(Clone)]
enum Live {
    All,
    /// Cells `cell + shift` for each `cell`. Shifting is lazy, since the
    /// pointer moves a lot.
    Cells(BTreeSet<i32>, i32),
}

impl Live {
    fn contains(&self, cell: i32) -> bool {
        match self {
            Self::All => true,
            Self::Cells(cells, shift) => cells.contains(&(cell - shift)),
        }
    }

    fn insert(&mut self, cell: i32) {
        if let Self::Cells(cells, shift) = self {
            cells.insert(cell - *shift);
        }
    }

    fn remove(&mut self, cell: i32) {
        if let Self::Cells(cells, shift) = self {
            cells.remove(&(cell - *shift));
        }
    }

    /// Update the cells used after a node to those used before it.
    fn before(&mut self, node: &AstNode) {
        match *node {
            AstNode::Move(amount) => {
                if let Self::Cells(_, shift) = self {
                    *shift += amount;
                }
            }
            AstNode::Print(offset) => self.insert(offset),
            AstNode::Read(offset) => self.remove(offset),
            AstNode::Loop(ref body) => self.before_loop(body),
            AstNode::Scan(_) | AstNode::Syscall => *self = Self::All,
            AstNode::DivMod { .. } | AstNode::Skip { .. } => {
                for cell in other_cells(node) {
                    self.insert(cell);
                }
            }
            _ => {
                if matches!(node, AstNode::Set(..) | AstNode::Word { .. }) {
                    for cell in writes(node) {
                        self.remove(cell);
                    }
                }
                for cell in reads(node) {
                    self.insert(cell);
                }
            }
        }
    }

    /// Update the cells used after a loop to those used before it, which
    /// are also the ones used after each iteration.
    fn before_loop(&mut self, body: &[AstNode]) {
        match loop_reads(body) {
            Some(reads) => {
                self.insert(0);
                for cell in reads {
                    self.insert(cell);
                }
            }
            None => *self = Self::All,
        }
    }
}

/// The cells a `DivMod` or `Skip` node accesses.
fn other_cells(node: &AstNode) -> Vec<i32> {
    match *node {
        AstNode::DivMod {
            dividend,
            dividend_len,
            divisor,
            divisor_len,
            quotient,
            ..
        } => (dividend..dividend + i32::from(dividend_len))
            .chain(divisor..divisor + i32::from(divisor_len))
            .chain([quotient])
            .collect(),
        AstNode::Skip {
            ref exits,
            ref steps,
        } => exits
            .iter()
            .map(|&(cell, ..)| cell)
            .chain(steps.iter().map(|&(cell, _)| cell))
            .collect(),
        _ => unreachable!("not a DivMod or Skip node: {node:?}"),
    }
}

/// The cells a loop body may read before writing them, relative to the data
/// pointer, or `None` if it may move the pointer.
#[allow(clippy::range_plus_one)]
fn loop_reads(body: &[AstNode]) -> Option<BTreeSet<i32>> {
    let mut read = BTreeSet::new();
    let mut written = BTreeSet::new();
    let mut pointer = 0;
    for node in body {
        let (node_reads, node_writes) = match *node {
            AstNode::Move(amount) => {
                pointer += amount;
                continue;
            }
            AstNode::Print(offset) => (vec![offset], 0..0),
            AstNode::Read(offset) => (vec![], offset..offset + 1),
            AstNode::Loop(ref body) => (loop_reads(body)?.into_iter().chain([0]).collect(), 0..0),
            AstNode::Scan(_) | AstNode::Syscall => return None,
            AstNode::DivMod { .. } | AstNode::Skip { .. } => (other_cells(node), 0..0),
            _ => (reads(node), writes(node)),
        };
        read.extend(
            node_reads
                .into_iter()
                .map(|cell| cell + pointer)
                .filter(|cell| !written.contains(cell)),
        );
        written.extend(node_writes.map(|cell| cell + pointer));
    }
    (pointer == 0).then_some(read)
}

/// The cells a straight-line node reads, including ones it adds to.
fn reads(node: &AstNode) -> Vec<i32> {
    let mut cells = match *node {
        AstNode::Add(offset, _) => vec![offset],
        AstNode::Set(..) => vec![],
        AstNode::MulAdd { src, dst, .. } => vec![src, dst],
        AstNode::CondAdd { lhs, rhs, dst, .. } => lhs
            .reads()
            .into_iter()
            .chain(rhs.reads())
            .chain([dst])
            .collect(),
        AstNode::ProductAdd {
            base,
            step,
            count,
            dst,
            ..
        } => [base, step, count]
            .iter()
            .filter_map(|operand| operand.reads())
            .chain([dst])
            .collect(),
        AstNode::Word { ref terms, .. } => terms.iter().flat_map(|term| term.reads()).collect(),
        _ => unreachable!("not a straight-line node: {node:?}"),
    };
    cells.sort_unstable();
    cells.dedup();
    cells
}

/// The cells a straight-line node writes.
#[allow(clippy::range_plus_one)]
fn writes(node: &AstNode) -> Range<i32> {
    match *node {
        AstNode::Add(offset, _) | AstNode::Set(offset, _) => offset..offset + 1,
        AstNode::MulAdd { dst, .. }
        | AstNode::CondAdd { dst, .. }
        | AstNode::ProductAdd { dst, .. } => dst..dst + 1,
        AstNode::Word { dst, len, .. } => dst..dst + i32::from(len),
        _ => unreachable!("not a straight-line node: {node:?}"),
    }
}

const fn is_word(node: &AstNode) -> bool {
    matches!(node, AstNode::Word { .. })
}

/// Where the values nodes in a run read come from.
struct Flow {
    /// For each node, the cells it reads and the node which last wrote each
    /// before it, if any
    sources: Vec<Vec<(i32, Option<usize>)>>,
    /// The node which writes each cell last
    last: FastMap<i32, usize>,
    /// The nodes writing each cell, in order
    writers: FastMap<i32, Vec<usize>>,
    /// For each node, the nodes reading what it writes, and the cells
    users: Vec<Vec<(usize, i32)>>,
}

impl Flow {
    fn new(nodes: &[AstNode]) -> Self {
        let mut last = FastMap::default();
        let mut sources = Vec::with_capacity(nodes.len());
        let mut writers: FastMap<i32, Vec<usize>> = FastMap::default();
        let mut users: Vec<Vec<(usize, i32)>> = vec![Vec::new(); nodes.len()];
        for (index, node) in nodes.iter().enumerate() {
            let read: Vec<(i32, Option<usize>)> = reads(node)
                .into_iter()
                .map(|cell| (cell, last.get(&cell).copied()))
                .collect();
            for &(cell, source) in &read {
                if let Some(source) = source {
                    users[source].push((index, cell));
                }
            }
            sources.push(read);
            for cell in writes(node) {
                last.insert(cell, index);
                writers.entry(cell).or_default().push(index);
            }
        }
        Self {
            sources,
            last,
            writers,
            users,
        }
    }

    /// Whether the final value of a cell is computed by a node which could
    /// be lifted.
    fn liftable(&self, nodes: &[AstNode], cell: i32) -> bool {
        self.last
            .get(&cell)
            .is_some_and(|&index| !is_word(&nodes[index]))
    }
}

/// Lift slices from a run of straight-line nodes, after which the cells in
/// `live` are used.
fn lift_run(mut nodes: Vec<AstNode>, live: &Live) -> Vec<AstNode> {
    if nodes.len() < 2 {
        return nodes;
    }

    // Lifting a slice can let ones depending on it be lifted, so keep going
    // until nothing changes. Cells are tried in the order their final values
    // are computed, so that slices computing values others depend on come
    // first.
    let mut scratch = Scratch::default();
    loop {
        let mut flow = Flow::new(&nodes);
        let mut cells: Vec<i32> = flow.last.keys().copied().collect();
        cells.sort_by_key(|cell| flow.last[cell]);

        let mut changed = false;
        for cell in cells {
            if !flow.liftable(&nodes, cell) {
                continue;
            }
            if let Some(lifted) = try_lift(&nodes, &flow, live, cell, &mut scratch) {
                nodes = lifted;
                flow = Flow::new(&nodes);
                scratch.tried.clear();
                changed = true;
            }
        }
        if !changed {
            return nodes;
        }
    }
}

/// State kept between attempts to lift slices from a run.
#[derive(Default)]
struct Scratch {
    /// The slices tried since the run last changed
    tried: FastSet<Vec<usize>>,
    /// Whether each node is in the slice being built
    members: Vec<bool>,
}

/// What a slice leaves in a cell it writes last.
#[derive(Clone, Copy, PartialEq)]
enum Output {
    Constant(u8),
    /// The value the cell had before the slice
    Unchanged,
    /// Part of a number
    Number,
}

/// A `Word` node's fields.
#[derive(Clone)]
struct Word {
    dst: i32,
    len: u8,
    terms: Vec<WordTerm>,
    constant: u64,
}

impl Word {
    fn dst(&self) -> Range<i32> {
        self.dst..self.dst + i32::from(self.len)
    }

    /// Nodes computing the word. Sums of bytes are left to byte nodes, which
    /// compile to better code than a `Word` node.
    fn into_nodes(self) -> Vec<AstNode> {
        if self.len > 1 {
            return vec![AstNode::Word {
                dst: self.dst,
                len: self.len,
                terms: self.terms.into(),
                constant: self.constant,
            }];
        }

        // The factor each cell is added with
        let mut factors: BTreeMap<i32, u8> = BTreeMap::new();
        for term in &self.terms {
            let factor = factors.entry(term.cell).or_insert(0);
            *factor = factor.wrapping_add(if term.negate { u8::MAX } else { 1 });
        }
        // The destination is read before it's written.
        let mut nodes = vec![match factors.remove(&self.dst) {
            Some(factor) => AstNode::MulAdd {
                src: self.dst,
                dst: self.dst,
                factor: factor.wrapping_sub(1),
            },
            None => AstNode::Set(self.dst, 0),
        }];
        #[allow(clippy::cast_possible_truncation)]
        nodes.push(AstNode::Add(self.dst, self.constant as u8));
        nodes.extend(factors.into_iter().map(|(src, factor)| AstNode::MulAdd {
            src,
            dst: self.dst,
            factor,
        }));
        nodes
    }
}

/// Cell values before and after running a slice.
struct Sample {
    before: BTreeMap<i32, u8>,
    after: BTreeMap<i32, u8>,
}

/// The little-endian number in some cells.
fn number(cells: &BTreeMap<i32, u8>, range: Range<i32>) -> u64 {
    range
        .rev()
        .fold(0, |number, cell| (number << 8) | u64::from(cells[&cell]))
}

/// The bits of `len` bytes.
const fn mask(len: u8) -> u64 {
    u64::MAX >> (64 - 8 * len as u32)
}

/// The nodes computing the final value of a cell and those they depend on,
/// in order, along with the nodes using values they compute (other than final
/// values used after them) and those changing what they read before they're
/// done. `None` if that includes a `Word` node or gets too big. `members` is
/// set for the nodes in the slice.
fn slice(nodes: &[AstNode], flow: &Flow, seed: i32, members: &mut [bool]) -> Option<Vec<usize>> {
    let mut slice = Vec::new();
    let mut pending = vec![flow.last[&seed]];
    loop {
        while let Some(index) = pending.pop() {
            // `Word` nodes stay, and the slice reads what they write.
            if is_word(&nodes[index]) || members[index] {
                continue;
            }
            members[index] = true;
            slice.push(index);
            if slice.len() > MAX_SLICE {
                return None;
            }
            pending.extend(flow.sources[index].iter().filter_map(|&(_, source)| source));
        }

        let end = *slice.iter().max()?;
        for &index in &slice {
            // Nodes changing what the slice reads before it ends
            for &(cell, source) in &flow.sources[index] {
                if source.is_some_and(|source| members[source]) {
                    continue;
                }
                let writers = flow.writers.get(&cell).map_or(&[][..], Vec::as_slice);
                let after = writers.partition_point(|&writer| Some(writer) <= source);
                pending.extend(
                    writers[after..]
                        .iter()
                        .take_while(|&&writer| writer < end)
                        .filter(|&&writer| !members[writer]),
                );
            }
            // Nodes using intermediate values
            pending.extend(
                flow.users[index]
                    .iter()
                    .filter(|&&(user, cell)| {
                        !members[user] && (user < end || flow.last[&cell] != index)
                    })
                    .map(|&(user, _)| user),
            );
        }
        if pending.is_empty() {
            slice.sort_unstable();
            return Some(slice);
        }
        if pending.iter().any(|&index| is_word(&nodes[index])) {
            return None;
        }
    }
}

/// Try to replace the slice computing the final value of a cell by `Word` and
/// `Set` nodes.
fn try_lift(
    nodes: &[AstNode],
    flow: &Flow,
    live: &Live,
    seed: i32,
    scratch: &mut Scratch,
) -> Option<Vec<AstNode>> {
    scratch.members.clear();
    scratch.members.resize(nodes.len(), false);
    let slice = slice(nodes, flow, seed, &mut scratch.members)?;
    if !scratch.tried.insert(slice.clone()) {
        return None;
    }
    let members = &scratch.members;
    let end = *slice.last()?;
    let in_slice = |source: Option<usize>| source.is_some_and(|index| members[index]);

    // The cells the slice reads from before it, and the nodes writing them.
    // It has to read each cell before writing it, and always the same value.
    let mut inputs: BTreeMap<i32, Option<usize>> = BTreeMap::new();
    let mut written = BTreeSet::new();
    for &index in &slice {
        for &(cell, source) in &flow.sources[index] {
            if in_slice(source) {
                continue;
            }
            if written.contains(&cell) || *inputs.entry(cell).or_insert(source) != source {
                return None;
            }
        }
        written.extend(writes(&nodes[index]));
    }
    if inputs.len() > MAX_INPUTS {
        return None;
    }

    let inputs: BTreeSet<i32> = inputs.keys().copied().collect();
    // The cells the slice leaves changed which are used after the run or by
    // nodes after the slice
    let outputs: BTreeSet<i32> = written
        .iter()
        .copied()
        .filter(|cell| {
            let last = flow.last[cell];
            members[last]
                && (live.contains(*cell)
                    || flow.users[last]
                        .iter()
                        .any(|&(user, used)| used == *cell && !members[user]))
        })
        .collect();
    // Look the slice up without copying it, since it's usually been seen.
    let mut hasher = FastHasher::default();
    for &index in &slice {
        nodes[index].hash(&mut hasher);
    }
    inputs.hash(&mut hasher);
    outputs.hash(&mut hasher);
    let hash = hasher.finish();
    let matches = |(body, other_inputs, other_outputs): &Key| {
        body.len() == slice.len()
            && body
                .iter()
                .zip(&slice)
                .all(|(node, &index)| *node == nodes[index])
            && *other_inputs == inputs
            && *other_outputs == outputs
    };
    let solution = SOLUTIONS
        .with(|solutions| {
            let solutions = solutions.borrow();
            let bucket = solutions.get(&hash)?;
            bucket
                .iter()
                .find(|(key, _)| matches(key))
                .map(|(_, solution)| solution.clone())
        })
        .unwrap_or_else(|| {
            let body: Vec<AstNode> = slice.iter().map(|&index| nodes[index].clone()).collect();
            let solution = solve(&body, &inputs, &written, &outputs);
            SOLUTIONS.with(|solutions| {
                solutions
                    .borrow_mut()
                    .entry(hash)
                    .or_default()
                    .push(((body, inputs, outputs), solution.clone()));
            });
            solution
        })?;

    let mut output = Vec::with_capacity(nodes.len());
    let mut solution = Some(solution);
    for (index, node) in nodes.iter().enumerate() {
        if index == end {
            output.extend(solution.take()?);
        } else if !members[index] {
            output.push(node.clone());
        }
    }
    Some(output)
}

/// A slice, the cells it reads before writing them, and the ones it leaves
/// changed which are used later
type Key = (Vec<AstNode>, BTreeSet<i32>, BTreeSet<i32>);
/// Replacements for slices, by hash
type Solutions = HashMap<u64, Vec<(Key, Option<Vec<AstNode>>)>>;

thread_local! {
    /// Replacements for slices seen before, by hash. Compiled programs repeat
    /// the same code a lot.
    static SOLUTIONS: RefCell<Solutions> = RefCell::default();
}

/// Find `Word` and `Set` nodes leaving the same values in the `outputs` as a
/// slice, given the cells it reads before writing and the ones it writes.
fn solve(
    body: &[AstNode],
    inputs: &BTreeSet<i32>,
    written: &BTreeSet<i32>,
    outputs: &BTreeSet<i32>,
) -> Option<Vec<AstNode>> {
    let samples = sample(body, inputs, written)?;

    // Classify the outputs, grouping neighboring cells which hold parts of
    // numbers.
    let outputs: Vec<(i32, Output)> = outputs
        .iter()
        .map(|&cell| {
            let first = samples[0].after[&cell];
            let output = if samples.iter().all(|sample| sample.after[&cell] == first) {
                Output::Constant(first)
            } else if inputs.contains(&cell)
                && samples
                    .iter()
                    .all(|sample| sample.after[&cell] == sample.before[&cell])
            {
                Output::Unchanged
            } else {
                Output::Number
            };
            (cell, output)
        })
        .collect();
    let mut groups: Vec<Range<i32>> = Vec::new();
    for &(cell, output) in &outputs {
        if output != Output::Number {
            continue;
        }
        match groups.last_mut() {
            Some(group) if group.end == cell => group.end += 1,
            #[allow(clippy::range_plus_one)]
            _ => groups.push(cell..cell + 1),
        }
    }
    // Only `ProductAdd` nodes multiply cells.
    let products = body
        .iter()
        .any(|node| matches!(node, AstNode::ProductAdd { .. }));
    let mut words = Vec::new();
    for group in groups {
        words.extend(split(group, inputs, &samples, products)?);
    }

    let constants: Vec<(i32, u8)> = outputs
        .iter()
        .filter_map(|&(cell, output)| match output {
            Output::Constant(value) => Some((cell, value)),
            _ => None,
        })
        .collect();
    // Each word counts as two nodes, since it's still a few instructions.
    if 2 * words.len() + constants.len() >= body.len() {
        return None;
    }

    // Words read their terms before others write them.
    let mut ordered: Vec<Word> = Vec::with_capacity(words.len());
    while !words.is_empty() {
        let next = words.iter().position(|word| {
            words.iter().all(|other| {
                std::ptr::eq(word, other)
                    || !other
                        .terms
                        .iter()
                        .any(|term| term.reads().any(|cell| word.dst().contains(&cell)))
            })
        })?;
        ordered.push(words.swap_remove(next));
    }

    let unchanged: Vec<i32> = outputs
        .iter()
        .filter(|&&(_, output)| output == Output::Unchanged)
        .map(|&(cell, _)| cell)
        .collect();
    if !prove(body, inputs, written, &ordered, &constants, &unchanged) {
        return None;
    }

    let mut replacement: Vec<AstNode> = ordered.into_iter().flat_map(Word::into_nodes).collect();
    replacement.extend(
        constants
            .into_iter()
            .map(|(cell, value)| AstNode::Set(cell, value)),
    );
    // The replacement has to be smaller, or it could be lifted again forever.
    let cost: usize = replacement
        .iter()
        .map(|node| if is_word(node) { 2 } else { 1 })
        .sum();
    (cost < body.len()).then_some(replacement)
}

/// Run a slice on random values of the cells it reads.
fn sample(
    body: &[AstNode],
    inputs: &BTreeSet<i32>,
    written: &BTreeSet<i32>,
) -> Option<Vec<Sample>> {
    // Deterministic, so that optimization is too.
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    let mut random = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state.to_le_bytes()[0]
    };

    let first = *inputs.first().into_iter().chain(written.first()).min()?;
    let last = *inputs.last().into_iter().chain(written.last()).max()?;
    let index = |cell: i32| usize::try_from(cell - first).unwrap();
    let cells: BTreeSet<i32> = inputs.union(written).copied().collect();

    let mut samples = Vec::with_capacity(SAMPLES);
    let mut tape = vec![0; index(last) + 1];
    for _ in 0..SAMPLES {
        // Comparisons often only differ at the edges, so those come up a lot.
        let before: BTreeMap<i32, u8> = cells
            .iter()
            .map(|&cell| {
                let value = random();
                let edge = [0, 1, 127, 128, 254, 255][usize::from(value % 6)];
                (cell, if random() < 64 { edge } else { value })
            })
            .collect();
        for (&cell, &value) in &before {
            tape[index(cell)] = value;
        }
        accelerate::run(body, &mut tape, -first, &mut 0)?;
        let after = cells
            .iter()
            .map(|&cell| (cell, tape[index(cell)]))
            .collect();
        samples.push(Sample { before, after });
    }
    Some(samples)
}

/// Express the numbers a slice leaves in some neighboring cells as words,
/// splitting them up if needed, preferring wide words.
fn split(
    cells: Range<i32>,
    inputs: &BTreeSet<i32>,
    samples: &[Sample],
    products: bool,
) -> Option<Vec<Word>> {
    let candidates = Candidates::new(inputs, samples);
    // The words covering the cells from each one to the end
    let mut covers: Vec<Option<Vec<Word>>> = (cells.start..=cells.end).map(|_| None).collect();
    covers[cells.len()] = Some(Vec::new());
    for start in (0..cells.len()).rev() {
        for len in (1..=MAX_LEN.min(u8::try_from(cells.len() - start).ok()?)).rev() {
            let rest = start + usize::from(len);
            if covers[rest].is_none() {
                continue;
            }
            let dst = cells.start + i32::try_from(start).ok()?;
            if let Some(word) = candidates.guess(dst, len, samples, products) {
                let mut words = vec![word];
                words.extend(covers[rest].clone()?);
                covers[start] = Some(words);
                break;
            }
        }
    }
    covers.swap_remove(0)
}

/// A term which may be part of a word, and its value in each sample.
struct Candidate {
    term: WordTerm,
    values: Vec<u64>,
}

/// Numbers held in the cells a slice reads, and their values in each sample.
struct Candidates {
    numbers: Vec<Candidate>,
}

impl Candidates {
    fn new(inputs: &BTreeSet<i32>, samples: &[Sample]) -> Self {
        let numbers = inputs
            .iter()
            .flat_map(|&cell| {
                (1..=MAX_LEN)
                    .take_while(move |&len| inputs.contains(&(cell + i32::from(len) - 1)))
                    .map(move |len| (cell, len))
            })
            .map(|(cell, len)| Candidate {
                term: WordTerm {
                    cell,
                    len,
                    times: None,
                    shift: 0,
                    negate: false,
                },
                values: samples
                    .iter()
                    .map(|sample| number(&sample.before, cell..cell + i32::from(len)))
                    .collect(),
            })
            .collect();
        Self { numbers }
    }

    /// Guess a sum of up to two numbers no wider than `len` bytes, or a
    /// product, and a constant, which the slice leaves in `len` cells from
    /// `dst`.
    fn guess(&self, dst: i32, len: u8, samples: &[Sample], products: bool) -> Option<Word> {
        let mask = mask(len);
        let targets: Vec<u64> = samples
            .iter()
            .map(|sample| number(&sample.after, dst..dst + i32::from(len)))
            .collect();
        let numbers: Vec<&Candidate> = self
            .numbers
            .iter()
            .filter(|number| number.term.len <= len)
            .collect();

        // Terms are given as candidates and whether they're negated.
        let fits = |terms: &[(&Candidate, bool)]| -> Option<Word> {
            let value = |sample: usize| {
                terms.iter().fold(0u64, |sum, &(candidate, negate)| {
                    if negate {
                        sum.wrapping_sub(candidate.values[sample])
                    } else {
                        sum.wrapping_add(candidate.values[sample])
                    }
                })
            };
            let constant = targets[0].wrapping_sub(value(0)) & mask;
            (1..samples.len())
                .all(|sample| value(sample).wrapping_add(constant) & mask == targets[sample])
                .then(|| Word {
                    dst,
                    len,
                    terms: terms
                        .iter()
                        .map(|&(candidate, negate)| WordTerm {
                            negate,
                            ..candidate.term
                        })
                        .collect(),
                    constant,
                })
        };

        let signs = [false, true];
        fits(&[])
            .or_else(|| {
                numbers
                    .iter()
                    .find_map(|&number| signs.iter().find_map(|&sign| fits(&[(number, sign)])))
            })
            .or_else(|| {
                numbers.iter().enumerate().find_map(|(index, &first)| {
                    numbers[index + 1..].iter().find_map(|&second| {
                        signs.iter().find_map(|&first_sign| {
                            signs
                                .iter()
                                .find_map(|&sign| fits(&[(first, first_sign), (second, sign)]))
                        })
                    })
                })
            })
            .or_else(|| {
                products
                    .then(|| self.guess_product(dst, len, &targets))
                    .flatten()
            })
    }

    /// Guess a product of two numbers of at least two bytes which fits in 64
    /// bits, shifted right by whole bytes but not past the result, and a
    /// constant, which the slice leaves in `len` cells from `dst`.
    fn guess_product(&self, dst: i32, len: u8, targets: &[u64]) -> Option<Word> {
        let mask = mask(len);
        let numbers: Vec<&Candidate> = self
            .numbers
            .iter()
            .filter(|number| number.term.len >= 2)
            .collect();
        numbers.iter().enumerate().find_map(|(index, x)| {
            numbers[index..]
                .iter()
                .filter(|y| x.term.len + y.term.len <= MAX_LEN)
                .find_map(|y| {
                    let width = x.term.len + y.term.len;
                    (0..=width.saturating_sub(len)).find_map(|shift| {
                        [false, true].into_iter().find_map(|negate| {
                            let term = WordTerm {
                                times: Some((y.term.cell, y.term.len)),
                                shift,
                                negate,
                                ..x.term
                            };
                            let value =
                                |sample: usize| term.eval(x.values[sample], Some(y.values[sample]));
                            let constant = targets[0].wrapping_sub(value(0)) & mask;
                            (1..targets.len())
                                .all(|sample| {
                                    value(sample).wrapping_add(constant) & mask == targets[sample]
                                })
                                .then(|| Word {
                                    dst,
                                    len,
                                    terms: vec![term],
                                    constant,
                                })
                        })
                    })
                })
        })
    }
}

/// Prove that a slice computes the given words and constants, and leaves the
/// `unchanged` cells as they were, for all values of its `inputs`.
///
/// Decision diagrams decide sums, but blow up on products, which are proven
/// with polynomials instead.
fn prove(
    body: &[AstNode],
    inputs: &BTreeSet<i32>,
    written: &BTreeSet<i32>,
    words: &[Word],
    constants: &[(i32, u8)],
    unchanged: &[i32],
) -> bool {
    let products = words
        .iter()
        .any(|word| word.terms.iter().any(|term| term.times.is_some()));
    prove_exactly(body, inputs, written, words, constants, unchanged) == Some(true)
        || (!products
            && prove_bits(body, inputs, written, words, constants, unchanged) == Some(true))
}

/// Prove a slice computes words with polynomials. See `prove`.
fn prove_exactly(
    body: &[AstNode],
    inputs: &BTreeSet<i32>,
    written: &BTreeSet<i32>,
    words: &[Word],
    constants: &[(i32, u8)],
    unchanged: &[i32],
) -> Option<bool> {
    let mut algebra = Algebra::default();
    let initial: BTreeMap<i32, Poly> = inputs.iter().map(|&cell| (cell, algebra.byte())).collect();
    let mut cells: BTreeMap<i32, Poly> = written
        .iter()
        .map(|&cell| (cell, Poly::constant(0)))
        .collect();
    cells.extend(initial.clone());
    algebra.run(body, &mut cells)?;

    let number = |values: &BTreeMap<i32, Poly>, range: Range<i32>| -> Option<Poly> {
        range.rev().try_fold(Poly::default(), |number, cell| {
            number.scale(256)?.add(values.get(&cell)?)
        })
    };
    for word in words {
        let mut expected = Poly::constant(i128::from(word.constant));
        for term in &word.terms {
            let mut value = number(&initial, term.number())?;
            if let Some((cell, len)) = term.times {
                value = value.mul(&number(&initial, cell..cell + i32::from(len))?)?;
            }
            for _ in 0..term.shift {
                value = algebra.floor(&value)?;
            }
            expected = if term.negate {
                expected.sub(&value)?
            } else {
                expected.add(&value)?
            };
        }
        let difference = number(&cells, word.dst())?.sub(&expected)?;
        if !difference.is_multiple_of(1 << (8 * u32::from(word.len))) {
            return Some(false);
        }
    }
    Some(
        constants
            .iter()
            .all(|&(cell, value)| cells[&cell] == Poly::constant(i128::from(value)))
            && unchanged.iter().all(|cell| cells[cell] == initial[cell]),
    )
}

/// Prove a slice computes words with decision diagrams, or give up if they
/// get too big. See `prove`.
fn prove_bits(
    body: &[AstNode],
    inputs: &BTreeSet<i32>,
    written: &BTreeSet<i32>,
    words: &[Word],
    constants: &[(i32, u8)],
    unchanged: &[i32],
) -> Option<bool> {
    // Order the variables such that the sums stay small: the bits of the
    // terms interleaved from least significant up, after all other cells.
    // Every word gets its own order, and constants are checked with the
    // first.
    let orders: Vec<Vec<(i32, usize)>> = if words.is_empty() {
        vec![Vec::new()]
    } else {
        words
            .iter()
            .map(|word| {
                let mut order = Vec::new();
                for byte in 0..i32::from(word.len) {
                    for bit in 0..8 {
                        for term in &word.terms {
                            if byte < i32::from(term.len) {
                                order.push((term.cell + byte, bit));
                            }
                        }
                    }
                }
                order
            })
            .collect()
    };

    orders
        .iter()
        .enumerate()
        .try_fold(true, |proven, (index, order)| {
            let check = || -> Option<bool> {
                let mut diagrams = Diagrams::default();
                let mut initial: BTreeMap<i32, Bits> =
                    written.iter().map(|&cell| (cell, constant(0))).collect();
                let ordered: BTreeSet<i32> = order.iter().map(|&(cell, _)| cell).collect();
                let order = inputs
                    .iter()
                    .filter(|cell| !ordered.contains(cell))
                    .flat_map(|&cell| (0..8).map(move |bit| (cell, bit)))
                    .chain(order.iter().copied());
                for (var, (cell, bit)) in order.enumerate() {
                    let bits = initial.entry(cell).or_insert_with(|| constant(0));
                    bits[bit] = diagrams.var(u32::try_from(var).ok()?)?;
                }

                let mut state = Symbolic {
                    diagrams: &mut diagrams,
                    cells: initial.clone(),
                };
                state.run(body, 0)?;
                let after = state.cells;
                let d = &mut diagrams;
                let bits = |cells: &BTreeMap<i32, Bits>, range: Range<i32>| -> Bits {
                    range.flat_map(|cell| cells[&cell].clone()).collect()
                };

                if let Some(word) = words.get(index) {
                    let width = usize::from(word.len) * 8;
                    let mut sum: Bits = (0..width)
                        .map(|bit| {
                            if word.constant >> bit & 1 == 1 {
                                TRUE
                            } else {
                                FALSE
                            }
                        })
                        .collect();
                    for term in &word.terms {
                        let mut value = bits(&initial, term.number());
                        value.resize(width, FALSE);
                        sum = if term.negate {
                            d.subtract(&sum, &value)?.0
                        } else {
                            d.add(&sum, &value, FALSE)?.0
                        };
                    }
                    if !d.equal_given(TRUE, &bits(&after, word.dst()), &sum)? {
                        return Some(false);
                    }
                }
                if index == 0 {
                    for &(cell, value) in constants {
                        if after[&cell] != constant(value) {
                            return Some(false);
                        }
                    }
                    for &cell in unchanged {
                        if after[&cell] != initial[&cell] {
                            return Some(false);
                        }
                    }
                }
                Some(true)
            };
            Some(proven && check()?)
        })
}
