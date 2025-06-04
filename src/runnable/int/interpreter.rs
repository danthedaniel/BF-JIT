use anyhow::{Context, Result, bail};
use std::cmp;
use std::collections::VecDeque;
use std::io::{self, Read, Write};

use super::instr::Instr;
use crate::parser::AstNode;
use crate::runnable::{BF_MEMORY_SIZE, Runnable};

/// BrainFuck virtual machine
pub struct Interpreter {
    program: Vec<Instr>,
    memory: Vec<u8>,
    /// Program counter
    pc: usize,
    /// Data pointer
    dp: usize,
    /// Reader used by brainfuck's , command
    io_read: Box<dyn Read>,
    /// Writer used by brainfuck's . command
    io_write: Box<dyn Write>,
}

impl Interpreter {
    pub fn new(nodes: VecDeque<AstNode>) -> Self {
        Interpreter {
            program: Self::compile(nodes),
            memory: vec![0u8; BF_MEMORY_SIZE],
            pc: 0,
            dp: 0,
            io_read: Box::new(io::stdin()),
            io_write: Box::new(io::stdout()),
        }
    }

    fn compile(nodes: VecDeque<AstNode>) -> Vec<Instr> {
        let mut instrs = Vec::new();

        for node in nodes {
            match node {
                AstNode::Incr(n) => instrs.push(Instr::Incr(n)),
                AstNode::Decr(n) => instrs.push(Instr::Decr(n)),
                AstNode::Next(n) => instrs.push(Instr::Next(n)),
                AstNode::Prev(n) => instrs.push(Instr::Prev(n)),
                AstNode::Print => instrs.push(Instr::Print),
                AstNode::Read => instrs.push(Instr::Read),
                AstNode::Set(n) => instrs.push(Instr::Set(n)),
                AstNode::AddTo(n) => instrs.push(Instr::AddTo(n)),
                AstNode::SubFrom(n) => instrs.push(Instr::SubFrom(n)),
                AstNode::MultiplyAddTo(offset, factor) => {
                    instrs.push(Instr::MultiplyAddTo(offset, factor))
                }
                AstNode::CopyTo(offsets) => instrs.push(Instr::CopyTo(offsets)),
                AstNode::Loop(vec) => {
                    let inner_loop = Self::compile(vec);
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

    /// Validate and calculate target memory position for operations with offsets
    fn get_target_position(&self, offset: i16) -> Result<usize> {
        let target_pos = self.dp as isize + offset as isize;

        if target_pos < 0 {
            bail!(
                "Memory access below zero: attempted to access position {}",
                target_pos
            );
        }

        let target_pos = target_pos as usize;
        if target_pos >= self.memory.len() {
            bail!(
                "Memory access out of bounds: attempted to access position {} (memory size: {})",
                target_pos,
                self.memory.len()
            );
        }

        Ok(target_pos)
    }

    /// Execute a single instruction on the VM.
    ///
    /// Returns Ok(true) to continue execution, Ok(false) when the program has terminated normally,
    /// or Err(_) on execution errors.
    pub fn step(&mut self) -> Result<bool> {
        // Terminate if the program counter is outside of the program.
        if self.pc >= self.program.len() {
            return Ok(false);
        }

        // If the data pointer ends up outside of memory, expand either to a
        // double of the current memory size, or the new data pointer location
        // (whichever is bigger).
        if self.dp >= self.memory.len() {
            let new_len = cmp::max(self.memory.len() * 2, self.dp + 1);
            self.memory.resize(new_len, 0);
        }

        let instr = self.program[self.pc].clone();
        let current = self.memory[self.dp];

        match instr {
            Instr::Incr(n) => {
                self.memory[self.dp] = current.wrapping_add(n);
            }
            Instr::Decr(n) => {
                self.memory[self.dp] = current.wrapping_sub(n);
            }
            Instr::Next(n) => {
                self.dp = self
                    .dp
                    .checked_add(n as usize)
                    .with_context(|| format!("Data pointer overflow: {} + {}", self.dp, n))?;
            }
            Instr::Prev(n) => {
                if self.dp < n as usize {
                    bail!(
                        "Attempted to move data pointer below zero: {} - {}",
                        self.dp,
                        n
                    );
                }
                self.dp -= n as usize;
            }
            Instr::Print => {
                self.io_write
                    .write_all(&[current])
                    .context("Failed to write output character")?;
            }
            Instr::Read => {
                let mut buf = [0u8; 1];
                match self.io_read.read_exact(&mut buf) {
                    Ok(()) => {
                        self.memory[self.dp] = buf[0];
                    }
                    Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                        // Default to newlines if the input stream is empty.
                        self.memory[self.dp] = b'\n';
                    }
                    Err(error) => {
                        return Err(error).context("Failed to read input character");
                    }
                }
            }
            Instr::Set(n) => {
                self.memory[self.dp] = n;
            }
            Instr::AddTo(offset) => {
                if self.memory[self.dp] != 0 {
                    let target_pos = self
                        .get_target_position(offset)
                        .context("Invalid target position for AddTo operation")?;

                    self.memory[target_pos] =
                        self.memory[target_pos].wrapping_add(self.memory[self.dp]);
                    self.memory[self.dp] = 0;
                }
            }
            Instr::SubFrom(offset) => {
                if self.memory[self.dp] != 0 {
                    let target_pos = self
                        .get_target_position(offset)
                        .context("Invalid target position for SubFrom operation")?;

                    self.memory[target_pos] =
                        self.memory[target_pos].wrapping_sub(self.memory[self.dp]);
                    self.memory[self.dp] = 0;
                }
            }
            Instr::MultiplyAddTo(offset, factor) => {
                if self.memory[self.dp] != 0 {
                    let target_pos = self
                        .get_target_position(offset)
                        .context("Invalid target position for MultiplyAddTo operation")?;

                    let value = self.memory[self.dp].wrapping_mul(factor);
                    self.memory[target_pos] = self.memory[target_pos].wrapping_add(value);
                    self.memory[self.dp] = 0;
                }
            }
            // TODO: Examine poor performance with CopyTo only seen in interpreter
            Instr::CopyTo(offsets) => {
                if self.memory[self.dp] != 0 {
                    let value = self.memory[self.dp];

                    for offset in offsets {
                        let target_pos = self.get_target_position(offset).with_context(|| {
                            format!(
                                "Invalid target position for CopyTo operation at offset {}",
                                offset
                            )
                        })?;

                        self.memory[target_pos] = self.memory[target_pos].wrapping_add(value);
                    }

                    self.memory[self.dp] = 0;
                }
            }
            Instr::BeginLoop(offset) => {
                if current == 0 {
                    self.pc += offset;
                }
            }
            Instr::EndLoop(offset) => {
                if current != 0 {
                    self.pc -= offset;
                }
            }
        }

        self.pc += 1;
        Ok(true)
    }

    pub fn reset(&mut self) {
        for i in 0..self.memory.len() {
            self.memory[i] = 0;
        }

        self.pc = 0;
        self.dp = 0;
    }
}

impl Runnable for Interpreter {
    fn run(&mut self) -> Result<()> {
        let result = loop {
            match self.step() {
                Ok(true) => continue,
                Ok(false) => break Ok(()),
                Err(error) => break Err(error),
            };
        };

        self.reset();
        result
    }
}
#[cfg(test)]
mod tests {
    use super::super::super::test_buffer::SharedBuffer;
    use super::*;
    use crate::parser::Ast;
    use std::io::Cursor;

    #[test]
    fn run_hello_world() {
        let ast = Ast::parse(include_str!("../../../tests/programs/hello_world.bf")).unwrap();
        let mut fucker = Interpreter::new(ast.data);
        let shared_buffer = SharedBuffer::new();
        fucker.io_write = Box::new(shared_buffer.clone());

        fucker.run().unwrap();

        let output_string = shared_buffer.get_string_content();
        assert_eq!(output_string, "Hello World!\n");
    }

    #[test]
    fn run_rot13() {
        // This rot13 program terminates after 16 characters so we can test it. Otherwise it would
        // wait on input forever.
        let ast = Ast::parse(include_str!("../../../tests/programs/rot13-16char.bf")).unwrap();
        let mut fucker = Interpreter::new(ast.data);
        let shared_buffer = SharedBuffer::new();
        fucker.io_write = Box::new(shared_buffer.clone());
        let in_cursor = Box::new(Cursor::new("Hello World! 123".as_bytes().to_vec()));
        fucker.io_read = in_cursor;

        fucker.run().unwrap();

        let output_string = shared_buffer.get_string_content();
        assert_eq!(output_string, "Uryyb Jbeyq! 123");
    }

    #[test]
    fn test_multiply_add_to() {
        use crate::parser::AstNode;
        use std::collections::VecDeque;

        // Create a simple program that tests MultiplyAddTo
        // Set cell 0 to 5, then multiply by 3 and add to cell 2
        let mut nodes = VecDeque::new();
        nodes.push_back(AstNode::Set(5)); // Set current cell to 5
        nodes.push_back(AstNode::MultiplyAddTo(2, 3)); // Multiply by 3, add to cell at offset +2

        let mut interpreter = Interpreter::new(nodes);
        // Step through the program without resetting
        while interpreter.step().unwrap_or(false) {}

        // Cell 0 should be 0 (cleared after operation)
        assert_eq!(interpreter.memory[0], 0);
        // Cell 2 should be 15 (5 * 3)
        assert_eq!(interpreter.memory[2], 15);
    }
}
