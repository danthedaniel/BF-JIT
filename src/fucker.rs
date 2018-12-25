use libc::getchar;
use std::char;
use std::collections::HashSet;
use std::fmt;
use std::io::Write;
use std::time::SystemTime;

/// Get seconds since the UNIX epoch.
fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// Append a number with an SI suffix.
fn human_format(num: f64) -> String {
    let suffixes = ['k', 'M', 'G', 'T'];
    let index = (num.log10() / 3.0).floor() as usize - 1;

    if let Some(suffix) = suffixes.get(index) {
        let power = (index + 1) * 3;
        format!("{:.2} {}", num / 10.0_f64.powi(power as i32), suffix)
    } else {
        format!("{:.2} ", num)
    }
}

/// BrainFuck instruction
#[derive(Copy, Clone, Debug)]
pub enum Instr {
    Incr(u8),
    Decr(u8),
    Next(usize),
    Prev(usize),
    Print,
    Read,
    BeginLoop(Option<usize>),
    EndLoop(Option<usize>),
}

impl Instr {
    /// Convert a character slice into a Vec of Instr.
    pub fn parse(input: Vec<char>) -> Vec<Instr> {
        let no_comments = Instr::strip_comments(input);
        let optimized = Instr::optimize(no_comments);
        Instr::parse_loops(optimized)
    }

    /// Remove all non-control characters from the input.
    fn strip_comments(input: Vec<char>) -> Vec<char> {
        let control_chars: HashSet<char> = ['+', '-', '>', '<', '.', ',', '[', ']']
            .iter()
            .cloned()
            .collect();

        input
            .into_iter()
            .filter(|c| control_chars.contains(c))
            .collect()
    }

    // Convert runs of +, -, < and > into bulk operations.
    fn optimize(input: Vec<char>) -> Vec<Instr> {
        let mut output: Vec<Instr> = Vec::new();

        for c in input {
            let len = output.len();
            let last = if len > 0 { Some(output[len - 1]) } else { None };

            // For each operator +, -, < and >, if the last instruction in the
            // output Vec is the same, then increment that instruction instead
            // of adding another identical instruction.
            //
            // All other operators are just appended to the Vec.
            //
            // Loop jump positions are left uncalculated, to be determined
            // later.
            match (c, last) {
                // Incr
                ('+', Some(Instr::Incr(n))) => {
                    output[len - 1] = Instr::Incr(n + 1);
                }
                ('+', _) => output.push(Instr::Incr(1)),
                // Decr
                ('-', Some(Instr::Decr(n))) => {
                    output[len - 1] = Instr::Decr(n + 1);
                }
                ('-', _) => output.push(Instr::Decr(1)),
                // Next
                ('>', Some(Instr::Next(n))) => {
                    output[len - 1] = Instr::Next(n + 1);
                }
                ('>', _) => output.push(Instr::Next(1)),
                // Prev
                ('<', Some(Instr::Prev(n))) => {
                    output[len - 1] = Instr::Prev(n + 1);
                }
                ('<', _) => output.push(Instr::Prev(1)),
                // Other
                ('.', _) => output.push(Instr::Print),
                (',', _) => output.push(Instr::Read),
                ('[', _) => output.push(Instr::BeginLoop(None)),
                (']', _) => output.push(Instr::EndLoop(None)),
                // Comments should already be stripped
                (_, _) => unreachable!(),
            }
        }

        output
    }

    // Resolve loop jump positions.
    fn parse_loops(input: Vec<Instr>) -> Vec<Instr> {
        let mut output = input.clone();
        let mut stack: Vec<usize> = Vec::new();

        for (pos, instr) in input.iter().enumerate() {
            match instr {
                Instr::BeginLoop(None) => stack.push(pos),
                Instr::EndLoop(None) => {
                    let ret_pos = stack.pop().unwrap_or_else(|| panic!("More ] than ["));
                    output[pos] = Instr::EndLoop(Some(ret_pos));
                    output[ret_pos] = Instr::BeginLoop(Some(pos));
                }
                _ => {}
            }
        }

        if stack.len() > 0 {
            panic!("More [ than ]");
        }

        output
    }

    // Number of BrainFuck instructions this represents.
    pub fn ops(&self) -> u64 {
        match self {
            Instr::Incr(n) => *n as u64,
            Instr::Decr(n) => *n as u64,
            Instr::Next(n) => *n as u64,
            Instr::Prev(n) => *n as u64,
            Instr::Print => 1,
            Instr::Read => 1,
            Instr::BeginLoop(_) => 1,
            Instr::EndLoop(_) => 1,
        }
    }
}

impl fmt::Display for Instr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Instr::Incr(n) => write!(f, "INC\t0x{:04X}", n),
            Instr::Decr(n) => write!(f, "DEC\t0x{:04X}", n),
            Instr::Next(n) => write!(f, "NEXT\t0x{:04X}", n),
            Instr::Prev(n) => write!(f, "PREV\t0x{:04X}", n),
            Instr::Print => write!(f, "PRINT"),
            Instr::Read => write!(f, "READ"),
            Instr::BeginLoop(Some(end_pos)) => write!(f, "BEGIN\t0x{:04X}", end_pos),
            Instr::BeginLoop(None) => write!(f, "BEGIN\tNULL"),
            Instr::EndLoop(Some(ret_pos)) => write!(f, "END\t0x{:04X}", ret_pos),
            Instr::EndLoop(None) => write!(f, "END\tNULL"),
        }
    }
}

const FUCKER_MEM_SIZE: usize = 0x2000;

/// BrainFuck virtual machine
pub struct Fucker {
    program: Vec<Instr>,
    memory: [u8; FUCKER_MEM_SIZE],
    /// Program counter
    pc: usize,
    /// Data pointer
    dp: usize,
}

impl Fucker {
    pub fn new(program: Vec<Instr>) -> Self {
        Fucker {
            program: program,
            memory: [0; FUCKER_MEM_SIZE],
            pc: 0,
            dp: 0,
        }
    }

    pub fn run(&mut self) {
        let start = unix_time();
        let mut ops = 0u64;

        loop {
            if self.pc >= self.program.len() {
                let end = unix_time();
                let rate = ops as f64 / (end - start) as f64;
                println!("{} seconds", end - start);
                println!("{}ops/second", human_format(rate));
                return;
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
                    self.dp -= n;
                }
                Instr::Print => {
                    print!("{}", char::from_u32(current as u32).unwrap_or('?'));
                    std::io::stdout().flush().unwrap();
                }
                Instr::Read => {
                    self.memory[self.dp] = unsafe { getchar() } as u8;
                }
                Instr::BeginLoop(Some(end_pos)) => {
                    if current == 0 {
                        self.pc = end_pos;
                    }
                }
                Instr::EndLoop(Some(ret_pos)) => {
                    if current != 0 {
                        self.pc = ret_pos;
                    }
                }
                // This would only happen if a BeginLoop or EndLoop is left with
                // a None address inside.
                _ => unreachable!(),
            }

            ops += instr.ops();
            self.pc += 1;
        }
    }
}
