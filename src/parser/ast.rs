use std::collections::VecDeque;

/// BrainFuck AST node
#[derive(Debug, Clone, PartialEq)]
pub enum ASTNode {
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
    /// Loop over the contained instructions while the current memory cell is
    /// not zero.
    Loop(VecDeque<ASTNode>),
}

/// Container for a vector of ASTNodes.
#[derive(Debug, Clone)]
pub struct AST {
    pub data: VecDeque<ASTNode>,
}

impl AST {
    /// Convert raw input into an AST.
    pub fn parse(input: &str) -> Result<Self, String> {
        let mut output = VecDeque::new();
        let mut loops = VecDeque::new();

        for character in input.chars() {
            let next_node = match character {
                '+' => ASTNode::Incr(1),
                '-' => ASTNode::Decr(1),
                '>' => ASTNode::Next(1),
                '<' => ASTNode::Prev(1),
                '.' => ASTNode::Print,
                ',' => ASTNode::Read,
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
                    // emitting a Loop ASTNode.
                    if output.is_empty() {
                        continue;
                    }

                    current_loop = Self::combine_consecutive_nodes(&mut current_loop);

                    if let Some(node) = Self::simplify_loop(&current_loop) {
                        node
                    } else {
                        ASTNode::Loop(current_loop)
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

        Ok(AST {
            data: Self::combine_consecutive_nodes(&mut output),
        })
    }

    /// If a shorthand for the provided loop exists, return that.
    fn simplify_loop(input: &VecDeque<ASTNode>) -> Option<ASTNode> {
        // Zero loop
        if input.len() == 1 {
            match input[0] {
                ASTNode::Incr(1) => return Some(ASTNode::Set(0)),
                ASTNode::Decr(1) => return Some(ASTNode::Set(0)),
                _ => return None,
            }
        }

        // Move current cell if not 0
        if input.len() == 4 {
            match (&input[0], &input[1], &input[2], &input[3]) {
                // Add
                (ASTNode::Decr(1), ASTNode::Prev(a), ASTNode::Incr(1), ASTNode::Next(b))
                    if *a == *b =>
                {
                    let offset = -(*a as isize);
                    return Some(ASTNode::AddTo(offset));
                }
                (ASTNode::Decr(1), ASTNode::Next(a), ASTNode::Incr(1), ASTNode::Prev(b))
                    if *a == *b =>
                {
                    let offset = *a as isize;
                    return Some(ASTNode::AddTo(offset));
                }
                // Sub
                (ASTNode::Decr(1), ASTNode::Prev(a), ASTNode::Decr(1), ASTNode::Next(b))
                    if *a == *b =>
                {
                    let offset = -(*a as isize);
                    return Some(ASTNode::SubFrom(offset));
                }
                (ASTNode::Decr(1), ASTNode::Next(a), ASTNode::Decr(1), ASTNode::Prev(b))
                    if *a == *b =>
                {
                    let offset = *a as isize;
                    return Some(ASTNode::SubFrom(offset));
                }
                _ => return None,
            };
        }

        None
    }

    /// Convert runs of instructions into bulk operations.
    fn combine_consecutive_nodes(input: &mut VecDeque<ASTNode>) -> VecDeque<ASTNode> {
        let mut output = VecDeque::new();

        while let Some(next_node) = input.pop_front() {
            let prev_node = output.back();

            // For each operator +, -, < and >, if the last instruction in the
            // output Vec is the same, then increment that instruction instead
            // of adding another identical instruction.
            let combined = match (prev_node, &next_node) {
                // Combine sequential Incr, Decr, Next and Prev
                (Some(ASTNode::Incr(b)), ASTNode::Incr(a)) => ASTNode::Incr(a.wrapping_add(*b)),
                (Some(ASTNode::Decr(b)), ASTNode::Decr(a)) => ASTNode::Decr(a.wrapping_add(*b)),
                (Some(ASTNode::Next(b)), ASTNode::Next(a)) => ASTNode::Next(a.wrapping_add(*b)),
                (Some(ASTNode::Prev(b)), ASTNode::Prev(a)) => ASTNode::Prev(a.wrapping_add(*b)),
                // Combine Incr or Decr with Set
                (Some(ASTNode::Set(a)), ASTNode::Incr(b)) => ASTNode::Set(a.wrapping_add(*b)),
                (Some(ASTNode::Set(a)), ASTNode::Decr(b)) => ASTNode::Set(a.wrapping_sub(*b)),
                _ => {
                    // Node is not combineable, just move into the output vector
                    output.push_back(next_node);
                    continue;
                }
            };

            // Replace last node with the combined one
            output.pop_back();
            output.push_back(combined);
        }

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn too_many_loop_begins() {
        let ast = AST::parse("[[]");
        assert!(ast.is_err());
    }

    #[test]
    fn too_many_loop_ends() {
        let ast = AST::parse("[]]");
        assert!(ast.is_err());
    }

    #[test]
    fn run_length_encode() {
        let ast = AST::parse("+++++").unwrap();
        assert_eq!(ast.data.len(), 1);
        assert_eq!(ast.data[0], ASTNode::Incr(5));
    }

    #[test]
    fn simplify_to_set() {
        let ast = AST::parse("+[-]").unwrap();
        assert_eq!(ast.data.len(), 2);
        assert_eq!(ast.data[0], ASTNode::Incr(1));
        assert_eq!(ast.data[1], ASTNode::Set(0));
    }

    #[test]
    fn simplify_to_add() {
        let ast = AST::parse("+[->+<]").unwrap();
        assert_eq!(ast.data.len(), 2);
        assert_eq!(ast.data[0], ASTNode::Incr(1));
        assert_eq!(ast.data[1], ASTNode::AddTo(1));
    }

    #[test]
    fn simplify_to_sub() {
        let ast = AST::parse("+[->-<]").unwrap();
        assert_eq!(ast.data.len(), 2);
        assert_eq!(ast.data[0], ASTNode::Incr(1));
        assert_eq!(ast.data[1], ASTNode::SubFrom(1));
    }

    #[test]
    fn removes_leading_loops() {
        let ast = AST::parse("[-]").unwrap();
        assert_eq!(ast.data.len(), 0);
    }

    #[test]
    fn parses_rot13() {
        let ast = AST::parse(include_str!("../../test/programs/rot13.bf"));
        assert!(ast.is_ok());
    }

    #[test]
    fn parses_mandelbrot() {
        let ast = AST::parse(include_str!("../../test/programs/mandelbrot.bf"));
        assert!(ast.is_ok());
    }
}
