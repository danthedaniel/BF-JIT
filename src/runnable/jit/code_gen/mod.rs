#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
pub use self::x86_64::*;

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
pub use self::aarch64::*;

use crate::parser::AstNode;

/// A straight-line operation, with offsets relative to the data pointer at
/// the start of its run.
enum Step {
    /// An `Add`, `Set`, `MulAdd`, `CondAdd` or `ProductAdd` node
    Node(AstNode),
    /// A loop which only sets cells, including its counter to 0, so it runs at
    /// most once
    SetIf { counter: i32, sets: Vec<(i32, u8)> },
}

/// The most cells (other than the counter) a loop compiled as a
/// `Step::SetIf` may set.
const MAX_SET_IF_CELLS: usize = 4;

/// The cells (other than the counter) and values set by a loop which only sets
/// cells and runs at most once.
fn set_if_cells(body: &[AstNode]) -> Option<Vec<(i32, u8)>> {
    let mut sets: Vec<(i32, u8)> = Vec::new();
    for node in body {
        let AstNode::Set(offset, value) = *node else {
            return None;
        };
        sets.retain(|&(cell, _)| cell != offset);
        sets.push((offset, value));
    }
    if !sets.contains(&(0, 0)) {
        return None;
    }
    sets.retain(|&(cell, _)| cell != 0);
    (sets.len() <= MAX_SET_IF_CELLS).then_some(sets)
}

/// Whether a node can be part of a run compiled by `straight_line`.
pub fn is_straight_line(node: &AstNode) -> bool {
    match node {
        AstNode::Add(..)
        | AstNode::Set(..)
        | AstNode::MulAdd { .. }
        | AstNode::CondAdd { .. }
        | AstNode::ProductAdd { .. }
        | AstNode::Word { .. }
        | AstNode::Move(_) => true,
        AstNode::Loop(body) => set_if_cells(body).is_some(),
        _ => false,
    }
}

/// Convert a run of straight-line nodes into steps relative to the data
/// pointer at its start, returning them with the run's pointer movement.
fn steps(nodes: &[AstNode]) -> (Vec<Step>, i32) {
    let mut moved = 0;
    let mut steps = Vec::new();
    for node in nodes {
        match node {
            AstNode::Move(amount) => moved += amount,
            AstNode::Loop(body) => steps.push(Step::SetIf {
                counter: moved,
                sets: set_if_cells(body)
                    .unwrap()
                    .into_iter()
                    .map(|(offset, value)| (offset + moved, value))
                    .collect(),
            }),
            node => steps.push(Step::Node(node.shifted(moved))),
        }
    }
    (steps, moved)
}

/// The cells a straight-line step accesses.
fn accessed_cells(step: &Step) -> Vec<i32> {
    let node = match step {
        Step::Node(node) => node,
        Step::SetIf { counter, sets } => {
            return std::iter::once(*counter)
                .chain(sets.iter().map(|&(offset, _)| offset))
                .collect();
        }
    };
    match *node {
        AstNode::Add(offset, _) | AstNode::Set(offset, _) => vec![offset],
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
        AstNode::Word {
            dst,
            len,
            ref terms,
            ..
        } => terms
            .iter()
            .flat_map(|term| term.reads())
            .chain(dst..dst + i32::from(len))
            .collect(),
        _ => unreachable!("not a straight-line node: {node:?}"),
    }
}

/// Cells whose value at the start of a loop iteration is used, rather than
/// being set first. The counter (cell 0) always is.
fn live_cells(cells: &[i32], steps: &[Step]) -> Vec<i32> {
    cells
        .iter()
        .copied()
        .filter(|&cell| {
            cell == 0
                || !matches!(
                    steps
                        .iter()
                        .find(|step| accessed_cells(step).contains(&cell)),
                    Some(Step::Node(AstNode::Set(..)))
                )
        })
        .collect()
}
