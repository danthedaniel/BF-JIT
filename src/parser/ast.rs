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
                _ => return None,
            };
        }

        None
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
