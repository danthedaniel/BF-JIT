use anyhow::{Context, Result, bail};
use std::cmp;
use std::collections::VecDeque;
use std::io::{self, Read, Write};

use super::instr::Instr;
use crate::parser::AstNode;
use crate::runnable::syscall::{execute_syscall, parse_syscall_args};
use crate::runnable::{BF_MEMORY_SIZE, Runnable};

/// brainfuck virtual machine
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
    pub fn new(ast: VecDeque<AstNode>) -> Self {
        Self {
            program: Self::compile(ast),
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
                AstNode::MultiplyAddTo(offset, factor) => {
                    instrs.push(Instr::MultiplyAddTo(offset, factor));
                }
                AstNode::AddTo(offsets) => instrs.push(Instr::AddTo(offsets)),
                AstNode::SubFrom(offsets) => instrs.push(Instr::SubFrom(offsets)),
                AstNode::Loop(vec) => {
                    let inner_loop = Self::compile(vec);
                    // Add 1 to the offset to account for the BeginLoop/EndLoop instr
                    let offset = inner_loop.len() + 1;

                    instrs.push(Instr::BeginLoop(offset));
                    instrs.extend(inner_loop);
                    instrs.push(Instr::EndLoop(offset));
                }
                AstNode::Syscall => instrs.push(Instr::Syscall),
            }
        }

        instrs
    }

    /// Validate and calculate target memory position for operations with offsets
    fn get_target_position(&self, offset: i16) -> Result<usize> {
        let target_pos = isize::try_from(self.dp).unwrap() + offset as isize;

        if target_pos < 0 {
            bail!(
                "Memory access below zero: attempted to access position {}",
                target_pos
            );
        }

        #[allow(clippy::cast_sign_loss)]
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
    #[allow(clippy::too_many_lines)]
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
            // TODO: Examine poor performance with AddTo only seen in interpreter
            Instr::AddTo(offsets) => {
                if self.memory[self.dp] != 0 {
                    let value = self.memory[self.dp];

                    for offset in offsets {
                        let target_pos = self.get_target_position(offset).with_context(|| {
                            format!(
                                "Invalid target position for AddTo operation at offset {offset}"
                            )
                        })?;

                        self.memory[target_pos] = self.memory[target_pos].wrapping_add(value);
                    }

                    self.memory[self.dp] = 0;
                }
            }
            Instr::SubFrom(offsets) => {
                if self.memory[self.dp] != 0 {
                    let value = self.memory[self.dp];

                    for offset in offsets {
                        let target_pos = self.get_target_position(offset).with_context(|| {
                            format!(
                                "Invalid target position for CopyTo operation at offset {offset}"
                            )
                        })?;

                        self.memory[target_pos] = self.memory[target_pos].wrapping_sub(value);
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
            Instr::Syscall => {
                let result = self.do_syscall()?;
                self.memory[self.dp] = result;
            }
        }

        self.pc += 1;
        Ok(true)
    }

    pub fn reset(&mut self) {
        self.memory = vec![0u8; BF_MEMORY_SIZE];
        self.pc = 0;
        self.dp = 0;
    }

    /// Execute a syscall using the systemf convention.
    ///
    /// Returns the low byte of the syscall return value.
    fn do_syscall(&self) -> Result<u8> {
        let memory = &self.memory[self.dp..];
        let mem_base_ptr = self.memory.as_ptr();

        let args =
            parse_syscall_args(memory, mem_base_ptr).map_err(|e| anyhow::anyhow!("{}", e))?;

        #[allow(clippy::cast_possible_truncation)]
        Ok(execute_syscall(&args) as u8)
    }
}

impl Runnable for Interpreter {
    fn run(&mut self) -> Result<()> {
        let result = loop {
            match self.step() {
                Ok(true) => {}
                Ok(false) => break Ok(()),
                Err(error) => break Err(error),
            }
        };

        self.reset();
        result
    }
}
#[cfg(test)]
mod tests {
    use super::super::super::test_buffer::TestBuffer;
    use super::*;
    use crate::parser::AstNode;
    use std::io::Cursor;

    #[test]
    fn run_hello_world() {
        let ast = AstNode::parse(include_str!("../../../tests/programs/hello_world.bf"), false).unwrap();
        let mut fucker = Interpreter::new(ast);
        let shared_buffer = TestBuffer::new();
        fucker.io_write = Box::new(shared_buffer.clone());

        fucker.run().unwrap();

        let output_string = shared_buffer.get_string_content();
        assert_eq!(output_string, "Hello World!\n");
    }

    #[test]
    fn run_rot13() {
        // This rot13 program terminates after 16 characters so we can test it. Otherwise it would
        // wait on input forever.
        let ast = AstNode::parse(include_str!("../../../tests/programs/rot13-16char.bf"), false).unwrap();
        let mut fucker = Interpreter::new(ast);
        let shared_buffer = TestBuffer::new();
        fucker.io_write = Box::new(shared_buffer.clone());
        let in_cursor = Box::new(Cursor::new(b"Hello World! 123".to_vec()));
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
