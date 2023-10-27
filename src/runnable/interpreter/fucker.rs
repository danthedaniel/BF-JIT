use std::cmp;
use std::collections::VecDeque;
use std::io::{self, Read, Write};

use super::super::Runnable;
use super::instr::Instr;
use crate::parser::AstNode;
use crate::runnable::BF_MEMORY_SIZE;

/// BrainFuck virtual machine
pub struct Fucker {
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

impl Fucker {
    pub fn new(nodes: VecDeque<AstNode>) -> Self {
        Fucker {
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

    /// Execute a single instruction on the VM.
    ///
    /// Returns false when the program has terminated.
    pub fn step(&mut self) -> bool {
        // Terminate if the program counter is outside of the program.
        if self.pc >= self.program.len() {
            return false;
        }

        // If the data pointer ends up outside of memory, expand either to a
        // double of the current memory size, or the new data pointer location
        // (whichever is bigger).
        if self.dp >= self.memory.len() {
            let new_len = cmp::max(self.memory.len() * 2, self.dp);
            self.memory.resize(new_len, 0);
        }

        let instr = self.program[self.pc];
        let current = self.memory[self.dp];

        match instr {
            Instr::Incr(n) => {
                self.memory[self.dp] = current.wrapping_add(n);
            }
            Instr::Decr(n) => {
                self.memory[self.dp] = current.wrapping_sub(n);
            }
            Instr::Next(n) => {
                self.dp += n;
            }
            Instr::Prev(n) => {
                if self.dp < n {
                    eprintln!("Attempted to point below memory location 0.");
                    return false;
                }

                self.dp -= n;
            }
            Instr::Print => {
                if let Err(msg) = self.io_write.write_all(&[current]) {
                    eprintln!("{}", msg);
                    return false;
                }
            }
            Instr::Read => {
                let mut buf = [0u8; 1];
                if let Err(error) = self.io_read.read_exact(&mut buf) {
                    if error.kind() != io::ErrorKind::UnexpectedEof {
                        eprintln!("{}", error);
                        return false;
                    }

                    // Default to newlines if the input stream is empty.
                    buf[0] = b'\n';
                }
                self.memory[self.dp] = buf[0];
            }
            Instr::Set(n) => {
                self.memory[self.dp] = n;
            }
            Instr::AddTo(n) => {
                if self.memory[self.dp] != 0 {
                    let target_pos = self.dp as isize + n;

                    if (target_pos < 0) || (target_pos as usize >= self.memory.len()) {
                        eprintln!("Attempted to move data outside of the bounds of memory");
                        return false;
                    }

                    self.memory[target_pos as usize] =
                        self.memory[target_pos as usize].wrapping_add(self.memory[self.dp]);
                    self.memory[self.dp] = 0;
                }
            }
            Instr::SubFrom(n) => {
                if self.memory[self.dp] != 0 {
                    let target_pos = self.dp as isize + n;

                    if (target_pos < 0) || (target_pos as usize >= self.memory.len()) {
                        eprintln!("Attempted to move data outside of the bounds of memory");
                        return false;
                    }

                    self.memory[target_pos as usize] =
                        self.memory[target_pos as usize].wrapping_sub(self.memory[self.dp]);
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

        true
    }

    pub fn reset(&mut self) {
        for i in 0..(self.memory.len() - 1) {
            self.memory[i] = 0;
        }

        self.pc = 0;
        self.dp = 0;
    }
}

impl Runnable for Fucker {
    fn run(&mut self) {
        while self.step() {}
        self.reset();
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
        let ast = Ast::parse(include_str!("../../../test/programs/hello_world.bf")).unwrap();
        let mut fucker = Fucker::new(ast.data);
        let shared_buffer = SharedBuffer::new();
        fucker.io_write = Box::new(shared_buffer.clone());

        fucker.run();

        let output_string = shared_buffer.get_string_content();
        assert_eq!(output_string, "Hello World!\n");
    }

    #[test]
    fn run_rot13() {
        // This rot13 program terminates after 16 characters so we can test it. Otherwise it would
        // wait on input forever.
        let ast = Ast::parse(include_str!("../../../test/programs/rot13-16char.bf")).unwrap();
        let mut fucker = Fucker::new(ast.data);
        let shared_buffer = SharedBuffer::new();
        fucker.io_write = Box::new(shared_buffer.clone());
        let in_cursor = Box::new(Cursor::new("Hello World! 123".as_bytes().to_vec()));
        fucker.io_read = in_cursor;

        fucker.run();

        let output_string = shared_buffer.get_string_content();
        assert_eq!(output_string, "Uryyb Jbeyq! 123");
    }
}
