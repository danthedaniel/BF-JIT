use std::fmt;

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
