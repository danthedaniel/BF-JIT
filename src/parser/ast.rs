use std::collections::VecDeque;
use std::mem;

use super::super::runnable::{Fucker, JITMemory, Runnable};
use super::Instr;

/// Functions called by JIT-compiled code.
mod jit_functions {
    use libc::getchar;
    use std::char;
    use std::io::Write;

    /// Print a single byte to stdout.
    pub extern "C" fn print(byte: u8) {
        print!("{}", char::from_u32(byte as u32).unwrap_or('?'));
        std::io::stdout().flush().unwrap();
    }

    /// Read a single byte from stdin.
    pub extern "C" fn read() -> u8 {
        unsafe { getchar() as u8 }
    }
}

/// Convert an expression to a native-endian order byte array after a type cast.
macro_rules! to_ne_bytes {
    ($ptr:expr, $ptr_type:ty) => {{
        let bytes: [u8; mem::size_of::<$ptr_type>()] = unsafe { mem::transmute($ptr as $ptr_type) };
        bytes
    }};
}

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
    Loop(VecDeque<ASTNode>),
}

/// Container for a vector of ASTNodes.
#[derive(Debug, Clone)]
pub struct AST {
    data: VecDeque<ASTNode>,
}

impl AST {
    fn nodes_to_instrs(nodes: &mut VecDeque<ASTNode>) -> Vec<Instr> {
        let mut instrs = Vec::new();

        while let Some(mut node) = nodes.pop_front() {
            match &mut node {
                ASTNode::Incr(n) => instrs.push(Instr::Incr(*n)),
                ASTNode::Decr(n) => instrs.push(Instr::Decr(*n)),
                ASTNode::Next(n) => instrs.push(Instr::Next(*n)),
                ASTNode::Prev(n) => instrs.push(Instr::Prev(*n)),
                ASTNode::Print => instrs.push(Instr::Print),
                ASTNode::Read => instrs.push(Instr::Read),
                ASTNode::Loop(vec) => {
                    let inner_loop = Self::nodes_to_instrs(vec);
                    // Add 1 to the offset to account for the BeginLoop/EndLoop instr
                    let offset = inner_loop.len() + 1;

                    instrs.push(Instr::BeginLoop(offset));
                    instrs.extend(inner_loop);
                    instrs.push(Instr::EndLoop(offset));
                }
            }
        }

        instrs
    }

    /// Convert raw input into an AST.
    pub fn parse(input: String) -> Result<Self, String> {
        let mut output = VecDeque::new();
        let mut loops: VecDeque<VecDeque<ASTNode>> = VecDeque::new();

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

                    ASTNode::Loop(Self::shallow_run_length_optimize(&mut current_loop))
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
            data: Self::shallow_run_length_optimize(&mut output),
        })
    }

    /// Convert runs of +, -, < and > into bulk operations.
    fn shallow_run_length_optimize(input: &mut VecDeque<ASTNode>) -> VecDeque<ASTNode> {
        let mut output = VecDeque::new();

        while let Some(next_node) = input.pop_front() {
            let prev_node = output.back();

            // For each operator +, -, < and >, if the last instruction in the
            // output Vec is the same, then increment that instruction instead
            // of adding another identical instruction.
            let combined = match (prev_node, &next_node) {
                (Some(ASTNode::Incr(b)), ASTNode::Incr(a)) => ASTNode::Incr(a.wrapping_add(*b)),
                (Some(ASTNode::Decr(b)), ASTNode::Decr(a)) => ASTNode::Decr(a.wrapping_add(*b)),
                (Some(ASTNode::Next(b)), ASTNode::Next(a)) => ASTNode::Next(a.wrapping_add(*b)),
                (Some(ASTNode::Prev(b)), ASTNode::Prev(a)) => ASTNode::Prev(a.wrapping_add(*b)),
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

    pub fn int(&self) -> Box<dyn Runnable> {
        Box::new(Fucker::new(Self::nodes_to_instrs(&mut self.data.clone())))
    }

    /// Initialize a JIT compiled version of this program.
    #[cfg(target_arch = "x86_64")]
    pub fn jit(&self) -> Result<Box<dyn Runnable>, String> {
        let mut bytes = Vec::new();

        // push   rbp
        bytes.push(0x55);

        // mov    rbp,rsp
        bytes.push(0x48);
        bytes.push(0x89);
        bytes.push(0xe5);

        // Store pointer to brainfuck memory (first argument) in r10
        // mov    r10,rdi
        bytes.push(0x49);
        bytes.push(0x89);
        bytes.push(0xfa);

        bytes.extend(Self::jit_loop(&self.data));

        // mov    rsp,rbp
        bytes.push(0x48);
        bytes.push(0x89);
        bytes.push(0xec);

        // pop    rbp
        bytes.push(0x5d);

        // ret
        bytes.push(0xc3);

        Ok(Box::new(JITMemory::new(bytes)))
    }

    /// No-op version of jit() for unsupported architectures.
    #[cfg(not(target_arch = "x86_64"))]
    pub fn jit(&self) -> Result<Box<dyn Runnable>, String> {
        Err(format!("Unsupported JIT architecture."))
    }

    /// Convert a vector of ASTNodes into a sequence of executable bytes.
    ///
    /// r10 is used to hold the data pointer.
    #[cfg(target_arch = "x86_64")]
    fn jit_loop(data: &VecDeque<ASTNode>) -> Vec<u8> {
        let mut bytes: Vec<u8> = Vec::new();

        for node in data {
            match node {
                ASTNode::Incr(n) => {
                    // add    BYTE PTR [r10],n
                    bytes.push(0x41);
                    bytes.push(0x80);
                    bytes.push(0x02);
                    bytes.push(*n);
                }
                ASTNode::Decr(n) => {
                    // sub    BYTE PTR [r10],n
                    bytes.push(0x41);
                    bytes.push(0x80);
                    bytes.push(0x2a);
                    bytes.push(*n);
                }
                ASTNode::Next(n) => {
                    // HACK: Assumes usize won't be more than 32 bit...
                    let n_bytes = (*n as u32).to_ne_bytes();

                    // add    r10,n
                    bytes.push(0x49);
                    bytes.push(0x81);
                    bytes.push(0xc2);
                    bytes.push(n_bytes[0]);
                    bytes.push(n_bytes[1]);
                    bytes.push(n_bytes[2]);
                    bytes.push(n_bytes[3]);
                }
                ASTNode::Prev(n) => {
                    // HACK: Assumes usize won't be more than 32 bit...
                    let n_bytes = (*n as u32).to_ne_bytes();

                    // sub    r10,n
                    bytes.push(0x49);
                    bytes.push(0x81);
                    bytes.push(0xea);
                    bytes.push(n_bytes[0]);
                    bytes.push(n_bytes[1]);
                    bytes.push(n_bytes[2]);
                    bytes.push(n_bytes[3]);
                }
                ASTNode::Print => {
                    let print_ptr_bytes =
                        to_ne_bytes!(jit_functions::print, extern "C" fn(u8) -> ());

                    // Move the current memory cell into the first argument register
                    // movzx    rdi,BYTE PTR [r10]
                    bytes.push(0x49);
                    bytes.push(0x0f);
                    bytes.push(0xb6);
                    bytes.push(0x3a);

                    // Push data pointer onto stack
                    // push    r10
                    bytes.push(0x41);
                    bytes.push(0x52);

                    // Push return address onto stack
                    // push   rax
                    bytes.push(0x50);

                    // Copy function pointer for print() into rax
                    // movabs rax,print
                    bytes.push(0x48);
                    bytes.push(0xb8);
                    bytes.push(print_ptr_bytes[0]);
                    bytes.push(print_ptr_bytes[1]);
                    bytes.push(print_ptr_bytes[2]);
                    bytes.push(print_ptr_bytes[3]);
                    bytes.push(print_ptr_bytes[4]);
                    bytes.push(print_ptr_bytes[5]);
                    bytes.push(print_ptr_bytes[6]);
                    bytes.push(print_ptr_bytes[7]);

                    // Call print()
                    // call   rax
                    bytes.push(0xff);
                    bytes.push(0xd0);

                    // Pop return address from the stack
                    // pop    rax
                    bytes.push(0x58);

                    // Pop data pointer from the stack
                    // pop    r10
                    bytes.push(0x41);
                    bytes.push(0x5a);
                }
                ASTNode::Read => {
                    let read_ptr_bytes = to_ne_bytes!(jit_functions::read, extern "C" fn() -> u8);

                    // Push data pointer onto stack
                    // push    r10
                    bytes.push(0x41);
                    bytes.push(0x52);

                    // Push return address onto stack
                    // push   rax
                    bytes.push(0x50);

                    // Copy function pointer for read() into rax
                    // movabs rax,read
                    bytes.push(0x48);
                    bytes.push(0xb8);
                    bytes.push(read_ptr_bytes[0]);
                    bytes.push(read_ptr_bytes[1]);
                    bytes.push(read_ptr_bytes[2]);
                    bytes.push(read_ptr_bytes[3]);
                    bytes.push(read_ptr_bytes[4]);
                    bytes.push(read_ptr_bytes[5]);
                    bytes.push(read_ptr_bytes[6]);
                    bytes.push(read_ptr_bytes[7]);

                    // Call read()
                    // call   rax
                    bytes.push(0xff);
                    bytes.push(0xd0);

                    // Copy return value into current cell.
                    // mov    BYTE PTR [r10],al
                    bytes.push(0x41);
                    bytes.push(0x88);
                    bytes.push(0x02);

                    // Pop return address from the stack.
                    // pop    rax
                    bytes.push(0x58);

                    // Pop data pointer from the stack.
                    // pop    r10
                    bytes.push(0x41);
                    bytes.push(0x5a);
                }
                ASTNode::Loop(vec) => {
                    let inner_loop_bytes = Self::jit_loop(&vec);
                    let inner_loop_size = inner_loop_bytes.len() as i32;

                    let end_loop_size: i32 = 10; // Bytes
                    let byte_offset = inner_loop_size + end_loop_size;

                    // Check if the current memory cell equals zero.
                    // cmp    BYTE PTR [r10],0x0
                    bytes.push(0x41);
                    bytes.push(0x80);
                    bytes.push(0x3a);
                    bytes.push(0x00);

                    let offset_bytes = byte_offset.to_ne_bytes();

                    // Jump to the end of the loop if equal.
                    // je    offset
                    bytes.push(0x0f);
                    bytes.push(0x84);
                    bytes.push(offset_bytes[0]);
                    bytes.push(offset_bytes[1]);
                    bytes.push(offset_bytes[2]);
                    bytes.push(offset_bytes[3]);

                    bytes.extend(inner_loop_bytes);

                    // Check if the current memory cell equals zero.
                    // cmp    BYTE PTR [r10],0x0
                    bytes.push(0x41);
                    bytes.push(0x80);
                    bytes.push(0x3a);
                    bytes.push(0x00);

                    let offset_bytes = (-byte_offset).to_ne_bytes();

                    // Jump back to the beginning of the loop if not equal.
                    // jne    offset
                    bytes.push(0x0f);
                    bytes.push(0x85);
                    bytes.push(offset_bytes[0]);
                    bytes.push(offset_bytes[1]);
                    bytes.push(offset_bytes[2]);
                    bytes.push(offset_bytes[3]);
                }
            }
        }

        bytes
    }
}
