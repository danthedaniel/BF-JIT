/// BrainFuck instruction
#[derive(Copy, Clone, Debug)]
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
    /// Set a value for the current cell.
    Set(u8),
    /// Add the current cell to the cell n spaces away and set the current cell to 0.
    Add(isize),
    /// Subtract the current cell from the cell n spaces away and set the current cell to 0.
    Sub(isize),
    /// If the current memory cell is 0, jump forward by the contained offset.
    BeginLoop(usize),
    /// If the current memory cell is not 0, jump backward by the contained offset.
    EndLoop(usize),
}
