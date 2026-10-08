use crate::parser::Operand;

/// brainfuck instruction
///
/// All offsets are relative to the data pointer.
#[derive(Clone, Debug)]
pub enum Instr {
    /// Add to a memory cell (wrapping).
    Add(i32, u8),
    /// Set a memory cell to a literal value.
    Set(i32, u8),
    /// Add the cell at `src` multiplied by `factor` to the cell at `dst`.
    MulAdd { src: i32, dst: i32, factor: u8 },
    /// Add `value` to the cell at `dst` if `lhs < rhs`.
    CondAdd {
        lhs: Operand,
        rhs: Operand,
        dst: i32,
        value: u8,
    },
    /// Add `value` times a byte of `base + step * count` to the cell at `dst`.
    ProductAdd {
        base: Operand,
        step: Operand,
        count: Operand,
        high: bool,
        dst: i32,
        value: u8,
    },
    /// Divide a multi-byte number by another, keeping the remainder and
    /// adding `factor` times the quotient to a cell.
    DivMod {
        dividend: i32,
        dividend_len: u8,
        divisor: i32,
        divisor_len: u8,
        quotient: i32,
        factor: u8,
    },
    /// Step cells by the number of steps until one reaches a target.
    Skip {
        exits: Box<[(i32, u8, u8)]>,
        steps: Box<[(i32, u8)]>,
    },
    /// Shift the data pointer.
    Move(i32),
    /// Display a memory cell as an ASCII character.
    Print(i32),
    /// Read one character from stdin into a memory cell.
    Read(i32),
    /// Shift the data pointer by a stride until it points to a zero cell.
    Scan(i32),
    /// If the current memory cell is 0, jump forward by the contained offset.
    BeginLoop(usize),
    /// If the current memory cell is not 0, jump backward by the contained offset.
    EndLoop(usize),
    /// Execute a syscall (systemf extension).
    Syscall,
}
