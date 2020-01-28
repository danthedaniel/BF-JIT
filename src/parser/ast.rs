use super::Instr;
use super::Program;

/// BrainFuck AST node
#[derive(Debug, Clone)]
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
    /// Loop over the contained instructions while the current memory cell is
    /// not zero.
    Loop(Vec<ASTNode>),
}

/// Container for a vector of ASTNodes.
#[derive(Debug, Clone)]
pub struct AST {
    data: Vec<ASTNode>,
}

impl AST {
    pub fn to_program(&self) -> Program {
        let instrs = Self::to_instrs(self.data.clone(), 0);
        Program::new(instrs)
    }

    fn to_instrs(nodes: Vec<ASTNode>, start: usize) -> Vec<Instr> {
        let mut instrs = Vec::new();

        for node in nodes {
            let program_index = start + instrs.len();

            match node {
                ASTNode::Incr(n) => instrs.push(Instr::Incr(n)),
                ASTNode::Decr(n) => instrs.push(Instr::Decr(n)),
                ASTNode::Next(n) => instrs.push(Instr::Next(n)),
                ASTNode::Prev(n) => instrs.push(Instr::Prev(n)),
                ASTNode::Print => instrs.push(Instr::Print),
                ASTNode::Read => instrs.push(Instr::Read),
                ASTNode::Loop(vec) => {
                    let inner_loop = Self::to_instrs(vec.clone(), program_index + 1);

                    instrs.push(Instr::BeginLoop(program_index + inner_loop.len() + 1));
                    instrs.extend(inner_loop);
                    instrs.push(Instr::EndLoop(program_index));
                }
            }
        }

        instrs
    }

    pub fn from_char_vec(input: Vec<char>) -> Result<Self, String> {
        if input.len() == 0 {
            // Return an empty program
            return Ok(Self { data: Vec::new() });
        }

        Ok(AST {
            data: Self::parse(&input[..])?,
        })
    }

    /// Convert raw input into a vector of ASTNodes.
    fn parse(input: &[char]) -> Result<Vec<ASTNode>, String> {
        let mut output = Vec::new();
        let mut loops: Vec<Vec<ASTNode>> = Vec::new();

        for character in input {
            /// Add a node to either the current loop or the top-level output.
            macro_rules! push_node {
                ($node:expr) => {{
                    let depth = loops.len();

                    if depth == 0 {
                        output.push($node);
                    } else {
                        loops[depth - 1].push($node);
                    }
                }};
            }

            match character {
                '+' => push_node!(ASTNode::Incr(1)),
                '-' => push_node!(ASTNode::Decr(1)),
                '>' => push_node!(ASTNode::Next(1)),
                '<' => push_node!(ASTNode::Prev(1)),
                '.' => push_node!(ASTNode::Print),
                ',' => push_node!(ASTNode::Read),
                '[' => loops.push(Vec::new()),
                ']' => {
                    let current_loop = loops.pop().ok_or(format!("More ] than ["))?;

                    // Do not add loop if it will be the first element in the
                    // ASTNode vector. This is because:
                    //
                    // 1. The BrainFuck machine starts all cells at 0
                    // 2. Loops are skipped when the current cell is 0
                    //
                    // So if no non-loops have executed there is no use in
                    // emitting a Loop ASTNode.
                    if output.len() > 0 {
                        let optimized_loop = Self::shallow_run_length_optimize(current_loop);
                        push_node!(ASTNode::Loop(optimized_loop));
                    }
                }
                // All other characters are comments and will be ignored
                _ => {}
            }
        }

        if loops.len() > 0 {
            // Anywhere deeper than the top level should always return from the '['
            // match branch above.
            //
            // Example program that will cause this error:
            //
            // [[]
            println!("");
            return Err(format!("More [ than ]"));
        }

        Ok(Self::shallow_run_length_optimize(output))
    }

    /// Convert runs of +, -, < and > into bulk operations.
    fn shallow_run_length_optimize(input: Vec<ASTNode>) -> Vec<ASTNode> {
        let mut output: Vec<ASTNode> = Vec::new();

        for node in input {
            let len = output.len();
            let last = output.get(len.checked_sub(1).unwrap_or(0));

            // For each operator +, -, < and >, if the last instruction in the
            // output Vec is the same, then increment that instruction instead
            // of adding another identical instruction.
            //
            // All other operators are just appended to the Vec.
            //
            // Loop jump positions are left un-calculated, to be determined
            // later.
            match (node, last) {
                // Incr
                (ASTNode::Incr(a), Some(ASTNode::Incr(b))) => {
                    output[len - 1] = ASTNode::Incr(a.wrapping_add(*b));
                }
                (ASTNode::Incr(a), _) => output.push(ASTNode::Incr(a)),
                // Decr
                (ASTNode::Decr(a), Some(ASTNode::Decr(b))) => {
                    output[len - 1] = ASTNode::Decr(a.wrapping_add(*b));
                }
                (ASTNode::Decr(a), _) => output.push(ASTNode::Decr(a)),
                // Next
                (ASTNode::Next(a), Some(ASTNode::Next(b))) => {
                    output[len - 1] = ASTNode::Next(a.wrapping_add(*b));
                }
                (ASTNode::Next(a), _) => output.push(ASTNode::Next(a)),
                // Prev
                (ASTNode::Prev(a), Some(ASTNode::Prev(b))) => {
                    output[len - 1] = ASTNode::Prev(a.wrapping_add(*b));
                }
                (ASTNode::Prev(a), _) => output.push(ASTNode::Prev(a)),
                // Print
                (ASTNode::Print, _) => output.push(ASTNode::Print),
                // Read
                (ASTNode::Read, _) => output.push(ASTNode::Read),
                // Loop
                (ASTNode::Loop(vec), _) => output.push(ASTNode::Loop(vec.clone())),
            }
        }

        output
    }
}
