use libc::getchar;
use std::char;
use std::io::Write;

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
        let control_chars = ['+', '-', '>', '<', '.', ',', '[', ']'];
        input
            .into_iter()
            .filter(|c| control_chars.iter().any(|x| x == c))
            .collect()
    }

    /// Parse a [ ... ] BrainFuck loop.
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
                _ => {} // All other characters are comments
            }

            pos += 1;
        }

        (output, pos)
    }
}

/// BrainFuck virtual machine
pub struct Fucker {
    program: Vec<Instr>,
    memory: [u8; 0x2000],
    pc: usize,
    sp: usize,
}

impl Fucker {
    pub fn new(program: Vec<Instr>) -> Self {
        Fucker {
            program: program,
            memory: [0; 0x2000],
            pc: 0,
            sp: 0,
        }
    }

    pub fn run(&mut self) {
        loop {
            if self.pc >= self.program.len() {
                return;
            }

            let instr = self.program[self.pc];
            let current = self.memory[self.sp];

            match instr {
                Instr::Incr => {
                    self.memory[self.sp] = current.wrapping_add(1);
                }
                Instr::Decr => {
                    self.memory[self.sp] = current.wrapping_sub(1);
                }
                Instr::Next => {
                    self.sp += 1;
                }
                Instr::Prev => {
                    self.sp -= 1;
                }
                Instr::Print => {
                    print!("{}", char::from_u32(current as u32).unwrap_or('?'));
                    std::io::stdout().flush();
                }
                Instr::Read => {
                    self.memory[self.sp] = unsafe { getchar() } as u8;
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
        }
    }
}
