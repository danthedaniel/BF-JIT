use crate::runnable::jit::jit_promise::JITPromiseID;
use crate::runnable::jit::jit_target::VTableEntry;

pub const RET_BYTES: [u8; 4] = [0xd6, 0x5f, 0x03, 0xc0];
const PTR_SIZE: u32 = 8;

// ARM64 register usage:
// x19 - BrainFuck memory pointer (callee-saved)
// x20 - JITTarget pointer (callee-saved)
// x21 - VTable pointer (callee-saved)
// x0-x7 - Function arguments and return values
// x8-x18 - Temporary registers
// x29 - Frame pointer
// x30 - Link register

fn emit_u32(bytes: &mut Vec<u8>, instruction: u32) {
    bytes.extend_from_slice(&instruction.to_le_bytes());
}

fn encode_signed_imm9(offset: isize) -> u32 {
    ((offset as u32) & 0x1FF) << 12
}

fn load_immediate_x9(bytes: &mut Vec<u8>, value: isize) {
    let value_u64 = value as u64;

    // movz x9, #(value & 0xFFFF)
    emit_u32(bytes, 0xd2800009 | (((value_u64 & 0xFFFF) as u32) << 5));

    if value_u64 > 0xFFFF || value < 0 {
        // movk x9, #((value >> 16) & 0xFFFF), lsl #16
        emit_u32(
            bytes,
            0xf2a00009 | ((((value_u64 >> 16) & 0xFFFF) as u32) << 5),
        );
    }

    if value_u64 > 0xFFFFFFFF || value < 0 {
        // movk x9, #((value >> 32) & 0xFFFF), lsl #32
        emit_u32(
            bytes,
            0xf2c00009 | ((((value_u64 >> 32) & 0xFFFF) as u32) << 5),
        );
    }

    if value < 0 {
        // movk x9, #((value >> 48) & 0xFFFF), lsl #48
        emit_u32(
            bytes,
            0xf2e00009 | ((((value_u64 >> 48) & 0xFFFF) as u32) << 5),
        );
    }
}

fn callee_save_to_stack(bytes: &mut Vec<u8>) {
    // Save callee-saved registers and link register
    // stp x29, x30, [sp, #-16]!
    emit_u32(bytes, 0xa9bf7bfd);

    // stp x19, x20, [sp, #-16]!
    emit_u32(bytes, 0xa9bf53f3);

    // stp x21, x22, [sp, #-16]!
    emit_u32(bytes, 0xa9bf5bf5);

    // mov x29, sp (set frame pointer)
    emit_u32(bytes, 0x910003fd);
}

pub fn wrapper(bytes: &mut Vec<u8>, content: Vec<u8>) {
    callee_save_to_stack(bytes);

    // Store pointer to brainfuck memory (first argument x0) in x19
    // mov x19, x0
    emit_u32(bytes, 0xaa0003f3);

    // Store pointer to JITTarget (second argument x1) in x20
    // mov x20, x1
    emit_u32(bytes, 0xaa0103f4);

    // Store pointer to vtable (third argument x2) in x21
    // mov x21, x2
    emit_u32(bytes, 0xaa0203f5);

    bytes.extend(content);

    // Return the data pointer
    // mov x0, x19
    emit_u32(bytes, 0xaa1303e0);

    callee_restore_from_stack(bytes);

    // ret
    emit_u32(bytes, u32::from_be_bytes(RET_BYTES));
}

fn callee_restore_from_stack(bytes: &mut Vec<u8>) {
    // Restore callee-saved registers
    // ldp x21, x22, [sp], #16
    emit_u32(bytes, 0xa8c15bf5);

    // ldp x19, x20, [sp], #16
    emit_u32(bytes, 0xa8c153f3);

    // ldp x29, x30, [sp], #16
    emit_u32(bytes, 0xa8c17bfd);
}

pub fn decr(bytes: &mut Vec<u8>, n: u8) {
    // Load byte from [x19]
    // ldrb w8, [x19]
    emit_u32(bytes, 0x39400268);

    // Subtract n
    // sub w8, w8, #n
    emit_u32(bytes, 0x51000108 | ((n as u32) << 10));

    // Store byte back to [x19]
    // strb w8, [x19]
    emit_u32(bytes, 0x39000268);
}

pub fn incr(bytes: &mut Vec<u8>, n: u8) {
    // Load byte from [x19]
    // ldrb w8, [x19]
    emit_u32(bytes, 0x39400268);

    // Add n
    // add w8, w8, #n
    emit_u32(bytes, 0x11000108 | ((n as u32) << 10));

    // Store byte back to [x19]
    // strb w8, [x19]
    emit_u32(bytes, 0x39000268);
}

pub fn next(bytes: &mut Vec<u8>, n: usize) {
    if n <= 4095 {
        // add x19, x19, #n
        emit_u32(bytes, 0x91000273 | ((n as u32) << 10));
    } else {
        // For larger values, use a temporary register
        // mov x8, #n (using movz/movk instructions)
        let n_u64 = n as u64;

        // movz x8, #(n & 0xFFFF)
        emit_u32(bytes, 0xd2800008 | (((n_u64 & 0xFFFF) as u32) << 5));

        if n_u64 > 0xFFFF {
            // movk x8, #((n >> 16) & 0xFFFF), lsl #16
            emit_u32(bytes, 0xf2a00008 | ((((n_u64 >> 16) & 0xFFFF) as u32) << 5));
        }

        if n_u64 > 0xFFFFFFFF {
            // movk x8, #((n >> 32) & 0xFFFF), lsl #32
            emit_u32(bytes, 0xf2c00008 | ((((n_u64 >> 32) & 0xFFFF) as u32) << 5));
        }

        // add x19, x19, x8
        emit_u32(bytes, 0x8b080273);
    }
}

pub fn prev(bytes: &mut Vec<u8>, n: usize) {
    if n <= 4095 {
        // sub x19, x19, #n
        emit_u32(bytes, 0xd1000273 | ((n as u32) << 10));
    } else {
        // For larger values, use a temporary register
        let n_u64 = n as u64;

        // movz x8, #(n & 0xFFFF)
        emit_u32(bytes, 0xd2800008 | (((n_u64 & 0xFFFF) as u32) << 5));

        if n_u64 > 0xFFFF {
            // movk x8, #((n >> 16) & 0xFFFF), lsl #16
            emit_u32(bytes, 0xf2a00008 | ((((n_u64 >> 16) & 0xFFFF) as u32) << 5));
        }

        if n_u64 > 0xFFFFFFFF {
            // movk x8, #((n >> 32) & 0xFFFF), lsl #32
            emit_u32(bytes, 0xf2c00008 | ((((n_u64 >> 32) & 0xFFFF) as u32) << 5));
        }

        // sub x19, x19, x8
        emit_u32(bytes, 0xcb080273);
    }
}

fn fn_call_pre(bytes: &mut Vec<u8>) {
    // Save x19-x21 on stack (they might be modified by the call)
    // stp x19, x20, [sp, #-16]!
    emit_u32(bytes, 0xa9bf53f3);

    // str x21, [sp, #-16]!
    emit_u32(bytes, 0xf81f0ff5);
}

fn fn_call_post(bytes: &mut Vec<u8>) {
    // Restore x21
    // ldr x21, [sp], #16
    emit_u32(bytes, 0xf84107f5);

    // Restore x19-x20
    // ldp x19, x20, [sp], #16
    emit_u32(bytes, 0xa8c153f3);
}

/// Make a call to a vtable entry in x21.
fn call_vtable_entry(bytes: &mut Vec<u8>, entry: VTableEntry) {
    let offset = (entry as u32) * PTR_SIZE;

    // Load function pointer from vtable
    // ldr x8, [x21, #offset]
    emit_u32(bytes, 0xf9400008 | (21 << 5) | ((offset / 8) << 10));

    // Call the function
    // blr x8
    emit_u32(bytes, 0xd63f0100);
}

pub fn print(bytes: &mut Vec<u8>) {
    fn_call_pre(bytes);

    // Move the JITTarget pointer into the first argument register
    // mov x0, x20
    emit_u32(bytes, 0xaa1403e0);

    // Load the current memory cell into the second argument register
    // ldrb w1, [x19]
    emit_u32(bytes, 0x39400261);

    call_vtable_entry(bytes, VTableEntry::Print);

    fn_call_post(bytes);
}

pub fn read(bytes: &mut Vec<u8>) {
    fn_call_pre(bytes);

    // Move the JITTarget pointer into the first argument register
    // mov x0, x20
    emit_u32(bytes, 0xaa1403e0);

    call_vtable_entry(bytes, VTableEntry::Read);

    fn_call_post(bytes);

    // Copy return value into current cell
    // strb w0, [x19]
    emit_u32(bytes, 0x39000260);
}

pub fn set(bytes: &mut Vec<u8>, value: u8) {
    // mov w8, #value
    emit_u32(bytes, 0x52800008 | ((value as u32) << 5));

    // strb w8, [x19]
    emit_u32(bytes, 0x39000268);
}

pub fn add(bytes: &mut Vec<u8>, offset: isize) {
    // Load current cell value
    // ldrb w8, [x19]
    emit_u32(bytes, 0x39400268);

    // Load value at offset
    if (-256..=255).contains(&offset) {
        // ldrsb w9, [x19, #offset]
        emit_u32(bytes, 0x38c00269 | encode_signed_imm9(offset));
    } else {
        // Load offset into x9
        load_immediate_x9(bytes, offset);

        // ldrb w10, [x19, x9]
        emit_u32(bytes, 0x38696a6a);

        // add w10, w10, w8
        emit_u32(bytes, 0x0b08014a);

        // strb w10, [x19, x9]
        emit_u32(bytes, 0x3829626a);

        // mov w8, #0
        emit_u32(bytes, 0x52800008);

        // strb w8, [x19]
        emit_u32(bytes, 0x39000268);

        return;
    }

    // Add to value at offset
    // ldrb w9, [x19, #offset]
    let offset_encoded = if offset >= 0 {
        0x39400269 | ((offset as u32) << 10)
    } else {
        // For negative offsets, use ldurb
        0x38400269 | encode_signed_imm9(offset)
    };
    emit_u32(bytes, offset_encoded);

    // add w9, w9, w8
    emit_u32(bytes, 0x0b080129);

    // Store back at offset
    if (0..=4095).contains(&offset) {
        // strb w9, [x19, #offset]
        emit_u32(bytes, 0x39000269 | ((offset as u32) << 10));
    } else {
        // sturb w9, [x19, #offset]
        emit_u32(bytes, 0x38000269 | encode_signed_imm9(offset));
    }

    // Set current cell to 0
    // strb wzr, [x19]
    emit_u32(bytes, 0x3900027f);
}

pub fn sub(bytes: &mut Vec<u8>, offset: isize) {
    // Load current cell value
    // ldrb w8, [x19]
    emit_u32(bytes, 0x39400268);

    // Load value at offset
    if (-256..=255).contains(&offset) {
        // ldrsb w9, [x19, #offset]
        emit_u32(bytes, 0x38c00269 | encode_signed_imm9(offset));
    } else {
        // Load offset into x9
        load_immediate_x9(bytes, offset);

        // ldrb w10, [x19, x9]
        emit_u32(bytes, 0x38696a6a);

        // sub w10, w10, w8
        emit_u32(bytes, 0x4b08014a);

        // strb w10, [x19, x9]
        emit_u32(bytes, 0x3829626a);

        // mov w8, #0
        emit_u32(bytes, 0x52800008);

        // strb w8, [x19]
        emit_u32(bytes, 0x39000268);

        return;
    }

    // Subtract from value at offset
    // ldrb w9, [x19, #offset]
    let offset_encoded = if offset >= 0 {
        0x39400269 | ((offset as u32) << 10)
    } else {
        // For negative offsets, use ldurb
        0x38400269 | encode_signed_imm9(offset)
    };
    emit_u32(bytes, offset_encoded);

    // sub w9, w9, w8
    emit_u32(bytes, 0x4b080129);

    // Store back at offset
    if (0..=4095).contains(&offset) {
        // strb w9, [x19, #offset]
        emit_u32(bytes, 0x39000269 | ((offset as u32) << 10));
    } else {
        // sturb w9, [x19, #offset]
        emit_u32(bytes, 0x38000269 | encode_signed_imm9(offset));
    }

    // Set current cell to 0
    // strb wzr, [x19]
    emit_u32(bytes, 0x3900027f);
}

pub fn aot_loop(bytes: &mut Vec<u8>, inner_loop_bytes: Vec<u8>) {
    // Check if the current memory cell equals zero
    // ldrb w8, [x19]
    emit_u32(bytes, 0x39400268);

    // cbz w8, end_label
    let skip_offset = (inner_loop_bytes.len() / 4 + 2) as u32; // +2 for the branch back instruction
    emit_u32(bytes, 0x34000008 | (skip_offset << 5));

    // loop_start:
    bytes.extend(inner_loop_bytes);

    // Check if the current memory cell equals zero
    // ldrb w8, [x19]
    emit_u32(bytes, 0x39400268);

    // cbnz w8, loop_start
    let loop_offset = -((bytes.len() / 4 - 1) as i32);
    emit_u32(bytes, 0x35000008 | ((loop_offset as u32 & 0x7FFFF) << 5));

    // end_label:
}

pub fn jit_loop(bytes: &mut Vec<u8>, loop_id: JITPromiseID) {
    // Save x20 and x21 on stack
    // stp x20, x21, [sp, #-16]!
    emit_u32(bytes, 0xa9bf57f4);

    // Move the JITTarget pointer into the first argument
    // mov x0, x20
    emit_u32(bytes, 0xaa1403e0);

    // Move target index into the second argument
    let loop_id_u64 = loop_id.value() as u64;

    // movz x1, #(loop_index & 0xFFFF)
    emit_u32(bytes, 0xd2800001 | (((loop_id_u64 & 0xFFFF) as u32) << 5));

    if loop_id_u64 > 0xFFFF {
        // movk x1, #((loop_index >> 16) & 0xFFFF), lsl #16
        emit_u32(
            bytes,
            0xf2a00001 | ((((loop_id_u64 >> 16) & 0xFFFF) as u32) << 5),
        );
    }

    if loop_id_u64 > 0xFFFFFFFF {
        // movk x1, #((loop_index >> 32) & 0xFFFF), lsl #32
        emit_u32(
            bytes,
            0xf2c00001 | ((((loop_id_u64 >> 32) & 0xFFFF) as u32) << 5),
        );
    }

    // Move data pointer into the third argument
    // mov x2, x19
    emit_u32(bytes, 0xaa1303e2);

    call_vtable_entry(bytes, VTableEntry::JITCallback);

    // Take return value and store as the new data pointer
    // mov x19, x0
    emit_u32(bytes, 0xaa0003f3);

    // Restore x20 and x21
    // ldp x20, x21, [sp], #16
    emit_u32(bytes, 0xa8c157f4);
}

pub fn multiply_add(bytes: &mut Vec<u8>, offset: isize, factor: u8) {
    // Load current cell value
    // ldrb w8, [x19]
    emit_u32(bytes, 0x39400268);

    // Multiply by factor
    // mov w9, #factor
    emit_u32(bytes, 0x52800009 | ((factor as u32) << 5));

    // mul w8, w8, w9
    emit_u32(bytes, 0x1b097d08);

    // Load value at offset and add
    if (-256..=255).contains(&offset) {
        // ldrb w9, [x19, #offset]
        let offset_encoded = if offset >= 0 {
            0x39400269 | ((offset as u32) << 10)
        } else {
            0x38400269 | encode_signed_imm9(offset)
        };
        emit_u32(bytes, offset_encoded);

        // add w9, w9, w8
        emit_u32(bytes, 0x0b080129);

        // Store back at offset
        if (0..=4095).contains(&offset) {
            // strb w9, [x19, #offset]
            emit_u32(bytes, 0x39000269 | ((offset as u32) << 10));
        } else {
            // sturb w9, [x19, #offset]
            emit_u32(bytes, 0x38000269 | encode_signed_imm9(offset));
        }
    } else {
        // Load offset into x9
        load_immediate_x9(bytes, offset);

        // ldrb w10, [x19, x9]
        emit_u32(bytes, 0x38696a6a);

        // add w10, w10, w8
        emit_u32(bytes, 0x0b08014a);

        // strb w10, [x19, x9]
        emit_u32(bytes, 0x3829626a);
    }

    // Set current cell to 0
    // strb wzr, [x19]
    emit_u32(bytes, 0x3900027f);
}

pub fn copy_to(bytes: &mut Vec<u8>, offsets: Vec<isize>) {
    // Load current cell value
    // ldrb w8, [x19]
    emit_u32(bytes, 0x39400268);

    for offset in offsets {
        if (-256..=255).contains(&offset) {
            // ldrb w9, [x19, #offset]
            let offset_encoded = if offset >= 0 {
                0x39400269 | ((offset as u32) << 10)
            } else {
                0x38400269 | encode_signed_imm9(offset)
            };
            emit_u32(bytes, offset_encoded);

            // add w9, w9, w8
            emit_u32(bytes, 0x0b080129);

            // Store back at offset
            if (0..=4095).contains(&offset) {
                // strb w9, [x19, #offset]
                emit_u32(bytes, 0x39000269 | ((offset as u32) << 10));
            } else {
                // sturb w9, [x19, #offset]
                emit_u32(bytes, 0x38000269 | encode_signed_imm9(offset));
            }
        } else {
            // Load offset into x9
            load_immediate_x9(bytes, offset);

            // ldrb w10, [x19, x9]
            emit_u32(bytes, 0x38696a6a);

            // add w10, w10, w8
            emit_u32(bytes, 0x0b08014a);

            // strb w10, [x19, x9]
            emit_u32(bytes, 0x3829626a);
        }
    }

    // Set current cell to 0
    // strb wzr, [x19]
    emit_u32(bytes, 0x3900027f);
}
