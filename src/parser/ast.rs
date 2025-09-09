use anyhow::{Result, bail};
use std::collections::VecDeque;

/// brainfuck AST node
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum AstNode {
    /// Add to the current memory cell.
    Incr(u8),
    /// Remove from the current memory cell.
    Decr(u8),
    /// Shift the data pointer to the right.
    Next(u16),
    /// Shift the data pointer to the left.
    Prev(u16),
    /// Display the current memory cell as an ASCII character.
    Print,
    /// Read one character from stdin.
    Read,
    /// Set a literal value in the current cell.
    Set(u8),
    /// Multiply current cell by a factor and add to cell at offset, then set current to 0.
    MultiplyAddTo(i16, u8),
    /// Add current cell to multiple offsets, then set current to 0.
    AddTo(Vec<i16>),
    /// Substract current cell from multiple offsets, then set current to 0.
    SubFrom(Vec<i16>),
    /// Loop over the contained instructions while the current memory cell is
    /// not zero.
    Loop(VecDeque<AstNode>),
}

impl AstNode {
    /// Convert raw input into an AST.
    pub fn parse(input: &str) -> Result<VecDeque<AstNode>> {
        let mut output = VecDeque::new();
        let mut loops = VecDeque::new();

        let mut line = 1;
        let mut col = 0;

        for character in input.chars() {
            col += 1;

            let next_node = match character {
                '+' => AstNode::Incr(1),
                '-' => AstNode::Decr(1),
                '>' => AstNode::Next(1),
                '<' => AstNode::Prev(1),
                '.' => AstNode::Print,
                ',' => AstNode::Read,
                '[' => {
                    loops.push_back(VecDeque::new());
                    continue;
                }
                ']' => {
                    // Example program that will cause this error:
                    //
                    // []]
                    let current_loop = loops.pop_back().ok_or_else(|| {
                        anyhow::anyhow!(format!("Line {line}:{col} - Unmatched ']' bracket"))
                    })?;

                    // Do not add loop if we can statically determine that it will be a no-op.
                    if Self::sets_to_zero(output.back()) {
                        continue;
                    }

                    let optimized_loop = Self::combine_consecutive_nodes(&current_loop);
                    Self::simplify_loop(&optimized_loop).unwrap_or(AstNode::Loop(optimized_loop))
                }
                '\n' => {
                    line += 1;
                    col = 0;
                    continue;
                }
                // All other characters are comments and will be ignored
                _ => continue,
            };

            // Where to add the new node. First try to add to the innermost loop.
            // If there are no loops, then add to the top level output.
            let node_target = loops.back_mut().unwrap_or(&mut output);
            node_target.push_back(next_node);
        }

        if !loops.is_empty() {
            // Example program that will cause this error:
            //
            // [[]
            bail!(format!("Line {line}:{col} - Unmatched '[' bracket"));
        }

        Ok(Self::combine_consecutive_nodes(&output))
    }

    /// Whether the data pointer points to zero after execution of the node.
    fn sets_to_zero(node: Option<&AstNode>) -> bool {
        #[allow(clippy::match_like_matches_macro, clippy::match_same_arms)]
        match node {
            // If there's no node, then no code has executed yet. All cells start at zero.
            None => true,
            Some(AstNode::Set(0)) => true,
            Some(AstNode::MultiplyAddTo(_, _)) => true,
            Some(AstNode::AddTo(_)) => true,
            Some(AstNode::SubFrom(_)) => true,
            _ => false,
        }
    }

    /// If a shorthand for the provided loop exists, return that.
    fn simplify_loop(input: &VecDeque<AstNode>) -> Option<AstNode> {
        let strategies = [
            Self::create_set_node,
            Self::create_multiplyaddto_node,
            Self::create_addto_node,
            Self::create_subfrom_node,
        ];

        for strategy in strategies {
            if let Some(node) = strategy(input) {
                return Some(node);
            }
        }

        None
    }

    /// Try to convert a loop into a `AstNode::Set(0)` node.
    fn create_set_node(input: &VecDeque<AstNode>) -> Option<AstNode> {
        if input.len() != 1 {
            return None;
        }

        match input[0] {
            AstNode::Incr(1) | AstNode::Decr(1) => Some(AstNode::Set(0)),
            _ => None,
        }
    }

    /// Try to convert a loop into a `AstNode::MultiplyAddTo` node.
    fn create_multiplyaddto_node(input: &VecDeque<AstNode>) -> Option<AstNode> {
        if input.len() != 4 {
            return None;
        }

        match (&input[0], &input[1], &input[2], &input[3]) {
            (AstNode::Decr(1), AstNode::Prev(a), AstNode::Incr(n), AstNode::Next(b))
                if *a == *b && *n > 1 =>
            {
                let offset: i16 = (-i32::from(*a)).try_into().ok()?;
                Some(AstNode::MultiplyAddTo(offset, *n))
            }
            (AstNode::Decr(1), AstNode::Next(a), AstNode::Incr(n), AstNode::Prev(b))
                if *a == *b && *n > 1 =>
            {
                let offset = i16::try_from(*a).ok()?;
                Some(AstNode::MultiplyAddTo(offset, *n))
            }
            _ => None,
        }
    }

    /// Try to convert a loop into a `AstNode::AddTo` node.
    fn create_addto_node(input: &VecDeque<AstNode>) -> Option<AstNode> {
        if input.len() < 3 {
            return None;
        }
        if input[0] != AstNode::Decr(1) {
            return None;
        }

        let mut position: i16 = 0;
        let mut targets = Vec::new();

        for node in input.iter().skip(1) {
            match node {
                AstNode::Next(n) => {
                    let n_i16 = i16::try_from(*n).ok()?;
                    position = position.checked_add(n_i16)?;
                }
                AstNode::Prev(n) => {
                    let n_i16 = i16::try_from(*n).ok()?;
                    position = position.checked_sub(n_i16)?;
                }
                AstNode::Incr(1) => {
                    if position == 0 {
                        return None;
                    }

                    targets.push(position);
                }
                _ => return None,
            }
        }

        // Must return to starting position
        if position != 0 {
            return None;
        }
        if targets.is_empty() {
            return None;
        }

        Some(AstNode::AddTo(targets))
    }

    /// Try to convert a loop into a `AstNode::SubFrom` node.
    fn create_subfrom_node(input: &VecDeque<AstNode>) -> Option<AstNode> {
        if input.len() < 3 {
            return None;
        }
        if input[0] != AstNode::Decr(1) {
            return None;
        }

        let mut position: i16 = 0;
        let mut targets = Vec::new();

        for node in input.iter().skip(1) {
            match node {
                AstNode::Next(n) => {
                    let n_i16 = i16::try_from(*n).ok()?;
                    position = position.checked_add(n_i16)?;
                }
                AstNode::Prev(n) => {
                    let n_i16 = i16::try_from(*n).ok()?;
                    position = position.checked_sub(n_i16)?;
                }
                AstNode::Decr(1) => {
                    if position == 0 {
                        return None;
                    }

                    targets.push(position);
                }
                _ => return None,
            }
        }

        // Must return to starting position
        if position != 0 {
            return None;
        }
        if targets.is_empty() {
            return None;
        }

        Some(AstNode::SubFrom(targets))
    }

    /// Convert runs of instructions into bulk operations.
    fn combine_consecutive_nodes(input: &VecDeque<AstNode>) -> VecDeque<AstNode> {
        let mut output = VecDeque::new();

        for next_node in input {
            let prev_node = output.back();

            // If possible, replace previous node with new combined node.
            if let Some(combined_node) = Self::combined_node(prev_node, next_node) {
                output.pop_back();
                output.push_back(combined_node);
                continue;
            }

            // Remove previous node if pair can be eliminated.
            if Self::eliminate_pair(prev_node, next_node) {
                output.pop_back();
                continue;
            }

            // Otherwise, add next node to output.
            output.push_back(next_node.clone());
        }

        output
    }

    /// For each operator `+`, `-`, `<` and `>`, if the last instruction in the
    /// output Vec is the same, then increment that instruction instead
    /// of adding another identical instruction.
    fn combined_node(prev_node: Option<&AstNode>, next_node: &AstNode) -> Option<AstNode> {
        match (prev_node, next_node) {
            // Combine sequential Incr, Decr, Next and Prev
            // Keep wrapping behavior for Incr/Decr as that's intended BrainFuck semantics
            (Some(AstNode::Incr(b)), AstNode::Incr(a)) => Some(AstNode::Incr(a.wrapping_add(*b))),
            (Some(AstNode::Decr(b)), AstNode::Decr(a)) => Some(AstNode::Decr(a.wrapping_add(*b))),
            // Use checked arithmetic for Next/Prev to prevent unexpected overflows
            (Some(AstNode::Next(b)), AstNode::Next(a)) => a.checked_add(*b).map(AstNode::Next),
            (Some(AstNode::Prev(b)), AstNode::Prev(a)) => a.checked_add(*b).map(AstNode::Prev),
            // Partial cancellation with checked arithmetic
            (Some(AstNode::Incr(a)), AstNode::Decr(b)) if *a > *b => {
                a.checked_sub(*b).map(AstNode::Incr)
            }
            (Some(AstNode::Incr(a)), AstNode::Decr(b)) if *a < *b => {
                b.checked_sub(*a).map(AstNode::Decr)
            }
            (Some(AstNode::Decr(a)), AstNode::Incr(b)) if *a > *b => {
                a.checked_sub(*b).map(AstNode::Decr)
            }
            (Some(AstNode::Decr(a)), AstNode::Incr(b)) if *a < *b => {
                b.checked_sub(*a).map(AstNode::Incr)
            }
            // Partial cancellation for Next/Prev with checked arithmetic
            (Some(AstNode::Next(a)), AstNode::Prev(b)) if *a > *b => {
                a.checked_sub(*b).map(AstNode::Next)
            }
            (Some(AstNode::Next(a)), AstNode::Prev(b)) if *a < *b => {
                b.checked_sub(*a).map(AstNode::Prev)
            }
            (Some(AstNode::Prev(a)), AstNode::Next(b)) if *a > *b => {
                a.checked_sub(*b).map(AstNode::Prev)
            }
            (Some(AstNode::Prev(a)), AstNode::Next(b)) if *a < *b => {
                b.checked_sub(*a).map(AstNode::Next)
            }
            // Combine Incr or Decr with Set (keep wrapping for byte operations)
            (Some(AstNode::Set(a)), AstNode::Incr(b)) => Some(AstNode::Set(a.wrapping_add(*b))),
            (Some(AstNode::Set(a)), AstNode::Decr(b)) => Some(AstNode::Set(a.wrapping_sub(*b))),
            // Node is not combinable
            _ => None,
        }
    }

    /// Dead code elimination: operations that cancel each other
    fn eliminate_pair(prev_node: Option<&AstNode>, next_node: &AstNode) -> bool {
        match (prev_node, next_node) {
            (Some(AstNode::Incr(a)), AstNode::Decr(b)) if *a == *b => true,
            (Some(AstNode::Decr(a)), AstNode::Incr(b)) if *a == *b => true,
            (Some(AstNode::Next(a)), AstNode::Prev(b)) if *a == *b => true,
            (Some(AstNode::Prev(a)), AstNode::Next(b)) if *a == *b => true,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn too_many_loop_begins() {
        let ast = AstNode::parse("[[]");
        assert!(ast.is_err());
    }

    #[test]
    fn too_many_loop_ends() {
        let ast = AstNode::parse("[]]");
        assert!(ast.is_err());
    }

    #[test]
    fn run_length_encode() {
        let ast = AstNode::parse("+++++").unwrap();
        assert_eq!(ast.len(), 1);
        assert_eq!(ast[0], AstNode::Incr(5));
    }

    #[test]
    fn simplify_to_set() {
        let ast = AstNode::parse("+[-]+++").unwrap();
        assert_eq!(ast.len(), 2);
        assert_eq!(ast[0], AstNode::Incr(1));
        assert_eq!(ast[1], AstNode::Set(3));
    }

    #[test]
    fn simplify_to_add() {
        let ast = AstNode::parse("+[->+<]").unwrap();
        assert_eq!(ast.len(), 2);
        assert_eq!(ast[0], AstNode::Incr(1));
        assert_eq!(ast[1], AstNode::AddTo(vec![1]));
    }

    #[test]
    fn simplify_to_sub() {
        let ast = AstNode::parse("+[->-<]").unwrap();
        assert_eq!(ast.len(), 2);
        assert_eq!(ast[0], AstNode::Incr(1));
        assert_eq!(ast[1], AstNode::SubFrom(vec![1]));
    }

    #[test]
    fn removes_leading_loops() {
        let ast = AstNode::parse("[-]").unwrap();
        assert_eq!(ast.len(), 0);
    }

    #[test]
    fn simplify_to_multiply() {
        let ast = AstNode::parse("+[->>+++<<]").unwrap();
        assert_eq!(ast.len(), 2);
        assert_eq!(ast[0], AstNode::Incr(1));
        assert_eq!(ast[1], AstNode::MultiplyAddTo(2, 3));
    }

    #[test]
    fn simplify_to_copy() {
        let ast = AstNode::parse("+[->>+>+<<<]").unwrap();
        assert_eq!(ast.len(), 2);
        assert_eq!(ast[0], AstNode::Incr(1));
        assert_eq!(ast[1], AstNode::AddTo(vec![2, 3]));
    }

    #[test]
    fn dead_code_elimination() {
        // Complete cancellation
        let ast = AstNode::parse("+-").unwrap();
        assert_eq!(ast.len(), 0);

        let ast = AstNode::parse("><").unwrap();
        assert_eq!(ast.len(), 0);

        // Partial cancellation
        let ast = AstNode::parse("+++--").unwrap();
        assert_eq!(ast.len(), 1);
        assert_eq!(ast[0], AstNode::Incr(1));

        let ast = AstNode::parse("++---").unwrap();
        assert_eq!(ast.len(), 1);
        assert_eq!(ast[0], AstNode::Decr(1));
    }

    #[test]
    fn parses_rot13() {
        let ast = AstNode::parse(include_str!("../../tests/programs/rot13-16char.bf"));
        assert!(ast.is_ok());
    }

    #[test]
    fn parses_mandelbrot() {
        let ast = AstNode::parse(include_str!("../../tests/programs/mandelbrot.bf"));
        assert!(ast.is_ok());
    }
}
