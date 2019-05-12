use std::char;
use std::cmp;
use std::fmt;
use std::io::Write;
use std::mem;
use std::ops::Index;

use libc::getchar;

use jit_memory::JITMemory;
use runnable::Runnable;

/// Print a single byte to stdout.
fn print(byte: u8) {
    print!("{}", char::from_u32(byte as u32).unwrap_or('?'));
    std::io::stdout().flush().unwrap();
}

/// Read a single byte from stdin.
fn read() -> u8 {
    unsafe { getchar() as u8 }
}

/// Convert an expression to a native-endian order byte array after a type cast.
macro_rules! to_ne_bytes {
    ($ptr:expr, $ptr_type:ty) => {{
        let bytes: [u8; mem::size_of::<$ptr_type>()] = unsafe { mem::transmute($ptr as $ptr_type) };
        bytes
    }};
}

/// BrainFuck instructions must all have the same size to simplify jumping.
///
/// This represents the max size a BrainFuck instruction could represent in the
/// target architecture. If an instruction uses less than this many bytes it
/// should be padded with NOPs.
#[cfg(target_arch = "x86_64")]
const BF_INSTR_SIZE: i32 = 22;

#[cfg(not(target_arch = "x86_64"))]
const BF_INSTR_SIZE: i32 = 0;

/// BrainFuck instruction
#[derive(Copy, Clone)]
pub enum Instr {
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
    /// If the current memory cell is 0, jump to the matching EndLoop (whos
    /// index is held inside).
    BeginLoop(Option<usize>),
    /// If the current memory cell is not 0, jump to the matching BeginLoop.
    EndLoop(Option<usize>),
}

impl Instr {
    /// Convert an Instr into a sequence of executable bytes.
    ///
    /// r10 is used to hold the data pointer.
    #[cfg(target_arch = "x86_64")]
    pub fn jit(&self, this_pos: usize) -> Result<Vec<u8>, String> {
        let mut bytes: Vec<u8> = Vec::new();

        match self {
            Instr::Incr(n) => {
                // add    BYTE PTR [r10],n
                bytes.push(0x41);
                bytes.push(0x80);
                bytes.push(0x02);
                bytes.push(*n);
            }
            Instr::Decr(n) => {
                // sub    BYTE PTR [r10],n
                bytes.push(0x41);
                bytes.push(0x80);
                bytes.push(0x2a);
                bytes.push(*n);
            }
            Instr::Next(n) => {
                // add    r10,n

                // HACK: Assumes usize won't be more than 32 bit...
                let n_bytes = (*n as u32).to_ne_bytes();

                bytes.push(0x49);
                bytes.push(0x81);
                bytes.push(0xc2);
                bytes.push(n_bytes[0]);
                bytes.push(n_bytes[1]);
                bytes.push(n_bytes[2]);
                bytes.push(n_bytes[3]);
            }
            Instr::Prev(n) => {
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
            Instr::Print => {
                let print_ptr_bytes = to_ne_bytes!(print, fn(u8) -> ());

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
            Instr::Read => {
                let read_ptr_bytes = to_ne_bytes!(read, fn() -> u8);

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
            Instr::BeginLoop(Some(pos)) => {
                let begin_loop_size: i32 = 10; // Bytes

                let offset = (*pos as i32 - this_pos as i32) * BF_INSTR_SIZE - begin_loop_size;
                let offset_bytes = offset.to_ne_bytes();

                // Check if the current memory cell equals zero.
                // cmp    BYTE PTR [r10],0x0
                bytes.push(0x41);
                bytes.push(0x80);
                bytes.push(0x3a);
                bytes.push(0x00);

                // Jump to the end of the loop if equal.
                // je    offset
                bytes.push(0x0f);
                bytes.push(0x84);
                bytes.push(offset_bytes[0]);
                bytes.push(offset_bytes[1]);
                bytes.push(offset_bytes[2]);
                bytes.push(offset_bytes[3]);
            }
            Instr::EndLoop(Some(pos)) => {
                let end_loop_size: i32 = 10; // Bytes

                let offset = (*pos as i32 - this_pos as i32) * BF_INSTR_SIZE - end_loop_size;
                let offset_bytes = offset.to_ne_bytes();

                // Check if the current memory cell equals zero.
                // cmp    BYTE PTR [r10],0x0
                bytes.push(0x41);
                bytes.push(0x80);
                bytes.push(0x3a);
                bytes.push(0x00);

                // Jump back to the beginning of the loop if not equal.
                // jne    offset
                bytes.push(0x0f);
                bytes.push(0x85);
                bytes.push(offset_bytes[0]);
                bytes.push(offset_bytes[1]);
                bytes.push(offset_bytes[2]);
                bytes.push(offset_bytes[3]);
            }
            _ => Err(format!("Can not JIT {:?}", self))?,
        };

        while bytes.len() < BF_INSTR_SIZE as usize {
            // nop
            bytes.push(0x90);
        }

        Ok(bytes)
    }
}

/// Display Instr similar to assembly.
impl fmt::Debug for Instr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Instr::Incr(1) => write!(f, "INC"),
            Instr::Incr(n) => write!(f, "ADD\t0x{:04X}", n),
            Instr::Decr(1) => write!(f, "DEC"),
            Instr::Decr(n) => write!(f, "SUB\t0x{:04X}", n),
            Instr::Next(1) => write!(f, "NEXT"),
            Instr::Next(n) => write!(f, "NEXT\t0x{:04X}", n),
            Instr::Prev(1) => write!(f, "PREV"),
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

#[derive(Clone)]
pub struct Program {
    pub data: Vec<Instr>,
}

impl Program {
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Initialize a JIT compiled version of this program.
    #[cfg(target_arch = "x86_64")]
    pub fn jit(&self) -> Result<Box<Runnable>, String> {
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

        for (index, instr) in self.data.iter().enumerate() {
            bytes.extend(instr.jit(index)?);
        }

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
    pub fn jit(&self) -> Option<Box<Runnable>> {
        Err("Unsupported JIT architecture.")
    }

    /// Initialize a BrainFuck interpreter that will use this program.
    pub fn int(&self) -> Box<Runnable> {
        Box::new(Fucker::new(self.clone()))
    }

    /// Convert a character slice into a Vec of Instr.
    pub fn parse(input: Vec<char>) -> Result<Program, String> {
        if input.len() == 0 {
            return Ok(Program { data: Vec::new() });
        }

        let comment_block_end = Program::code_start(input.clone());

        let optimized = Program::optimize(input[comment_block_end..].to_vec());
        let data = Program::parse_loops(optimized)?;

        Ok(Program { data: data })
    }

    /// This returns the index first non-comment character in the program.
    /// Many BrainFuck programs use a starting loop as a comment block.
    fn code_start(input: Vec<char>) -> usize {
        // If the code begins with a loop, treat it as a comment.
        if input[0] == '[' {
            // End of comment block is one character past the first `]`
            input
                .iter()
                .position(|&c| c == ']')
                .map(|x| x + 1)
                .unwrap_or(0)
        } else {
            0 // No starting comment block
        }
    }

    /// Convert runs of +, -, < and > into bulk operations.
    fn optimize(input: Vec<char>) -> Vec<Instr> {
        let mut output: Vec<Instr> = Vec::new();

        for c in input {
            let len = output.len();
            let last = output.get(len.checked_sub(1).unwrap_or(0)).map(|&x| x);

            // For each operator +, -, < and >, if the last instruction in the
            // output Vec is the same, then increment that instruction instead
            // of adding another identical instruction.
            //
            // All other operators are just appended to the Vec.
            //
            // Loop jump positions are left un-calculated, to be determined
            // later.
            match (c, last) {
                // Incr
                ('+', Some(Instr::Incr(n))) => {
                    output[len - 1] = Instr::Incr(n.wrapping_add(1));
                }
                ('+', _) => output.push(Instr::Incr(1)),
                // Decr
                ('-', Some(Instr::Decr(n))) => {
                    output[len - 1] = Instr::Decr(n.wrapping_add(1));
                }
                ('-', _) => output.push(Instr::Decr(1)),
                // Next
                ('>', Some(Instr::Next(n))) => {
                    output[len - 1] = Instr::Next(n.wrapping_add(1));
                }
                ('>', _) => output.push(Instr::Next(1)),
                // Prev
                ('<', Some(Instr::Prev(n))) => {
                    output[len - 1] = Instr::Prev(n.wrapping_add(1));
                }
                ('<', _) => output.push(Instr::Prev(1)),
                // Other
                ('.', _) => output.push(Instr::Print),
                (',', _) => output.push(Instr::Read),
                ('[', _) => output.push(Instr::BeginLoop(None)),
                (']', _) => output.push(Instr::EndLoop(None)),
                // All other characters are comments and will be ignored.
                (_, _) => {}
            }
        }

        output
    }

    /// Resolve loop jump positions.
    fn parse_loops(input: Vec<Instr>) -> Result<Vec<Instr>, String> {
        let mut output = input.clone();
        let mut stack: Vec<usize> = Vec::new();

        for (pos, instr) in input.iter().enumerate() {
            match instr {
                Instr::BeginLoop(None) => stack.push(pos),
                Instr::EndLoop(None) => {
                    let ret_pos = stack.pop().ok_or(format!("More ] than ["))?;
                    output[pos] = Instr::EndLoop(Some(ret_pos));
                    output[ret_pos] = Instr::BeginLoop(Some(pos));
                }
                _ => {}
            }
        }

        if stack.len() > 0 {
            return Err(format!("More [ than ]"));
        }

        Ok(output)
    }
}

impl Index<usize> for Program {
    type Output = Instr;

    fn index(&self, index: usize) -> &Instr {
        &self.data[index]
    }
}

impl fmt::Debug for Program {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Addr\tInstr\tOperands\n")?;

        for (pos, instr) in self.data.iter().enumerate() {
            write!(f, "0x{:04X}\t{:?}\n", pos, instr)?;
        }

        write!(f, "\n")
    }
}

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
            _ => {
                eprintln!("Can not execute {:?}", instr);
                return false;
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
