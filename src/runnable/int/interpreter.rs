use anyhow::{Context, Result, bail};
use std::cmp;
use std::io::{self, Read, Write};

use super::instr::Instr;
use crate::parser::{AstNode, Operand};
use crate::runnable::syscall::{execute_syscall, parse_syscall_args};
use crate::runnable::{BF_MEMORY_SIZE, Runnable};

/// brainfuck virtual machine
pub struct Interpreter {
    program: Vec<Instr>,
    pub(crate) memory: Vec<u8>,
    /// Program counter
    pc: usize,
    /// Data pointer
    dp: usize,
    /// Reader used by brainfuck's , command
    pub(crate) io_read: Box<dyn Read>,
    /// Writer used by brainfuck's . command
    pub(crate) io_write: Box<dyn Write>,
}

impl Interpreter {
    pub fn new(ast: Vec<AstNode>) -> Self {
        Self {
            program: Self::compile(ast),
            memory: vec![0u8; BF_MEMORY_SIZE],
            pc: 0,
            dp: 0,
            io_read: Box::new(io::stdin()),
            io_write: Box::new(io::stdout()),
        }
    }

    fn compile(nodes: Vec<AstNode>) -> Vec<Instr> {
        let mut instrs = Vec::new();

        for node in nodes {
            match node {
                AstNode::Add(offset, n) => instrs.push(Instr::Add(offset, n)),
                AstNode::Set(offset, n) => instrs.push(Instr::Set(offset, n)),
                AstNode::MulAdd { src, dst, factor } => {
                    instrs.push(Instr::MulAdd { src, dst, factor });
                }
                AstNode::CondAdd {
                    lhs,
                    rhs,
                    dst,
                    value,
                } => instrs.push(Instr::CondAdd {
                    lhs,
                    rhs,
                    dst,
                    value,
                }),
                AstNode::ProductAdd {
                    base,
                    step,
                    count,
                    high,
                    dst,
                    value,
                } => instrs.push(Instr::ProductAdd {
                    base,
                    step,
                    count,
                    high,
                    dst,
                    value,
                }),
                AstNode::Move(n) => instrs.push(Instr::Move(n)),
                AstNode::Print(offset) => instrs.push(Instr::Print(offset)),
                AstNode::Read(offset) => instrs.push(Instr::Read(offset)),
                AstNode::Scan(stride) => instrs.push(Instr::Scan(stride)),
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

    /// Validate and calculate a memory position relative to the data pointer,
    /// growing memory if needed.
    fn position(&mut self, offset: i32) -> Result<usize> {
        let Some(position) = self.dp.checked_add_signed(offset.try_into()?) else {
            bail!(
                "Memory access below zero: attempted to access position {} + {offset}",
                self.dp
            );
        };

        // If the position is outside of memory, expand either to a double of
        // the current memory size, or the new position (whichever is bigger).
        if position >= self.memory.len() {
            let new_len = cmp::max(self.memory.len() * 2, position + 1);
            self.memory.resize(new_len, 0);
        }

        Ok(position)
    }

    fn cell(&mut self, offset: i32) -> Result<&mut u8> {
        let position = self.position(offset)?;
        Ok(&mut self.memory[position])
    }

    fn operand(&mut self, operand: Operand) -> Result<u8> {
        Ok(match operand.reads() {
            Some(offset) => operand.eval(*self.cell(offset)?),
            None => operand.bias,
        })
    }

    /// Move the data pointer.
    fn shift(&mut self, amount: i32) -> Result<()> {
        self.dp = self
            .dp
            .checked_add_signed(amount as isize)
            .with_context(|| {
                format!(
                    "Attempted to move data pointer out of bounds: {} + {}",
                    self.dp, amount
                )
            })?;
        Ok(())
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

        match self.program[self.pc] {
            Instr::Add(offset, n) => {
                let cell = self.cell(offset)?;
                *cell = cell.wrapping_add(n);
            }
            Instr::Set(offset, n) => {
                *self.cell(offset)? = n;
            }
            Instr::MulAdd { src, dst, factor } => {
                // Optimized loops which wouldn't have run don't touch the destination.
                let value = *self.cell(src)?;
                if value != 0 {
                    let cell = self.cell(dst)?;
                    *cell = cell.wrapping_add(value.wrapping_mul(factor));
                }
            }
            Instr::CondAdd {
                lhs,
                rhs,
                dst,
                value,
            } => {
                if self.operand(lhs)? < self.operand(rhs)? {
                    let cell = self.cell(dst)?;
                    *cell = cell.wrapping_add(value);
                }
            }
            Instr::ProductAdd {
                base,
                step,
                count,
                high,
                dst,
                value,
            } => {
                let total = u16::from(self.operand(base)?)
                    + u16::from(self.operand(step)?) * u16::from(self.operand(count)?);
                let [high_byte, low_byte] = total.to_be_bytes();
                let byte = if high { high_byte } else { low_byte };
                let cell = self.cell(dst)?;
                *cell = cell.wrapping_add(byte.wrapping_mul(value));
            }
            Instr::Move(n) => self.shift(n)?,
            Instr::Print(offset) => {
                let value = *self.cell(offset)?;
                self.io_write
                    .write_all(&[value])
                    .context("Failed to write output character")?;
            }
            Instr::Read(offset) => {
                let mut buf = [0u8; 1];
                let value = match self.io_read.read_exact(&mut buf) {
                    Ok(()) => buf[0],
                    // Default to newlines if the input stream is empty.
                    Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => b'\n',
                    Err(error) => {
                        return Err(error).context("Failed to read input character");
                    }
                };
                *self.cell(offset)? = value;
            }
            Instr::Scan(stride) => {
                while *self.cell(0)? != 0 {
                    self.shift(stride)?;
                }
            }
            Instr::BeginLoop(offset) => {
                if *self.cell(0)? == 0 {
                    self.pc += offset;
                }
            }
            Instr::EndLoop(offset) => {
                if *self.cell(0)? != 0 {
                    self.pc -= offset;
                }
            }
            Instr::Syscall => {
                self.position(0)?;
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

        let syscall_args =
            parse_syscall_args(memory, mem_base_ptr).map_err(|e| anyhow::anyhow!("{}", e))?;

        #[allow(clippy::cast_possible_truncation)]
        Ok(execute_syscall(&syscall_args) as u8)
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
        let ast = AstNode::parse(
            include_str!("../../../tests/programs/hello_world.bf"),
            false,
        )
        .unwrap();
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
        let ast = AstNode::parse(
            include_str!("../../../tests/programs/rot13-16char.bf"),
            false,
        )
        .unwrap();
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
        // Set cell 0 to 5, then multiply by 3 and add to cell 2
        let nodes = vec![
            AstNode::Set(0, 5),
            AstNode::MulAdd {
                src: 0,
                dst: 2,
                factor: 3,
            },
        ];

        let mut interpreter = Interpreter::new(nodes);
        // Step through the program without resetting
        while interpreter.step().unwrap_or(false) {}

        assert_eq!(interpreter.memory[0], 5);
        assert_eq!(interpreter.memory[2], 15);
    }
}
