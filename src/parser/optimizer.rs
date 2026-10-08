//! Optimization passes over a freshly parsed AST.
//!
//! Pointer movement is folded into node offsets, loops with a computable
//! effect are replaced by straight-line code, and straight-line code is
//! symbolically executed and re-emitted with the minimum number of writes.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;

use super::{AstNode, Operand};

/// Runs longer than this are flushed before looking into a loop, to keep
/// optimization time linear.
const MAX_RUN: usize = 256;

/// Blocks are split rather than building expressions bigger than this, to
/// keep optimization time linear.
const MAX_EXPR_SIZE: usize = 32;

/// Knowledge about cells further than this from the data pointer is dropped.
const MAX_KNOWN_DISTANCE: i32 = 1024;

/// Optimize a program. All cells start at zero. Unless `keep_tape` is set,
/// cells are only correct as far as the program observes them.
pub fn optimize(nodes: Vec<AstNode>, keep_tape: bool) -> Vec<AstNode> {
    let live = if keep_tape { Live::All } else { Live::none() };
    optimize_block(annotate(nodes), Known::zeroed(), &live)
}

/// Maps switch to a tree rather than moving more cells than this to insert one.
const MAX_INSERT_MOVES: usize = 4096;

/// A map from cells to values, iterated in order of cell. The maps used while
/// optimizing usually hold few cells and are created and cloned a lot, which
/// a sorted vector is much cheaper for than a tree. Big ones which are
/// inserted into anywhere but near the end switch to a tree, so inserting
/// doesn't take quadratic time.
#[derive(Clone)]
enum CellMap<V> {
    Small(Vec<(i32, V)>),
    Large(BTreeMap<i32, V>),
}

impl<V> Default for CellMap<V> {
    fn default() -> Self {
        Self::Small(Vec::new())
    }
}

impl<V> CellMap<V> {
    fn get(&self, cell: i32) -> Option<&V> {
        match self {
            Self::Small(cells) => {
                let index = cells.binary_search_by_key(&cell, |&(other, _)| other).ok()?;
                Some(&cells[index].1)
            }
            Self::Large(cells) => cells.get(&cell),
        }
    }

    fn insert(&mut self, cell: i32, value: V) {
        let mut value = Some(value);
        let slot = self.get_or_insert_with(cell, || value.take().unwrap());
        if let Some(value) = value {
            *slot = value;
        }
    }

    /// The value of a cell, inserting `value()` if there is none.
    fn get_or_insert_with(&mut self, cell: i32, value: impl FnOnce() -> V) -> &mut V {
        let found = match self {
            Self::Small(cells) => cells.binary_search_by_key(&cell, |&(other, _)| other),
            Self::Large(_) => Ok(0),
        };
        if let (Self::Small(cells), Err(index)) = (&mut *self, found)
            && cells.len() - index > MAX_INSERT_MOVES
        {
            *self = Self::Large(std::mem::take(cells).into_iter().collect());
        }
        match self {
            Self::Small(cells) => {
                let index = found.unwrap_or_else(|index| {
                    cells.insert(index, (cell, value()));
                    index
                });
                &mut cells[index].1
            }
            Self::Large(cells) => cells.entry(cell).or_insert_with(value),
        }
    }

    fn iter(&self) -> CellIter<'_, V> {
        match self {
            Self::Small(cells) => CellIter::Small(cells.iter()),
            Self::Large(cells) => CellIter::Large(cells.iter()),
        }
    }

    fn keys(&self) -> impl Iterator<Item = i32> {
        self.iter().map(|(cell, _)| cell)
    }

    /// Subtract `amount` from every cell, keeping those for which `keep`
    /// returns true.
    fn shift(&mut self, amount: i32, keep: impl Fn(i32) -> bool) {
        match self {
            // Shifting every cell by the same amount keeps them sorted.
            Self::Small(cells) => cells.retain_mut(|(cell, _)| {
                *cell -= amount;
                keep(*cell)
            }),
            Self::Large(cells) => {
                *cells = std::mem::take(cells)
                    .into_iter()
                    .map(|(cell, value)| (cell - amount, value))
                    .filter(|&(cell, _)| keep(cell))
                    .collect();
            }
        }
    }
}

/// Iterator over the cells of a `CellMap` and their values.
enum CellIter<'a, V> {
    Small(std::slice::Iter<'a, (i32, V)>),
    Large(std::collections::btree_map::Iter<'a, i32, V>),
}

impl<'a, V> Iterator for CellIter<'a, V> {
    type Item = (i32, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Small(cells) => cells.next().map(|(cell, value)| (*cell, value)),
            Self::Large(cells) => cells.next().map(|(cell, value)| (*cell, value)),
        }
    }
}

/// Knowledge about cell values relative to some data pointer position.
#[derive(Clone, Default)]
struct Known {
    values: CellMap<Option<u8>>,
    /// Value of every cell not in `values`.
    default: Option<u8>,
}

impl Known {
    fn zeroed() -> Self {
        Self {
            values: CellMap::default(),
            default: Some(0),
        }
    }

    /// State at the exit of a loop: the current cell is zero.
    fn loop_exit() -> Self {
        Self {
            values: CellMap::Small(vec![(0, Some(0))]),
            default: None,
        }
    }

    fn get(&self, offset: i32) -> Option<u8> {
        self.values.get(offset).copied().unwrap_or(self.default)
    }

    /// Re-base after the data pointer moved by `amount`. Forgets about
    /// distant cells to keep optimization fast.
    fn shift(&mut self, amount: i32) {
        let keep_all = self.default.is_some();
        self.values.shift(amount, |offset| {
            keep_all || offset.abs() <= MAX_KNOWN_DISTANCE
        });
    }
}

/// The cells whose current value may be read later, relative to the data
/// pointer.
#[derive(Clone, Debug)]
enum Live {
    All,
    /// Cells `cell + shift` for each `cell` in the sorted deque. Shifting is
    /// lazy since unoptimized code moves the pointer a lot. Not a tree since
    /// these are cloned for every loop, and a deque since code often walks
    /// the tape backwards, inserting at the front.
    Cells(VecDeque<i32>, i32),
}

impl Live {
    const fn none() -> Self {
        Self::Cells(VecDeque::new(), 0)
    }

    fn contains(&self, offset: i32) -> bool {
        match self {
            Self::All => true,
            Self::Cells(cells, shift) => cells.binary_search(&(offset - shift)).is_ok(),
        }
    }

    fn insert(&mut self, offset: i32) {
        if let Self::Cells(cells, shift) = self
            && let Err(index) = cells.binary_search(&(offset - *shift))
        {
            cells.insert(index, offset - *shift);
        }
    }

    fn remove(&mut self, offset: i32) {
        if let Self::Cells(cells, shift) = self
            && let Ok(index) = cells.binary_search(&(offset - *shift))
        {
            cells.remove(index);
        }
    }

    fn union(&mut self, other: &Self) {
        match (&mut *self, other) {
            (Self::Cells(cells, shift), Self::Cells(others, other_shift)) => {
                let delta = other_shift - *shift;
                let added = others
                    .iter()
                    .filter(|&&cell| cells.binary_search(&(cell + delta)).is_err())
                    .count();
                if added == 0 {
                    return;
                }

                // Merge from the back, so the cells only move once.
                let mut kept = cells.len();
                let mut others = others.iter().rev().map(|cell| cell + delta).peekable();
                cells.resize(kept + added, 0);
                let cells = cells.make_contiguous();
                for index in (0..cells.len()).rev() {
                    let Some(&other) = others.peek() else {
                        // The remaining cells are already in place.
                        break;
                    };
                    if kept > 0 && cells[kept - 1] >= other {
                        if cells[kept - 1] == other {
                            others.next();
                        }
                        kept -= 1;
                        cells[index] = cells[kept];
                    } else {
                        others.next();
                        cells[index] = other;
                    }
                }
            }
            _ => *self = Self::All,
        }
    }

    /// The cells live before the data pointer moves by `amount`, given the
    /// cells live after.
    fn before_move(&self, amount: i32) -> Self {
        match self {
            Self::All => Self::All,
            Self::Cells(cells, shift) => Self::Cells(cells.clone(), shift + amount),
        }
    }

    /// The cells live before an unoptimized node, given the cells live after.
    fn into_before(self, node: &Raw) -> Self {
        let mut live = self;
        match node {
            Raw::Node(AstNode::Move(amount)) => {
                if let Self::Cells(_, shift) = &mut live {
                    *shift += amount;
                }
            }
            Raw::Node(AstNode::Set(offset, _) | AstNode::Read(offset)) => live.remove(*offset),
            Raw::Node(AstNode::Print(offset)) => live.insert(*offset),
            Raw::Node(AstNode::Add(..)) => {}
            Raw::Loop(_, summary) if summary.clear => live.remove(0),
            Raw::Loop(_, summary) => live.union(&summary.reads),
            Raw::Node(_) => return Self::All,
        }
        live
    }

    /// The cells live before unoptimized nodes, given the cells live after.
    fn before_all(&self, nodes: &[Raw]) -> Self {
        nodes.iter().rev().fold(self.clone(), Self::into_before)
    }
}

/// An unoptimized node. Loops carry what liveness analysis needs to know
/// about them, so it's computed only once.
enum Raw {
    Node(AstNode),
    Loop(Vec<Raw>, Summary),
}

struct Summary {
    /// Whether the loop is `[-]`, which clears the cell whatever its value.
    clear: bool,
    /// Whether the body leaves the data pointer where it started.
    balanced: bool,
    /// The cells the loop may read before writing them, relative to its data
    /// pointer.
    reads: Live,
}

/// Annotate unoptimized nodes for liveness analysis.
fn annotate(nodes: Vec<AstNode>) -> Vec<Raw> {
    nodes
        .into_iter()
        .map(|node| match node {
            AstNode::Loop(body) => {
                let clear = matches!(body[..], [AstNode::Add(0, step)] if step % 2 == 1);
                let body = annotate(body);
                let balanced = balanced(&body);
                let reads = if balanced {
                    let mut reads = Live::none().before_all(&body);
                    reads.insert(0);
                    reads
                } else {
                    Live::All
                };
                Raw::Loop(
                    body,
                    Summary {
                        clear,
                        balanced,
                        reads,
                    },
                )
            }
            node => Raw::Node(node),
        })
        .collect()
}

/// Whether unoptimized nodes leave the data pointer where it started.
fn balanced(nodes: &[Raw]) -> bool {
    let mut offset = 0;
    for node in nodes {
        match node {
            Raw::Node(AstNode::Move(amount)) => offset += amount,
            Raw::Node(AstNode::Syscall) => return false,
            Raw::Loop(_, summary) if !summary.balanced => return false,
            _ => {}
        }
    }
    offset == 0
}

/// A function of the cell values at the start of a block.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Atom {
    /// The value of a cell.
    Cell(i32),
    /// 1 if `lhs < rhs`, otherwise 0.
    Less(Rc<Expr>, Rc<Expr>),
    /// A byte of `base + step * count`. The high byte is how often adding
    /// `step` to `base` `count` times wraps around.
    Product {
        base: Rc<Expr>,
        step: Rc<Expr>,
        count: Rc<Expr>,
        high: bool,
    },
}

impl Atom {
    /// The expressions this atom is computed from.
    fn parts(&self) -> impl Iterator<Item = &Expr> {
        let parts: [Option<&Expr>; 3] = match self {
            Self::Cell(_) => [None, None, None],
            Self::Less(lhs, rhs) => [Some(lhs), Some(rhs), None],
            Self::Product {
                base, step, count, ..
            } => [Some(base), Some(step), Some(count)],
        };
        parts.into_iter().flatten()
    }
}

/// A cell value as a linear combination of atoms: `constant + sum(coefficient * atom)`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Expr {
    constant: u8,
    terms: Terms,
}

/// Atoms with non-zero coefficients, sorted by atom. Expressions rarely have
/// more than a few terms, so a vector is much cheaper than a map, and most
/// have at most one, which is kept inline.
#[derive(Clone, Debug, Default)]
enum Terms {
    #[default]
    None,
    One((Atom, u8)),
    Many(Vec<(Atom, u8)>),
}

impl Terms {
    fn as_slice(&self) -> &[(Atom, u8)] {
        match self {
            Self::None => &[],
            Self::One(term) => std::slice::from_ref(term),
            Self::Many(terms) => terms,
        }
    }

    fn get(&self, atom: &Atom) -> Option<&u8> {
        let terms = self.as_slice();
        let index = terms.binary_search_by(|(other, _)| other.cmp(atom)).ok()?;
        Some(&terms[index].1)
    }

    fn iter(&self) -> impl Iterator<Item = (&Atom, &u8)> {
        self.into_iter()
    }

    fn keys(&self) -> impl Iterator<Item = &Atom> {
        self.as_slice().iter().map(|(atom, _)| atom)
    }

    fn len(&self) -> usize {
        self.as_slice().len()
    }

    fn is_empty(&self) -> bool {
        self.as_slice().is_empty()
    }

    fn add(&mut self, atom: Atom, coefficient: u8) {
        if coefficient == 0 {
            return;
        }
        match self {
            Self::None => *self = Self::One((atom, coefficient)),
            Self::One((other, entry)) if *other == atom => {
                *entry = entry.wrapping_add(coefficient);
                if *entry == 0 {
                    *self = Self::None;
                }
            }
            Self::One(_) => {
                let Self::One(term) = std::mem::take(self) else {
                    unreachable!()
                };
                let mut terms = vec![term, (atom, coefficient)];
                terms.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
                *self = Self::Many(terms);
            }
            Self::Many(terms) => match terms.binary_search_by(|(other, _)| other.cmp(&atom)) {
                Ok(index) => {
                    let entry = &mut terms[index].1;
                    *entry = entry.wrapping_add(coefficient);
                    if *entry == 0 {
                        terms.remove(index);
                    }
                }
                Err(index) => terms.insert(index, (atom, coefficient)),
            },
        }
    }
}

// Terms are compared as sorted lists, however they're stored.
impl PartialEq for Terms {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for Terms {}

impl PartialOrd for Terms {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Terms {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_slice().cmp(other.as_slice())
    }
}

impl<'a> IntoIterator for &'a Terms {
    type Item = (&'a Atom, &'a u8);
    type IntoIter = std::iter::Map<
        std::slice::Iter<'a, (Atom, u8)>,
        fn(&'a (Atom, u8)) -> (&'a Atom, &'a u8),
    >;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice()
            .iter()
            .map(|(atom, coefficient)| (atom, coefficient))
    }
}

impl Expr {
    const fn constant(value: u8) -> Self {
        Self {
            constant: value,
            terms: Terms::None,
        }
    }

    fn cell(offset: i32) -> Self {
        Self::atom(Atom::Cell(offset))
    }

    fn atom(atom: Atom) -> Self {
        Self {
            constant: 0,
            terms: Terms::One((atom, 1)),
        }
    }

    fn operand(operand: Operand) -> Self {
        let mut expr = Self::constant(operand.bias);
        if operand.scale != 0 {
            expr.add_term(Atom::Cell(operand.cell), operand.scale);
        }
        expr
    }

    /// 1 if `lhs < rhs`, otherwise 0.
    fn less(lhs: Self, rhs: Self) -> Self {
        match (lhs.as_constant(), rhs.as_constant()) {
            (Some(l), Some(r)) => Self::constant(u8::from(l < r)),
            // Nothing is below 0 or above 255.
            (_, Some(0)) | (Some(u8::MAX), _) => Self::constant(0),
            _ => Self::atom(Atom::Less(Rc::new(lhs), Rc::new(rhs))),
        }
    }

    /// A byte of `base + step * count`.
    fn product(base: Self, step: Self, count: Self, high: bool) -> Self {
        match (base.as_constant(), step.as_constant(), count.as_constant()) {
            (Some(base), Some(step), Some(count)) => {
                let [high_byte, low_byte] =
                    (u16::from(base) + u16::from(step) * u16::from(count)).to_be_bytes();
                Self::constant(if high { high_byte } else { low_byte })
            }
            (_, Some(0), _) | (_, _, Some(0)) if high => Self::constant(0),
            // The low byte is linear when either factor is constant.
            (_, Some(step), _) if !high => {
                let mut expr = count.scaled(step);
                expr.add_scaled(&base, 1);
                expr
            }
            (_, _, Some(count)) if !high => {
                let mut expr = step.scaled(count);
                expr.add_scaled(&base, 1);
                expr
            }
            _ => Self::atom(Atom::Product {
                base: Rc::new(base),
                step: Rc::new(step),
                count: Rc::new(count),
                high,
            }),
        }
    }

    /// Whether this expression is just `atom`.
    fn is_atom(&self, atom: &Atom) -> bool {
        self.constant == 0 && matches!(self.terms.as_slice(), [(other, 1)] if other == atom)
    }

    fn as_constant(&self) -> Option<u8> {
        self.terms.is_empty().then_some(self.constant)
    }

    /// This expression as an operand, if it depends on at most one cell.
    fn as_operand(&self) -> Option<Operand> {
        match *self.terms.as_slice() {
            [] => Some(Operand::constant(self.constant)),
            [(Atom::Cell(cell), scale)] => Some(Operand {
                cell,
                scale,
                bias: self.constant,
            }),
            _ => None,
        }
    }

    /// Whether this expression is always 0 or 1.
    fn is_boolean(&self) -> bool {
        match *self.terms.as_slice() {
            [] => self.constant <= 1,
            [(Atom::Less(..), k)] => matches!((k, self.constant), (1, 0) | (u8::MAX, 1)),
            _ => false,
        }
    }

    fn add_term(&mut self, atom: Atom, coefficient: u8) {
        self.terms.add(atom, coefficient);
    }

    fn add_scaled(&mut self, other: &Self, factor: u8) {
        self.constant = self
            .constant
            .wrapping_add(other.constant.wrapping_mul(factor));

        for (atom, &coefficient) in &other.terms {
            self.add_term(atom.clone(), coefficient.wrapping_mul(factor));
        }
    }

    fn scaled(&self, factor: u8) -> Self {
        let mut expr = Self::constant(0);
        expr.add_scaled(self, factor);
        expr
    }

    fn minus(&self, other: &Self) -> Self {
        let mut expr = self.clone();
        expr.add_scaled(other, u8::MAX);
        expr
    }

    /// Collect the cells read by this expression, possibly more than once.
    fn reads(&self, cells: &mut Vec<i32>) {
        for atom in self.terms.keys() {
            match atom {
                Atom::Cell(offset) => cells.push(*offset),
                atom => {
                    for part in atom.parts() {
                        part.reads(cells);
                    }
                }
            }
        }
    }

    /// The number of atoms in this expression, including nested ones.
    fn size(&self) -> usize {
        self.terms
            .keys()
            .map(|atom| 1 + atom.parts().map(Expr::size).sum::<usize>())
            .sum()
    }

    fn reads_cell(&self, offset: i32) -> bool {
        self.terms.keys().any(|atom| match atom {
            Atom::Cell(cell) => *cell == offset,
            atom => atom.parts().any(|part| part.reads_cell(offset)),
        })
    }

    /// Replace cells with known values by constants.
    fn substitute(&self, known: &Known) -> Self {
        let mut expr = Self::constant(self.constant);
        for (atom, &coefficient) in &self.terms {
            let value = match atom {
                Atom::Cell(offset) => known
                    .get(*offset)
                    .map_or_else(|| Self::cell(*offset), Self::constant),
                Atom::Less(lhs, rhs) => Self::less(lhs.substitute(known), rhs.substitute(known)),
                Atom::Product {
                    base,
                    step,
                    count,
                    high,
                } => Self::product(
                    base.substitute(known),
                    step.substitute(known),
                    count.substitute(known),
                    *high,
                ),
            };
            expr.add_scaled(&value, coefficient);
        }
        expr
    }
}

/// Symbolic execution of a straight-line run of nodes.
#[derive(Default)]
struct Affine {
    known: Known,
    exprs: CellMap<Expr>,
}

impl Affine {
    fn new(known: Known) -> Self {
        Self {
            known,
            exprs: CellMap::default(),
        }
    }

    fn initial(&self, offset: i32) -> Expr {
        self.known
            .get(offset)
            .map_or_else(|| Expr::cell(offset), Expr::constant)
    }

    fn get(&self, offset: i32) -> Expr {
        self.exprs
            .get(offset)
            .cloned()
            .unwrap_or_else(|| self.initial(offset))
    }

    /// The expression for a cell, for modification.
    fn get_mut(&mut self, offset: i32) -> &mut Expr {
        let initial = self.known.get(offset);
        self.exprs
            .get_or_insert_with(offset, || initial.map_or_else(|| Expr::cell(offset), Expr::constant))
    }

    /// Whether an expression is the initial value of a cell.
    fn is_initial(known: &Known, offset: i32, expr: &Expr) -> bool {
        match known.get(offset) {
            Some(value) => expr.as_constant() == Some(value),
            None => {
                expr.constant == 0
                    && expr.terms.len() == 1
                    && expr.terms.get(&Atom::Cell(offset)) == Some(&1)
            }
        }
    }

    /// The current value of an operand.
    fn operand(&self, operand: Operand) -> Expr {
        let mut expr = Expr::constant(operand.bias);
        if operand.scale != 0 {
            expr.add_scaled(&self.get(operand.cell), operand.scale);
        }
        expr
    }

    /// Whether applying a node would build an overly big expression.
    fn too_complex(&self, node: &AstNode) -> bool {
        let cells = match *node {
            AstNode::MulAdd { src, dst, .. } => vec![src, dst],
            AstNode::CondAdd { lhs, rhs, dst, .. } => vec![lhs.cell, rhs.cell, dst],
            AstNode::ProductAdd {
                base,
                step,
                count,
                dst,
                ..
            } => vec![base.cell, step.cell, count.cell, dst],
            _ => return false,
        };
        let size: usize = cells
            .into_iter()
            .filter_map(|cell| self.exprs.get(cell))
            .map(Expr::size)
            .sum();
        size > MAX_EXPR_SIZE
    }

    fn apply(&mut self, node: &AstNode) {
        match *node {
            AstNode::Add(offset, value) => {
                let expr = self.get_mut(offset);
                expr.constant = expr.constant.wrapping_add(value);
            }
            AstNode::Set(offset, value) => {
                self.exprs.insert(offset, Expr::constant(value));
            }
            AstNode::MulAdd { src, dst, factor } => {
                let source = self.get(src);
                self.get_mut(dst).add_scaled(&source, factor);
            }
            AstNode::CondAdd {
                lhs,
                rhs,
                dst,
                value,
            } => {
                let condition = Expr::less(self.operand(lhs), self.operand(rhs));
                self.get_mut(dst).add_scaled(&condition, value);
            }
            AstNode::ProductAdd {
                base,
                step,
                count,
                high,
                dst,
                value,
            } => {
                let product = Expr::product(
                    self.operand(base),
                    self.operand(step),
                    self.operand(count),
                    high,
                );
                self.get_mut(dst).add_scaled(&product, value);
            }
            _ => unreachable!("not a straight-line node: {node:?}"),
        }
    }

    /// Cells whose value changed, with their new value.
    fn changed(&self) -> impl Iterator<Item = (i32, &Expr)> {
        self.exprs
            .iter()
            .filter(|&(offset, expr)| !Self::is_initial(&self.known, offset, expr))
    }

    /// Knowledge about cells after this block.
    fn known_after(&self) -> Known {
        let mut known = self.known.clone();
        for (offset, expr) in self.changed() {
            known.values.insert(offset, expr.as_constant());
        }
        known
    }

    /// Knowledge about cells after this block, without copying what's known
    /// at its start.
    fn into_known_after(self) -> Known {
        let Self { mut known, exprs } = self;
        for (offset, expr) in exprs.iter() {
            if !Self::is_initial(&known, offset, expr) {
                known.values.insert(offset, expr.as_constant());
            }
        }
        known
    }

    /// Emit an equivalent sequence of nodes, or `None` if that isn't possible.
    /// Only cells in `live` need their final value.
    fn lower(&self, live: &Live) -> Option<Vec<AstNode>> {
        // Blocks are small, so sorted vectors are used rather than maps.
        let changed: Vec<(i32, &Expr)> = self
            .changed()
            .filter(|&(offset, _)| live.contains(offset))
            .collect();
        let is_changed =
            |offset: i32| changed.binary_search_by_key(&offset, |&(cell, _)| cell).is_ok();

        let mut read = Vec::new();
        for (_, expr) in &changed {
            expr.reads(&mut read);
        }
        read.sort_unstable();
        read.dedup();

        // Cells can hold temporary values if their initial value isn't
        // needed and they end up holding a constant (which is written
        // afterwards), or they're dead. So can cells the block touches without
        // changing them, if their value is known and restored.
        let scratch: Vec<(i32, Option<u8>)> = self
            .exprs
            .keys()
            .filter(|offset| read.binary_search(offset).is_err())
            .filter_map(|offset| {
                let index = changed.binary_search_by_key(&offset, |&(cell, _)| cell);
                let restore = match index {
                    _ if !live.contains(offset) => None,
                    Ok(index) => Some(changed[index].1.as_constant()?),
                    Err(_) => Some(self.known.get(offset)?),
                };
                Some((offset, restore))
            })
            .collect();

        let writes = changed
            .iter()
            .filter(|(offset, _)| {
                scratch
                    .binary_search_by_key(offset, |&(cell, _)| cell)
                    .is_err()
            })
            .copied()
            .collect();
        let free = scratch.iter().map(|&(offset, _)| offset).collect();
        let (mut output, used) = Schedule::new(writes, free).run()?;

        for (offset, restore) in scratch {
            if let Some(value) = restore
                && (is_changed(offset) || used.contains(&offset))
            {
                output.push(AstNode::Set(offset, value));
            }
        }
        Some(output)
    }
}

/// Orders the computations of a block such that every cell's initial value is
/// read before it's overwritten. Sub-expressions which can't be expressed as
/// an operand are computed once into temporary cells.
///
/// The tasks are writing the final value of each cell in `writes`, followed
/// by computing each temporary. Writes come first since they're preferred:
/// they free up temporaries.
struct Schedule<'a> {
    output: Vec<AstNode>,
    /// Final value of each cell, sorted by cell
    writes: Vec<(i32, &'a Expr)>,
    temporaries: Vec<&'a Expr>,
    /// Cell holding each temporary, once it's computed
    computed: Vec<Option<i32>>,
    /// Cells available for temporary values
    free: Vec<i32>,
    /// Cells whose initial value is read by each task
    reads: Vec<Vec<i32>>,
    /// Temporaries each task uses, in ascending order
    uses: Vec<Vec<usize>>,
    /// Number of pending tasks reading each cell's initial value, sorted by
    /// cell
    readers: Vec<(i32, usize)>,
    /// Number of pending tasks using each temporary
    users: Vec<usize>,
}

impl<'a> Schedule<'a> {
    fn new(writes: Vec<(i32, &'a Expr)>, free: Vec<i32>) -> Self {
        let mut schedule = Self {
            output: Vec::new(),
            writes,
            temporaries: Vec::new(),
            computed: Vec::new(),
            free,
            reads: Vec::new(),
            uses: Vec::new(),
            readers: Vec::new(),
            users: Vec::new(),
        };

        for index in 0..schedule.writes.len() {
            schedule.collect_temporaries(schedule.writes[index].1);
        }
        for index in 0..schedule.writes.len() {
            let (offset, expr) = schedule.writes[index];
            schedule.add_task(Some(offset), expr);
        }
        for index in 0..schedule.temporaries.len() {
            schedule.add_task(None, schedule.temporaries[index]);
        }

        let mut read = Vec::new();
        for (task, cells) in schedule.reads.iter().enumerate() {
            read.extend(
                cells
                    .iter()
                    .filter(|&&cell| schedule.written_by(task) != Some(cell)),
            );
        }
        read.sort_unstable();
        for cell in read {
            match schedule.readers.last_mut() {
                Some((last, count)) if *last == cell => *count += 1,
                _ => schedule.readers.push((cell, 1)),
            }
        }

        schedule
    }

    /// The cell a task writes the final value of, if it's a write.
    fn written_by(&self, task: usize) -> Option<i32> {
        self.writes.get(task).map(|&(offset, _)| offset)
    }

    /// The task writing the final value of a cell, if there is one.
    fn write_task(&self, cell: i32) -> Option<usize> {
        self.writes
            .binary_search_by_key(&cell, |&(offset, _)| offset)
            .ok()
    }

    /// Whether a task can run: the temporaries it uses are computed and, if
    /// it overwrites a cell, nothing else needs the cell's initial value.
    fn ready(&self, task: usize, missing: &[usize]) -> bool {
        missing[task] == 0
            && self.written_by(task).is_none_or(|offset| {
                self.readers
                    .binary_search_by_key(&offset, |&(cell, _)| cell)
                    .map_or(0, |index| self.readers[index].1)
                    == 0
            })
    }

    /// Find the sub-expressions which have to be computed into temporaries
    /// because they can't be expressed as operands.
    fn collect_temporaries(&mut self, expr: &'a Expr) {
        for atom in expr.terms.keys() {
            for part in atom.parts() {
                if part.as_operand().is_none() && !self.temporaries.contains(&part) {
                    self.collect_temporaries(part);
                    self.temporaries.push(part);
                    self.computed.push(None);
                    self.users.push(0);
                }
            }
        }
    }

    /// The temporary holding the value of an atom, if there is one.
    fn temporary_for(&self, atom: &Atom) -> Option<usize> {
        self.temporaries.iter().position(|other| other.is_atom(atom))
    }

    /// Record which cells and temporaries a task computing `expr` reads. The
    /// task writes the final value of the cell `write`, or computes the next
    /// temporary.
    fn add_task(&mut self, write: Option<i32>, expr: &Expr) {
        let mut reads = Vec::new();
        let mut uses = Vec::new();
        let computing = match write {
            Some(_) => None,
            None => Some(self.uses.len() - self.writes.len()),
        };

        for atom in expr.terms.keys() {
            if let Some(temporary) = self.temporary_for(atom)
                && computing != Some(temporary)
            {
                uses.push(temporary);
                continue;
            }
            match atom {
                Atom::Cell(offset) => reads.push(*offset),
                atom => {
                    for part in atom.parts() {
                        if let Some(operand) = part.as_operand() {
                            reads.extend(operand.reads());
                        } else {
                            uses.push(
                                self.temporaries
                                    .iter()
                                    .position(|&other| other == part)
                                    .unwrap(),
                            );
                        }
                    }
                }
            }
        }

        reads.sort_unstable();
        reads.dedup();
        uses.sort_unstable();
        uses.dedup();
        for &temporary in &uses {
            self.users[temporary] += 1;
        }
        self.reads.push(reads);
        self.uses.push(uses);
    }

    /// Returns the nodes and the cells used for temporaries.
    fn run(mut self) -> Option<(Vec<AstNode>, Vec<i32>)> {
        let mut used = Vec::new();

        // Number of temporaries each task uses which aren't computed yet
        let mut missing: Vec<usize> = self.uses.iter().map(Vec::len).collect();
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); self.temporaries.len()];
        for (task, uses) in self.uses.iter().enumerate() {
            for &temporary in uses {
                dependents[temporary].push(task);
            }
        }

        let mut queue: BTreeSet<usize> = (0..self.uses.len())
            .filter(|&task| self.ready(task, &missing))
            .collect();

        let mut done = 0;
        while let Some(task) = queue.pop_first() {
            done += 1;

            if let Some(offset) = self.written_by(task) {
                let expr = self.writes[task].1;
                self.assign(offset, expr, false)?;
            } else {
                let index = task - self.writes.len();
                let cell = self.free.pop()?;
                used.push(cell);
                self.assign(cell, self.temporaries[index], true)?;
                self.computed[index] = Some(cell);

                for &dependent in &dependents[index] {
                    missing[dependent] -= 1;
                    if self.ready(dependent, &missing) {
                        queue.insert(dependent);
                    }
                }
            }

            for index in 0..self.reads[task].len() {
                let cell = self.reads[task][index];
                if self.written_by(task) == Some(cell) {
                    continue;
                }
                if let Ok(reader) = self
                    .readers
                    .binary_search_by_key(&cell, |&(other, _)| other)
                {
                    self.readers[reader].1 -= 1;
                    if let Some(write) = self.write_task(cell)
                        && self.ready(write, &missing)
                    {
                        queue.insert(write);
                    }
                }
            }
            for &temporary in &self.uses[task] {
                self.users[temporary] -= 1;
                if self.users[temporary] == 0 {
                    self.free.push(self.computed[temporary].unwrap());
                }
            }
        }

        (done == self.uses.len()).then_some((self.output, used))
    }

    /// Make `dst` hold the value of `expr`. Unless it's `scratch`, `dst`
    /// currently holds its initial value.
    fn assign(&mut self, dst: i32, expr: &Expr, scratch: bool) -> Option<()> {
        let coefficient = if scratch {
            None
        } else {
            expr.terms.get(&Atom::Cell(dst)).copied()
        };
        let reads_self = |atom: &Atom| {
            !scratch
                && match atom {
                    Atom::Cell(offset) => *offset == dst,
                    atom => atom.parts().any(|part| part.reads_cell(dst)),
                }
        };

        // Conditions reading the cell itself have to be emitted first, while
        // it still holds its initial value. That only works if the rest of
        // the expression adds to the cell.
        let self_conditions: Vec<_> = expr
            .terms
            .iter()
            .filter(|(atom, _)| !matches!(atom, Atom::Cell(_)) && reads_self(atom))
            .collect();
        if !self_conditions.is_empty() && coefficient != Some(1) {
            return None;
        }
        for (atom, &value) in self_conditions {
            self.term(dst, atom, value)?;
        }

        match coefficient {
            Some(coefficient) => {
                if coefficient != 1 {
                    // cell += cell * (k - 1)  ==>  cell *= k
                    self.term(dst, &Atom::Cell(dst), coefficient.wrapping_sub(1))?;
                }
                if expr.constant != 0 {
                    self.output.push(AstNode::Add(dst, expr.constant));
                }
            }
            None => self.output.push(AstNode::Set(dst, expr.constant)),
        }

        for (atom, &value) in &expr.terms {
            if !reads_self(atom) {
                self.term(dst, atom, value)?;
            }
        }

        Some(())
    }

    /// Add `value * atom` to `dst`.
    fn term(&mut self, dst: i32, atom: &Atom, value: u8) -> Option<()> {
        let computed = self
            .temporary_for(atom)
            .and_then(|index| self.computed[index]);
        let node = match (atom, computed) {
            (_, Some(src)) if src != dst => AstNode::MulAdd {
                src,
                dst,
                factor: value,
            },
            (Atom::Cell(src), _) => AstNode::MulAdd {
                src: *src,
                dst,
                factor: value,
            },
            (Atom::Less(lhs, rhs), _) => AstNode::CondAdd {
                lhs: self.operand(lhs)?,
                rhs: self.operand(rhs)?,
                dst,
                value,
            },
            (
                Atom::Product {
                    base,
                    step,
                    count,
                    high,
                },
                _,
            ) => AstNode::ProductAdd {
                base: self.operand(base)?,
                step: self.operand(step)?,
                count: self.operand(count)?,
                high: *high,
                dst,
                value,
            },
        };
        self.output.push(node);
        Some(())
    }

    fn operand(&self, expr: &Expr) -> Option<Operand> {
        expr.as_operand().or_else(|| {
            let index = self.temporaries.iter().position(|&other| other == expr)?;
            Some(Operand::cell(self.computed[index]?))
        })
    }
}

const fn is_straight_line(node: &AstNode) -> bool {
    matches!(
        node,
        AstNode::Add(..)
            | AstNode::Set(..)
            | AstNode::MulAdd { .. }
            | AstNode::CondAdd { .. }
            | AstNode::ProductAdd { .. }
    )
}

/// Lower straight-line nodes which can't be lowered as a whole by lowering
/// each half separately.
fn lower_halves(nodes: &[AstNode], known: &Known, live: &Live) -> Vec<AstNode> {
    if nodes.len() < 2 {
        return nodes.to_vec();
    }

    let (first, second) = nodes.split_at(nodes.len() / 2);
    let mut affine = Affine::new(known.clone());
    for node in first {
        affine.apply(node);
    }
    let mut output = affine
        .lower(&Live::All)
        .unwrap_or_else(|| lower_halves(first, known, &Live::All));

    let known = affine.into_known_after();
    let mut affine = Affine::new(known.clone());
    for node in second {
        affine.apply(node);
    }
    output.extend(
        affine
            .lower(live)
            .unwrap_or_else(|| lower_halves(second, &known, live)),
    );
    output
}

/// Optimize a run of straight-line, `Print` and `Read` nodes, after which
/// only the cells in `live` are used. Returns the new nodes and the knowledge
/// about cells after them.
fn optimize_run(nodes: Vec<AstNode>, known: Known, live: &Live) -> (Vec<AstNode>, Known) {
    let mut output = Vec::new();
    // Nodes symbolically executed by `affine`
    let mut straight = Vec::new();
    let mut affine = Affine::new(known);

    // Emit the current block and start a new one.
    let flush = |affine: &mut Affine,
                 straight: &mut Vec<AstNode>,
                 output: &mut Vec<AstNode>,
                 live: &Live| {
        output.extend(
            affine
                .lower(live)
                .unwrap_or_else(|| lower_halves(straight, &affine.known, live)),
        );
        *affine = Affine::new(std::mem::take(affine).into_known_after());
        straight.clear();
    };

    for node in nodes {
        match node {
            AstNode::Print(_) => {
                flush(&mut affine, &mut straight, &mut output, &Live::All);
                output.push(node);
            }
            AstNode::Read(offset) => {
                flush(&mut affine, &mut straight, &mut output, &Live::All);
                affine.known.values.insert(offset, None);
                output.push(node);
            }
            _ => {
                if affine.too_complex(&node) {
                    flush(&mut affine, &mut straight, &mut output, &Live::All);
                }
                affine.apply(&node);
                straight.push(node);
            }
        }
    }
    flush(&mut affine, &mut straight, &mut output, live);

    (output, affine.known)
}

/// The knowledge about cells after a run of straight-line, `Print` and `Read`
/// nodes, updated as nodes are added to the run.
struct KnownAfterRun(Affine);

impl KnownAfterRun {
    fn new(known: Known) -> Self {
        Self(Affine::new(known))
    }

    fn push(&mut self, node: &AstNode) {
        let affine = &mut self.0;
        match node {
            AstNode::Print(_) => {}
            AstNode::Read(offset) => {
                let mut known = std::mem::take(affine).into_known_after();
                known.values.insert(*offset, None);
                *affine = Affine::new(known);
            }
            _ => {
                if affine.too_complex(node) {
                    *affine = Affine::new(std::mem::take(affine).into_known_after());
                }
                affine.apply(node);
            }
        }
    }

    fn get(&self) -> Known {
        self.0.known_after()
    }
}

/// Result of optimizing a loop.
enum OptimizedLoop {
    /// Straight-line nodes equivalent to the loop.
    Inline(Vec<AstNode>),
    Node(AstNode),
}

/// Optimize the body of a loop and try to replace the loop with something
/// cheaper. `entry` describes the cells when the loop is reached, `exit` the
/// cells used after it and `live` the cells used by the next iteration or
/// after the loop.
fn optimize_loop(body: Vec<Raw>, entry: &Known, exit: &Live, live: &Live) -> OptimizedLoop {
    let body = optimize_block(body, Known::default(), live);

    if let [AstNode::Move(stride)] = body[..] {
        return OptimizedLoop::Node(AstNode::Scan(stride));
    }

    solve_loop(&body, entry, exit, live).unwrap_or(OptimizedLoop::Node(AstNode::Loop(body)))
}

/// Computes the total effect of a loop which runs `iterations` times.
struct Summation<'a> {
    /// The effect of one iteration
    body: &'a Affine,
    iterations: Expr,
}

impl Summation<'_> {
    /// The amount a cell changes by in one iteration, if that doesn't depend
    /// on the cell itself.
    fn increment(&self, offset: i32) -> Option<Expr> {
        let increment = self.body.get(offset).minus(&Expr::cell(offset));
        (!increment.reads_cell(offset)).then_some(increment)
    }

    /// The sum of an expression over all iterations.
    fn sum(&self, expr: &Expr) -> Option<Expr> {
        let mut total = self.iterations.scaled(expr.constant);
        for (atom, &coefficient) in &expr.terms {
            total.add_scaled(&self.sum_atom(atom)?, coefficient);
        }
        Some(total)
    }

    fn sum_atom(&self, atom: &Atom) -> Option<Expr> {
        // A cell which doesn't change sums to n * cell.
        if let Atom::Cell(cell) = atom {
            let value = Expr::cell(*cell);
            return self
                .invariant(&value)
                .then(|| Expr::product(Expr::constant(0), value, self.iterations.clone(), false));
        }
        let Atom::Less(lhs, rhs) = atom else {
            return None;
        };

        // [0 < rhs] where rhs changes by an odd amount each iteration: it's
        // zero in exactly one iteration t out of every 256, so the condition
        // holds in n - [t < n] of them.
        if lhs.as_constant() == Some(0)
            && let Some(rhs) = rhs.as_operand()
        {
            let change = rhs
                .scale
                .wrapping_mul(self.increment(rhs.cell)?.as_constant()?);
            if change % 2 == 0 {
                return None;
            }
            // rhs + t * change == 0  =>  t = -rhs / change
            let ratio = inverse(change).wrapping_neg();
            let zero_at = Expr::operand(Operand {
                cell: rhs.cell,
                scale: rhs.scale.wrapping_mul(ratio),
                bias: rhs.bias.wrapping_mul(ratio),
            });
            let hits = Expr::less(zero_at, self.iterations.clone());
            return Some(self.iterations.minus(&hits));
        }

        // [255 - cell < increment] where the cell is increased by 0 or 1 each
        // iteration: whether it wrapped around. The total increment is below
        // 256 so it wraps at most once.
        if let Some(Operand {
            cell,
            scale: u8::MAX,
            bias: u8::MAX,
        }) = lhs.as_operand()
            && rhs.is_boolean()
            && self.increment(cell).as_ref() == Some(&**rhs)
        {
            return Some(Expr::less((**lhs).clone(), self.sum(rhs)?));
        }

        // [255 - cell < step] where the cell is increased by the same step
        // each iteration: it wraps around (cell + n * step) / 256 times.
        if let Some(Operand {
            cell,
            scale: u8::MAX,
            bias: u8::MAX,
        }) = lhs.as_operand()
            && self.increment(cell).as_ref() == Some(&**rhs)
            && self.invariant(rhs)
        {
            return Some(Expr::product(
                Expr::cell(cell),
                (**rhs).clone(),
                self.iterations.clone(),
                true,
            ));
        }

        None
    }

    /// Whether an expression has the same value in every iteration.
    fn invariant(&self, expr: &Expr) -> bool {
        let mut cells = Vec::new();
        expr.reads(&mut cells);
        cells
            .into_iter()
            .all(|cell| self.body.get(cell) == Expr::cell(cell))
    }
}

/// Solve loops which don't move the data pointer and step the current cell
/// by an odd constant: they run a computable number of times. The other
/// cells must be set to constants or change by amounts whose sum over all
/// iterations is computable.
///
/// `exit` are the cells used after the loop and `live` those used by the next
/// iteration or after the loop. Other cells are temporaries.
fn solve_loop(body: &[AstNode], entry: &Known, exit: &Live, live: &Live) -> Option<OptimizedLoop> {
    let mut affine = Affine::new(Known::default());
    for node in body {
        if !is_straight_line(node) {
            return None;
        }
        affine.apply(node);
    }

    let summation = Summation {
        body: &affine,
        iterations: Expr::constant(0),
    };

    // The loop runs n times where `initial + n * delta == 0 (mod 256)`, so
    // `n = initial * -inverse(delta)`. That's only solvable for odd deltas.
    let delta = summation.increment(0)?.as_constant()?;
    if delta % 2 == 0 {
        return None;
    }
    let summation = Summation {
        iterations: Expr::cell(0).scaled(inverse(delta).wrapping_neg()),
        ..summation
    };

    // Cells set to a constant keep their value if the loop doesn't run. If
    // that value isn't known and is used later, the solution only applies if
    // the loop runs.
    let guarded = affine.exprs.iter().any(|(offset, expr)| {
        offset != 0
            && expr.as_constant().is_some()
            && entry.get(offset).is_none()
            && exit.contains(offset)
    });
    let runs = Expr::less(Expr::constant(0), Expr::cell(0));

    let mut result = Affine::new(if guarded {
        Known::default()
    } else {
        entry.clone()
    });

    for (offset, expr) in affine.exprs.iter() {
        let value = if !live.contains(offset) {
            // Unchanged, but available as scratch space.
            Expr::cell(offset)
        } else if offset == 0 {
            Expr::constant(0)
        } else if let Some(constant) = expr.as_constant() {
            match entry.get(offset) {
                Some(old) if !guarded => {
                    let mut value = Expr::constant(old);
                    value.add_scaled(&runs, constant.wrapping_sub(old));
                    value
                }
                _ => Expr::constant(constant),
            }
        } else {
            let mut value = Expr::cell(offset);
            value.add_scaled(&summation.sum(&summation.increment(offset)?)?, 1);
            value
        };
        let value = value.substitute(&result.known);
        result.exprs.insert(offset, value);
    }

    let mut nodes = result.lower(exit)?;
    if guarded {
        // Run once if the counter is non-zero.
        nodes.push(AstNode::Set(0, 0));
        Some(OptimizedLoop::Node(AstNode::Loop(nodes)))
    } else {
        Some(OptimizedLoop::Inline(nodes))
    }
}

/// Multiplicative inverse of an odd number modulo 256.
fn inverse(value: u8) -> u8 {
    // Newton's method: each step doubles the number of correct bits.
    let mut inverse = value;
    for _ in 0..3 {
        inverse = inverse.wrapping_mul(2u8.wrapping_sub(value.wrapping_mul(inverse)));
    }
    inverse
}

/// Optimize a sequence of nodes, given knowledge about cells at its start.
///
/// Pointer movement is deferred and folded into the offsets of the other
/// nodes. It is only performed before loops and syscalls, and at the end.
/// `live` are the cells used after the nodes.
fn optimize_block(nodes: Vec<Raw>, mut known: Known, live: &Live) -> Vec<AstNode> {
    let mut output = Vec::new();
    // Pending pointer movement
    let mut offset = 0;
    // Pending straight-line nodes, relative to the pointer before `offset`
    let mut run = Vec::new();
    // The knowledge about cells after `run`
    let mut after_run = KnownAfterRun::new(known.clone());

    // The cells used after each loop, and those used by it or after it,
    // relative to its pointer.
    let mut live_around_loops = Vec::new();
    let mut current = live.clone();
    for node in nodes.iter().rev() {
        let Raw::Loop(_, summary) = node else {
            current = current.into_before(node);
            continue;
        };
        let after = current.clone();
        current = current.into_before(node);
        let before = if summary.clear {
            let mut before = after.clone();
            before.union(&summary.reads);
            before
        } else {
            // That's what the cells live before a loop are, unless it's `[-]`.
            current.clone()
        };
        live_around_loops.push((after, before));
    }

    // `live` is relative to the pointer at the end of the run, which is
    // `offset` away from the run's start.
    let flush_run = |run: &mut Vec<AstNode>,
                     known: &mut Known,
                     output: &mut Vec<AstNode>,
                     live: &Live,
                     offset: i32| {
        let live = live.before_move(offset);
        let (nodes, after) = optimize_run(std::mem::take(run), std::mem::take(known), &live);
        output.extend(nodes);
        *known = after;
    };
    let flush_move = |offset: &mut i32, known: &mut Known, output: &mut Vec<AstNode>| {
        if *offset != 0 {
            output.push(AstNode::Move(*offset));
            known.shift(*offset);
            *offset = 0;
        }
    };

    for node in nodes {
        match node {
            Raw::Node(AstNode::Move(amount)) => offset += amount,
            Raw::Loop(body, _) => {
                let (live_after, live_before) = &live_around_loops.pop().unwrap();
                if run.len() > MAX_RUN {
                    flush_run(&mut run, &mut known, &mut output, live_before, offset);
                    after_run = KnownAfterRun::new(known.clone());
                }
                let mut entry = after_run.get();
                entry.shift(offset);

                match optimize_loop(body, &entry, live_after, live_before) {
                    OptimizedLoop::Inline(nodes) => {
                        for node in nodes {
                            let node = shift(&node, offset);
                            after_run.push(&node);
                            run.push(node);
                        }
                    }
                    OptimizedLoop::Node(node) => {
                        // Skip loops which are statically known to never run.
                        if entry.get(0) == Some(0) {
                            continue;
                        }

                        flush_run(&mut run, &mut known, &mut output, live_before, offset);
                        flush_move(&mut offset, &mut known, &mut output);
                        output.push(node);
                        known = Known::loop_exit();
                        after_run = KnownAfterRun::new(known.clone());
                    }
                }
            }
            Raw::Node(AstNode::Syscall) => {
                flush_run(&mut run, &mut known, &mut output, &Live::All, offset);
                flush_move(&mut offset, &mut known, &mut output);
                output.push(AstNode::Syscall);
                known = Known::default();
                after_run = KnownAfterRun::new(known.clone());
            }
            Raw::Node(node) => {
                let node = shift(&node, offset);
                after_run.push(&node);
                run.push(node);
            }
        }
    }

    flush_run(&mut run, &mut known, &mut output, live, offset);
    flush_move(&mut offset, &mut known, &mut output);

    output
}

/// Shift the offsets of a straight-line, `Print` or `Read` node.
fn shift(node: &AstNode, amount: i32) -> AstNode {
    match *node {
        AstNode::Add(o, value) => AstNode::Add(o + amount, value),
        AstNode::Set(o, value) => AstNode::Set(o + amount, value),
        AstNode::MulAdd { src, dst, factor } => AstNode::MulAdd {
            src: src + amount,
            dst: dst + amount,
            factor,
        },
        AstNode::CondAdd {
            lhs,
            rhs,
            dst,
            value,
        } => AstNode::CondAdd {
            lhs: lhs.shift(amount),
            rhs: rhs.shift(amount),
            dst: dst + amount,
            value,
        },
        AstNode::ProductAdd {
            base,
            step,
            count,
            high,
            dst,
            value,
        } => AstNode::ProductAdd {
            base: base.shift(amount),
            step: step.shift(amount),
            count: count.shift(amount),
            high,
            dst: dst + amount,
            value,
        },
        AstNode::Print(o) => AstNode::Print(o + amount),
        AstNode::Read(o) => AstNode::Read(o + amount),
        _ => unreachable!("not a straight-line node: {node:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_union() {
        let sets: [&[i32]; 5] = [&[], &[0], &[-3, 1, 2], &[-5, -3, 0, 2, 7], &[1, 2, 3, 4]];
        for a in sets {
            for b in sets {
                for shift in -3..=3 {
                    let mut live = Live::Cells(a.iter().copied().collect(), 0);
                    live.union(&Live::Cells(b.iter().copied().collect(), shift));
                    let mut expected: Vec<i32> = a.iter().chain(b).copied().collect();
                    for cell in &mut expected[a.len()..] {
                        *cell += shift;
                    }
                    expected.sort_unstable();
                    expected.dedup();
                    assert!(matches!(live, Live::Cells(cells, 0) if cells == expected));
                }
            }
        }
    }

    #[test]
    fn inverse_is_correct() {
        for value in (1..=255u8).step_by(2) {
            assert_eq!(value.wrapping_mul(inverse(value)), 1);
        }
    }
}
