//! Acceleration of loops which repeat the same step until a condition fails.
//!
//! Compilers targeting brainfuck implement multi-byte arithmetic with loops
//! over bytes. Division for example becomes a loop which subtracts the
//! divisor from the dividend and counts, until the dividend would drop below
//! zero. Every iteration but the last does the same thing, so all of them
//! can be done at once by a division, leaving only the last one to run.
//! Similarly, loops which count down several cells until one of them reaches
//! some value can skip to the iteration where that happens.
//!
//! Candidates are guessed by running the loop body on random cell values,
//! then proven to hold for all cell values by symbolically executing the body
//! on binary decision diagrams.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::optimizer::inverse;
use super::{AstNode, Operand};

/// Bodies with more nodes than this aren't considered.
const MAX_BODY: usize = 512;
/// The number of random cell values the body is run on.
const SAMPLES: usize = 64;
/// Concrete runs of nested loops are given up after this many iterations.
const MAX_ITERATIONS: usize = 1024;
/// Symbolic runs of nested loops are given up after this many iterations.
const MAX_UNROLL: usize = 4;
/// Proofs are given up when they need more decision diagram nodes than this.
const MAX_NODES: usize = 1 << 20;
/// The widest number supported, in bytes.
const MAX_WIDTH: i32 = 8;
/// The most cell values a counting loop may stop at.
const MAX_EXITS: usize = 4;

thread_local! {
    /// Results for loop bodies seen before. Compiled programs repeat the same
    /// code a lot.
    static CACHE: RefCell<HashMap<Vec<AstNode>, Option<AstNode>>> = RefCell::default();
}

/// Find a node which performs all iterations of a loop but the last one, so
/// that it can be prepended to the loop body.
///
/// Either the loop has to divide: each iteration but the last subtracts a
/// number held in cells it doesn't change from a number in other cells and
/// adds a constant to a counter. It stopping when the first number is below the
/// second, or when the second is zero.
///
/// Or the loop has to count: each iteration but the last adds a constant to
/// each cell it reads. It stopping when one of the cells reaches some value.
pub fn accelerate(body: &[AstNode]) -> Option<AstNode> {
    if size(body) > MAX_BODY {
        return None;
    }
    if let Some(cached) = CACHE.with(|cache| cache.borrow().get(body).cloned()) {
        return cached;
    }

    let result = cells(body).and_then(|cells| {
        let samples = sample(body, &cells)?;
        let changes = constant_changes(&cells, &samples);
        guess(&cells, &samples, &changes)
            .into_iter()
            .find(|division| prove(body, &cells, division))
            .map(|division| division.node())
            .or_else(|| count(body, &cells, &samples, &changes))
    });
    CACHE.with(|cache| cache.borrow_mut().insert(body.to_vec(), result.clone()));
    result
}

/// The number of nodes, including nested ones. Stops counting past `MAX_BODY`.
fn size(nodes: &[AstNode]) -> usize {
    let mut total = 0;
    for node in nodes {
        total += 1;
        if let AstNode::Loop(body) = node {
            total += size(body);
        }
        if total > MAX_BODY {
            break;
        }
    }
    total
}

/// The cells a balanced loop body accesses, relative to the data pointer.
/// `None` if it does anything but straight-line arithmetic and balanced loops.
fn cells(body: &[AstNode]) -> Option<BTreeSet<i32>> {
    fn collect(nodes: &[AstNode], mut pointer: i32, cells: &mut BTreeSet<i32>) -> Option<i32> {
        for node in nodes {
            match *node {
                AstNode::Add(offset, _) | AstNode::Set(offset, _) => {
                    cells.insert(pointer + offset);
                }
                AstNode::MulAdd { src, dst, .. } => {
                    cells.extend([pointer + src, pointer + dst]);
                }
                AstNode::CondAdd { lhs, rhs, dst, .. } => {
                    for operand in [lhs, rhs] {
                        cells.extend(operand.reads().map(|cell| pointer + cell));
                    }
                    cells.insert(pointer + dst);
                }
                AstNode::ProductAdd {
                    base,
                    step,
                    count,
                    dst,
                    ..
                } => {
                    for operand in [base, step, count] {
                        cells.extend(operand.reads().map(|cell| pointer + cell));
                    }
                    cells.insert(pointer + dst);
                }
                AstNode::Skip {
                    ref exits,
                    ref steps,
                } => {
                    cells.extend(exits.iter().map(|&(cell, ..)| pointer + cell));
                    cells.extend(steps.iter().map(|&(cell, _)| pointer + cell));
                }
                AstNode::Move(amount) => pointer += amount,
                AstNode::Loop(ref body) => {
                    cells.insert(pointer);
                    if collect(body, pointer, cells)? != pointer {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        Some(pointer)
    }

    let mut cells = BTreeSet::from([0]);
    (collect(body, 0, &mut cells)? == 0).then_some(cells)
}

/// Cell values before and after running the loop body once.
struct Sample {
    before: BTreeMap<i32, u8>,
    after: BTreeMap<i32, u8>,
}

impl Sample {
    /// The little-endian number in `len` cells from `start`, before or after.
    fn number(&self, after: bool, start: i32, len: i32) -> u64 {
        let cells = if after { &self.after } else { &self.before };
        (start..start + len)
            .rev()
            .fold(0, |number, cell| (number << 8) | u64::from(cells[&cell]))
    }
}

/// Run the body on random cell values for which the loop runs, keeping the
/// samples after which it runs again.
fn sample(body: &[AstNode], cells: &BTreeSet<i32>) -> Option<Vec<Sample>> {
    // Deterministic, so that optimization is too.
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut random = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state.to_le_bytes()[0]
    };

    let mut samples = Vec::new();
    for _ in 0..SAMPLES {
        let mut before: BTreeMap<i32, u8> = cells.iter().map(|&cell| (cell, random())).collect();
        before.insert(0, before[&0].max(1));
        let after = run_once(body, cells, &before)?;
        if after[&0] != 0 {
            samples.push(Sample { before, after });
        }
    }
    (samples.len() >= SAMPLES / 4).then_some(samples)
}

/// Run the body once on the given cell values.
fn run_once(
    body: &[AstNode],
    cells: &BTreeSet<i32>,
    before: &BTreeMap<i32, u8>,
) -> Option<BTreeMap<i32, u8>> {
    let base = *cells.first()?;
    let index = |cell: i32| usize::try_from(cell - base).unwrap();
    let mut tape = vec![0; index(*cells.last()?) + 1];
    for (&cell, &value) in before {
        tape[index(cell)] = value;
    }
    let mut budget = MAX_ITERATIONS;
    run(body, &mut tape, -base, &mut budget)?;
    Some(
        cells
            .iter()
            .map(|&cell| (cell, tape[index(cell)]))
            .collect(),
    )
}

/// The cells whose values before an iteration affect those after it in some
/// of the samples.
fn sampled_reads(body: &[AstNode], cells: &BTreeSet<i32>, samples: &[Sample]) -> BTreeSet<i32> {
    let affects = |cell: i32| {
        samples.iter().take(4).any(|sample| {
            [1, 128].into_iter().any(|change: u8| {
                let mut before = sample.before.clone();
                let value = before[&cell].wrapping_add(change);
                // The loop doesn't run with the current cell at zero.
                if cell == 0 && value == 0 {
                    return false;
                }
                before.insert(cell, value);
                run_once(body, cells, &before).as_ref() != Some(&sample.after)
            })
        })
    };
    cells
        .iter()
        .copied()
        .filter(|&cell| affects(cell))
        .collect()
}

/// Run nodes accepted by `cells` on a tape, with the data pointer at index
/// `pointer`.
fn run(nodes: &[AstNode], tape: &mut [u8], mut pointer: i32, budget: &mut usize) -> Option<()> {
    let at = |pointer: i32, offset: i32| usize::try_from(pointer + offset).unwrap();
    for node in nodes {
        match *node {
            AstNode::Add(offset, value) => {
                let cell = &mut tape[at(pointer, offset)];
                *cell = cell.wrapping_add(value);
            }
            AstNode::Set(offset, value) => tape[at(pointer, offset)] = value,
            AstNode::MulAdd { src, dst, factor } => {
                let value = tape[at(pointer, src)].wrapping_mul(factor);
                let cell = &mut tape[at(pointer, dst)];
                *cell = cell.wrapping_add(value);
            }
            AstNode::CondAdd {
                lhs,
                rhs,
                dst,
                value,
            } => {
                let operand = |operand: Operand| match operand.reads() {
                    Some(cell) => operand.eval(tape[at(pointer, cell)]),
                    None => operand.bias,
                };
                if operand(lhs) < operand(rhs) {
                    let cell = &mut tape[at(pointer, dst)];
                    *cell = cell.wrapping_add(value);
                }
            }
            AstNode::ProductAdd {
                base,
                step,
                count,
                high,
                dst,
                value,
            } => {
                let operand = |operand: Operand| match operand.reads() {
                    Some(cell) => u16::from(operand.eval(tape[at(pointer, cell)])),
                    None => u16::from(operand.bias),
                };
                let [high_byte, low_byte] =
                    (operand(base) + operand(step) * operand(count)).to_be_bytes();
                let byte = if high { high_byte } else { low_byte };
                let cell = &mut tape[at(pointer, dst)];
                *cell = cell.wrapping_add(byte.wrapping_mul(value));
            }
            AstNode::Skip {
                ref exits,
                ref steps,
            } => {
                let count = exits
                    .iter()
                    .map(|&(cell, target, factor)| {
                        target
                            .wrapping_sub(tape[at(pointer, cell)])
                            .wrapping_mul(factor)
                    })
                    .min()?;
                for &(cell, step) in steps {
                    let cell = &mut tape[at(pointer, cell)];
                    *cell = cell.wrapping_add(count.wrapping_mul(step));
                }
            }
            AstNode::Move(amount) => pointer += amount,
            AstNode::Loop(ref body) => {
                while tape[at(pointer, 0)] != 0 {
                    *budget = budget.checked_sub(1)?;
                    run(body, tape, pointer, budget)?;
                }
            }
            _ => return None,
        }
    }
    Some(())
}

/// A loop which divides, as described by `accelerate`. Numbers are
/// little-endian.
#[derive(Debug)]
struct Division {
    dividend: i32,
    dividend_len: i32,
    divisor: i32,
    divisor_len: i32,
    /// The cell counting iterations, and how much it counts by.
    counter: Option<(i32, u8)>,
}

impl Division {
    /// The node performing all iterations but the last.
    fn node(&self) -> AstNode {
        let (quotient, factor) = self.counter.unwrap_or((self.dividend, 0));
        AstNode::DivMod {
            dividend: self.dividend,
            dividend_len: u8::try_from(self.dividend_len).unwrap(),
            divisor: self.divisor,
            divisor_len: u8::try_from(self.divisor_len).unwrap(),
            quotient,
            factor,
        }
    }
}

/// The cells which change by the same amount in every sample, and that amount.
fn constant_changes(cells: &BTreeSet<i32>, samples: &[Sample]) -> BTreeMap<i32, u8> {
    cells
        .iter()
        .filter_map(|&cell| {
            let change = |sample: &Sample| sample.after[&cell].wrapping_sub(sample.before[&cell]);
            let first = change(&samples[0]);
            samples
                .iter()
                .all(|sample| change(sample) == first)
                .then_some((cell, first))
        })
        .collect()
}

/// Guess which cells hold the numbers of a division, from samples after which
/// the loop runs again.
fn guess(cells: &BTreeSet<i32>, samples: &[Sample], changes: &BTreeMap<i32, u8>) -> Vec<Division> {
    let unchanged: BTreeSet<i32> = changes
        .iter()
        .filter(|&(_, &change)| change == 0)
        .map(|(&cell, _)| cell)
        .collect();
    let counters: Vec<(i32, u8)> = changes
        .iter()
        .filter(|&(&cell, &change)| cell != 0 && change != 0)
        .map(|(&cell, &change)| (cell, change))
        .collect();
    if counters.len() > 1 || !unchanged.contains(&0) {
        return Vec::new();
    }
    let counter = counters.first().copied();
    let changing = |cell: i32| cells.contains(&cell) && !unchanged.contains(&cell);
    let is_unchanged =
        |start: i32, len: i32| (start..start + len).all(|cell| unchanged.contains(&cell));

    // Prefer the widest dividend, then the widest divisor.
    let mut divisions = Vec::new();
    for dividend_len in (1..=MAX_WIDTH).rev() {
        for &dividend in cells {
            let span = dividend..dividend + dividend_len;
            if !span.clone().all(changing) || counter.is_some_and(|(cell, _)| span.contains(&cell))
            {
                continue;
            }
            let mask = u64::MAX >> (64 - 8 * dividend_len.unsigned_abs());
            let decrease = |sample: &Sample| {
                sample
                    .number(false, dividend, dividend_len)
                    .wrapping_sub(sample.number(true, dividend, dividend_len))
                    & mask
            };
            for divisor_len in (1..=dividend_len).rev() {
                for &divisor in &unchanged {
                    if is_unchanged(divisor, divisor_len)
                        && samples.iter().all(|sample| {
                            sample.number(false, divisor, divisor_len) == decrease(sample)
                        })
                    {
                        divisions.push(Division {
                            dividend,
                            dividend_len,
                            divisor,
                            divisor_len,
                            counter,
                        });
                    }
                }
            }
        }
    }
    divisions
}

/// Prove that a loop divides as guessed. `cells` are the cells its body
/// accesses.
///
/// Running all iterations but the last at once is correct if every iteration
/// starting with the dividend at least the (non-zero) divisor runs the loop
/// again, subtracts the divisor, and steps the counter, while leaving all
/// other cells it reads unchanged. Every other iteration has to end the loop.
/// Then the last iteration doesn't depend on anything the others did but
/// update the dividend and counter.
fn prove(body: &[AstNode], cells: &BTreeSet<i32>, division: &Division) -> bool {
    prove_division(body, cells, division).unwrap_or(false)
}

fn prove_division(body: &[AstNode], cells: &BTreeSet<i32>, division: &Division) -> Option<bool> {
    let dividend: Vec<i32> =
        (division.dividend..division.dividend + division.dividend_len).collect();
    let divisor: Vec<i32> = (division.divisor..division.divisor + division.divisor_len).collect();

    // Order the variables such that sums and comparisons of the numbers stay
    // small: other cells, then the bits of the numbers interleaved from least
    // significant up, then the counter.
    let mut order: Vec<(i32, usize)> = Vec::new();
    let counter = division.counter.map(|(cell, _)| cell);
    for &cell in cells {
        if !dividend.contains(&cell) && !divisor.contains(&cell) && Some(cell) != counter {
            order.extend((0..8).map(|bit| (cell, bit)));
        }
    }
    for (i, &cell) in dividend.iter().enumerate() {
        for bit in 0..8 {
            if let Some(&other) = divisor.get(i) {
                order.push((other, bit));
            }
            order.push((cell, bit));
        }
    }
    if let Some(cell) = counter {
        order.extend((0..8).map(|bit| (cell, bit)));
    }

    let Iteration {
        mut diagrams,
        initial,
        after,
        runs,
        again,
        read,
    } = Iteration::new(body, cells, &order)?;
    let d = &mut diagrams;

    // Numbers are zero-extended to the width of the dividend.
    let number = |values: &BTreeMap<i32, Bits>, cells: &[i32]| -> Bits {
        let mut bits: Bits = cells.iter().flat_map(|cell| values[cell].clone()).collect();
        bits.resize(dividend.len() * 8, FALSE);
        bits
    };

    // The loop runs again exactly when the divisor is non-zero and at most
    // the dividend, assuming it ran.
    let x = number(&initial, &dividend);
    let y = number(&initial, &divisor);
    let below = d.less(&x, &y)?;
    let divisor_nonzero = d.nonzero(&y)?;
    let not_below = d.not(below)?;
    let divides = d.and(divisor_nonzero, not_below)?;
    let mismatch = d.xor(again, divides)?;
    if d.and(runs, mismatch)? != FALSE {
        return Some(false);
    }

    // When it does, the dividend decreases by the divisor.
    let condition = d.and(runs, divides)?;
    let (difference, _) = d.subtract(&x, &y)?;
    if !d.equal_given(condition, &number(&after, &dividend), &difference)? {
        return Some(false);
    }
    if let Some((cell, step)) = division.counter {
        let (stepped, _) = d.add(&initial[&cell], &constant(step), FALSE)?;
        if !d.equal_given(condition, &after[&cell], &stepped)? {
            return Some(false);
        }
    }

    // Any other cell the body reads stays the same.
    for &cell in cells {
        if (read.contains(&cell) || divisor.contains(&cell))
            && !dividend.contains(&cell)
            && Some(cell) != counter
            && !d.equal_given(condition, &after[&cell], &initial[&cell])?
        {
            return Some(false);
        }
    }
    Some(true)
}

/// Prove that a loop counts, and find out how far. `changes` are the
/// amounts cells changed by in samples where the loop ran again.
///
/// Running all iterations but the last at once is correct if every iteration
/// changes each cell it reads by a constant amount, unless one of them holds
/// some value. Those iterations end the loop, so the loop runs until the
/// first cell reaches its value.
fn count(
    body: &[AstNode],
    cells: &BTreeSet<i32>,
    samples: &[Sample],
    changes: &BTreeMap<i32, u8>,
) -> Option<AstNode> {
    // Cells which are read have to change by a constant, so give up early if
    // the samples show otherwise.
    if !sampled_reads(body, cells, samples)
        .iter()
        .all(|cell| changes.contains_key(cell))
    {
        return None;
    }

    let order: Vec<(i32, usize)> = cells
        .iter()
        .flat_map(|&cell| (0..8).map(move |bit| (cell, bit)))
        .collect();
    let Iteration {
        mut diagrams,
        initial,
        after,
        runs,
        again,
        read,
    } = Iteration::new(body, cells, &order)?;
    let d = &mut diagrams;

    // The values which stop the loop. Only cells stepping by an odd amount
    // reach every value, so they're the only ones considered.
    let continues = d.and(runs, again)?;
    let mut exits = Vec::new();
    let mut stopping = FALSE;
    for (&cell, &step) in changes {
        if step % 2 == 0 {
            continue;
        }
        // Whether the loop can run or continue with each value of the cell
        let vars: Vec<u32> = initial[&cell].iter().map(|&bit| d.variable(bit)).collect();
        let can_run = d.project(runs, &vars)?;
        let can_continue = d.project(continues, &vars)?;
        for target in 0..=u8::MAX {
            let value = |var| {
                let bit = vars.iter().position(|&other| other == var).unwrap();
                target & (1 << bit) != 0
            };
            // Values the loop doesn't run with don't matter.
            if d.holds(can_run, value) && !d.holds(can_continue, value) {
                exits.push((cell, target, inverse(step)));
                let reached = d.equal_constant(&initial[&cell], target)?;
                stopping = d.or(stopping, reached)?;
                if exits.len() > MAX_EXITS {
                    return None;
                }
            }
        }
    }
    if exits.is_empty() {
        return None;
    }

    // The loop runs again exactly when no cell reached its value.
    let condition = d.not(stopping)?;
    let mismatch = d.xor(again, condition)?;
    if d.and(runs, mismatch)? != FALSE {
        return None;
    }

    // Then cells change by constant amounts.
    let condition = d.and(runs, condition)?;
    let mut steps = Vec::new();
    for &cell in cells {
        let step = changes.get(&cell).copied();
        let stepped = match step {
            Some(step) => d.add(&initial[&cell], &constant(step), FALSE)?.0,
            None => initial[&cell].clone(),
        };
        if d.equal_given(condition, &after[&cell], &stepped)? {
            if let Some(step) = step.filter(|&step| step != 0) {
                steps.push((cell, step));
            }
        } else if read.contains(&cell) {
            return None;
        }
    }
    Some(AstNode::Skip { exits, steps })
}

/// One iteration of a loop body, symbolically executed.
struct Iteration {
    diagrams: Diagrams,
    /// Cell values before the iteration, as variables
    initial: BTreeMap<i32, Bits>,
    /// Cell values after the iteration, as functions of `initial`
    after: BTreeMap<i32, Bits>,
    /// Whether the iteration runs: the current cell is non-zero.
    runs: Bdd,
    /// Whether the loop runs again afterwards.
    again: Bdd,
    /// The cells whose values before the iteration affect those after it.
    read: BTreeSet<i32>,
}

impl Iteration {
    /// `order` lists the bits of the body's `cells`, from the first variable
    /// in the decision diagrams to the last. Orders keeping related bits
    /// together keep diagrams small.
    fn new(body: &[AstNode], cells: &BTreeSet<i32>, order: &[(i32, usize)]) -> Option<Self> {
        let mut diagrams = Diagrams::default();
        let mut initial: BTreeMap<i32, Bits> =
            cells.iter().map(|&cell| (cell, vec![FALSE; 8])).collect();
        let mut variables = BTreeMap::new();
        for (var, &(cell, bit)) in order.iter().enumerate() {
            let variable = diagrams.var(u32::try_from(var).ok()?)?;
            initial.get_mut(&cell)?[bit] = variable;
            variables.insert(variable, cell);
        }

        let mut state = Symbolic {
            diagrams: &mut diagrams,
            cells: initial.clone(),
        };
        state.run(body, 0)?;
        let after = state.cells;

        let mut support = BTreeSet::new();
        for bits in after.values() {
            for &bit in bits {
                diagrams.support(bit, &mut support);
            }
        }
        let read = support.iter().map(|variable| variables[variable]).collect();

        Some(Self {
            runs: diagrams.nonzero(&initial[&0])?,
            again: diagrams.nonzero(&after[&0])?,
            diagrams,
            initial,
            after,
            read,
        })
    }
}

/// A binary decision diagram: an index into `Diagrams::nodes`.
type Bdd = u32;
const FALSE: Bdd = 0;
const TRUE: Bdd = 1;

/// A number as diagrams for its bits, least significant first.
type Bits = Vec<Bdd>;

/// The decision node `if var { high } else { low }`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Node {
    var: u32,
    low: Bdd,
    high: Bdd,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Op {
    And,
    Or,
    Xor,
}

/// Reduced ordered binary decision diagrams, so equal functions are the same
/// diagram. Operations return `None` when there are too many nodes.
struct Diagrams {
    nodes: Vec<Node>,
    unique: HashMap<Node, Bdd>,
    cache: HashMap<(Op, Bdd, Bdd), Bdd>,
}

impl Default for Diagrams {
    fn default() -> Self {
        // Terminals order after all variables.
        let terminal = |value| Node {
            var: u32::MAX,
            low: value,
            high: value,
        };
        Self {
            nodes: vec![terminal(FALSE), terminal(TRUE)],
            unique: HashMap::new(),
            cache: HashMap::new(),
        }
    }
}

impl Diagrams {
    fn var(&mut self, var: u32) -> Option<Bdd> {
        self.node(var, FALSE, TRUE)
    }

    fn node(&mut self, var: u32, low: Bdd, high: Bdd) -> Option<Bdd> {
        if low == high {
            return Some(low);
        }
        let node = Node { var, low, high };
        if let Some(&bdd) = self.unique.get(&node) {
            return Some(bdd);
        }
        if self.nodes.len() >= MAX_NODES {
            return None;
        }
        let bdd = Bdd::try_from(self.nodes.len()).ok()?;
        self.nodes.push(node);
        self.unique.insert(node, bdd);
        Some(bdd)
    }

    fn apply(&mut self, op: Op, a: Bdd, b: Bdd) -> Option<Bdd> {
        let (a, b) = (a.min(b), a.max(b));
        match op {
            Op::And if a == FALSE => return Some(FALSE),
            Op::And if a == TRUE || a == b => return Some(b),
            Op::Or if a == TRUE => return Some(TRUE),
            Op::Or if a == FALSE || a == b => return Some(b),
            Op::Xor if a == b => return Some(FALSE),
            Op::Xor if a == FALSE => return Some(b),
            _ => {}
        }
        if let Some(&bdd) = self.cache.get(&(op, a, b)) {
            return Some(bdd);
        }

        let (x, y) = (self.nodes[a as usize], self.nodes[b as usize]);
        let var = x.var.min(y.var);
        let split = |node: Node, bdd: Bdd| {
            if node.var == var {
                (node.low, node.high)
            } else {
                (bdd, bdd)
            }
        };
        let ((a_low, a_high), (b_low, b_high)) = (split(x, a), split(y, b));
        let low = self.apply(op, a_low, b_low)?;
        let high = self.apply(op, a_high, b_high)?;
        let bdd = self.node(var, low, high)?;
        self.cache.insert((op, a, b), bdd);
        Some(bdd)
    }

    fn and(&mut self, a: Bdd, b: Bdd) -> Option<Bdd> {
        self.apply(Op::And, a, b)
    }

    fn or(&mut self, a: Bdd, b: Bdd) -> Option<Bdd> {
        self.apply(Op::Or, a, b)
    }

    fn xor(&mut self, a: Bdd, b: Bdd) -> Option<Bdd> {
        self.apply(Op::Xor, a, b)
    }

    fn not(&mut self, a: Bdd) -> Option<Bdd> {
        self.xor(a, TRUE)
    }

    /// `if condition { then } else { otherwise }`
    fn select(&mut self, condition: Bdd, then: Bdd, otherwise: Bdd) -> Option<Bdd> {
        let then = self.and(condition, then)?;
        let not = self.not(condition)?;
        let otherwise = self.and(not, otherwise)?;
        self.or(then, otherwise)
    }

    /// The variable a diagram for a single variable tests.
    fn variable(&self, bdd: Bdd) -> u32 {
        self.nodes[bdd as usize].var
    }

    /// Whether a diagram holds for some values of the variables not in
    /// `vars`, as a diagram over `vars`.
    fn project(&mut self, bdd: Bdd, vars: &[u32]) -> Option<Bdd> {
        fn project(
            diagrams: &mut Diagrams,
            bdd: Bdd,
            vars: &[u32],
            done: &mut HashMap<Bdd, Bdd>,
        ) -> Option<Bdd> {
            if bdd <= TRUE {
                return Some(bdd);
            }
            if let Some(&projected) = done.get(&bdd) {
                return Some(projected);
            }
            let node = diagrams.nodes[bdd as usize];
            let low = project(diagrams, node.low, vars, done)?;
            let high = project(diagrams, node.high, vars, done)?;
            let projected = if vars.contains(&node.var) {
                diagrams.node(node.var, low, high)?
            } else {
                diagrams.or(low, high)?
            };
            done.insert(bdd, projected);
            Some(projected)
        }
        project(self, bdd, vars, &mut HashMap::new())
    }

    /// Whether a diagram holds for the given variable values.
    fn holds(&self, mut bdd: Bdd, value: impl Fn(u32) -> bool) -> bool {
        while bdd > TRUE {
            let node = self.nodes[bdd as usize];
            bdd = if value(node.var) { node.high } else { node.low };
        }
        bdd == TRUE
    }

    /// The variables a diagram depends on.
    fn support(&self, bdd: Bdd, vars: &mut BTreeSet<Bdd>) {
        let mut stack = vec![bdd];
        let mut seen = BTreeSet::new();
        while let Some(bdd) = stack.pop() {
            if bdd <= TRUE || !seen.insert(bdd) {
                continue;
            }
            let node = self.nodes[bdd as usize];
            // Variables are represented by their diagram.
            vars.insert(
                self.unique[&Node {
                    var: node.var,
                    low: FALSE,
                    high: TRUE,
                }],
            );
            stack.extend([node.low, node.high]);
        }
    }

    /// The sum of two numbers of the same width and a carry, and the carry out.
    fn add(&mut self, a: &[Bdd], b: &[Bdd], mut carry: Bdd) -> Option<(Bits, Bdd)> {
        let mut sum = Vec::with_capacity(a.len());
        for (&a, &b) in a.iter().zip(b) {
            let half = self.xor(a, b)?;
            sum.push(self.xor(half, carry)?);
            // The carry is the bits if they're equal, otherwise the carry in.
            carry = self.select(half, carry, a)?;
        }
        Some((sum, carry))
    }

    /// The difference of two numbers of the same width, and whether it wrapped.
    fn subtract(&mut self, a: &[Bdd], b: &[Bdd]) -> Option<(Bits, Bdd)> {
        let inverted = b
            .iter()
            .map(|&bit| self.not(bit))
            .collect::<Option<Bits>>()?;
        let (difference, carry) = self.add(a, &inverted, TRUE)?;
        Some((difference, self.not(carry)?))
    }

    /// Whether `a < b`, for numbers of the same width.
    fn less(&mut self, a: &[Bdd], b: &[Bdd]) -> Option<Bdd> {
        // The most significant differing bit decides.
        let mut less = FALSE;
        for (&a, &b) in a.iter().zip(b) {
            let differ = self.xor(a, b)?;
            less = self.select(differ, b, less)?;
        }
        Some(less)
    }

    fn nonzero(&mut self, a: &[Bdd]) -> Option<Bdd> {
        a.iter().try_fold(FALSE, |any, &bit| self.or(any, bit))
    }

    /// The low byte of a byte times a constant.
    fn multiply(&mut self, a: &[Bdd], factor: u8) -> Option<Bits> {
        let mut product = constant(0);
        for shift in 0..8 {
            if factor & (1 << shift) != 0 {
                let mut shifted = vec![FALSE; shift];
                shifted.extend_from_slice(&a[..8 - shift]);
                product = self.add(&product, &shifted, FALSE)?.0;
            }
        }
        Some(product)
    }

    fn equal_constant(&mut self, a: &[Bdd], value: u8) -> Option<Bdd> {
        let differences = a
            .iter()
            .zip(constant(value))
            .map(|(&a, b)| self.xor(a, b))
            .collect::<Option<Bits>>()?;
        let differ = self.nonzero(&differences)?;
        self.not(differ)
    }

    /// Whether two numbers are equal whenever `condition` holds.
    fn equal_given(&mut self, condition: Bdd, a: &[Bdd], b: &[Bdd]) -> Option<bool> {
        for (&a, &b) in a.iter().zip(b) {
            let differ = self.xor(a, b)?;
            if self.and(condition, differ)? != FALSE {
                return Some(false);
            }
        }
        Some(true)
    }
}

/// The bits of a byte.
fn constant(value: u8) -> Bits {
    (0..8)
        .map(|bit| if value & (1 << bit) != 0 { TRUE } else { FALSE })
        .collect()
}

/// Symbolic execution of a loop body, with cell values as functions of the
/// cell values before it.
struct Symbolic<'a> {
    diagrams: &'a mut Diagrams,
    cells: BTreeMap<i32, Bits>,
}

impl Symbolic<'_> {
    fn get(&self, cell: i32) -> Option<Bits> {
        self.cells.get(&cell).cloned()
    }

    fn add_to(&mut self, cell: i32, value: &[Bdd]) -> Option<()> {
        let (sum, _) = self.diagrams.add(&self.get(cell)?, value, FALSE)?;
        self.cells.insert(cell, sum);
        Some(())
    }

    fn operand(&mut self, operand: Operand, pointer: i32) -> Option<Bits> {
        let Some(cell) = operand.reads() else {
            return Some(constant(operand.bias));
        };
        let scaled = self
            .diagrams
            .multiply(&self.get(pointer + cell)?, operand.scale)?;
        Some(
            self.diagrams
                .add(&scaled, &constant(operand.bias), FALSE)?
                .0,
        )
    }

    /// Run nodes accepted by `cells` with the data pointer at `pointer`.
    fn run(&mut self, nodes: &[AstNode], mut pointer: i32) -> Option<()> {
        for node in nodes {
            match *node {
                AstNode::Add(offset, value) => self.add_to(pointer + offset, &constant(value))?,
                AstNode::Set(offset, value) => {
                    self.cells.insert(pointer + offset, constant(value));
                }
                AstNode::MulAdd { src, dst, factor } => {
                    let product = self.diagrams.multiply(&self.get(pointer + src)?, factor)?;
                    self.add_to(pointer + dst, &product)?;
                }
                AstNode::CondAdd {
                    lhs,
                    rhs,
                    dst,
                    value,
                } => {
                    let lhs = self.operand(lhs, pointer)?;
                    let rhs = self.operand(rhs, pointer)?;
                    let condition = self.diagrams.less(&lhs, &rhs)?;
                    let addend = constant(value)
                        .into_iter()
                        .map(|bit| self.diagrams.and(condition, bit))
                        .collect::<Option<Bits>>()?;
                    self.add_to(pointer + dst, &addend)?;
                }
                AstNode::ProductAdd {
                    base,
                    step,
                    count,
                    high,
                    dst,
                    value,
                } => {
                    let mut total = self.operand(base, pointer)?;
                    total.resize(16, FALSE);
                    let step = self.operand(step, pointer)?;
                    let count = self.operand(count, pointer)?;
                    for (shift, &bit) in count.iter().enumerate() {
                        let mut shifted = vec![FALSE; shift];
                        for &step in &step {
                            shifted.push(self.diagrams.and(bit, step)?);
                        }
                        shifted.resize(16, FALSE);
                        total = self.diagrams.add(&total, &shifted, FALSE)?.0;
                    }
                    let byte = if high { &total[8..] } else { &total[..8] };
                    let product = self.diagrams.multiply(byte, value)?;
                    self.add_to(pointer + dst, &product)?;
                }
                AstNode::Skip {
                    ref exits,
                    ref steps,
                } => self.skip(exits, steps, pointer)?,
                AstNode::Move(amount) => pointer += amount,
                AstNode::Loop(ref body) => self.run_loop(body, pointer)?,
                _ => return None,
            }
        }
        Some(())
    }

    fn skip(&mut self, exits: &[(i32, u8, u8)], steps: &[(i32, u8)], pointer: i32) -> Option<()> {
        let d = &mut *self.diagrams;
        let mut count: Option<Bits> = None;
        for &(cell, target, factor) in exits {
            let (distance, _) = d.subtract(&constant(target), &self.cells[&(pointer + cell)])?;
            let steps = d.multiply(&distance, factor)?;
            count = Some(match count {
                None => steps,
                Some(count) => {
                    let less = d.less(&steps, &count)?;
                    steps
                        .iter()
                        .zip(&count)
                        .map(|(&steps, &count)| d.select(less, steps, count))
                        .collect::<Option<Bits>>()?
                }
            });
        }
        let count = count?;
        for &(cell, step) in steps {
            let change = self.diagrams.multiply(&count, step)?;
            self.add_to(pointer + cell, &change)?;
        }
        Some(())
    }

    /// Unroll a loop until it's known to have stopped.
    fn run_loop(&mut self, body: &[AstNode], pointer: i32) -> Option<()> {
        for _ in 0..=MAX_UNROLL {
            let runs = self.diagrams.nonzero(&self.get(pointer)?)?;
            if runs == FALSE {
                return Some(());
            }
            let before = self.cells.clone();
            self.run(body, pointer)?;
            for (cell, bits) in &mut self.cells {
                for (bit, &old) in bits.iter_mut().zip(&before[cell]) {
                    *bit = self.diagrams.select(runs, *bit, old)?;
                }
            }
        }
        None
    }
}
