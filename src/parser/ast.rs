use anyhow::{Result, bail};

use super::optimizer::optimize;

/// brainfuck AST node
///
/// All offsets are relative to the data pointer.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum AstNode {
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
    /// The high byte is the carry out of adding `step` to `base` `count` times.
    ProductAdd {
        base: Operand,
        step: Operand,
        count: Operand,
        high: bool,
        dst: i32,
        value: u8,
    },
    /// Divide the little-endian number in the `dividend_len` cells from
    /// `dividend` by the one in the `divisor_len` cells from `divisor`, unless
    /// that is zero. The remainder replaces the dividend, and `factor` times
    /// the quotient is added to the cell at `quotient`.
    DivMod {
        dividend: i32,
        dividend_len: u8,
        divisor: i32,
        divisor_len: u8,
        quotient: i32,
        factor: u8,
    },
    /// Shift the data pointer.
    Move(i32),
    /// Display a memory cell as an ASCII character.
    Print(i32),
    /// Read one character from stdin into a memory cell.
    Read(i32),
    /// Loop over the contained instructions while the current memory cell is
    /// not zero.
    Loop(Vec<AstNode>),
    /// Shift the data pointer by a stride until it points to a zero cell.
    Scan(i32),
    /// Execute a syscall (systemf extension).
    /// The syscall arguments are read from the tape starting at the current cell.
    Syscall,
}

/// The value `scale * cell + bias` (wrapping), or the constant `bias` if `scale` is 0.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Operand {
    pub cell: i32,
    pub scale: u8,
    pub bias: u8,
}

impl Operand {
    pub const fn constant(value: u8) -> Self {
        Self {
            cell: 0,
            scale: 0,
            bias: value,
        }
    }

    pub const fn cell(cell: i32) -> Self {
        Self {
            cell,
            scale: 1,
            bias: 0,
        }
    }

    /// The cell this operand reads, if any.
    pub const fn reads(self) -> Option<i32> {
        if self.scale == 0 {
            None
        } else {
            Some(self.cell)
        }
    }

    pub const fn shift(self, amount: i32) -> Self {
        if self.scale == 0 {
            return self;
        }
        Self {
            cell: self.cell + amount,
            ..self
        }
    }

    pub const fn eval(self, cell: u8) -> u8 {
        self.scale.wrapping_mul(cell).wrapping_add(self.bias)
    }
}

impl AstNode {
    /// Convert raw input into an optimized AST.
    ///
    /// If `enable_syscalls` is true, the `%` character will be parsed as a syscall instruction.
    pub fn parse(input: &str, enable_syscalls: bool) -> Result<Vec<AstNode>> {
        Self::parse_raw(input, enable_syscalls).map(|nodes| optimize(nodes, false))
    }

    /// Like `parse`, but keeps the final state of every cell rather than
    /// only what's observable.
    #[cfg(test)]
    pub fn parse_keeping_tape(input: &str) -> Result<Vec<AstNode>> {
        Self::parse_raw(input, false).map(|nodes| optimize(nodes, true))
    }

    fn parse_raw(input: &str, enable_syscalls: bool) -> Result<Vec<AstNode>> {
        let mut output = Vec::new();
        let mut loops: Vec<Vec<AstNode>> = Vec::new();

        let mut line = 1;
        let mut col = 0;

        for character in input.chars() {
            col += 1;

            let next_node = match character {
                '+' => AstNode::Add(0, 1),
                '-' => AstNode::Add(0, u8::MAX),
                '>' => AstNode::Move(1),
                '<' => AstNode::Move(-1),
                '.' => AstNode::Print(0),
                ',' => AstNode::Read(0),
                '%' if enable_syscalls => AstNode::Syscall,
                '[' => {
                    loops.push(Vec::new());
                    continue;
                }
                ']' => {
                    // Example program that will cause this error:
                    //
                    // []]
                    let body = loops.pop().ok_or_else(|| {
                        anyhow::anyhow!(format!("Line {line}:{col} - Unmatched ']' bracket"))
                    })?;
                    AstNode::Loop(body)
                }
                '\n' => {
                    line += 1;
                    col = 0;
                    continue;
                }
                // All other characters are comments and will be ignored
                _ => continue,
            };

            // Where to add the new node. First try to add to the innermost loop.
            // If there are no loops, then add to the top level output.
            loops.last_mut().unwrap_or(&mut output).push(next_node);
        }

        if !loops.is_empty() {
            // Example program that will cause this error:
            //
            // [[]
            bail!(format!("Line {line}:{col} - Unmatched '[' bracket"));
        }

        Ok(output)
    }
}

/// The largest distance from the data pointer at which a node accesses memory.
///
/// Optimized loops may access cells the original loop wouldn't have touched
/// (without changing them), so memory should be padded by this much.
pub fn max_offset(nodes: &[AstNode]) -> u32 {
    nodes
        .iter()
        .map(|node| match node {
            AstNode::Add(offset, _)
            | AstNode::Set(offset, _)
            | AstNode::Print(offset)
            | AstNode::Read(offset) => offset.unsigned_abs(),
            AstNode::MulAdd { src, dst, .. } => src.unsigned_abs().max(dst.unsigned_abs()),
            AstNode::CondAdd { lhs, rhs, dst, .. } => lhs
                .cell
                .unsigned_abs()
                .max(rhs.cell.unsigned_abs())
                .max(dst.unsigned_abs()),
            AstNode::ProductAdd {
                base,
                step,
                count,
                dst,
                ..
            } => [base.cell, step.cell, count.cell, *dst]
                .into_iter()
                .map(i32::unsigned_abs)
                .max()
                .unwrap(),
            AstNode::DivMod {
                dividend,
                dividend_len,
                divisor,
                divisor_len,
                quotient,
                ..
            } => [
                *dividend,
                dividend + i32::from(*dividend_len) - 1,
                *divisor,
                divisor + i32::from(*divisor_len) - 1,
                *quotient,
            ]
            .into_iter()
            .map(i32::unsigned_abs)
            .max()
            .unwrap(),
            AstNode::Loop(body) => max_offset(body),
            AstNode::Move(_) | AstNode::Scan(_) | AstNode::Syscall => 0,
        })
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Vec<AstNode> {
        AstNode::parse_keeping_tape(input).unwrap()
    }

    #[test]
    fn too_many_loop_begins() {
        let ast = AstNode::parse("[[]", false);
        assert!(ast.is_err());
    }

    #[test]
    fn too_many_loop_ends() {
        let ast = AstNode::parse("[]]", false);
        assert!(ast.is_err());
    }

    #[test]
    fn run_length_encode() {
        assert_eq!(parse(",+++++"), [AstNode::Read(0), AstNode::Add(0, 5)]);
    }

    #[test]
    fn known_zero_start() {
        assert_eq!(parse("+++++"), [AstNode::Set(0, 5)]);
    }

    #[test]
    fn simplify_to_set() {
        assert_eq!(parse(",[-]+++"), [AstNode::Read(0), AstNode::Set(0, 3)]);
    }

    #[test]
    fn simplify_to_add() {
        assert_eq!(
            parse(",[->+<]"),
            [
                AstNode::Read(0),
                AstNode::Set(1, 0),
                AstNode::MulAdd {
                    src: 0,
                    dst: 1,
                    factor: 1
                },
                AstNode::Set(0, 0),
            ]
        );
    }

    #[test]
    fn simplify_to_sub() {
        assert_eq!(
            parse(",[->-<]"),
            [
                AstNode::Read(0),
                AstNode::Set(1, 0),
                AstNode::MulAdd {
                    src: 0,
                    dst: 1,
                    factor: 255
                },
                AstNode::Set(0, 0),
            ]
        );
    }

    #[test]
    fn simplify_odd_step() {
        // Increments of 3 reach zero after `x * inverse(-3)` iterations.
        assert_eq!(
            parse(",[+++>+<]"),
            [
                AstNode::Read(0),
                AstNode::Set(1, 0),
                AstNode::MulAdd {
                    src: 0,
                    dst: 1,
                    factor: 85
                },
                AstNode::Set(0, 0),
            ]
        );
    }

    #[test]
    fn removes_dead_loops() {
        assert_eq!(parse("[-]"), []);
        assert_eq!(
            parse(",[.,][.]"),
            [
                AstNode::Read(0),
                AstNode::Loop(vec![AstNode::Print(0), AstNode::Read(0)]),
            ]
        );
    }

    #[test]
    fn simplify_to_multiply() {
        assert_eq!(
            parse(",[->>+++<<]"),
            [
                AstNode::Read(0),
                AstNode::Set(2, 0),
                AstNode::MulAdd {
                    src: 0,
                    dst: 2,
                    factor: 3
                },
                AstNode::Set(0, 0),
            ]
        );
    }

    #[test]
    fn simplify_to_scan() {
        assert_eq!(parse(",[>>>]"), [AstNode::Read(0), AstNode::Scan(3)]);
    }

    #[test]
    fn affine_block() {
        // Copy cell 0 to cells 2 and 3, then move cell 1 into cell 0.
        let copy = |src, dst| AstNode::MulAdd {
            src,
            dst,
            factor: 1,
        };
        assert_eq!(
            parse(",>,<[->>+>+<<<]>[-<+>]<"),
            [
                AstNode::Read(0),
                AstNode::Read(1),
                AstNode::Set(2, 0),
                copy(0, 2),
                AstNode::Set(3, 0),
                copy(0, 3),
                AstNode::Set(0, 0),
                copy(1, 0),
                AstNode::Set(1, 0),
            ]
        );
    }

    #[test]
    fn dead_code_elimination() {
        assert_eq!(parse(",+-"), [AstNode::Read(0)]);
        assert_eq!(parse("><"), []);
        assert_eq!(parse(",+++--"), [AstNode::Read(0), AstNode::Add(0, 1)]);
        assert_eq!(parse(",++---"), [AstNode::Read(0), AstNode::Add(0, 255)]);
    }

    #[test]
    fn parses_rot13() {
        let ast = AstNode::parse(include_str!("../../tests/programs/rot13-16char.bf"), false);
        assert!(ast.is_ok());
    }

    #[test]
    fn parses_mandelbrot() {
        let ast = AstNode::parse(include_str!("../../tests/programs/mandelbrot.bf"), false);
        assert!(ast.is_ok());
    }

    #[test]
    fn syscall_ignored_when_disabled() {
        // % should be treated as a comment when syscalls are disabled
        assert_eq!(parse(",+%+"), [AstNode::Read(0), AstNode::Add(0, 2)]);
    }

    #[test]
    fn syscall_parsed_when_enabled() {
        let ast = AstNode::parse(",+%+.", true).unwrap();
        assert_eq!(
            ast,
            [
                AstNode::Read(0),
                AstNode::Add(0, 1),
                AstNode::Syscall,
                AstNode::Add(0, 1),
                AstNode::Print(0),
            ]
        );
    }
}
