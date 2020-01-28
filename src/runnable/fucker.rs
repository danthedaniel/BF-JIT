use std::cmp;
use std::io::Write;

use libc::getchar;

use super::super::parser::{Instr, Program};
use super::Runnable;

/// BrainFuck virtual machine
pub struct Fucker {
    program: Program,
    memory: Vec<u8>,
    /// Program counter
    pub pc: usize,
    /// Data pointer
    pub dp: usize,
}

impl Fucker {
    pub fn new(program: Program) -> Self {
        Fucker {
            program: program,
            memory: vec![0u8; 0x4000],
            pc: 0,
            dp: 0,
        }
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
            Instr::BeginLoop(end_pos) => {
                if current == 0 {
                    self.pc = end_pos;
                }
            }
            Instr::EndLoop(ret_pos) => {
                if current != 0 {
                    self.pc = ret_pos;
                }
            }
        }

        self.pc += 1;

        return true;
    }
}

impl Runnable for Fucker {
    fn run(&mut self) {
        while self.step() {}
    }
}
