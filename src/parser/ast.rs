use std::collections::VecDeque;

/// BrainFuck AST node
#[derive(Debug, Clone, PartialEq)]
pub enum AstNode {
    /// Add to the current memory cell.
    Incr(u8),
    /// Remove from the current memory cell.
    Decr(u8),
    /// Shift the data pointer to the right.
    Next(usize),
    /// Shift the data pointer to the left.
    Prev(usize),
    /// Display the current memory cell as an ASCII character.
    Print,
    /// Read one character from stdin.
    Read,
    /// Set a literal value in the current cell.
    Set(u8),
    /// Add the current cell to the cell n spaces away and set the current cell to 0.
    AddTo(isize),
    /// Subtract the current cell from the cell n spaces away and set the current cell to 0.
    SubFrom(isize),
    /// Multiply current cell by a factor and add to cell at offset, then set current to 0.
    MultiplyAddTo(isize, u8),
    /// Copy current cell to multiple offsets, then set current to 0.
    CopyTo(Vec<isize>),
    /// Loop over the contained instructions while the current memory cell is
    /// not zero.
    Loop(VecDeque<AstNode>),
}

/// Container for a vector of AstNodes.
#[derive(Debug, Clone)]
pub struct Ast {
    pub data: VecDeque<AstNode>,
}

impl Ast {
    /// Convert raw input into an AST.
    pub fn parse(input: &str) -> Result<Self, String> {
        let mut output = VecDeque::new();
        let mut loops = VecDeque::new();

        for character in input.chars() {
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
                    let mut current_loop = loops.pop_back().ok_or("More ] than [")?;

                    // Do not add loop if it will be the first element in the
                    // output vector. This is because:
                    //
                    // 1. The BrainFuck machine starts all cells at 0
                    // 2. Loops are skipped when the current cell is 0
                    //
                    // So if no non-loops have executed there is no use in
                    // emitting a Loop AstNode.
                    if output.is_empty() {
                        continue;
                    }

                    current_loop = Self::combine_consecutive_nodes(&mut current_loop);

                    if let Some(node) = Self::simplify_loop(&current_loop) {
                        node
                    } else {
                        AstNode::Loop(current_loop)
                    }
                }
                // All other characters are comments and will be ignored
                _ => continue,
            };

            loops.back_mut().unwrap_or(&mut output).push_back(next_node);
        }

        if !loops.is_empty() {
            // Example program that will cause this error:
            //
            // [[]
            return Err("More [ than ]".to_string());
        }

        Ok(Ast {
            data: Self::combine_consecutive_nodes(&mut output),
        })
    }

    /// If a shorthand for the provided loop exists, return that.
    fn simplify_loop(input: &VecDeque<AstNode>) -> Option<AstNode> {
        // Zero loop
        if input.len() == 1 {
            match input[0] {
                AstNode::Incr(1) => return Some(AstNode::Set(0)),
                AstNode::Decr(1) => return Some(AstNode::Set(0)),
                _ => return None,
            }
        }

        // Move current cell if not 0
        if input.len() == 4 {
            match (&input[0], &input[1], &input[2], &input[3]) {
                // AddTo
                (AstNode::Decr(1), AstNode::Prev(a), AstNode::Incr(1), AstNode::Next(b))
                    if *a == *b =>
                {
                    let offset = -(*a as isize);
                    return Some(AstNode::AddTo(offset));
                }
                (AstNode::Decr(1), AstNode::Next(a), AstNode::Incr(1), AstNode::Prev(b))
                    if *a == *b =>
                {
                    let offset = *a as isize;
                    return Some(AstNode::AddTo(offset));
                }
                // SubFrom
                (AstNode::Decr(1), AstNode::Prev(a), AstNode::Decr(1), AstNode::Next(b))
                    if *a == *b =>
                {
                    let offset = -(*a as isize);
                    return Some(AstNode::SubFrom(offset));
                }
                (AstNode::Decr(1), AstNode::Next(a), AstNode::Decr(1), AstNode::Prev(b))
                    if *a == *b =>
                {
                    let offset = *a as isize;
                    return Some(AstNode::SubFrom(offset));
                }
                // MultiplyAddTo
                (AstNode::Decr(1), AstNode::Next(a), AstNode::Incr(n), AstNode::Prev(b))
                    if *a == *b && *n > 1 =>
                {
                    let offset = *a as isize;
                    return Some(AstNode::MultiplyAddTo(offset, *n));
                }
                (AstNode::Decr(1), AstNode::Prev(a), AstNode::Incr(n), AstNode::Next(b))
                    if *a == *b && *n > 1 =>
                {
                    let offset = -(*a as isize);
                    return Some(AstNode::MultiplyAddTo(offset, *n));
                }
                _ => {}
            };
        }

        // Check for copy loops (e.g., [->>+>+<<<])
        if Self::is_copy_loop(input) {
            if let Some(copy_node) = Self::create_copy_node(input) {
                return Some(copy_node);
            }
        }

        None
    }

    /// Check if the loop is a copy loop pattern
    fn is_copy_loop(input: &VecDeque<AstNode>) -> bool {
        if input.is_empty() || input[0] != AstNode::Decr(1) {
            return false;
        }

        let mut position = 0isize;
        let mut increments = std::collections::HashMap::new();

        for node in input.iter().skip(1) {
            match node {
                AstNode::Next(n) => position += *n as isize,
                AstNode::Prev(n) => position -= *n as isize,
                AstNode::Incr(n) => {
                    *increments.entry(position).or_insert(0) += *n as i32;
                }
                _ => return false,
            }
        }

        // Must return to starting position
        if position != 0 {
            return false;
        }

        // All increments must be 1
        increments.values().all(|&v| v == 1)
    }

    /// Create a CopyTo node from a copy loop
    fn create_copy_node(input: &VecDeque<AstNode>) -> Option<AstNode> {
        let mut position = 0isize;
        let mut targets = Vec::new();

        for node in input.iter().skip(1) {
            match node {
                AstNode::Next(n) => position += *n as isize,
                AstNode::Prev(n) => position -= *n as isize,
                AstNode::Incr(1) => targets.push(position),
                _ => {}
            }
        }

        if !targets.is_empty() {
            Some(AstNode::CopyTo(targets))
        } else {
            None
        }
    }

    /// Convert runs of instructions into bulk operations.
    fn combine_consecutive_nodes(input: &mut VecDeque<AstNode>) -> VecDeque<AstNode> {
        let mut output = VecDeque::new();

        while let Some(next_node) = input.pop_front() {
            let prev_node = output.back();

            // For each operator +, -, < and >, if the last instruction in the
            // output Vec is the same, then increment that instruction instead
            // of adding another identical instruction.
            let combined = match (prev_node, &next_node) {
                // Combine sequential Incr, Decr, Next and Prev
                (Some(AstNode::Incr(b)), AstNode::Incr(a)) => {
                    Some(AstNode::Incr(a.wrapping_add(*b)))
                }
                (Some(AstNode::Decr(b)), AstNode::Decr(a)) => {
                    Some(AstNode::Decr(a.wrapping_add(*b)))
                }
                (Some(AstNode::Next(b)), AstNode::Next(a)) => {
                    Some(AstNode::Next(a.wrapping_add(*b)))
                }
                (Some(AstNode::Prev(b)), AstNode::Prev(a)) => {
                    Some(AstNode::Prev(a.wrapping_add(*b)))
                }
                // Dead code elimination: operations that cancel each other
                (Some(AstNode::Incr(a)), AstNode::Decr(b)) if *a == *b => {
                    output.pop_back();
                    continue;
                }
                (Some(AstNode::Decr(a)), AstNode::Incr(b)) if *a == *b => {
                    output.pop_back();
                    continue;
                }
                (Some(AstNode::Next(a)), AstNode::Prev(b)) if *a == *b => {
                    output.pop_back();
                    continue;
                }
                (Some(AstNode::Prev(a)), AstNode::Next(b)) if *a == *b => {
                    output.pop_back();
                    continue;
                }
                // Partial cancellation
                (Some(AstNode::Incr(a)), AstNode::Decr(b)) if *a > *b => {
                    Some(AstNode::Incr(a - b))
                }
                (Some(AstNode::Incr(a)), AstNode::Decr(b)) if *a < *b => {
                    Some(AstNode::Decr(b - a))
                }
                (Some(AstNode::Decr(a)), AstNode::Incr(b)) if *a > *b => {
                    Some(AstNode::Decr(a - b))
                }
                (Some(AstNode::Decr(a)), AstNode::Incr(b)) if *a < *b => {
                    Some(AstNode::Incr(b - a))
                }
                // Combine Incr or Decr with Set
                (Some(AstNode::Set(a)), AstNode::Incr(b)) => Some(AstNode::Set(a.wrapping_add(*b))),
                (Some(AstNode::Set(a)), AstNode::Decr(b)) => Some(AstNode::Set(a.wrapping_sub(*b))),
                // Node is not combinable
                _ => None,
            };

            if let Some(new_node) = combined {
                // Replace last node with the combined one
                output.pop_back();
                output.push_back(new_node);
            } else {
                output.push_back(next_node);
            }
        }

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn too_many_loop_begins() {
        let ast = Ast::parse("[[]");
        assert!(ast.is_err());
    }

    #[test]
    fn too_many_loop_ends() {
        let ast = Ast::parse("[]]");
        assert!(ast.is_err());
    }

    #[test]
    fn run_length_encode() {
        let ast = Ast::parse("+++++").unwrap();
        assert_eq!(ast.data.len(), 1);
        assert_eq!(ast.data[0], AstNode::Incr(5));
    }

    #[test]
    fn simplify_to_set() {
        let ast = Ast::parse("+[-]+++").unwrap();
        assert_eq!(ast.data.len(), 2);
        assert_eq!(ast.data[0], AstNode::Incr(1));
        assert_eq!(ast.data[1], AstNode::Set(3));
    }

    #[test]
    fn simplify_to_add() {
        let ast = Ast::parse("+[->+<]").unwrap();
        assert_eq!(ast.data.len(), 2);
        assert_eq!(ast.data[0], AstNode::Incr(1));
        assert_eq!(ast.data[1], AstNode::AddTo(1));
    }

    #[test]
    fn simplify_to_sub() {
        let ast = Ast::parse("+[->-<]").unwrap();
        assert_eq!(ast.data.len(), 2);
        assert_eq!(ast.data[0], AstNode::Incr(1));
        assert_eq!(ast.data[1], AstNode::SubFrom(1));
    }

    #[test]
    fn removes_leading_loops() {
        let ast = Ast::parse("[-]").unwrap();
        assert_eq!(ast.data.len(), 0);
    }

    #[test]
    fn simplify_to_multiply() {
        let ast = Ast::parse("+[->>+++<<]").unwrap();
        assert_eq!(ast.data.len(), 2);
        assert_eq!(ast.data[0], AstNode::Incr(1));
        assert_eq!(ast.data[1], AstNode::MultiplyAddTo(2, 3));
    }

    #[test]
    fn simplify_to_copy() {
        let ast = Ast::parse("+[->>+>+<<<]").unwrap();
        assert_eq!(ast.data.len(), 2);
        assert_eq!(ast.data[0], AstNode::Incr(1));
        assert_eq!(ast.data[1], AstNode::CopyTo(vec![2, 3]));
    }

    #[test]
    fn dead_code_elimination() {
        // Complete cancellation
        let ast = Ast::parse("+-").unwrap();
        assert_eq!(ast.data.len(), 0);

        let ast = Ast::parse("><").unwrap();
        assert_eq!(ast.data.len(), 0);

        // Partial cancellation
        let ast = Ast::parse("+++--").unwrap();
        assert_eq!(ast.data.len(), 1);
        assert_eq!(ast.data[0], AstNode::Incr(1));

        let ast = Ast::parse("++---").unwrap();
        assert_eq!(ast.data.len(), 1);
        assert_eq!(ast.data[0], AstNode::Decr(1));
    }

    #[test]
    fn parses_rot13() {
        let ast = Ast::parse(include_str!("../../tests/programs/rot13-16char.bf"));
        assert!(ast.is_ok());
    }

    #[test]
    fn parses_mandelbrot() {
        let ast = Ast::parse(include_str!("../../tests/programs/mandelbrot.bf"));
        assert!(ast.is_ok());
    }
}
