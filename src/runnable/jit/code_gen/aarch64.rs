use crate::runnable::jit::jit_promise::JITPromiseID;
use crate::runnable::jit::jit_target::VTableEntry;

pub const RET: u32 = 0xd65f_03c0;
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

pub fn decr(bytes: &mut Vec<u8>, n: u8) {
    // Load byte from [x19]
    // ldrb w8, [x19]
    emit_u32(bytes, 0x3940_0268);

    // Subtract n
    // sub w8, w8, #n
    emit_u32(bytes, 0x5100_0108 | (u32::from(n) << 10));

    // Store byte back to [x19]
    // strb w8, [x19]
    emit_u32(bytes, 0x3900_0268);
}

pub fn incr(bytes: &mut Vec<u8>, n: u8) {
    // Load byte from [x19]
    // ldrb w8, [x19]
    emit_u32(bytes, 0x3940_0268);

    // Add n
    // add w8, w8, #n
    emit_u32(bytes, 0x1100_0108 | (u32::from(n) << 10));

    // Store byte back to [x19]
    // strb w8, [x19]
    emit_u32(bytes, 0x3900_0268);
}

pub fn next(bytes: &mut Vec<u8>, n: u16) {
    // For all values, use a temporary register
    // movz x8, #n
    emit_u32(bytes, 0xd280_0008 | (u32::from(n) << 5));

    // add x19, x19, x8
    emit_u32(bytes, 0x8b08_0273);
}

pub fn prev(bytes: &mut Vec<u8>, n: u16) {
    // For all values, use a temporary register
    // movz x8, #n
    emit_u32(bytes, 0xd280_0008 | (u32::from(n) << 5));

    // sub x19, x19, x8
    emit_u32(bytes, 0xcb08_0273);
}

fn fn_call_pre(bytes: &mut Vec<u8>) {
    // Save x19-x21 on stack (they might be modified by the call)
    // stp x19, x20, [sp, #-16]!
    emit_u32(bytes, 0xa9bf_53f3);

    // str x21, [sp, #-16]!
    emit_u32(bytes, 0xf81f_0ff5);
}

fn fn_call_post(bytes: &mut Vec<u8>) {
    // Restore x21
    // ldr x21, [sp], #16
    emit_u32(bytes, 0xf841_07f5);

    // Restore x19-x20
    // ldp x19, x20, [sp], #16
    emit_u32(bytes, 0xa8c1_53f3);
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

pub fn print(bytes: &mut Vec<u8>) {
    fn_call_pre(bytes);

    // Move the JITTarget pointer into the first argument register
    // mov x0, x20
    emit_u32(bytes, 0xaa14_03e0);

    // Load the current memory cell into the second argument register
    // ldrb w1, [x19]
    emit_u32(bytes, 0x3940_0261);

    call_vtable_entry(bytes, VTableEntry::Print);

    fn_call_post(bytes);
}

pub fn read(bytes: &mut Vec<u8>) {
    fn_call_pre(bytes);

    // Move the JITTarget pointer into the first argument register
    // mov x0, x20
    emit_u32(bytes, 0xaa14_03e0);

    call_vtable_entry(bytes, VTableEntry::Read);

    fn_call_post(bytes);

    // Copy return value into current cell
    // strb w0, [x19]
    emit_u32(bytes, 0x3900_0260);
}

pub fn set(bytes: &mut Vec<u8>, value: u8) {
    // mov w8, #value
    emit_u32(bytes, 0x5280_0008 | (u32::from(value) << 5));

    // strb w8, [x19]
    emit_u32(bytes, 0x3900_0268);
}

pub fn add(bytes: &mut Vec<u8>, offset: i16) {
    // Load current cell value (at_ptr)
    // ldrb w8, [x19]
    emit_u32(bytes, 0x3940_0268);

    // Load offset into register (sign-extended)
    #[allow(clippy::cast_sign_loss)]
    if offset >= 0 {
        // movz x9, #offset
        emit_u32(bytes, 0xd280_0009 | ((offset as u32) << 5));
    } else {
        // For negative values, use movn
        let not_offset = !offset;
        emit_u32(bytes, 0x9280_0009 | ((not_offset as u32) << 5));
    }

    // Load value at offset (at_offset)
    // ldrb w10, [x19, x9]
    emit_u32(bytes, 0x3869_6a6a);

    // Add the two values: at_ptr + at_offset
    // add w8, w8, w10
    emit_u32(bytes, 0x0b0a_0108);

    // Store the result back at offset location
    // strb w8, [x19, x9]
    emit_u32(bytes, 0x3829_6a68);

    // Set current cell to 0
    // strb wzr, [x19]
    emit_u32(bytes, 0x3900_027f);
}

pub fn sub(bytes: &mut Vec<u8>, offset: i16) {
    // Load current cell value (at_ptr)
    // ldrb w8, [x19]
    emit_u32(bytes, 0x3940_0268);

    // Load offset into register (sign-extended)
    #[allow(clippy::cast_sign_loss)]
    if offset >= 0 {
        // movz x9, #offset
        emit_u32(bytes, 0xd280_0009 | ((offset as u32) << 5));
    } else {
        // For negative values, use movn
        let not_offset = !offset;
        emit_u32(bytes, 0x9280_0009 | ((not_offset as u32) << 5));
    }

    // Load value at offset (at_offset)
    // ldrb w10, [x19, x9]
    emit_u32(bytes, 0x3869_6a6a);

    // Subtract: at_offset - at_ptr
    // sub w10, w10, w8
    emit_u32(bytes, 0x4b08_014a);

    // Store the result back at offset location
    // strb w10, [x19, x9]
    emit_u32(bytes, 0x3829_6a6a);

    // Set current cell to 0
    // strb wzr, [x19]
    emit_u32(bytes, 0x3900_027f);
}

pub fn aot_loop(bytes: &mut Vec<u8>, inner_loop_bytes: Vec<u8>) {
    // Check if the current memory cell equals zero
    // ldrb w8, [x19]
    emit_u32(bytes, 0x3940_0268);

    let inner_loop_instructions = inner_loop_bytes.len() / 4;
    // cbz w8
    let skip_offset = u32::try_from(inner_loop_instructions + 2).unwrap(); // +2 for the branch back instruction
    emit_u32(bytes, 0x3400_0008 | (skip_offset << 5));

    // loop_start:
    bytes.extend(inner_loop_bytes);

    // Check if the current memory cell equals zero
    // ldrb w8, [x19]
    emit_u32(bytes, 0x3940_0268);

    // cbnz w8, loop_start
    let loop_offset = -i32::try_from(inner_loop_instructions + 2).unwrap();
    #[allow(clippy::cast_sign_loss)]
    emit_u32(
        bytes,
        0x3500_0008 | ((loop_offset as u32 & 0x0007_FFFF) << 5),
    );
}

pub fn jit_loop(bytes: &mut Vec<u8>, loop_id: JITPromiseID) {
    // Save x20 and x21 on stack
    // stp x20, x21, [sp, #-16]!
    emit_u32(bytes, 0xa9bf_57f4);

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

    // Restore x20 and x21
    // ldp x20, x21, [sp], #16
    emit_u32(bytes, 0xa8c1_57f4);
}

pub fn multiply_add(bytes: &mut Vec<u8>, offset: i16, factor: u8) {
    // Load current cell value
    // ldrb w8, [x19]
    emit_u32(bytes, 0x3940_0268);

    // Multiply by factor
    // mov w9, #factor
    emit_u32(bytes, 0x5280_0009 | (u32::from(factor) << 5));

    // mul w8, w8, w9
    emit_u32(bytes, 0x1b09_7d08);

    // Load offset into w9 (32-bit value)
    #[allow(clippy::cast_sign_loss)]
    if offset >= 0 {
        // mov w9, #offset
        emit_u32(bytes, 0x5280_0009 | ((offset as u32) << 5));
    } else {
        // For negative values, use movn
        emit_u32(bytes, 0x1280_0009 | ((!offset as u32) << 5));
    }

    // ldrb w10, [x19, w9, sxtw]
    emit_u32(bytes, 0x38a9_6a6a);

    // add w10, w10, w8
    emit_u32(bytes, 0x0b08_014a);

    // strb w10, [x19, w9, sxtw]
    emit_u32(bytes, 0x3829_6a6a);

    // Set current cell to 0
    // strb wzr, [x19]
    emit_u32(bytes, 0x3900_027f);
}

pub fn copy_to(bytes: &mut Vec<u8>, offsets: Vec<i16>) {
    // Load current cell value
    // ldrb w8, [x19]
    emit_u32(bytes, 0x3940_0268);

    for offset in offsets {
        // Load offset into w9 (32-bit value)
        #[allow(clippy::cast_sign_loss)]
        if offset >= 0 {
            // mov w9, #offset
            emit_u32(bytes, 0x5280_0009 | ((offset as u32) << 5));
        } else {
            // For negative values, use movn
            emit_u32(bytes, 0x1280_0009 | ((!offset as u32) << 5));
        }

        // ldrb w10, [x19, w9, sxtw]
        emit_u32(bytes, 0x38a9_6a6a);

        // add w10, w10, w8
        emit_u32(bytes, 0x0b08_014a);

        // strb w10, [x19, w9, sxtw]
        emit_u32(bytes, 0x3829_6a6a);
    }

    // Set current cell to 0
    // strb wzr, [x19]
    emit_u32(bytes, 0x3900_027f);
}
