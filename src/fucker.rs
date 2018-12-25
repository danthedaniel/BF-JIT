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

/// BrainFuck instruction
#[derive(Copy, Clone, Debug)]
pub enum Instr {
    Incr,
    Decr,
    Next,
    Prev,
    Print,
    Read,
    BeginLoop(usize),
    EndLoop(usize),
}

impl Instr {
    /// Convert a character slice into a Vec of Instr.
    pub fn parse(input: Vec<char>) -> Vec<Instr> {
        Instr::parse_loop(Instr::strip_comments(input).as_slice(), None).0
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

    /// Parse a `[ ... ]` BrainFuck loop.
    ///
    /// * `input` - The entire input array.
    /// * `ret` - Loop's starting position. Will be None when at the top level.
    ///
    /// Returns the instructions in the loop, as well as the position of the end
    /// of the loop.
    fn parse_loop(input: &[char], ret: Option<usize>) -> (Vec<Instr>, usize) {
        let mut output = Vec::new();
        let mut pos = ret.map(|x| x + 1).unwrap_or(0);

        loop {
            if pos >= input.len() {
                break;
            }

            match input[pos] {
                '+' => output.push(Instr::Incr),
                '-' => output.push(Instr::Decr),
                '>' => output.push(Instr::Next),
                '<' => output.push(Instr::Prev),
                '.' => output.push(Instr::Print),
                ',' => output.push(Instr::Read),
                '[' => {
                    let (inner_output, jump_loc) = Instr::parse_loop(input, Some(pos));

                    output.push(Instr::BeginLoop(jump_loc));
                    output.append(&mut inner_output.clone());

                    pos = jump_loc;
                }
                ']' => {
                    output.push(Instr::EndLoop(
                        // Will only panic if there are more closing braces than
                        // opening braces.
                        ret.unwrap_or_else(|| panic!("Stack underflow!")),
                    ));
                    return (output, pos);
                }
                _ => unreachable!(), // All comments should already be stripped
            }

            pos += 1;
        }

        (output, pos)
    }
}

impl fmt::Display for Instr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Instr::Incr => write!(f, "INC"),
            Instr::Decr => write!(f, "DEC"),
            Instr::Next => write!(f, "NEXT"),
            Instr::Prev => write!(f, "PREV"),
            Instr::Print => write!(f, "PRINT"),
            Instr::Read => write!(f, "READ"),
            Instr::BeginLoop(end_pos) => write!(f, "BEGIN\t0x{:04X}", end_pos),
            Instr::EndLoop(ret_pos) => write!(f, "END\t0x{:04X}", ret_pos),
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
                println!("{} seconds", end - start);
                println!("{:.2} ops/second", ops as f64 / (end - start) as f64);
                return;
            }

            let instr = self.program[self.pc];
            let current = self.memory[self.dp];

            match instr {
                Instr::Incr => {
                    self.memory[self.dp] = current.wrapping_add(1);
                }
                Instr::Decr => {
                    self.memory[self.dp] = current.wrapping_sub(1);
                }
                Instr::Next => {
                    self.dp += 1;
                }
                Instr::Prev => {
                    self.dp -= 1;
                }
                Instr::Print => {
                    print!("{}", char::from_u32(current as u32).unwrap_or('?'));
                    std::io::stdout().flush().unwrap();
                }
                Instr::Read => {
                    self.memory[self.dp] = unsafe { getchar() } as u8;
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

            ops += 1;
            self.pc += 1;
        }
    }
}
