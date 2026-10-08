use std::collections::{BTreeMap, HashMap};

use crate::parser::{AstNode, Operand};
use crate::runnable::jit::executable_memory::VTableEntry;
use crate::runnable::jit::jit_promise::JITPromiseID;

pub const RET: u8 = 0xc3;
const PTR_SIZE: u8 = 8;

// Register usage:
// r10 - BrainFuck memory pointer (current cell)
// r11 - JITTarget pointer
// r12 - VTable pointer
// eax, ecx - Temporary registers
// edx, ebx, ebp, esi, edi, r8d, r9d, r13d, r14d - Cached memory cells within
//   straight-line code. Only the low byte of a cached cell is meaningful.
// r15 - BrainFuck memory base pointer (for syscalls)

const RAX: u8 = 0;
const RCX: u8 = 1;
const RDX: u8 = 2;
const RBX: u8 = 3;
const RBP: u8 = 5;
const RSI: u8 = 6;
const RDI: u8 = 7;
const R8: u8 = 8;
const R9: u8 = 9;
const R13: u8 = 13;
const R14: u8 = 14;

const MEM: u8 = 10;
const TMP: u8 = RAX;
const TMP2: u8 = RCX;
const CACHE_REGS: [u8; 9] = [RDX, RBX, RBP, RSI, RDI, R8, R9, R13, R14];

// Condition codes
const CC_B: u8 = 0x2;
const CC_AE: u8 = 0x3;
const CC_NE: u8 = 0x5;

fn callee_save_to_stack(bytes: &mut Vec<u8>) {
    // push   rbx
    bytes.push(0x53);

    // push   rbp
    bytes.push(0x55);

    // push   rdi
    bytes.push(0x57);

    // push   rsi
    bytes.push(0x56);

    // push   rsp
    bytes.push(0x54);

    // push   r12
    bytes.push(0x41);
    bytes.push(0x54);

    // push   r13
    bytes.push(0x41);
    bytes.push(0x55);

    // push   r14
    bytes.push(0x41);
    bytes.push(0x56);

    // push   r15
    bytes.push(0x41);
    bytes.push(0x57);
}

pub fn wrapper(bytes: &mut Vec<u8>, content: Vec<u8>) {
    callee_save_to_stack(bytes);

    // Store pointer to brainfuck memory (first argument) in r10
    // mov    r10,rdi
    bytes.push(0x49);
    bytes.push(0x89);
    bytes.push(0xfa);

    // Also store base memory pointer in r15 for syscalls
    // mov    r15,rdi
    bytes.push(0x49);
    bytes.push(0x89);
    bytes.push(0xff);

    // Store pointer to JITTarget (second argument) in r11
    // mov    r11,rsi
    bytes.push(0x49);
    bytes.push(0x89);
    bytes.push(0xf3);

    // Store pointer to vtable (third argument) in r12
    // mov    r12,rdx
    bytes.push(0x49);
    bytes.push(0x89);
    bytes.push(0xd4);

    bytes.extend(content);

    // Return the data pointer
    // mov    rax,r10
    bytes.push(0x4c);
    bytes.push(0x89);
    bytes.push(0xd0);

    callee_restore_from_stack(bytes);

    // ret
    bytes.push(RET);
}

/// Wrapper for JIT fragments (deferred loops).
/// Unlike `wrapper`, this does NOT set r15 (base memory pointer) because
/// fragments are called from within the main program and should inherit
/// the base pointer from the caller.
pub fn wrapper_fragment(bytes: &mut Vec<u8>, content: Vec<u8>) {
    callee_save_to_stack(bytes);

    // Store pointer to brainfuck memory (first argument) in r10
    // mov    r10,rdi
    bytes.push(0x49);
    bytes.push(0x89);
    bytes.push(0xfa);

    // NOTE: We do NOT set r15 here - fragments inherit r15 from caller

    // Store pointer to JITTarget (second argument) in r11
    // mov    r11,rsi
    bytes.push(0x49);
    bytes.push(0x89);
    bytes.push(0xf3);

    // Store pointer to vtable (third argument) in r12
    // mov    r12,rdx
    bytes.push(0x49);
    bytes.push(0x89);
    bytes.push(0xd4);

    bytes.extend(content);

    // Return the data pointer
    // mov    rax,r10
    bytes.push(0x4c);
    bytes.push(0x89);
    bytes.push(0xd0);

    callee_restore_from_stack(bytes);

    // ret
    bytes.push(RET);
}

fn callee_restore_from_stack(bytes: &mut Vec<u8>) {
    // pop    r15
    bytes.push(0x41);
    bytes.push(0x5f);

    // pop    r14
    bytes.push(0x41);
    bytes.push(0x5e);

    // pop    r13
    bytes.push(0x41);
    bytes.push(0x5d);

    // pop    r12
    bytes.push(0x41);
    bytes.push(0x5c);

    // pop    rsp
    bytes.push(0x5c);

    // pop    rsi
    bytes.push(0x5e);

    // pop    rdi
    bytes.push(0x5f);

    // pop    rbp
    bytes.push(0x5d);

    // pop    rbx
    bytes.push(0x5b);
}

/// Emit `[r10 + offset]` for an instruction whose `ModRM` reg field is `reg`.
/// The instruction's REX prefix must include REX.B.
fn cell_operand(bytes: &mut Vec<u8>, reg: u8, offset: i32) {
    if let Ok(offset) = i8::try_from(offset) {
        // ModRM: mod=01 (disp8), reg, rm=010 (r10 with REX.B)
        bytes.push(0x42 | ((reg & 7) << 3));
        bytes.extend_from_slice(&offset.to_le_bytes());
    } else {
        // ModRM: mod=10 (disp32), reg, rm=010 (r10 with REX.B)
        bytes.push(0x82 | ((reg & 7) << 3));
        bytes.extend_from_slice(&offset.to_le_bytes());
    }
}

/// Emit a REX prefix for registers in the `ModRM` reg and rm fields if one is
/// needed. `byte_regs` forces one so that registers 4-7 refer to spl, bpl, sil
/// and dil rather than ah, ch, dh and bh.
fn rex(bytes: &mut Vec<u8>, reg: u8, rm: u8, byte_regs: bool) {
    let prefix = 0x40 | ((reg >> 3) << 2) | (rm >> 3);
    if prefix != 0x40 || byte_regs {
        bytes.push(prefix);
    }
}

/// Emit an instruction with register operands in the `ModRM` reg and rm fields.
/// `reg` may also be an opcode extension.
fn reg_op(bytes: &mut Vec<u8>, opcode: &[u8], reg: u8, rm: u8, byte_regs: bool) {
    rex(bytes, reg, rm, byte_regs);
    bytes.extend_from_slice(opcode);
    bytes.push(0xc0 | ((reg & 7) << 3) | (rm & 7));
}

/// Emit an instruction with a register (or opcode extension) in the `ModRM`
/// reg field and `[r10 + offset]` in the rm field.
fn mem_op(bytes: &mut Vec<u8>, opcode: &[u8], reg: u8, offset: i32, byte_regs: bool) {
    rex(bytes, reg, MEM, byte_regs);
    bytes.extend_from_slice(opcode);
    cell_operand(bytes, reg, offset);
}

/// movzx e<reg>, byte [r10 + offset]
fn load_cell(bytes: &mut Vec<u8>, reg: u8, offset: i32) {
    mem_op(bytes, &[0x0f, 0xb6], reg, offset, false);
}

/// mov byte [r10 + offset], <reg>b
fn store_cell(bytes: &mut Vec<u8>, reg: u8, offset: i32) {
    mem_op(bytes, &[0x88], reg, offset, true);
}

/// mov byte [r10 + offset], value
fn store_cell_imm(bytes: &mut Vec<u8>, offset: i32, value: u8) {
    mem_op(bytes, &[0xc6], 0, offset, false);
    bytes.push(value);
}

/// mov e<dst>, e<src>
fn mov(bytes: &mut Vec<u8>, dst: u8, src: u8) {
    reg_op(bytes, &[0x89], src, dst, false);
}

/// mov e<reg>, value
fn mov_imm(bytes: &mut Vec<u8>, reg: u8, value: u8) {
    rex(bytes, 0, reg, false);
    bytes.push(0xb8 | (reg & 7));
    bytes.extend_from_slice(&u32::from(value).to_le_bytes());
}

// Arithmetic operations, as encoded in the `ModRM` reg field of opcode 0x83
const ADD: u8 = 0;
const ADC: u8 = 2;
const SBB: u8 = 3;
const SUB: u8 = 5;
const XOR: u8 = 6;
const CMP: u8 = 7;

/// <op> e<reg>, value  (sign-extended, which doesn't change the low byte)
fn arith_imm(bytes: &mut Vec<u8>, op: u8, reg: u8, value: u8) {
    reg_op(bytes, &[0x83], op, reg, false);
    bytes.push(value);
}

/// <op> e<dst>, e<src>
fn arith(bytes: &mut Vec<u8>, op: u8, dst: u8, src: u8) {
    reg_op(bytes, &[(op << 3) | 0x01], src, dst, false);
}

/// cmp <lhs>b, <rhs>b
fn cmp_bytes(bytes: &mut Vec<u8>, lhs: u8, rhs: u8) {
    reg_op(bytes, &[0x38], rhs, lhs, true);
}

/// cmp <reg>b, value
fn cmp_byte_imm(bytes: &mut Vec<u8>, reg: u8, value: u8) {
    reg_op(bytes, &[0x80], CMP, reg, true);
    bytes.push(value);
}

/// test <reg>b, <reg>b
fn test_byte(bytes: &mut Vec<u8>, reg: u8) {
    reg_op(bytes, &[0x84], reg, reg, true);
}

/// lea e<dst>, [r<base> + r<index> * scale + disp]  (scale is 1, 2, 4 or 8)
fn lea(bytes: &mut Vec<u8>, dst: u8, base: u8, index: Option<(u8, u8)>, disp: u8) {
    // An index field of 100 without REX.X means no index.
    let (index, scale) = index.unwrap_or((0b100, 1));
    let shift = match scale {
        1 => 0,
        2 => 1,
        4 => 2,
        8 => 3,
        _ => unreachable!("invalid lea scale: {scale}"),
    };
    let prefix = 0x40 | ((dst >> 3) << 2) | ((index >> 3) << 1) | (base >> 3);
    if prefix != 0x40 {
        bytes.push(prefix);
    }
    bytes.push(0x8d);
    // ModRM: mod=01 (disp8), reg=dst, rm=100 (SIB)
    bytes.push(0x44 | ((dst & 7) << 3));
    // SIB: scale, index, base
    bytes.push((shift << 6) | ((index & 7) << 3) | (base & 7));
    bytes.push(disp);
}

/// imul e<dst>, e<src>, value  (sign-extended, which doesn't change the low byte)
fn imul_imm(bytes: &mut Vec<u8>, dst: u8, src: u8, value: u8) {
    reg_op(bytes, &[0x6b], dst, src, false);
    bytes.push(value);
}

/// cmov<cc> e<dst>, e<src>
fn cmov(bytes: &mut Vec<u8>, cc: u8, dst: u8, src: u8) {
    reg_op(bytes, &[0x0f, 0x40 | cc], dst, src, false);
}

/// set<cc> <reg>b
fn set(bytes: &mut Vec<u8>, cc: u8, reg: u8) {
    reg_op(bytes, &[0x0f, 0x90 | cc], 0, reg, true);
}

/// movzx e<dst>, <src>b
fn movzx(bytes: &mut Vec<u8>, dst: u8, src: u8) {
    reg_op(bytes, &[0x0f, 0xb6], dst, src, true);
}

/// A memory cell whose value is known, either in a register or as a constant.
#[derive(Clone, Copy)]
struct Entry {
    /// The register allocated to the cell, if any. Cells which aren't known
    /// constants always have one.
    reg: Option<u8>,
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
                store_cell(bytes, CACHE_REGS[slot], offset);
            }
            self.entries.remove(&offset);
        }
        slot
    }

    /// Allocate a register to a cell.
    fn allocate(&mut self, bytes: &mut Vec<u8>, offset: i32) -> u8 {
        let slot = match self.holders.iter().position(Option::is_none) {
            Some(slot) => slot,
            None => self.evict(bytes),
        };
        self.holders[slot] = Some(offset);
        CACHE_REGS[slot]
    }

    /// The known constant value of a cell, without loading it.
    fn constant(&self, offset: i32) -> Option<u8> {
        self.entries.get(&offset).and_then(|entry| entry.constant)
    }

    /// Get a register holding the value of a cell.
    fn read(&mut self, bytes: &mut Vec<u8>, offset: i32) -> u8 {
        let Some(entry) = self.entries.get(&offset).copied() else {
            let reg = self.allocate(bytes, offset);
            load_cell(bytes, reg, offset);
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
        mov_imm(bytes, reg, entry.constant.unwrap());
        let entry = self.entries.get_mut(&offset).unwrap();
        entry.reg = Some(reg);
        entry.materialized = true;
        reg
    }

    /// Get a register holding the value of a cell which is about to be modified.
    fn modify(&mut self, bytes: &mut Vec<u8>, offset: i32) -> u8 {
        let reg = self.read(bytes, offset);
        let entry = self.entries.get_mut(&offset).unwrap();
        entry.dirty = true;
        entry.constant = None;
        reg
    }

    /// Get a register for a cell which is about to be overwritten.
    fn overwrite(&mut self, bytes: &mut Vec<u8>, offset: i32) -> u8 {
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

    fn write_back(bytes: &mut Vec<u8>, offset: i32, entry: Entry) {
        if !entry.dirty {
            return;
        }
        match (entry.constant, entry.reg) {
            (Some(value), _) if !entry.materialized => store_cell_imm(bytes, offset, value),
            (_, Some(reg)) => store_cell(bytes, reg, offset),
            _ => unreachable!("cell {offset} has no value"),
        }
    }

    fn flush(self, bytes: &mut Vec<u8>) {
        for (offset, entry) in self.entries {
            Self::write_back(bytes, offset, entry);
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
                    arith_imm(bytes, ADD, reg, value);
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
            _ => unreachable!("not a straight-line node: {node:?}"),
        }
    }

    /// Get the register and known constant value of a cell which is about to
    /// be conditionally added to, for `conditional_add`.
    fn conditional_dst(&mut self, bytes: &mut Vec<u8>, offset: i32) -> (u8, Option<u8>) {
        match self.constant(offset) {
            Some(constant) => (self.overwrite(bytes, offset), Some(constant)),
            None => (self.modify(bytes, offset), None),
        }
    }

    /// Compile a byte-wise addition with carry out:
    ///
    ///     CondAdd { lhs: ~x, rhs: y, dst: carry, value }
    ///     MulAdd { src: y, dst: x, factor: 1 }
    ///
    /// `~x < y` exactly when adding `y` to `x` carries, so this is a byte add
    /// followed by adding the carry flag. Returns false if the nodes don't
    /// match.
    fn add_with_carry(&mut self, bytes: &mut Vec<u8>, node: &AstNode, next: &AstNode) -> bool {
        let AstNode::CondAdd {
            lhs,
            rhs,
            dst,
            value,
        } = *node
        else {
            return false;
        };
        let (x, y) = (lhs.cell, rhs.cell);
        if (lhs.scale, lhs.bias, rhs.scale, rhs.bias) != (u8::MAX, u8::MAX, 1, 0)
            || *next
                != (AstNode::MulAdd {
                    src: y,
                    dst: x,
                    factor: 1,
                })
            || x == y
            || dst == x
            || dst == y
            || self.constant(x).is_some()
            || self.constant(y).is_some()
        {
            return false;
        }

        let y_reg = self.read(bytes, y);
        let x_reg = self.modify(bytes, x);
        let dst = self.conditional_dst(bytes, dst);
        conditional_add(bytes, dst, value, |bytes| {
            // add <x>b, <y>b
            reg_op(bytes, &[0x00], y_reg, x_reg, true);
            CC_B
        });
        true
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
                    if self.constant(offset) != Some(value) {
                        regs.push((self.modify(bytes, offset), value));
                    }
                }
                let counter = self.read(bytes, counter);
                test_byte(bytes, counter);
                for (reg, value) in regs {
                    mov_imm(bytes, TMP, value);
                    cmov(bytes, CC_NE, reg, TMP);
                }
            }
        }
        self.set(counter, 0);
    }

    /// Compile straight-line steps in order.
    fn compile(&mut self, bytes: &mut Vec<u8>, steps: &[Step]) {
        let mut position = 0;
        while position < steps.len() {
            self.position = position;
            match (&steps[position], steps.get(position + 1)) {
                (Step::Node(node), Some(Step::Node(next)))
                    if self.add_with_carry(bytes, node, next) =>
                {
                    position += 1;
                }
                (Step::Node(node), _) => self.node(bytes, node),
                (Step::SetIf { counter, sets }, _) => self.set_if(bytes, *counter, sets),
            }
            position += 1;
        }
    }
}

/// A straight-line operation, with offsets relative to the data pointer at
/// the start of its run.
enum Step {
    /// An `Add`, `Set`, `MulAdd`, `CondAdd` or `ProductAdd` node
    Node(AstNode),
    /// A loop which only sets cells, including its counter to 0, so it runs at
    /// most once
    SetIf { counter: i32, sets: Vec<(i32, u8)> },
}

/// The most cells (other than the counter) a loop compiled as a
/// `Step::SetIf` may set.
const MAX_SET_IF_CELLS: usize = 4;

/// The cells (other than the counter) and values set by a loop which only sets
/// cells and runs at most once.
fn set_if_cells(body: &[AstNode]) -> Option<Vec<(i32, u8)>> {
    let mut sets: Vec<(i32, u8)> = Vec::new();
    for node in body {
        let AstNode::Set(offset, value) = *node else {
            return None;
        };
        sets.retain(|&(cell, _)| cell != offset);
        sets.push((offset, value));
    }
    if !sets.contains(&(0, 0)) {
        return None;
    }
    sets.retain(|&(cell, _)| cell != 0);
    (sets.len() <= MAX_SET_IF_CELLS).then_some(sets)
}

/// Whether a node can be part of a run compiled by `straight_line`.
pub fn is_straight_line(node: &AstNode) -> bool {
    match node {
        AstNode::Add(..)
        | AstNode::Set(..)
        | AstNode::MulAdd { .. }
        | AstNode::CondAdd { .. }
        | AstNode::ProductAdd { .. }
        | AstNode::Move(_) => true,
        AstNode::Loop(body) => set_if_cells(body).is_some(),
        _ => false,
    }
}

/// A straight-line node with its offsets moved by `amount`.
fn shifted(node: &AstNode, amount: i32) -> AstNode {
    match *node {
        AstNode::Add(offset, value) => AstNode::Add(offset + amount, value),
        AstNode::Set(offset, value) => AstNode::Set(offset + amount, value),
        AstNode::MulAdd { src, dst, factor } => AstNode::MulAdd {
            src: src + amount,
            dst: dst + amount,
            factor,
        },
        AstNode::CondAdd {
            lhs,
            rhs,
            dst,
            value,
        } => AstNode::CondAdd {
            lhs: lhs.shift(amount),
            rhs: rhs.shift(amount),
            dst: dst + amount,
            value,
        },
        AstNode::ProductAdd {
            base,
            step,
            count,
            high,
            dst,
            value,
        } => AstNode::ProductAdd {
            base: base.shift(amount),
            step: step.shift(amount),
            count: count.shift(amount),
            high,
            dst: dst + amount,
            value,
        },
        _ => unreachable!("not a straight-line node: {node:?}"),
    }
}

/// Convert a run of straight-line nodes into steps relative to the data
/// pointer at its start, returning them with the run's pointer movement.
fn steps(nodes: &[AstNode]) -> (Vec<Step>, i32) {
    let mut moved = 0;
    let mut steps = Vec::new();
    for node in nodes {
        match node {
            AstNode::Move(amount) => moved += amount,
            AstNode::Loop(body) => steps.push(Step::SetIf {
                counter: moved,
                sets: set_if_cells(body)
                    .unwrap()
                    .into_iter()
                    .map(|(offset, value)| (offset + moved, value))
                    .collect(),
            }),
            node => steps.push(Step::Node(shifted(node, moved))),
        }
    }
    (steps, moved)
}

/// Compile a run of nodes for which `is_straight_line` holds. Cells are kept
/// in registers and written back at the end, and the data pointer is moved
/// once.
pub fn straight_line(bytes: &mut Vec<u8>, nodes: &[AstNode]) {
    let (steps, moved) = steps(nodes);
    let mut cache = CellCache::new(&steps);
    cache.compile(bytes, &steps);
    cache.flush(bytes);
    if moved != 0 {
        move_pointer(bytes, moved);
    }
}

/// The cells a straight-line step accesses.
fn accessed_cells(step: &Step) -> Vec<i32> {
    let node = match step {
        Step::Node(node) => node,
        Step::SetIf { counter, sets } => {
            return std::iter::once(*counter)
                .chain(sets.iter().map(|&(offset, _)| offset))
                .collect();
        }
    };
    match *node {
        AstNode::Add(offset, _) | AstNode::Set(offset, _) => vec![offset],
        AstNode::MulAdd { src, dst, .. } => vec![src, dst],
        AstNode::CondAdd { lhs, rhs, dst, .. } => lhs
            .reads()
            .into_iter()
            .chain(rhs.reads())
            .chain([dst])
            .collect(),
        AstNode::ProductAdd {
            base,
            step,
            count,
            dst,
            ..
        } => [base, step, count]
            .iter()
            .filter_map(|operand| operand.reads())
            .chain([dst])
            .collect(),
        _ => unreachable!("not a straight-line node: {node:?}"),
    }
}

/// Compile a loop whose body is straight-line code which doesn't move the
/// data pointer overall, keeping all cells in registers across iterations.
/// Returns false if there are too many cells.
pub fn register_loop(bytes: &mut Vec<u8>, nodes: &[AstNode]) -> bool {
    let (steps, moved) = steps(nodes);
    if moved != 0 {
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

    let mut head = Vec::new();
    let mut cache = CellCache::default();
    for &cell in &cells {
        cache.read(&mut head, cell);
    }
    let counter = cache.read(&mut head, 0);

    let mut body = Vec::new();
    cache.compile(&mut body, &steps);
    // A loop which sets its counter runs at most once (or forever), so loading
    // every cell up front doesn't pay off.
    if cache.constant(0).is_some() {
        return false;
    }
    bytes.extend(head);

    // Every cell must be in its register at the end of each iteration.
    for &cell in &cells {
        cache.read(&mut body, cell);
    }
    test_byte(&mut body, counter);

    // Length of the body including the jnz at its end
    let body_len = i32::try_from(body.len()).unwrap() + 6;
    test_byte(bytes, counter);
    // jz     end
    bytes.extend_from_slice(&[0x0f, 0x84]);
    bytes.extend_from_slice(&body_len.to_le_bytes());
    bytes.extend(body);
    // jnz    start
    bytes.extend_from_slice(&[0x0f, 0x85]);
    bytes.extend_from_slice(&(-body_len).to_le_bytes());

    cache.flush(bytes);
    true
}

/// e<rd> = e<rn> * factor, where rn and rd differ.
fn multiply(bytes: &mut Vec<u8>, rn: u8, rd: u8, factor: u8) {
    match factor {
        1 => mov(bytes, rd, rn),
        u8::MAX => {
            mov(bytes, rd, rn);
            // neg e<rd>
            reg_op(bytes, &[0xf7], 3, rd, false);
        }
        // lea e<rd>, [r<rn> + r<rn> * (factor - 1)]
        2 | 3 | 5 | 9 => lea(bytes, rd, rn, Some((rn, factor - 1)), 0),
        _ => imul_imm(bytes, rd, rn, factor),
    }
}

/// e<rd> += e<rn> * factor
fn multiply_add(bytes: &mut Vec<u8>, rn: u8, rd: u8, factor: u8) {
    match factor {
        // e<rd> * 255 == -e<rd>
        // neg e<rd>
        254 if rn == rd => reg_op(bytes, &[0xf7], 3, rd, false),
        1 => arith(bytes, ADD, rd, rn),
        u8::MAX => arith(bytes, SUB, rd, rn),
        // lea e<rd>, [r<rd> + r<rn> * factor]
        2 | 4 | 8 => lea(bytes, rd, rd, Some((rn, factor)), 0),
        _ => {
            imul_imm(bytes, TMP, rn, factor);
            arith(bytes, ADD, rd, TMP);
        }
    }
}

/// Get a register whose low byte holds the value of an operand which reads
/// the cell in register `reg`, computing it in `tmp` if needed.
fn operand_reg(bytes: &mut Vec<u8>, operand: Operand, reg: u8, tmp: u8) -> u8 {
    match (operand.scale, operand.bias) {
        (1, 0) => return reg,
        // lea e<tmp>, [r<reg> + bias]
        (1, bias) => lea(bytes, tmp, reg, None, bias),
        (u8::MAX, u8::MAX) => {
            mov(bytes, tmp, reg);
            // not e<tmp>
            reg_op(bytes, &[0xf7], 2, tmp, false);
        }
        (scale @ (2 | 3 | 5 | 9), bias) => {
            // lea e<tmp>, [r<reg> + r<reg> * (scale - 1) + bias]
            lea(bytes, tmp, reg, Some((reg, scale - 1)), bias);
        }
        (scale, bias) => {
            imul_imm(bytes, tmp, reg, scale);
            if bias != 0 {
                arith_imm(bytes, ADD, tmp, bias);
            }
        }
    }
    tmp
}

/// Compare `lhs < rhs`, returning the condition code which holds if it's true.
/// At least one operand must read a cell, and a constant `lhs` must be below
/// 255.
fn compare(bytes: &mut Vec<u8>, lhs: (Operand, Option<u8>), rhs: (Operand, Option<u8>)) -> u8 {
    match (lhs, rhs) {
        ((l, None), (r, Some(reg))) => {
            // lhs < rhs <=> rhs >= lhs + 1
            let reg = operand_reg(bytes, r, reg, TMP2);
            cmp_byte_imm(bytes, reg, l.bias + 1);
            CC_AE
        }
        ((l, Some(reg)), (r, None)) => {
            let reg = operand_reg(bytes, l, reg, TMP);
            cmp_byte_imm(bytes, reg, r.bias);
            CC_B
        }
        ((l, Some(lreg)), (r, Some(rreg))) => {
            let lreg = operand_reg(bytes, l, lreg, TMP);
            let rreg = operand_reg(bytes, r, rreg, TMP2);
            cmp_bytes(bytes, lreg, rreg);
            CC_B
        }
        ((_, None), (_, None)) => unreachable!("comparison of constants"),
    }
}

/// e<rd> += value if a condition holds, without branching. `compare` sets the
/// flags and returns the condition code (`CC_B` or `CC_AE`). `rd` comes with
/// the cell's constant value if it's known, in which case the register
/// doesn't hold it yet.
fn conditional_add(
    bytes: &mut Vec<u8>,
    (rd, constant): (u8, Option<u8>),
    value: u8,
    compare: impl FnOnce(&mut Vec<u8>) -> u8,
) {
    // Whether the result is just the condition (or its inverse), as a byte
    let flag = matches!((constant, value), (Some(0), 1) | (Some(1), u8::MAX));

    // Prepare the register before the comparison sets the flags.
    match constant {
        // xor e<rd>, e<rd>
        Some(_) if flag => arith(bytes, XOR, rd, rd),
        Some(constant) => mov_imm(bytes, rd, constant),
        None => {}
    }

    let cc = compare(bytes);
    if flag {
        // Inverting a condition code flips its lowest bit.
        let cc = if constant == Some(0) { cc } else { cc ^ 1 };
        set(bytes, cc, rd);
        return;
    }

    // The carry flag is set if the condition is CC_B, and clear if it's CC_AE.
    match (value, cc) {
        (1, CC_B) => arith_imm(bytes, ADC, rd, 0),
        // e<rd> - -1 - CF
        (1, _) => arith_imm(bytes, SBB, rd, u8::MAX),
        (u8::MAX, CC_B) => arith_imm(bytes, SBB, rd, 0),
        // e<rd> + -1 + CF
        (u8::MAX, _) => arith_imm(bytes, ADC, rd, u8::MAX),
        _ => {
            // lea e<tmp>, [r<rd> + value]
            lea(bytes, TMP, rd, None, value);
            cmov(bytes, cc, rd, TMP);
        }
    }
}

/// Load the byte value of an operand into `tmp`, zero-extended.
fn byte_value(bytes: &mut Vec<u8>, (operand, reg): (Operand, Option<u8>), tmp: u8) {
    match reg {
        Some(reg) => {
            let reg = operand_reg(bytes, operand, reg, tmp);
            movzx(bytes, tmp, reg);
        }
        None => mov_imm(bytes, tmp, operand.bias),
    }
}

/// e<rd> += value * byte of (base + step * count)
fn product_add(
    bytes: &mut Vec<u8>,
    [base, step, count]: [(Operand, Option<u8>); 3],
    high: bool,
    rd: u8,
    value: u8,
) {
    byte_value(bytes, step, TMP);
    byte_value(bytes, count, TMP2);
    // imul eax, ecx
    reg_op(bytes, &[0x0f, 0xaf], TMP, TMP2, false);
    byte_value(bytes, base, TMP2);
    arith(bytes, ADD, TMP, TMP2);
    if high {
        // shr eax, 8
        reg_op(bytes, &[0xc1], 5, TMP, false);
        bytes.push(8);
    }
    multiply_add(bytes, TMP, rd, value);
}

pub fn move_pointer(bytes: &mut Vec<u8>, amount: i32) {
    // add r10, amount
    bytes.extend_from_slice(&[0x49, 0x81, 0xc2]);
    bytes.extend_from_slice(&amount.to_le_bytes());
}

fn fn_call_pre(bytes: &mut Vec<u8>) {
    // Push data pointer onto stack
    // push    r10
    bytes.push(0x41);
    bytes.push(0x52);

    // Push JITTarget pointer onto stack
    // push   r11
    bytes.push(0x41);
    bytes.push(0x53);

    // Push vtable pointer onto stack
    // push   r12
    bytes.push(0x41);
    bytes.push(0x54);

    // Keep the stack 16 byte aligned for the call
    // push   r13
    bytes.push(0x41);
    bytes.push(0x55);
}

fn fn_call_post(bytes: &mut Vec<u8>) {
    // pop    r13
    bytes.push(0x41);
    bytes.push(0x5d);

    // Pop vtable pointer from the stack
    // pop    r12
    bytes.push(0x41);
    bytes.push(0x5c);

    // Pop JITTarget pointer from the stack
    // pop    r11
    bytes.push(0x41);
    bytes.push(0x5b);

    // Pop data pointer from the stack
    // pop    r10
    bytes.push(0x41);
    bytes.push(0x5a);
}

/// Make a call to a vtable entry in r12.
fn call_vtable_entry(bytes: &mut Vec<u8>, entry: VTableEntry) {
    // Call function pointer from vtable at index
    // call   QWORD PTR [r12+index]
    bytes.push(0x41);
    bytes.push(0xff);
    bytes.push(0x54);
    bytes.push(0x24);
    bytes.push((entry as u8) * PTR_SIZE);
}

pub fn print(bytes: &mut Vec<u8>, offset: i32) {
    fn_call_pre(bytes);

    // Move the JITTarget pointer into the first argument register
    // mov    rdi,r11
    bytes.push(0x4c);
    bytes.push(0x89);
    bytes.push(0xdf);

    // Move the memory cell into the second argument register
    // movzx  esi, byte [r10 + offset]
    bytes.extend_from_slice(&[0x41, 0x0f, 0xb6]);
    cell_operand(bytes, 6, offset);

    call_vtable_entry(bytes, VTableEntry::Print);

    fn_call_post(bytes);
}

pub fn read(bytes: &mut Vec<u8>, offset: i32) {
    fn_call_pre(bytes);

    // Move the JITTarget pointer into the first argument register
    // mov    rdi,r11
    bytes.push(0x4c);
    bytes.push(0x89);
    bytes.push(0xdf);

    call_vtable_entry(bytes, VTableEntry::Read);

    fn_call_post(bytes);

    // Copy return value into the memory cell.
    // mov    byte [r10 + offset], al
    bytes.extend_from_slice(&[0x41, 0x88]);
    cell_operand(bytes, 0, offset);
}

pub fn scan(bytes: &mut Vec<u8>, stride: i32) {
    // loop:
    // cmp    byte [r10], 0
    bytes.extend_from_slice(&[0x41, 0x80, 0x3a, 0x00]);
    // je     done (over the add and jmp)
    bytes.extend_from_slice(&[0x74, 9]);
    move_pointer(bytes, stride);
    // jmp    loop
    bytes.extend_from_slice(&[0xeb, 0xf1]);
    // done:
}

/// Emit a loop. `trailing_move` is a pointer movement at the end of the body.
pub fn aot_loop(bytes: &mut Vec<u8>, inner_loop_bytes: Vec<u8>, trailing_move: i32) {
    let mut inner_loop_bytes = inner_loop_bytes;
    if trailing_move != 0 {
        move_pointer(&mut inner_loop_bytes, trailing_move);
    }

    let inner_loop_size = i32::try_from(inner_loop_bytes.len()).unwrap();

    let end_loop_size: i32 = 10; // Bytes
    let byte_offset = inner_loop_size + end_loop_size;

    // Check if the current memory cell equals zero.
    // cmp    BYTE PTR [r10],0x0
    bytes.extend_from_slice(&[0x41, 0x80, 0x3a, 0x00]);

    // Jump to the end of the loop if equal.
    // je    offset
    bytes.extend_from_slice(&[0x0f, 0x84]);
    bytes.extend_from_slice(&byte_offset.to_le_bytes());

    bytes.extend(inner_loop_bytes);

    // Check if the current memory cell equals zero.
    // cmp    BYTE PTR [r10],0x0
    bytes.extend_from_slice(&[0x41, 0x80, 0x3a, 0x00]);

    // Jump back to the beginning of the loop if not equal.
    // jne    offset
    bytes.extend_from_slice(&[0x0f, 0x85]);
    bytes.extend_from_slice(&(-byte_offset).to_le_bytes());
}

pub fn jit_loop(bytes: &mut Vec<u8>, loop_id: JITPromiseID) {
    // Call the compiled fragment directly if there is one.
    // mov    rax, [r12 + fragments]
    bytes.extend_from_slice(&[
        0x49,
        0x8b,
        0x44,
        0x24,
        (VTableEntry::Fragments as u8) * PTR_SIZE,
    ]);
    // mov    rax, [rax + loop_id * 8]
    bytes.extend_from_slice(&[0x48, 0x8b, 0x80]);
    bytes.extend_from_slice(&(u32::from(loop_id.value()) * 8).to_le_bytes());
    // test   rax, rax
    bytes.extend_from_slice(&[0x48, 0x85, 0xc0]);
    // jz     callback (over the direct call below)
    bytes.extend_from_slice(&[0x74, 24]);
    // push   r11
    // push   r12
    bytes.extend_from_slice(&[0x41, 0x53, 0x41, 0x54]);
    // mov    rdi, r10
    // mov    rsi, r11
    // mov    rdx, r12
    bytes.extend_from_slice(&[0x4c, 0x89, 0xd7, 0x4c, 0x89, 0xde, 0x4c, 0x89, 0xe2]);
    // call   rax
    bytes.extend_from_slice(&[0xff, 0xd0]);
    // pop    r12
    // pop    r11
    bytes.extend_from_slice(&[0x41, 0x5c, 0x41, 0x5b]);
    // mov    r10, rax
    bytes.extend_from_slice(&[0x49, 0x89, 0xc2]);
    // jmp    done (over the callback below)
    bytes.extend_from_slice(&[0xeb, 27]);

    // callback:
    // Push JITTarget pointer onto stack
    // push   r11
    bytes.push(0x41);
    bytes.push(0x53);

    // Push vtable pointer onto stack
    // push   r12
    bytes.push(0x41);
    bytes.push(0x54);

    // Move the JITTarget pointer into the first argument
    // mov    rdi,r11
    bytes.push(0x4c);
    bytes.push(0x89);
    bytes.push(0xdf);

    // Move target index into the second argument (zero-extended to 64 bits)
    // mov    esi, loop_id
    bytes.push(0xbe);
    bytes.extend_from_slice(&u32::from(loop_id.value()).to_le_bytes());

    // Move data pointer into the third argument
    // mov rdx,r10
    bytes.push(0x4c);
    bytes.push(0x89);
    bytes.push(0xd2);

    call_vtable_entry(bytes, VTableEntry::JITCallback);

    // Take return value and store as the new data pointer
    // mov    r10,rax
    bytes.push(0x49);
    bytes.push(0x89);
    bytes.push(0xc2);

    // Pop vtable pointer from the stack
    // pop    r12
    bytes.push(0x41);
    bytes.push(0x5c);

    // Pop JITTarget pointer from the stack
    // pop    r11
    bytes.push(0x41);
    bytes.push(0x5b);
}

pub fn syscall(bytes: &mut Vec<u8>) {
    fn_call_pre(bytes);

    // Move the JITTarget pointer into the first argument register
    // mov    rdi,r11
    bytes.push(0x4c);
    bytes.push(0x89);
    bytes.push(0xdf);

    // Move the current memory pointer into the second argument register
    // mov    rsi,r10
    bytes.push(0x4c);
    bytes.push(0x89);
    bytes.push(0xd6);

    // Move the base memory pointer into the third argument register
    // mov    rdx,r15
    bytes.push(0x4c);
    bytes.push(0x89);
    bytes.push(0xfa);

    call_vtable_entry(bytes, VTableEntry::Syscall);

    fn_call_post(bytes);

    // Copy return value into current cell.
    // mov    BYTE PTR [r10],al
    bytes.push(0x41);
    bytes.push(0x88);
    bytes.push(0x02);
}
