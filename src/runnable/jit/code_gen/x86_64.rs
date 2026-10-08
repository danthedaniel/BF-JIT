use crate::parser::{AstNode, Operand};
use crate::runnable::jit::executable_memory::VTableEntry;
use crate::runnable::jit::jit_promise::JITPromiseID;

pub const RET: u8 = 0xc3;
const PTR_SIZE: u8 = 8;

// Register usage:
// r10 - BrainFuck memory pointer (current cell)
// r11 - JITTarget pointer
// r12 - VTable pointer
// eax, ecx, edx, r8, r9 - Temporary registers
// r15 - BrainFuck memory base pointer (for syscalls)

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
    // ModRM: mod=10 (disp32), reg, rm=010 (r10 with REX.B)
    bytes.push(0x82 | (reg << 3));
    bytes.extend_from_slice(&offset.to_le_bytes());
}

/// movzx e<reg>, byte [r10 + offset]  (reg is eax=0, ecx=1 or edx=2)
fn load_cell(bytes: &mut Vec<u8>, reg: u8, offset: i32) {
    bytes.extend_from_slice(&[0x41, 0x0f, 0xb6]);
    cell_operand(bytes, reg, offset);
}

/// Load an operand's value into e<reg> (eax=0, ecx=1 or edx=2), zero-extended from a byte.
fn load_operand(bytes: &mut Vec<u8>, reg: u8, operand: Operand) {
    if operand.scale == 0 {
        // mov e<reg>, imm32
        bytes.push(0xb8 + reg);
        bytes.extend_from_slice(&u32::from(operand.bias).to_le_bytes());
        return;
    }

    load_cell(bytes, reg, operand.cell);
    if operand.scale != 1 {
        // imul e<reg>, e<reg>, imm8
        bytes.extend_from_slice(&[0x6b, 0xc0 | (reg << 3) | reg, operand.scale]);
    }
    if operand.bias != 0 {
        // add e<reg>, imm8
        bytes.extend_from_slice(&[0x83, 0xc0 | reg, operand.bias]);
    }
    // movzx e<reg>, <reg>l
    bytes.extend_from_slice(&[0x0f, 0xb6, 0xc0 | (reg << 3) | reg]);
}

/// Compile a run of `Add`, `Set`, `MulAdd` and `CondAdd` nodes.
pub fn straight_line(bytes: &mut Vec<u8>, nodes: &[AstNode]) {
    for node in nodes {
        match *node {
            AstNode::Add(offset, value) => {
                // add byte [r10 + offset], value
                bytes.extend_from_slice(&[0x41, 0x80]);
                cell_operand(bytes, 0, offset);
                bytes.push(value);
            }
            AstNode::Set(offset, value) => {
                // mov byte [r10 + offset], value
                bytes.extend_from_slice(&[0x41, 0xc6]);
                cell_operand(bytes, 0, offset);
                bytes.push(value);
            }
            AstNode::MulAdd { src, dst, factor } => {
                load_cell(bytes, 0, src);
                let opcode = if factor == u8::MAX {
                    // sub byte [r10 + dst], al
                    0x28
                } else {
                    if factor != 1 {
                        // imul eax, eax, factor
                        bytes.extend_from_slice(&[0x6b, 0xc0, factor]);
                    }
                    // add byte [r10 + dst], al
                    0x00
                };
                bytes.extend_from_slice(&[0x41, opcode]);
                cell_operand(bytes, 0, dst);
            }
            AstNode::CondAdd {
                lhs,
                rhs,
                dst,
                value,
            } => {
                load_operand(bytes, 0, lhs);
                load_operand(bytes, 1, rhs);
                // cmp eax, ecx
                bytes.extend_from_slice(&[0x39, 0xc8]);
                // jae over the add
                bytes.extend_from_slice(&[0x73, 8]);
                // add byte [r10 + dst], value
                bytes.extend_from_slice(&[0x41, 0x80]);
                cell_operand(bytes, 0, dst);
                bytes.push(value);
            }
            AstNode::ProductAdd {
                base,
                step,
                count,
                high,
                dst,
                value,
            } => {
                load_operand(bytes, 0, base);
                load_operand(bytes, 1, step);
                load_operand(bytes, 2, count);
                // imul ecx, edx
                bytes.extend_from_slice(&[0x0f, 0xaf, 0xca]);
                // add eax, ecx
                bytes.extend_from_slice(&[0x01, 0xc8]);
                if high {
                    // shr eax, 8
                    bytes.extend_from_slice(&[0xc1, 0xe8, 0x08]);
                }
                if value != 1 {
                    // imul eax, eax, value
                    bytes.extend_from_slice(&[0x6b, 0xc0, value]);
                }
                // add byte [r10 + dst], al
                bytes.extend_from_slice(&[0x41, 0x00]);
                cell_operand(bytes, 0, dst);
            }
            _ => unreachable!("not a straight-line node: {node:?}"),
        }
    }
}

/// Load the little-endian number in `len` cells from `start` into rax
/// (`reg` = 0) or r8 (`reg` = 8), using r9 as a temporary.
fn load_number(bytes: &mut Vec<u8>, reg: u8, start: i32, len: u8) {
    let rex_b = reg >> 3;
    // xor e<reg>, e<reg>
    bytes.extend_from_slice(&[0x40 | (rex_b * 5), 0x31, 0xc0]);
    for i in (0..i32::from(len)).rev() {
        // shl <reg>, 8
        bytes.extend_from_slice(&[0x48 | rex_b, 0xc1, 0xe0, 8]);
        // movzx r9d, byte [r10 + offset]
        bytes.extend_from_slice(&[0x45, 0x0f, 0xb6]);
        cell_operand(bytes, 1, start + i);
        // or <reg>, r9
        bytes.extend_from_slice(&[0x4c | rex_b, 0x09, 0xc8]);
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

    load_number(bytes, 8, divisor, divisor_len);

    let mut divide = Vec::new();
    load_number(&mut divide, 0, dividend, dividend_len);
    // xor edx, edx
    divide.extend_from_slice(&[0x31, 0xd2]);
    // div r8
    divide.extend_from_slice(&[0x49, 0xf7, 0xf0]);
    for i in 0..i32::from(dividend_len) {
        // mov byte [r10 + offset], dl
        divide.extend_from_slice(&[0x41, 0x88]);
        cell_operand(&mut divide, 2, dividend + i);
        // shr rdx, 8
        divide.extend_from_slice(&[0x48, 0xc1, 0xea, 8]);
    }
    if factor != 0 {
        if factor != 1 {
            // imul eax, eax, factor
            divide.extend_from_slice(&[0x6b, 0xc0, factor]);
        }
        // add byte [r10 + quotient], al
        divide.extend_from_slice(&[0x41, 0x00]);
        cell_operand(&mut divide, 0, quotient);
    }

    // test r8, r8
    bytes.extend_from_slice(&[0x4d, 0x85, 0xc0]);
    // jz over the division
    bytes.extend_from_slice(&[0x0f, 0x84]);
    bytes.extend_from_slice(&i32::try_from(divide.len()).unwrap().to_le_bytes());
    bytes.extend(divide);
}

pub fn skip(bytes: &mut Vec<u8>, exits: &[(i32, u8, u8)], steps: &[(i32, u8)]) {
    // edx = the smallest number of steps until a cell reaches its target
    for (i, &(cell, target, factor)) in exits.iter().enumerate() {
        // mov eax, target
        bytes.push(0xb8);
        bytes.extend_from_slice(&u32::from(target).to_le_bytes());
        load_cell(bytes, 1, cell);
        // sub eax, ecx
        bytes.extend_from_slice(&[0x29, 0xc8]);
        // imul eax, eax, factor
        bytes.extend_from_slice(&[0x69, 0xc0]);
        bytes.extend_from_slice(&u32::from(factor).to_le_bytes());
        // movzx eax, al
        bytes.extend_from_slice(&[0x0f, 0xb6, 0xc0]);
        if i == 0 {
            // mov edx, eax
            bytes.extend_from_slice(&[0x89, 0xc2]);
        } else {
            // cmp eax, edx
            bytes.extend_from_slice(&[0x39, 0xd0]);
            // cmovb edx, eax
            bytes.extend_from_slice(&[0x0f, 0x42, 0xd0]);
        }
    }

    for &(cell, step) in steps {
        // imul eax, edx, step
        bytes.extend_from_slice(&[0x69, 0xc2]);
        bytes.extend_from_slice(&u32::from(step).to_le_bytes());
        // add byte [r10 + cell], al
        bytes.extend_from_slice(&[0x41, 0x00]);
        cell_operand(bytes, 0, cell);
    }
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
