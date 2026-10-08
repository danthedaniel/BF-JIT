use crate::parser::{AstNode, Operand};
use crate::runnable::jit::executable_memory::VTableEntry;
use crate::runnable::jit::jit_promise::JITPromiseID;

pub const RET: u32 = 0xd65f_03c0;
const PTR_SIZE: u32 = 8;

// Register usage:
// x19 - BrainFuck memory pointer (current cell, callee-saved)
// x20 - JITTarget pointer (callee-saved)
// x21 - VTable pointer (callee-saved)
// x22 - BrainFuck memory base pointer (for syscalls, callee-saved)
// x0-x7, x9-x15 - Cached memory cells within straight-line code
// x8 - Temporary register
// x16, x17 - Address calculation and temporary registers
// x29 - Frame pointer
// x30 - Link register

const MEM: u32 = 19;
const TMP: u32 = 8;
const ADDR: u32 = 16;
/// wzr when used as a store source
const ZERO: u32 = 31;
const CACHE_REGS: [u32; 15] = [0, 1, 2, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 15];

fn emit_u32(bytes: &mut Vec<u8>, instruction: u32) {
    bytes.extend_from_slice(&instruction.to_le_bytes());
}

#[allow(clippy::cast_sign_loss)]
const fn bits(value: i32, width: u32) -> u32 {
    (value as u32) & ((1 << width) - 1)
}

/// Load a signed 32-bit value into x`rd`.
fn mov_imm(bytes: &mut Vec<u8>, rd: u32, value: i32) {
    if (0..=0xffff).contains(&value) {
        // movz xd, #value
        emit_u32(bytes, 0xd280_0000 | (bits(value, 16) << 5) | rd);
    } else if (-0x1_0000..0).contains(&value) {
        // movn xd, #!value
        emit_u32(bytes, 0x9280_0000 | (bits(!value, 16) << 5) | rd);
    } else {
        // movz wd, #low
        emit_u32(bytes, 0x5280_0000 | (bits(value, 16) << 5) | rd);
        // movk wd, #high, lsl #16
        emit_u32(bytes, 0x72a0_0000 | (bits(value >> 16, 16) << 5) | rd);
        // sxtw xd, wd
        emit_u32(bytes, 0x9340_7c00 | (rd << 5) | rd);
    }
}

/// Load (`ldrb`) or store (`strb`) w`rt` at x19 + offset.
fn mem_byte(bytes: &mut Vec<u8>, load: bool, rt: u32, offset: i32) {
    if (0..4096).contains(&offset) {
        // ldrb/strb wt, [x19, #offset]
        let op = if load { 0x3940_0000 } else { 0x3900_0000 };
        emit_u32(bytes, op | (bits(offset, 12) << 10) | (MEM << 5) | rt);
    } else if (-256..0).contains(&offset) {
        // ldurb/sturb wt, [x19, #offset]
        let op = if load { 0x3840_0000 } else { 0x3800_0000 };
        emit_u32(bytes, op | (bits(offset, 9) << 12) | (MEM << 5) | rt);
    } else {
        mov_imm(bytes, ADDR, offset);
        // ldrb/strb wt, [x19, x16]
        let op = if load { 0x3860_6800 } else { 0x3820_6800 };
        emit_u32(bytes, op | (ADDR << 16) | (MEM << 5) | rt);
    }
}

fn load_byte(bytes: &mut Vec<u8>, rt: u32, offset: i32) {
    mem_byte(bytes, true, rt, offset);
}

fn store_byte(bytes: &mut Vec<u8>, rt: u32, offset: i32) {
    mem_byte(bytes, false, rt, offset);
}

fn callee_save_to_stack(bytes: &mut Vec<u8>) {
    // Save callee-saved registers and link register
    // stp x29, x30, [sp, #-16]!
    emit_u32(bytes, 0xa9bf_7bfd);

    // stp x19, x20, [sp, #-16]!
    emit_u32(bytes, 0xa9bf_53f3);

    // stp x21, x22, [sp, #-16]!
    emit_u32(bytes, 0xa9bf_5bf5);

    // mov x29, sp (set frame pointer)
    emit_u32(bytes, 0x9100_03fd);
}

pub fn wrapper(bytes: &mut Vec<u8>, content: Vec<u8>) {
    callee_save_to_stack(bytes);

    // Store pointer to brainfuck memory (first argument x0) in x19
    // mov x19, x0
    emit_u32(bytes, 0xaa00_03f3);

    // Also store base memory pointer in x22 for syscalls
    // mov x22, x0
    emit_u32(bytes, 0xaa00_03f6);

    // Store pointer to JITTarget (second argument x1) in x20
    // mov x20, x1
    emit_u32(bytes, 0xaa01_03f4);

    // Store pointer to vtable (third argument x2) in x21
    // mov x21, x2
    emit_u32(bytes, 0xaa02_03f5);

    bytes.extend(content);

    // Return the data pointer
    // mov x0, x19
    emit_u32(bytes, 0xaa13_03e0);

    callee_restore_from_stack(bytes);

    // ret
    emit_u32(bytes, RET);
}

/// Wrapper for JIT fragments (deferred loops).
/// Unlike `wrapper`, this does NOT set x22 (base memory pointer) because
/// fragments are called from within the main program and should inherit
/// the base pointer from the caller.
pub fn wrapper_fragment(bytes: &mut Vec<u8>, content: Vec<u8>) {
    callee_save_to_stack(bytes);

    // Store pointer to brainfuck memory (first argument x0) in x19
    // mov x19, x0
    emit_u32(bytes, 0xaa00_03f3);

    // NOTE: We do NOT set x22 here - fragments inherit x22 from caller

    // Store pointer to JITTarget (second argument x1) in x20
    // mov x20, x1
    emit_u32(bytes, 0xaa01_03f4);

    // Store pointer to vtable (third argument x2) in x21
    // mov x21, x2
    emit_u32(bytes, 0xaa02_03f5);

    bytes.extend(content);

    // Return the data pointer
    // mov x0, x19
    emit_u32(bytes, 0xaa13_03e0);

    callee_restore_from_stack(bytes);

    // ret
    emit_u32(bytes, RET);
}

fn callee_restore_from_stack(bytes: &mut Vec<u8>) {
    // Restore callee-saved registers
    // ldp x21, x22, [sp], #16
    emit_u32(bytes, 0xa8c1_5bf5);

    // ldp x19, x20, [sp], #16
    emit_u32(bytes, 0xa8c1_53f3);

    // ldp x29, x30, [sp], #16
    emit_u32(bytes, 0xa8c1_7bfd);
}

/// A memory cell held in a register.
#[derive(Clone, Copy)]
struct Entry {
    offset: i32,
    reg: u32,
    /// Whether memory is out of date
    dirty: bool,
    /// The cell's value if it's a known constant
    constant: Option<u8>,
    /// Whether the register holds the value. Constants are only moved into
    /// registers when needed.
    materialized: bool,
}

/// Memory cells held in registers during straight-line code.
#[derive(Default)]
struct CellCache {
    /// Least recently used first
    entries: Vec<Entry>,
}

impl CellCache {
    /// Find or allocate the entry for a cell, loading it from memory if `load`
    /// is set. Returns its index, which stays valid until the next lookup.
    fn entry(&mut self, bytes: &mut Vec<u8>, offset: i32, load: bool) -> usize {
        if let Some(index) = self.entries.iter().position(|entry| entry.offset == offset) {
            let entry = self.entries.remove(index);
            self.entries.push(entry);
            return self.entries.len() - 1;
        }

        let reg = if self.entries.len() < CACHE_REGS.len() {
            CACHE_REGS[self.entries.len()]
        } else {
            let evicted = self.entries.remove(0);
            Self::write_back(bytes, evicted);
            evicted.reg
        };

        if load {
            load_byte(bytes, reg, offset);
        }
        self.entries.push(Entry {
            offset,
            reg,
            dirty: false,
            constant: None,
            materialized: load,
        });
        self.entries.len() - 1
    }

    /// The known constant value of a cell, without loading it.
    fn constant(&self, offset: i32) -> Option<u8> {
        self.entries
            .iter()
            .find(|entry| entry.offset == offset)
            .and_then(|entry| entry.constant)
    }

    /// Get a register holding the value of a cell.
    fn read(&mut self, bytes: &mut Vec<u8>, offset: i32) -> u32 {
        let index = self.entry(bytes, offset, true);
        let entry = &mut self.entries[index];
        if !entry.materialized {
            // movz wreg, #constant
            emit_u32(
                bytes,
                0x5280_0000 | (u32::from(entry.constant.unwrap()) << 5) | entry.reg,
            );
            entry.materialized = true;
        }
        entry.reg
    }

    /// Get a register holding the value of a cell which is about to be modified.
    fn modify(&mut self, bytes: &mut Vec<u8>, offset: i32) -> u32 {
        let reg = self.read(bytes, offset);
        let entry = self.entries.last_mut().unwrap();
        entry.dirty = true;
        entry.constant = None;
        reg
    }

    /// Get a register for a cell which is about to be overwritten.
    fn overwrite(&mut self, bytes: &mut Vec<u8>, offset: i32) -> u32 {
        let index = self.entry(bytes, offset, false);
        let entry = &mut self.entries[index];
        entry.dirty = true;
        entry.constant = None;
        entry.materialized = true;
        entry.reg
    }

    fn set(&mut self, bytes: &mut Vec<u8>, offset: i32, value: u8) {
        let index = self.entry(bytes, offset, false);
        let entry = &mut self.entries[index];
        entry.dirty = true;
        entry.constant = Some(value);
        entry.materialized = false;
    }

    fn write_back(bytes: &mut Vec<u8>, entry: Entry) {
        if !entry.dirty {
            return;
        }
        match entry.constant {
            Some(0) if !entry.materialized => store_byte(bytes, ZERO, entry.offset),
            Some(value) if !entry.materialized => {
                // movz wreg, #value
                emit_u32(bytes, 0x5280_0000 | (u32::from(value) << 5) | entry.reg);
                store_byte(bytes, entry.reg, entry.offset);
            }
            _ => store_byte(bytes, entry.reg, entry.offset),
        }
    }

    fn flush(self, bytes: &mut Vec<u8>) {
        for entry in self.entries {
            Self::write_back(bytes, entry);
        }
    }

    /// Replace cells with known values by constants.
    fn fold(&self, operand: Operand) -> Operand {
        match operand.reads().and_then(|cell| self.constant(cell)) {
            Some(value) => Operand::constant(operand.eval(value)),
            None => operand,
        }
    }

    /// Compile a straight-line node.
    fn node(&mut self, bytes: &mut Vec<u8>, node: &AstNode) {
        match *node {
            AstNode::Add(offset, value) => {
                if let Some(constant) = self.constant(offset) {
                    self.set(bytes, offset, constant.wrapping_add(value));
                } else {
                    let reg = self.modify(bytes, offset);
                    // add wd, wd, #value
                    emit_u32(
                        bytes,
                        0x1100_0000 | (u32::from(value) << 10) | (reg << 5) | reg,
                    );
                }
            }
            AstNode::Set(offset, value) => self.set(bytes, offset, value),
            AstNode::MulAdd { src, dst, factor } => {
                if let Some(constant) = self.constant(src) {
                    self.node(bytes, &AstNode::Add(dst, constant.wrapping_mul(factor)));
                } else if src != dst && self.constant(dst) == Some(0) {
                    let src_reg = self.read(bytes, src);
                    let dst_reg = self.overwrite(bytes, dst);
                    multiply(bytes, src_reg, dst_reg, factor);
                } else {
                    let src_reg = self.read(bytes, src);
                    let dst_reg = self.modify(bytes, dst);
                    multiply_add(bytes, src_reg, dst_reg, factor);
                }
            }
            AstNode::CondAdd {
                lhs,
                rhs,
                dst,
                value,
            } => {
                let (lhs, rhs) = (self.fold(lhs), self.fold(rhs));
                let lhs_reg = lhs.reads().map(|cell| self.read(bytes, cell));
                let rhs_reg = rhs.reads().map(|cell| self.read(bytes, cell));
                let dst_reg = self.modify(bytes, dst);
                conditional_add(bytes, (lhs, lhs_reg), (rhs, rhs_reg), dst_reg, value);
            }
            AstNode::ProductAdd {
                base,
                step,
                count,
                high,
                dst,
                value,
            } => {
                let operands = [base, step, count].map(|operand| {
                    let operand = self.fold(operand);
                    (operand, operand.reads().map(|cell| self.read(bytes, cell)))
                });
                let dst_reg = self.modify(bytes, dst);
                product_add(bytes, operands, high, dst_reg, value);
            }
            _ => unreachable!("not a straight-line node: {node:?}"),
        }
    }
}

/// Compile a run of straight-line nodes. Cells are kept in registers and
/// written back at the end.
pub fn straight_line(bytes: &mut Vec<u8>, nodes: &[AstNode]) {
    let mut cache = CellCache::default();
    for node in nodes {
        cache.node(bytes, node);
    }
    cache.flush(bytes);
}

/// Compile a loop whose body is straight-line code which doesn't move the
/// data pointer, keeping all cells in registers across iterations. Returns
/// false if there are too many cells.
pub fn register_loop(bytes: &mut Vec<u8>, nodes: &[AstNode]) -> bool {
    let mut cells = vec![0];
    for node in nodes {
        match *node {
            AstNode::Add(offset, _) | AstNode::Set(offset, _) => cells.push(offset),
            AstNode::MulAdd { src, dst, .. } => cells.extend([src, dst]),
            AstNode::CondAdd { lhs, rhs, dst, .. } => {
                cells.extend(lhs.reads().into_iter().chain(rhs.reads()).chain([dst]));
            }
            AstNode::ProductAdd {
                base,
                step,
                count,
                dst,
                ..
            } => cells.extend(
                [base, step, count]
                    .iter()
                    .filter_map(|o| o.reads())
                    .chain([dst]),
            ),
            _ => unreachable!("not a straight-line node: {node:?}"),
        }
    }
    cells.sort_unstable();
    cells.dedup();
    if cells.len() > CACHE_REGS.len() {
        return false;
    }

    let mut cache = CellCache::default();
    for &cell in &cells {
        cache.read(bytes, cell);
    }
    let counter = cache.read(bytes, 0);

    let mut body = Vec::new();
    for node in nodes {
        cache.node(&mut body, node);
    }
    // Every cell must be in its register at the end of each iteration.
    for &cell in &cells {
        cache.read(&mut body, cell);
    }
    // tst wcounter, #0xff
    emit_u32(&mut body, 0x7200_1c1f | (counter << 5));

    let body_len = i32::try_from(body.len() / 4).unwrap();
    // tst wcounter, #0xff
    emit_u32(bytes, 0x7200_1c1f | (counter << 5));
    // b.eq end
    emit_u32(bytes, 0x5400_0000 | (bits(body_len + 2, 19) << 5));
    bytes.extend(body);
    // b.ne start
    emit_u32(bytes, 0x5400_0001 | (bits(-body_len, 19) << 5));

    cache.flush(bytes);
    true
}

/// wd = wn * factor
fn multiply(bytes: &mut Vec<u8>, rn: u32, rd: u32, factor: u8) {
    if factor == u8::MAX {
        // neg wd, wn
        emit_u32(bytes, 0x4b00_03e0 | (rn << 16) | rd);
    } else if factor.is_power_of_two() {
        // lsl wd, wn, #shift  (ubfm wd, wn, #(32 - shift) % 32, #(31 - shift))
        let shift = factor.trailing_zeros();
        let rotation = (32 - shift) % 32;
        let width = 31 - shift;
        emit_u32(
            bytes,
            0x5300_0000 | (rotation << 16) | (width << 10) | (rn << 5) | rd,
        );
    } else {
        // movz w8, #factor
        emit_u32(bytes, 0x5280_0000 | (u32::from(factor) << 5) | TMP);
        // mul wd, wn, w8
        emit_u32(bytes, 0x1b00_7c00 | (TMP << 16) | (rn << 5) | rd);
    }
}

/// wd += wn * factor
fn multiply_add(bytes: &mut Vec<u8>, rn: u32, rd: u32, factor: u8) {
    if rn == rd && factor == u8::MAX - 1 {
        // wd * 255 == -wd
        // neg wd, wd
        emit_u32(bytes, 0x4b00_03e0 | (rd << 16) | rd);
    } else if factor == u8::MAX {
        // sub wd, wd, wn
        emit_u32(bytes, 0x4b00_0000 | (rn << 16) | (rd << 5) | rd);
    } else if factor.is_power_of_two() {
        // add wd, wd, wn, lsl #shift
        let shift = factor.trailing_zeros();
        emit_u32(
            bytes,
            0x0b00_0000 | (rn << 16) | (shift << 10) | (rd << 5) | rd,
        );
    } else {
        // movz w8, #factor
        emit_u32(bytes, 0x5280_0000 | (u32::from(factor) << 5) | TMP);
        // madd wd, wn, w8, wd
        emit_u32(
            bytes,
            0x1b00_0000 | (TMP << 16) | (rd << 10) | (rn << 5) | rd,
        );
    }
}

// Condition codes
const COND_NE: u32 = 0b0001;
const COND_LO: u32 = 0b0011;
const COND_HI: u32 = 0b1000;

/// Compute the byte value of an operand held in register `reg` into `tmp`.
fn materialize(bytes: &mut Vec<u8>, operand: Operand, reg: u32, tmp: u32) {
    let mut src = reg;
    if operand.scale != 1 {
        // movz wtmp, #scale
        emit_u32(bytes, 0x5280_0000 | (u32::from(operand.scale) << 5) | tmp);
        // mul wtmp, wsrc, wtmp
        emit_u32(bytes, 0x1b00_7c00 | (tmp << 16) | (src << 5) | tmp);
        src = tmp;
    }
    if operand.bias != 0 {
        // add wtmp, wsrc, #bias
        emit_u32(
            bytes,
            0x1100_0000 | (u32::from(operand.bias) << 10) | (src << 5) | tmp,
        );
        src = tmp;
    }
    // and wtmp, wsrc, #0xff
    emit_u32(bytes, 0x1200_1c00 | (src << 5) | tmp);
}

/// wd += value if lhs < rhs. Operands which read a cell come with its register.
fn conditional_add(
    bytes: &mut Vec<u8>,
    lhs: (Operand, Option<u32>),
    rhs: (Operand, Option<u32>),
    rd: u32,
    value: u8,
) {
    let cond = match (lhs, rhs) {
        ((l, None), (r, None)) => {
            if l.bias < r.bias {
                // add wd, wd, #value
                emit_u32(
                    bytes,
                    0x1100_0000 | (u32::from(value) << 10) | (rd << 5) | rd,
                );
            }
            return;
        }
        ((l, None), (r, Some(reg))) if l.bias == 0 && r.scale == 1 && r.bias == 0 => {
            // tst wreg, #0xff
            emit_u32(bytes, 0x7200_1c1f | (reg << 5));
            COND_NE
        }
        ((l, None), (r, Some(reg))) => {
            materialize(bytes, r, reg, ADDR);
            // cmp w16, #lhs
            emit_u32(bytes, 0x7100_001f | (u32::from(l.bias) << 10) | (ADDR << 5));
            COND_HI
        }
        ((l, Some(reg)), (r, None)) => {
            materialize(bytes, l, reg, ADDR);
            // cmp w16, #rhs
            emit_u32(bytes, 0x7100_001f | (u32::from(r.bias) << 10) | (ADDR << 5));
            COND_LO
        }
        ((l, Some(lreg)), (r, Some(rreg))) => {
            materialize(bytes, l, lreg, ADDR);
            materialize(bytes, r, rreg, ADDR + 1);
            // cmp w16, w17
            emit_u32(bytes, 0x6b00_001f | ((ADDR + 1) << 16) | (ADDR << 5));
            COND_LO
        }
    };

    if value == 1 {
        // cinc wd, wd, cond  (csinc wd, wd, wd, !cond)
        emit_u32(
            bytes,
            0x1a80_0400 | (rd << 16) | ((cond ^ 1) << 12) | (rd << 5) | rd,
        );
    } else {
        // add w8, wd, #value
        emit_u32(
            bytes,
            0x1100_0000 | (u32::from(value) << 10) | (rd << 5) | TMP,
        );
        // csel wd, w8, wd, cond
        emit_u32(
            bytes,
            0x1a80_0000 | (rd << 16) | (cond << 12) | (TMP << 5) | rd,
        );
    }
}

/// Get the byte value of an operand into `tmp`, which is either computed or a
/// constant.
fn operand_value(bytes: &mut Vec<u8>, (operand, reg): (Operand, Option<u32>), tmp: u32) {
    match reg {
        Some(reg) => materialize(bytes, operand, reg, tmp),
        // movz wtmp, #value
        None => emit_u32(bytes, 0x5280_0000 | (u32::from(operand.bias) << 5) | tmp),
    }
}

/// wd += value * byte of (base + step * count)
fn product_add(
    bytes: &mut Vec<u8>,
    [base, step, count]: [(Operand, Option<u32>); 3],
    high: bool,
    rd: u32,
    value: u8,
) {
    operand_value(bytes, base, ADDR);
    operand_value(bytes, step, ADDR + 1);
    operand_value(bytes, count, TMP);
    // madd w16, w17, w8, w16
    emit_u32(
        bytes,
        0x1b00_0000 | (TMP << 16) | (ADDR << 10) | ((ADDR + 1) << 5) | ADDR,
    );
    if high {
        // lsr w16, w16, #8  (ubfm w16, w16, #8, #31)
        emit_u32(bytes, 0x5300_7c00 | (8 << 16) | (ADDR << 5) | ADDR);
    }
    multiply_add(bytes, ADDR, rd, value);
}

/// Load the little-endian number in `len` cells from `start` into x`rd`,
/// using x8 as a temporary.
fn load_number(bytes: &mut Vec<u8>, rd: u32, start: i32, len: u8) {
    let len = i32::from(len);
    load_byte(bytes, rd, start + len - 1);
    for i in (0..len - 1).rev() {
        load_byte(bytes, TMP, start + i);
        // orr xd, x8, xd, lsl #8
        emit_u32(bytes, 0xaa00_2000 | (rd << 16) | (TMP << 5) | rd);
    }
}

pub fn div_mod(bytes: &mut Vec<u8>, node: &AstNode) {
    let AstNode::DivMod {
        dividend,
        dividend_len,
        divisor,
        divisor_len,
        quotient,
        factor,
    } = *node
    else {
        unreachable!("not a division: {node:?}");
    };

    // x0 = dividend, then remainder; x1 = divisor; x2 = quotient
    load_number(bytes, 1, divisor, divisor_len);

    let mut divide = Vec::new();
    load_number(&mut divide, 0, dividend, dividend_len);
    // udiv x2, x0, x1
    emit_u32(&mut divide, 0x9ac0_0800 | (1 << 16) | 2);
    // msub x0, x2, x1, x0
    emit_u32(&mut divide, 0x9b00_8000 | (1 << 16) | (2 << 5));
    for i in 0..i32::from(dividend_len) {
        store_byte(&mut divide, 0, dividend + i);
        // lsr x0, x0, #8
        emit_u32(&mut divide, 0xd348_fc00);
    }
    if factor != 0 {
        load_byte(&mut divide, 3, quotient);
        // movz w4, #factor
        emit_u32(&mut divide, 0x5280_0000 | (u32::from(factor) << 5) | 4);
        // madd w3, w2, w4, w3
        emit_u32(
            &mut divide,
            0x1b00_0000 | (4 << 16) | (3 << 10) | (2 << 5) | 3,
        );
        store_byte(&mut divide, 3, quotient);
    }

    // cbz x1, over the division
    let skip = i32::try_from(divide.len() / 4).unwrap() + 1;
    emit_u32(bytes, 0xb400_0000 | (bits(skip, 19) << 5) | 1);
    bytes.extend(divide);
}

/// Add a value to x19.
fn add_to_pointer(bytes: &mut Vec<u8>, amount: i32) {
    if (0..4096).contains(&amount) {
        // add x19, x19, #amount
        emit_u32(
            bytes,
            0x9100_0000 | (bits(amount, 12) << 10) | (MEM << 5) | MEM,
        );
    } else if (-4095..0).contains(&amount) {
        // sub x19, x19, #-amount
        emit_u32(
            bytes,
            0xd100_0000 | (bits(-amount, 12) << 10) | (MEM << 5) | MEM,
        );
    } else {
        mov_imm(bytes, ADDR, amount);
        // add x19, x19, x16
        emit_u32(bytes, 0x8b00_0000 | (ADDR << 16) | (MEM << 5) | MEM);
    }
}

pub fn move_pointer(bytes: &mut Vec<u8>, amount: i32) {
    add_to_pointer(bytes, amount);
}

/// Move x19 by `amount` and load the new current cell into w8.
fn move_and_load(bytes: &mut Vec<u8>, amount: i32) {
    if amount != 0 && (-256..256).contains(&amount) {
        // ldrb w8, [x19, #amount]!
        emit_u32(
            bytes,
            0x3840_0c00 | (bits(amount, 9) << 12) | (MEM << 5) | TMP,
        );
    } else {
        if amount != 0 {
            add_to_pointer(bytes, amount);
        }
        load_byte(bytes, TMP, 0);
    }
}

/// Make a call to a vtable entry in x21.
fn call_vtable_entry(bytes: &mut Vec<u8>, entry: VTableEntry) {
    let offset = (entry as u32) * PTR_SIZE;

    // Load function pointer from vtable
    // ldr x8, [x21, #offset]
    emit_u32(bytes, 0xf940_0008 | (21 << 5) | ((offset / 8) << 10));

    // Call the function
    // blr x8
    emit_u32(bytes, 0xd63f_0100);
}

// x19-x22 are callee-saved, so calls don't need to preserve them.

pub fn print(bytes: &mut Vec<u8>, offset: i32) {
    // Move the JITTarget pointer into the first argument register
    // mov x0, x20
    emit_u32(bytes, 0xaa14_03e0);

    // Load the memory cell into the second argument register
    // ldrb w1, [x19, #offset]
    load_byte(bytes, 1, offset);

    call_vtable_entry(bytes, VTableEntry::Print);
}

pub fn read(bytes: &mut Vec<u8>, offset: i32) {
    // Move the JITTarget pointer into the first argument register
    // mov x0, x20
    emit_u32(bytes, 0xaa14_03e0);

    call_vtable_entry(bytes, VTableEntry::Read);

    // Copy return value into the memory cell
    // strb w0, [x19, #offset]
    store_byte(bytes, 0, offset);
}

/// Emit a conditional branch on w8 to an instruction offset (in instructions),
/// falling back to an unconditional branch for distant targets.
fn branch_on_tmp(bytes: &mut Vec<u8>, if_zero: bool, offset: i64) {
    let op = if if_zero { 0x3400_0000 } else { 0x3500_0000 };

    if (-(1 << 18)..(1 << 18)).contains(&offset) {
        // cbz/cbnz w8, offset
        #[allow(clippy::cast_possible_truncation)]
        emit_u32(bytes, op | (bits(offset as i32, 19) << 5) | TMP);
    } else {
        // cbnz/cbz w8, +2 (skip the b)
        emit_u32(bytes, (op ^ 0x0100_0000) | (2 << 5) | TMP);
        // b offset - 1
        #[allow(clippy::cast_possible_truncation)]
        emit_u32(bytes, 0x1400_0000 | bits((offset - 1) as i32, 26));
    }
}

/// Instructions emitted by `branch_on_tmp`.
const fn branch_len(offset: i64) -> i64 {
    if -(1 << 18) <= offset && offset < (1 << 18) {
        1
    } else {
        2
    }
}

/// Loop bodies up to this many instructions are unrolled. Duplicating code
/// is fine since all branches are relative.
const MAX_UNROLLED_BODY: i64 = 24;

/// Emit a loop. `trailing_move` is a pointer movement at the end of the body,
/// which is folded into the condition check.
pub fn aot_loop(bytes: &mut Vec<u8>, inner_loop_bytes: Vec<u8>, trailing_move: i32) {
    // Check if the current memory cell equals zero
    // ldrb w8, [x19]
    load_byte(bytes, TMP, 0);

    let mut tail = Vec::new();
    move_and_load(&mut tail, trailing_move);
    let body_len = i64::try_from((inner_loop_bytes.len() + tail.len()) / 4).unwrap();

    if body_len <= MAX_UNROLLED_BODY {
        // Small bodies run twice per iteration, halving taken branches:
        //
        //     cbz w8, end
        // loop:
        //     body
        //     cbz w8, end
        //     body
        //     cbnz w8, loop
        // end:
        branch_on_tmp(bytes, true, 3 + 2 * body_len);
        bytes.extend(&inner_loop_bytes);
        bytes.extend(&tail);
        branch_on_tmp(bytes, true, 2 + body_len);
        bytes.extend(inner_loop_bytes);
        bytes.extend(tail);
        branch_on_tmp(bytes, false, -(1 + 2 * body_len));
        return;
    }

    // Branch back over the body and tail, to the start of the body.
    let back = -body_len;
    let back_len = branch_len(back);

    // Skip the body, tail and back branch (the check itself counts as one).
    let skip = body_len + back_len + 1;
    let skip = skip + branch_len(skip) - 1;
    branch_on_tmp(bytes, true, skip);

    bytes.extend(inner_loop_bytes);
    bytes.extend(tail);
    branch_on_tmp(bytes, false, back);
}

/// Cells checked per iteration of an unrolled scan.
const SCAN_UNROLL: i32 = 4;

pub fn scan(bytes: &mut Vec<u8>, stride: i32) {
    let unrolled = stride * SCAN_UNROLL;
    if !(-256..256).contains(&unrolled) {
        let mut body = Vec::new();
        move_and_load(&mut body, stride);
        aot_loop_raw(bytes, &body);
        return;
    }

    // Check several cells per iteration to reduce taken branches:
    //
    //     ldrb w8, [x19]
    //     cbz w8, done
    // loop:
    //     ldrb w8, [x19, #stride * k]   (for k in 1..UNROLL)
    //     cbz w8, found_k
    //     ldrb w8, [x19, #stride * UNROLL]!
    //     cbnz w8, loop
    //     b done
    // found_k:
    //     add x19, x19, #stride * k
    //     b done
    // done:
    let unroll = SCAN_UNROLL as usize;
    let checks = (unroll - 1) * 2;
    // Instruction index of each found_k block relative to the loop start.
    let found = |k: usize| checks + 3 + (k - 1) * 2;
    let done = found(unroll);

    load_byte(bytes, TMP, 0);
    branch_on_tmp(bytes, true, i64::try_from(done + 1).unwrap());

    for k in 1..unroll {
        let index = (k - 1) * 2;
        load_byte(bytes, TMP, stride * i32::try_from(k).unwrap());
        branch_on_tmp(bytes, true, i64::try_from(found(k) - index - 1).unwrap());
    }
    move_and_load(bytes, unrolled);
    branch_on_tmp(bytes, false, -i64::try_from(checks + 1).unwrap());
    // b done
    emit_u32(
        bytes,
        0x1400_0000 | u32::try_from(done - checks - 2).unwrap(),
    );

    for k in 1..unroll {
        add_to_pointer(bytes, stride * i32::try_from(k).unwrap());
        // b done
        emit_u32(
            bytes,
            0x1400_0000 | u32::try_from(done - found(k) - 1).unwrap(),
        );
    }
}

/// A loop whose body ends by loading the current cell into w8.
fn aot_loop_raw(bytes: &mut Vec<u8>, body: &[u8]) {
    let body_len = i64::try_from(body.len() / 4).unwrap();

    // ldrb w8, [x19]
    load_byte(bytes, TMP, 0);
    // cbz w8, end
    branch_on_tmp(bytes, true, body_len + 2);
    bytes.extend_from_slice(body);
    // cbnz w8, start
    branch_on_tmp(bytes, false, -body_len);
}

pub fn jit_loop(bytes: &mut Vec<u8>, loop_id: JITPromiseID) {
    // Call the compiled fragment directly if there is one:
    //
    //     ldr x16, [x21, #fragments]
    //     movz x17, #loop_id
    //     ldr x16, [x16, x17, lsl #3]
    //     cbz x16, callback
    //     mov x0, x19
    //     mov x1, x20
    //     mov x2, x21
    //     blr x16
    //     b done
    // callback:
    //     (call jit_callback)
    // done:
    //     mov x19, x0
    let offset = (VTableEntry::Fragments as u32) * PTR_SIZE;
    emit_u32(bytes, 0xf940_0000 | ((offset / 8) << 10) | (21 << 5) | ADDR);
    emit_u32(
        bytes,
        0xd280_0000 | (u32::from(loop_id.value()) << 5) | (ADDR + 1),
    );
    emit_u32(bytes, 0xf860_7800 | ((ADDR + 1) << 16) | (ADDR << 5) | ADDR);
    emit_u32(bytes, 0xb400_0000 | (6 << 5) | ADDR);
    emit_u32(bytes, 0xaa13_03e0);
    emit_u32(bytes, 0xaa14_03e1);
    emit_u32(bytes, 0xaa15_03e2);
    emit_u32(bytes, 0xd63f_0000 | (ADDR << 5));
    emit_u32(bytes, 0x1400_0006);

    // Move the JITTarget pointer into the first argument
    // mov x0, x20
    emit_u32(bytes, 0xaa14_03e0);

    // Move target index into the second argument
    // movz x1, #loop_id.value()
    emit_u32(bytes, 0xd280_0001 | (u32::from(loop_id.value()) << 5));

    // Move data pointer into the third argument
    // mov x2, x19
    emit_u32(bytes, 0xaa13_03e2);

    call_vtable_entry(bytes, VTableEntry::JITCallback);

    // Take return value and store as the new data pointer
    // mov x19, x0
    emit_u32(bytes, 0xaa00_03f3);
}

pub fn syscall(bytes: &mut Vec<u8>) {
    // Move the JITTarget pointer into the first argument register
    // mov x0, x20
    emit_u32(bytes, 0xaa14_03e0);

    // Move the current memory pointer into the second argument register
    // mov x1, x19
    emit_u32(bytes, 0xaa13_03e1);

    // Move the base memory pointer into the third argument register
    // mov x2, x22
    emit_u32(bytes, 0xaa16_03e2);

    call_vtable_entry(bytes, VTableEntry::Syscall);

    // Copy return value into current cell
    // strb w0, [x19]
    emit_u32(bytes, 0x3900_0260);
}
