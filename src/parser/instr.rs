use std::fmt;
use std::mem;

/// Functions called by JIT-compiled code.
mod jit_functions {
    use libc::getchar;
    use std::char;
    use std::io::Write;

    /// Print a single byte to stdout.
    pub fn print(byte: u8) {
        print!("{}", char::from_u32(byte as u32).unwrap_or('?'));
        std::io::stdout().flush().unwrap();
    }

    /// Read a single byte from stdin.
    pub fn read() -> u8 {
        unsafe { getchar() as u8 }
    }
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
    /// If the current memory cell is 0, jump forward by the contained offset.
    BeginLoop(usize),
    /// If the current memory cell is not 0, jump backward by the contained offset.
    EndLoop(usize),
}

impl Instr {
    /// Convert an ASTNode into a sequence of executable bytes.
    ///
    /// r10 is used to hold the data pointer.
    #[cfg(target_arch = "x86_64")]
    pub fn jit(&self) -> Vec<u8> {
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
                // HACK: Assumes usize won't be more than 32 bit...
                let n_bytes = (*n as u32).to_ne_bytes();

                // add    r10,n
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
                let print_ptr_bytes = to_ne_bytes!(jit_functions::print, fn(u8) -> ());

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
                let read_ptr_bytes = to_ne_bytes!(jit_functions::read, fn() -> u8);

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
            Instr::BeginLoop(offset) => {
                let begin_loop_size: i32 = 10; // Bytes

                let byte_offset = (*offset as i32) * BF_INSTR_SIZE - begin_loop_size;
                let offset_bytes = byte_offset.to_ne_bytes();

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
            Instr::EndLoop(offset) => {
                let end_loop_size: i32 = 10; // Bytes

                let byte_offset = -(*offset as i32) * BF_INSTR_SIZE - end_loop_size;
                let offset_bytes = byte_offset.to_ne_bytes();

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
        };

        while bytes.len() < BF_INSTR_SIZE as usize {
            // nop
            bytes.push(0x90);
        }

        bytes
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
            Instr::BeginLoop(end_pos) => write!(f, "BEGIN\t0x{:04X}", end_pos),
            Instr::EndLoop(ret_pos) => write!(f, "END\t0x{:04X}", ret_pos),
        }
    }
}
