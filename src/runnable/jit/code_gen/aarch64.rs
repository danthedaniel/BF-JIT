use std::collections::{BTreeMap, HashMap};
use std::ops::Range;

use super::{Step, accessed_cells, live_cells, steps};
use crate::parser::{AstNode, Operand, WordTerm};
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
// x30 - Base for distant cells within straight-line code
// x29 - Frame pointer
// x30 - Link register

const MEM: u32 = 19;
const TMP: u32 = 8;
const ADDR: u32 = 16;
/// Points near cells far from x19 in straight-line code. It's the link
/// register, which is saved on entry and free between calls.
const FAR: u32 = 30;
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

/// The registers cells can be addressed through with an immediate offset:
/// x19, and x30 if it points `far` cells away from it.
fn bases(far: Option<i32>) -> impl Iterator<Item = (u32, i32)> {
    std::iter::once((MEM, 0)).chain(far.map(|far| (FAR, far)))
}

/// Load (`ldrb`) or store (`strb`) w`rt` at x19 + offset.
fn mem_byte(bytes: &mut Vec<u8>, load: bool, rt: u32, offset: i32, far: Option<i32>) {
    for (base, origin) in bases(far) {
        let offset = offset - origin;
        if (0..4096).contains(&offset) {
            // ldrb/strb wt, [xbase, #offset]
            let op = if load { 0x3940_0000 } else { 0x3900_0000 };
            emit_u32(bytes, op | (bits(offset, 12) << 10) | (base << 5) | rt);
            return;
        } else if (-256..0).contains(&offset) {
            // ldurb/sturb wt, [xbase, #offset]
            let op = if load { 0x3840_0000 } else { 0x3800_0000 };
            emit_u32(bytes, op | (bits(offset, 9) << 12) | (base << 5) | rt);
            return;
        }
    }
    mov_imm(bytes, ADDR, offset);
    // ldrb/strb wt, [x19, x16]
    let op = if load { 0x3860_6800 } else { 0x3820_6800 };
    emit_u32(bytes, op | (ADDR << 16) | (MEM << 5) | rt);
}

fn load_byte(bytes: &mut Vec<u8>, rt: u32, offset: i32) {
    mem_byte(bytes, true, rt, offset, None);
}

fn store_byte(bytes: &mut Vec<u8>, rt: u32, offset: i32) {
    mem_byte(bytes, false, rt, offset, None);
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

/// movz wd, #value
fn movz(bytes: &mut Vec<u8>, rd: u32, value: u8) {
    emit_u32(bytes, 0x5280_0000 | (u32::from(value) << 5) | rd);
}

/// add wd, wn, #value
fn add_imm(bytes: &mut Vec<u8>, rd: u32, rn: u32, value: u8) {
    emit_u32(
        bytes,
        0x1100_0000 | (u32::from(value) << 10) | (rn << 5) | rd,
    );
}

/// and wd, wn, #0xff
fn and_byte(bytes: &mut Vec<u8>, rd: u32, rn: u32) {
    emit_u32(bytes, 0x1200_1c00 | (rn << 5) | rd);
}

/// lsl wd, wn, #shift  (ubfm wd, wn, #(32 - shift) % 32, #(31 - shift))
fn lsl(bytes: &mut Vec<u8>, rd: u32, rn: u32, shift: u32) {
    let rotation = (32 - shift) % 32;
    let width = 31 - shift;
    emit_u32(
        bytes,
        0x5300_0000 | (rotation << 16) | (width << 10) | (rn << 5) | rd,
    );
}

/// cmp wn, #value
fn cmp_imm(bytes: &mut Vec<u8>, rn: u32, value: u8) {
    emit_u32(bytes, 0x7100_001f | (u32::from(value) << 10) | (rn << 5));
}

/// tst wn, #0xff
fn tst_byte(bytes: &mut Vec<u8>, rn: u32) {
    emit_u32(bytes, 0x7200_1c1f | (rn << 5));
}

/// csel wd, wn, wm, cond
fn csel(bytes: &mut Vec<u8>, cond: u32, rd: u32, rn: u32, rm: u32) {
    emit_u32(
        bytes,
        0x1a80_0000 | (rm << 16) | (cond << 12) | (rn << 5) | rd,
    );
}

/// cset wd, cond  (csinc wd, wzr, wzr, !cond)
fn cset(bytes: &mut Vec<u8>, cond: u32, rd: u32) {
    emit_u32(bytes, 0x1a9f_07e0 | ((cond ^ 1) << 12) | rd);
}

/// A memory cell whose value is known, either in a register or as a constant.
#[derive(Clone, Copy)]
struct Entry {
    /// The register allocated to the cell, if any. Cells which aren't known
    /// constants always have one.
    reg: Option<u32>,
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
    entries: BTreeMap<i32, Entry>,
    /// The cell each cache register is allocated to
    holders: [Option<i32>; CACHE_REGS.len()],
    /// Indexes of the nodes accessing each cell, and whether they read it
    accesses: HashMap<i32, Vec<(usize, bool)>>,
    /// Index of the node being compiled
    position: usize,
    /// Where x30 points, relative to x19, if set
    far: Option<i32>,
}

impl CellCache {
    /// A cache for compiling `steps` in order, which evicts the cells needed
    /// furthest in the future when it runs out of registers.
    fn new(steps: &[Step]) -> Self {
        let mut cache = Self::default();
        for (position, step) in steps.iter().enumerate() {
            let read = !matches!(step, Step::Node(AstNode::Set(..)));
            for cell in accessed_cells(step) {
                cache
                    .accesses
                    .entry(cell)
                    .or_default()
                    .push((position, read));
            }
        }
        cache
    }

    /// Point x30 at the nearest of the given cells which x19 can't address
    /// with an immediate offset, if any.
    fn reach(&mut self, bytes: &mut Vec<u8>, cells: impl Iterator<Item = i32>) {
        if let Some(far) = cells.filter(|cell| !(-256..4096).contains(cell)).min() {
            add_offset(bytes, FAR, MEM, far);
            self.far = Some(far);
        }
    }

    /// The first access to a cell at or after the current node.
    fn next_access(&self, offset: i32) -> Option<(usize, bool)> {
        let accesses = self.accesses.get(&offset)?;
        let index = accesses.partition_point(|&(position, _)| position < self.position);
        accesses.get(index).copied()
    }

    /// Free up a register, preferring the cell whose value is needed furthest
    /// in the future and then cells which don't have to be stored.
    fn evict(&mut self, bytes: &mut Vec<u8>) -> usize {
        let key = |offset: i32| {
            let entry = self.entries[&offset];
            let next_read = match self.next_access(offset) {
                // Cells accessed by the current node are always needed.
                Some((position, read)) if read || position == self.position => position,
                // Cells which are overwritten next or never accessed again
                _ => usize::MAX,
            };
            (next_read, entry.constant.is_some() || !entry.dirty)
        };
        let slot = (0..CACHE_REGS.len())
            .max_by_key(|&slot| key(self.holders[slot].unwrap()))
            .unwrap();

        let offset = self.holders[slot].take().unwrap();
        let overwritten = matches!(self.next_access(offset), Some((_, false)));
        let entry = self.entries.get_mut(&offset).unwrap();
        if entry.constant.is_some() {
            // The constant is still known.
            entry.reg = None;
            entry.materialized = false;
        } else {
            if entry.dirty && !overwritten {
                mem_byte(bytes, false, CACHE_REGS[slot], offset, self.far);
            }
            self.entries.remove(&offset);
        }
        slot
    }

    /// Allocate a register to a cell.
    fn allocate(&mut self, bytes: &mut Vec<u8>, offset: i32) -> u32 {
        let slot = match self.holders.iter().position(Option::is_none) {
            Some(slot) => slot,
            None => self.evict(bytes),
        };
        self.holders[slot] = Some(offset);
        CACHE_REGS[slot]
    }

    /// Allocate a register to a cell whose value won't be used before it's
    /// set, without loading it.
    fn reserve(&mut self, offset: i32) {
        let reg = self.allocate(&mut Vec::new(), offset);
        self.entries.insert(
            offset,
            Entry {
                reg: Some(reg),
                dirty: false,
                constant: None,
                materialized: true,
            },
        );
    }

    /// The known constant value of a cell, without loading it.
    fn constant(&self, offset: i32) -> Option<u8> {
        self.entries.get(&offset).and_then(|entry| entry.constant)
    }

    /// Get a register holding the value of a cell.
    fn read(&mut self, bytes: &mut Vec<u8>, offset: i32) -> u32 {
        let Some(entry) = self.entries.get(&offset).copied() else {
            let reg = self.allocate(bytes, offset);
            mem_byte(bytes, true, reg, offset, self.far);
            self.entries.insert(
                offset,
                Entry {
                    reg: Some(reg),
                    dirty: false,
                    constant: None,
                    materialized: true,
                },
            );
            return reg;
        };
        if entry.materialized {
            return entry.reg.unwrap();
        }

        let reg = match entry.reg {
            Some(reg) => reg,
            None => self.allocate(bytes, offset),
        };
        movz(bytes, reg, entry.constant.unwrap());
        let entry = self.entries.get_mut(&offset).unwrap();
        entry.reg = Some(reg);
        entry.materialized = true;
        reg
    }

    /// Get a register holding the value of a cell which is about to be modified.
    fn modify(&mut self, bytes: &mut Vec<u8>, offset: i32) -> u32 {
        let reg = self.read(bytes, offset);
        let entry = self.entries.get_mut(&offset).unwrap();
        entry.dirty = true;
        entry.constant = None;
        reg
    }

    /// Get a register for a cell which is about to be overwritten.
    fn overwrite(&mut self, bytes: &mut Vec<u8>, offset: i32) -> u32 {
        let reg = match self.entries.get(&offset).and_then(|entry| entry.reg) {
            Some(reg) => reg,
            None => self.allocate(bytes, offset),
        };
        self.entries.insert(
            offset,
            Entry {
                reg: Some(reg),
                dirty: true,
                constant: None,
                materialized: true,
            },
        );
        reg
    }

    fn set(&mut self, offset: i32, value: u8) {
        let reg = self.entries.get(&offset).and_then(|entry| entry.reg);
        self.entries.insert(
            offset,
            Entry {
                reg,
                dirty: true,
                constant: Some(value),
                materialized: false,
            },
        );
    }

    fn write_back(&self, bytes: &mut Vec<u8>, offset: i32, entry: Entry) {
        if !entry.dirty {
            return;
        }
        let reg = match (entry.constant, entry.reg) {
            (Some(0), _) if !entry.materialized => ZERO,
            (Some(value), _) if !entry.materialized => {
                movz(bytes, TMP, value);
                TMP
            }
            (_, Some(reg)) => reg,
            _ => unreachable!("cell {offset} has no value"),
        };
        mem_byte(bytes, false, reg, offset, self.far);
    }

    fn flush(self, bytes: &mut Vec<u8>) {
        for (&offset, &entry) in &self.entries {
            self.write_back(bytes, offset, entry);
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
                    self.set(offset, constant.wrapping_add(value));
                } else {
                    let reg = self.modify(bytes, offset);
                    add_imm(bytes, reg, reg, value);
                }
            }
            AstNode::Set(offset, value) => self.set(offset, value),
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
                match (lhs.reads(), rhs.reads()) {
                    (None, None) => {
                        if lhs.bias < rhs.bias {
                            self.node(bytes, &AstNode::Add(dst, value));
                        }
                        return;
                    }
                    // Nothing is greater than 255 or less than 0.
                    (None, Some(_)) if lhs.bias == u8::MAX => return,
                    (Some(_), None) if rhs.bias == 0 => return,
                    _ => {}
                }
                let lhs_reg = lhs.reads().map(|cell| self.read(bytes, cell));
                let rhs_reg = rhs.reads().map(|cell| self.read(bytes, cell));
                let dst = self.conditional_dst(bytes, dst);
                conditional_add(bytes, dst, value, |bytes| {
                    compare(bytes, (lhs, lhs_reg), (rhs, rhs_reg))
                });
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
            AstNode::Word {
                dst,
                len,
                ref terms,
                constant,
            } => {
                // Words are computed in memory.
                for term in terms {
                    self.store(bytes, term.reads());
                }
                word(bytes, dst, len, terms, constant, self.far);
                self.forget(dst..dst + i32::from(len));
            }
            _ => unreachable!("not a straight-line node: {node:?}"),
        }
    }

    /// Bring memory up to date for some cells.
    fn store(&mut self, bytes: &mut Vec<u8>, cells: impl Iterator<Item = i32>) {
        for cell in cells {
            if let Some(&entry) = self.entries.get(&cell)
                && entry.dirty
            {
                self.write_back(bytes, cell, entry);
                self.entries.get_mut(&cell).unwrap().dirty = false;
            }
        }
    }

    /// Drop cells whose values only memory holds now.
    fn forget(&mut self, cells: Range<i32>) {
        for cell in cells {
            self.entries.remove(&cell);
            if let Some(holder) = self
                .holders
                .iter_mut()
                .find(|holder| **holder == Some(cell))
            {
                *holder = None;
            }
        }
    }

    /// Get the register and known constant value of a cell which is about to
    /// be conditionally added to, for `conditional_add`.
    fn conditional_dst(&mut self, bytes: &mut Vec<u8>, offset: i32) -> (u32, Option<u8>) {
        match self.constant(offset) {
            Some(constant) => (self.overwrite(bytes, offset), Some(constant)),
            None => (self.modify(bytes, offset), None),
        }
    }

    /// Set cells to constants if the counter isn't zero, without branching,
    /// and then set the counter to zero.
    fn set_if(&mut self, bytes: &mut Vec<u8>, counter: i32, sets: &[(i32, u8)]) {
        match self.constant(counter) {
            Some(0) => {}
            Some(_) => {
                for &(offset, value) in sets {
                    self.set(offset, value);
                }
            }
            None => {
                let mut regs = Vec::new();
                for &(offset, value) in sets {
                    match self.constant(offset) {
                        Some(constant) if constant == value => {}
                        Some(constant) => {
                            regs.push((self.overwrite(bytes, offset), value, Some(constant)));
                        }
                        None => regs.push((self.modify(bytes, offset), value, None)),
                    }
                }
                let counter = self.read(bytes, counter);

                tst_byte(bytes, counter);
                for (reg, value, constant) in regs {
                    match (constant, value) {
                        // The result is just the condition, or its inverse.
                        (Some(0), 1) => cset(bytes, COND_NE, reg),
                        (Some(1), 0) => cset(bytes, COND_EQ, reg),
                        _ => {
                            // Neither of these changes the flags.
                            if let Some(constant) = constant {
                                movz(bytes, reg, constant);
                            }
                            let src = if value == 0 {
                                ZERO
                            } else {
                                movz(bytes, TMP, value);
                                TMP
                            };
                            csel(bytes, COND_NE, reg, src, reg);
                        }
                    }
                }
            }
        }
        self.set(counter, 0);
    }

    /// Compile straight-line steps in order.
    fn compile(&mut self, bytes: &mut Vec<u8>, steps: &[Step]) {
        for (position, step) in steps.iter().enumerate() {
            self.position = position;
            match step {
                Step::Node(node) => self.node(bytes, node),
                Step::SetIf { counter, sets } => self.set_if(bytes, *counter, sets),
            }
        }
    }
}

/// Compile a run of nodes for which `is_straight_line` holds. Cells are kept
/// in registers and written back at the end, and the data pointer is moved
/// once.
pub fn straight_line(bytes: &mut Vec<u8>, nodes: &[AstNode]) {
    let (steps, moved) = steps(nodes);
    let mut cache = CellCache::new(&steps);
    cache.reach(bytes, steps.iter().flat_map(accessed_cells));
    cache.compile(bytes, &steps);
    cache.flush(bytes);
    if moved != 0 {
        move_pointer(bytes, moved);
    }
}

/// Compile a loop whose body is straight-line code which doesn't move the
/// data pointer overall, keeping all cells in registers across iterations.
/// Returns false if there are too many cells.
pub fn register_loop(bytes: &mut Vec<u8>, nodes: &[AstNode]) -> bool {
    let (steps, moved) = steps(nodes);
    // Words leave cells in memory, which the body can't do between
    // iterations.
    if moved != 0
        || steps
            .iter()
            .any(|step| matches!(step, Step::Node(AstNode::Word { .. })))
    {
        return false;
    }
    let mut cells: Vec<i32> = std::iter::once(0)
        .chain(steps.iter().flat_map(accessed_cells))
        .collect();
    cells.sort_unstable();
    cells.dedup();
    if cells.len() > CACHE_REGS.len() {
        return false;
    }
    let live = live_cells(&cells, &steps);

    // The counter is loaded first, and the other cells only once the loop is
    // known to run, since many of these loops are conditionals which often
    // don't.
    let mut check = Vec::new();
    let mut cache = CellCache::default();
    let counter = cache.read(&mut check, 0);
    tst_byte(&mut check, counter);
    let mut head = Vec::new();
    cache.reach(&mut head, cells.iter().copied());
    for &cell in &cells {
        if live.contains(&cell) {
            cache.read(&mut head, cell);
        } else {
            cache.reserve(cell);
        }
    }

    let mut body = Vec::new();
    cache.compile(&mut body, &steps);
    // A loop which sets its counter runs at most once (or forever), so loading
    // every cell up front doesn't pay off.
    if cache.constant(0).is_some() {
        return false;
    }

    // Cells used by the next iteration must be in their registers.
    for &cell in &live {
        cache.read(&mut body, cell);
    }
    tst_byte(&mut body, counter);

    // Write back cells after running the body at least once. Memory is
    // already up to date if it doesn't run.
    let mut tail = Vec::new();
    cache.flush(&mut tail);

    let head_len = i32::try_from(head.len() / 4).unwrap();
    let body_len = i32::try_from(body.len() / 4).unwrap();
    let tail_len = i32::try_from(tail.len() / 4).unwrap();
    bytes.extend(check);
    // b.eq end
    emit_u32(
        bytes,
        0x5400_0000 | (bits(head_len + body_len + tail_len + 2, 19) << 5),
    );
    bytes.extend(head);
    bytes.extend(body);
    // b.ne start
    emit_u32(bytes, 0x5400_0001 | (bits(-body_len, 19) << 5));
    bytes.extend(tail);
    // end:
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

// Condition codes. Inverting one flips its lowest bit.
const COND_EQ: u32 = 0b0000;
const COND_NE: u32 = 0b0001;
const COND_HS: u32 = 0b0010;
const COND_LO: u32 = 0b0011;
const COND_HI: u32 = 0b1000;

/// Get a register whose low byte holds the value of an operand which reads
/// the cell in register `reg`, computing it in `tmp` if needed.
fn operand_reg(bytes: &mut Vec<u8>, operand: Operand, reg: u32, tmp: u32) -> u32 {
    let src = match operand.scale {
        1 => reg,
        u8::MAX if operand.bias == u8::MAX => {
            // mvn wtmp, wreg
            emit_u32(bytes, 0x2a20_03e0 | (reg << 16) | tmp);
            return tmp;
        }
        u8::MAX => {
            // neg wtmp, wreg
            emit_u32(bytes, 0x4b00_03e0 | (reg << 16) | tmp);
            tmp
        }
        scale if scale.is_power_of_two() => {
            lsl(bytes, tmp, reg, scale.trailing_zeros());
            tmp
        }
        scale => {
            movz(bytes, tmp, scale);
            // mul wtmp, wreg, wtmp
            emit_u32(bytes, 0x1b00_7c00 | (tmp << 16) | (reg << 5) | tmp);
            tmp
        }
    };
    if operand.bias == 0 {
        return src;
    }
    add_imm(bytes, tmp, src, operand.bias);
    tmp
}

/// Compute the byte value of an operand held in register `reg` into `tmp`.
fn materialize(bytes: &mut Vec<u8>, operand: Operand, reg: u32, tmp: u32) {
    let src = operand_reg(bytes, operand, reg, tmp);
    and_byte(bytes, tmp, src);
}

/// Compare `lhs < rhs`, returning the condition code which holds if it's true.
/// Operands which read a cell come with its register. At least one operand
/// must read a cell, and a constant `lhs` must be below 255.
fn compare(bytes: &mut Vec<u8>, lhs: (Operand, Option<u32>), rhs: (Operand, Option<u32>)) -> u32 {
    match (lhs, rhs) {
        // 0 < x
        ((l, None), (r, Some(reg))) if l.bias == 0 && (r.scale, r.bias) == (1, 0) => {
            tst_byte(bytes, reg);
            COND_NE
        }
        // lhs < x + (lhs + 1) <=> the addition doesn't wrap <=> x < 256 - (lhs + 1)
        ((l, None), (r, Some(reg)))
            if r.scale == 1 && r.bias != 0 && r.bias == l.bias.wrapping_add(1) =>
        {
            and_byte(bytes, ADDR, reg);
            cmp_imm(bytes, ADDR, r.bias.wrapping_neg());
            COND_LO
        }
        // ~x < rhs <=> adding rhs to x carries <=> x >= 256 - rhs, and
        // x + rhs < rhs <=> the addition wraps <=> x >= 256 - rhs
        ((l, Some(reg)), (r, None))
            if (l.scale, l.bias) == (u8::MAX, u8::MAX) || (l.scale == 1 && l.bias == r.bias) =>
        {
            and_byte(bytes, ADDR, reg);
            cmp_imm(bytes, ADDR, r.bias.wrapping_neg());
            COND_HS
        }
        // ~x < y <=> adding y to x carries, which shows in the carry flag
        // when both are shifted to the top byte
        ((l, Some(lreg)), (r, Some(rreg)))
            if (l.scale, l.bias, r.scale, r.bias) == (u8::MAX, u8::MAX, 1, 0) =>
        {
            lsl(bytes, ADDR, lreg, 24);
            // cmn w16, wrreg, lsl #24
            emit_u32(bytes, 0x2b00_001f | (rreg << 16) | (24 << 10) | (ADDR << 5));
            COND_HS
        }
        ((l, None), (r, Some(reg))) => {
            materialize(bytes, r, reg, ADDR);
            cmp_imm(bytes, ADDR, l.bias);
            COND_HI
        }
        ((l, Some(reg)), (r, None)) => {
            materialize(bytes, l, reg, ADDR);
            cmp_imm(bytes, ADDR, r.bias);
            COND_LO
        }
        ((l, Some(lreg)), (r, Some(rreg))) => {
            materialize(bytes, l, lreg, ADDR);
            let rreg = operand_reg(bytes, r, rreg, ADDR + 1);
            // cmp w16, wrreg, uxtb
            emit_u32(bytes, 0x6b20_001f | (rreg << 16) | (ADDR << 5));
            COND_LO
        }
        ((_, None), (_, None)) => unreachable!("comparison of constants"),
    }
}

/// wd += value if a condition holds, without branching. `compare` sets the
/// flags and returns the condition code. `rd` comes with the cell's constant
/// value if it's known, in which case the register doesn't hold it yet.
fn conditional_add(
    bytes: &mut Vec<u8>,
    (rd, constant): (u32, Option<u8>),
    value: u8,
    compare: impl FnOnce(&mut Vec<u8>) -> u32,
) {
    let cond = compare(bytes);
    match (constant, value) {
        // The result is just the condition, or its inverse.
        (Some(0), 1) => cset(bytes, cond, rd),
        (Some(1), u8::MAX) => cset(bytes, cond ^ 1, rd),
        _ => {
            if let Some(constant) = constant {
                movz(bytes, rd, constant);
            }
            if value == 1 {
                // cinc wd, wd, cond  (csinc wd, wd, wd, !cond)
                emit_u32(
                    bytes,
                    0x1a80_0400 | (rd << 16) | ((cond ^ 1) << 12) | (rd << 5) | rd,
                );
            } else {
                add_imm(bytes, TMP, rd, value);
                csel(bytes, cond, rd, TMP, rd);
            }
        }
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

/// The base register and offset addressing a cell with an unscaled offset,
/// for accesses of up to 8 bytes. Distant cells are addressed through x16.
fn address(bytes: &mut Vec<u8>, cell: i32, far: Option<i32>) -> (u32, i32) {
    if let Some(address) = bases(far)
        .map(|(base, origin)| (base, cell - origin))
        .find(|(_, offset)| (-256..=248).contains(offset))
    {
        return address;
    }
    match far {
        Some(far) if (0..4096).contains(&(cell - far)) => add_offset(bytes, ADDR, FAR, cell - far),
        _ => add_offset(bytes, ADDR, MEM, cell),
    }
    (ADDR, 0)
}

/// Load (`ldur`) or store (`stur`) `size` bytes (1, 2, 4 or 8) of x`rt`.
fn mem_unscaled(bytes: &mut Vec<u8>, load: bool, rt: u32, (base, offset): (u32, i32), size: u8) {
    let size_bits = match size {
        1 => 0,
        2 => 1,
        4 => 2,
        8 => 3,
        _ => unreachable!("invalid access size: {size}"),
    };
    let op = if load { 0x3840_0000 } else { 0x3800_0000 };
    emit_u32(
        bytes,
        op | (size_bits << 30) | (bits(offset, 9) << 12) | (base << 5) | rt,
    );
}

/// Load the little-endian number in `len` cells from `cell` into x`rd`.
/// Reads up to 7 cells past the number.
fn load_word(bytes: &mut Vec<u8>, rd: u32, cell: i32, len: u8, far: Option<i32>) {
    let address = address(bytes, cell, far);
    let size = len.next_power_of_two();
    mem_unscaled(bytes, true, rd, address, size);
    if size != len {
        // ubfx xd, xd, #0, #(8 * len)
        emit_u32(
            bytes,
            0xd340_0000 | ((u32::from(len) * 8 - 1) << 10) | (rd << 5) | rd,
        );
    }
}

/// Store the low `len` bytes of x`rs` to the cells from `cell`, shifting x`rs`.
fn store_word(bytes: &mut Vec<u8>, rs: u32, cell: i32, len: u8, far: Option<i32>) {
    let (base, mut offset) = address(bytes, cell, far);
    let mut left = len;
    while left > 0 {
        // The largest power of two that fits
        let size = 1 << (7 - left.leading_zeros());
        mem_unscaled(bytes, false, rs, (base, offset), size);
        left -= size;
        offset += i32::from(size);
        if left > 0 {
            // lsr xs, xs, #(8 * size)
            let shift = u32::from(size) * 8;
            emit_u32(bytes, 0xd340_fc00 | (shift << 16) | (rs << 5) | rs);
        }
    }
}

/// Load a 64-bit constant into x`rd`.
fn mov_imm64(bytes: &mut Vec<u8>, rd: u32, value: u64) {
    // movz xd, #low
    emit_u32(
        bytes,
        0xd280_0000 | (u32::try_from(value & 0xffff).unwrap() << 5) | rd,
    );
    for shift in 1..4 {
        let part = u32::try_from(value >> (16 * shift) & 0xffff).unwrap();
        if part != 0 {
            // movk xd, #part, lsl #(16 * shift)
            emit_u32(bytes, 0xf280_0000 | (shift << 21) | (part << 5) | rd);
        }
    }
}

/// Compute a `Word` node in memory, using x8, x16 and x17.
fn word(
    bytes: &mut Vec<u8>,
    dst: i32,
    len: u8,
    terms: &[WordTerm],
    constant: u64,
    far: Option<i32>,
) {
    // Whether x8 holds the sum of the terms so far
    let mut sum = constant != 0 || terms.first().is_none_or(|term| term.negate);
    if sum {
        mov_imm64(bytes, TMP, constant);
    }
    for term in terms {
        let rd = if sum { ADDR + 1 } else { TMP };
        load_word(bytes, rd, term.cell, term.len, far);
        if let Some((cell, len)) = term.times {
            load_word(bytes, ADDR, cell, len, far);
            // mul xd, xd, x16
            emit_u32(bytes, 0x9b00_7c00 | (ADDR << 16) | (rd << 5) | rd);
        }
        if term.shift != 0 {
            // lsr xd, xd, #(8 * shift)
            let shift = u32::from(term.shift) * 8;
            emit_u32(bytes, 0xd340_fc00 | (shift << 16) | (rd << 5) | rd);
        }
        if sum {
            // add/sub x8, x8, x17
            let op = if term.negate {
                0xcb00_0000
            } else {
                0x8b00_0000
            };
            emit_u32(bytes, op | ((ADDR + 1) << 16) | (TMP << 5) | TMP);
        }
        sum = true;
    }
    store_word(bytes, TMP, dst, len, far);
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

pub fn skip(bytes: &mut Vec<u8>, exits: &[(i32, u8, u8)], steps: &[(i32, u8)]) {
    // w3 = the smallest number of steps until a cell reaches its target
    for (i, &(cell, target, factor)) in exits.iter().enumerate() {
        load_byte(bytes, 0, cell);
        // movz w1, #target
        emit_u32(bytes, 0x5280_0000 | (u32::from(target) << 5) | 1);
        // sub w1, w1, w0
        emit_u32(bytes, 0x4b00_0021);
        // movz w2, #factor
        emit_u32(bytes, 0x5280_0000 | (u32::from(factor) << 5) | 2);
        // mul w1, w1, w2
        emit_u32(bytes, 0x1b02_7c21);
        // and w1, w1, #0xff
        emit_u32(bytes, 0x1200_1c21);
        if i == 0 {
            // mov w3, w1
            emit_u32(bytes, 0x2a01_03e3);
        } else {
            // cmp w1, w3
            emit_u32(bytes, 0x6b03_003f);
            // csel w3, w1, w3, lo
            emit_u32(bytes, 0x1a83_3023);
        }
    }

    for &(cell, step) in steps {
        load_byte(bytes, 0, cell);
        // movz w2, #step
        emit_u32(bytes, 0x5280_0000 | (u32::from(step) << 5) | 2);
        // madd w0, w3, w2, w0
        emit_u32(bytes, 0x1b02_0060);
        store_byte(bytes, 0, cell);
    }
}

/// xd = xn + amount, using x16 for big amounts.
fn add_offset(bytes: &mut Vec<u8>, rd: u32, rn: u32, amount: i32) {
    if (0..4096).contains(&amount) {
        // add xd, xn, #amount
        emit_u32(
            bytes,
            0x9100_0000 | (bits(amount, 12) << 10) | (rn << 5) | rd,
        );
    } else if (-4095..0).contains(&amount) {
        // sub xd, xn, #-amount
        emit_u32(
            bytes,
            0xd100_0000 | (bits(-amount, 12) << 10) | (rn << 5) | rd,
        );
    } else {
        mov_imm(bytes, ADDR, amount);
        // add xd, xn, x16
        emit_u32(bytes, 0x8b00_0000 | (ADDR << 16) | (rn << 5) | rd);
    }
}

/// Add a value to x19.
fn add_to_pointer(bytes: &mut Vec<u8>, amount: i32) {
    add_offset(bytes, MEM, MEM, amount);
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
