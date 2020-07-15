use std::cmp;
use std::collections::VecDeque;
use std::io::Write;

use libc::getchar;

use super::super::parser::{ASTNode, Instr};
use super::Runnable;

/// BrainFuck virtual machine
pub struct Fucker {
    program: Vec<Instr>,
    memory: Vec<u8>,
    /// Program counter
    pub pc: usize,
    /// Data pointer
    pub dp: usize,
}

impl Fucker {
    pub fn new(nodes: &VecDeque<ASTNode>) -> Self {
        Fucker {
            program: Self::compile(nodes),
            memory: vec![0u8; 0x4000],
            pc: 0,
            dp: 0,
        }
    }

    fn compile(nodes: &VecDeque<ASTNode>) -> Vec<Instr> {
        let mut instrs = Vec::new();

        for node in nodes {
            match node {
                ASTNode::Incr(n) => instrs.push(Instr::Incr(*n)),
                ASTNode::Decr(n) => instrs.push(Instr::Decr(*n)),
                ASTNode::Next(n) => instrs.push(Instr::Next(*n)),
                ASTNode::Prev(n) => instrs.push(Instr::Prev(*n)),
                ASTNode::Print => instrs.push(Instr::Print),
                ASTNode::Read => instrs.push(Instr::Read),
                ASTNode::Loop(vec) => {
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
                if let Err(msg) = std::io::stdout()
                    .write(&[current])
                    .and_then(|_size| std::io::stdout().flush())
                {
                    eprintln!("{}", msg);
                    return false;
                }
            }
            Instr::Read => {
                self.memory[self.dp] = unsafe { getchar() as u8 };
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

#[cfg(target_arch = "x86_64")]
#[cfg(test)]
mod tests {
    use super::super::super::parser::AST;
    use super::*;

    #[test]
    fn run_hello_world() {
        let ast = AST::parse(include_str!("../../test/programs/hello_world.bf")).unwrap();
        let mut fucker = Fucker::new(&ast.data);
        fucker.run();
    }
}
