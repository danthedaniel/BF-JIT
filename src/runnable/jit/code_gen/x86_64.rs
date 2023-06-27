use super::super::jit_promise::JITPromiseID;
use super::super::jit_target::JITTargetVTable;

pub const RET: u8 = 0xc3;

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

pub fn decr(bytes: &mut Vec<u8>, n: u8) {
    // sub    BYTE PTR [r10],n
    bytes.push(0x41);
    bytes.push(0x80);
    bytes.push(0x2a);
    bytes.push(n);
}

pub fn incr(bytes: &mut Vec<u8>, n: u8) {
    // add    BYTE PTR [r10],n
    bytes.push(0x41);
    bytes.push(0x80);
    bytes.push(0x02);
    bytes.push(n);
}

pub fn next(bytes: &mut Vec<u8>, n: usize) {
    // HACK: Assumes usize won't be more than 32 bit...
    let n_bytes = (n as u32).to_ne_bytes();

    // add    r10,n
    bytes.push(0x49);
    bytes.push(0x81);
    bytes.push(0xc2);
    bytes.push(n_bytes[0]);
    bytes.push(n_bytes[1]);
    bytes.push(n_bytes[2]);
    bytes.push(n_bytes[3]);
}

pub fn prev(bytes: &mut Vec<u8>, n: usize) {
    // HACK: Assumes usize won't be more than 32 bit...
    let n_bytes = (n as u32).to_ne_bytes();

    // sub    r10,n
    bytes.push(0x49);
    bytes.push(0x81);
    bytes.push(0xea);
    bytes.push(n_bytes[0]);
    bytes.push(n_bytes[1]);
    bytes.push(n_bytes[2]);
    bytes.push(n_bytes[3]);
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
}

fn fn_call_post(bytes: &mut Vec<u8>) {
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
fn call_vtable_entry(bytes: &mut Vec<u8>, entry: JITTargetVTable) {
    // Load function pointer from vtable at index into rax
    // call   QWORD PTR [r12+index]
    bytes.push(0x41);
    bytes.push(0xff);
    bytes.push(0x54);
    bytes.push(0x24);
    bytes.push((entry as u8) * 8);
}

pub fn print(bytes: &mut Vec<u8>) {
    fn_call_pre(bytes);

    // Move the JITTarget pointer into the first argument register
    // mov    rdi,r11
    bytes.push(0x4c);
    bytes.push(0x89);
    bytes.push(0xdf);

    // Move the current memory cell into the second argument register
    // movzx    rsi,BYTE PTR [r10]
    bytes.push(0x49);
    bytes.push(0x0f);
    bytes.push(0xb6);
    bytes.push(0x32);

    call_vtable_entry(bytes, JITTargetVTable::Print);

    fn_call_post(bytes);
}

pub fn read(bytes: &mut Vec<u8>) {
    fn_call_pre(bytes);

    // Move the JITTarget pointer into the first argument register
    // mov    rdi,r11
    bytes.push(0x4c);
    bytes.push(0x89);
    bytes.push(0xdf);

    call_vtable_entry(bytes, JITTargetVTable::Read);

    fn_call_post(bytes);

    // Copy return value into current cell.
    // mov    BYTE PTR [r10],al
    bytes.push(0x41);
    bytes.push(0x88);
    bytes.push(0x02);
}

pub fn set(bytes: &mut Vec<u8>, value: u8) {
    // Set current memory cell to the value
    // mov    BYTE PTR [r10],value
    bytes.push(0x41);
    bytes.push(0xc6);
    bytes.push(0x02);
    bytes.push(value);
}

pub fn add(bytes: &mut Vec<u8>, offset: isize) {
    // Copy the current cell into EAX.
    // movzx  eax,BYTE PTR [r10]
    bytes.push(0x41);
    bytes.push(0x0f);
    bytes.push(0xb6);
    bytes.push(0x02);

    let offset_bytes = offset.to_ne_bytes();

    // Set r13 to the offset.
    // movabs r13,offset
    bytes.push(0x49);
    bytes.push(0xbd);
    bytes.push(offset_bytes[0]);
    bytes.push(offset_bytes[1]);
    bytes.push(offset_bytes[2]);
    bytes.push(offset_bytes[3]);
    bytes.push(offset_bytes[4]);
    bytes.push(offset_bytes[5]);
    bytes.push(offset_bytes[6]);
    bytes.push(offset_bytes[7]);

    // Add the current cell (now in EAX) to the cell at the offset.
    // add    BYTE PTR [r10+r13],al
    bytes.push(0x43);
    bytes.push(0x00);
    bytes.push(0x04);
    bytes.push(0x2a);

    // Set the current memory cell to 0.
    // mov    BYTE PTR [r10],0
    bytes.push(0x41);
    bytes.push(0xc6);
    bytes.push(0x02);
    bytes.push(0x00);
}

pub fn sub(bytes: &mut Vec<u8>, offset: isize) {
    // Copy the current cell into EAX.
    // movzx  eax,BYTE PTR [r10]
    bytes.push(0x41);
    bytes.push(0x0f);
    bytes.push(0xb6);
    bytes.push(0x02);

    let offset_bytes = offset.to_ne_bytes();

    // Set r13 to the offset.
    // movabs r13,offset
    bytes.push(0x49);
    bytes.push(0xbd);
    bytes.push(offset_bytes[0]);
    bytes.push(offset_bytes[1]);
    bytes.push(offset_bytes[2]);
    bytes.push(offset_bytes[3]);
    bytes.push(offset_bytes[4]);
    bytes.push(offset_bytes[5]);
    bytes.push(offset_bytes[6]);
    bytes.push(offset_bytes[7]);

    // Add the current cell (now in EAX) to the cell at the offset.
    // sub    BYTE PTR [r10+r13],al
    bytes.push(0x43);
    bytes.push(0x28);
    bytes.push(0x04);
    bytes.push(0x2a);

    // Set the current memory cell to 0.
    // mov    BYTE PTR [r10],0
    bytes.push(0x41);
    bytes.push(0xc6);
    bytes.push(0x02);
    bytes.push(0x00);
}

pub fn aot_loop(bytes: &mut Vec<u8>, inner_loop_bytes: Vec<u8>) {
    let inner_loop_size = inner_loop_bytes.len() as i32;

    let end_loop_size: i32 = 10; // Bytes
    let byte_offset = inner_loop_size + end_loop_size;

    // Check if the current memory cell equals zero.
    // cmp    BYTE PTR [r10],0x0
    bytes.push(0x41);
    bytes.push(0x80);
    bytes.push(0x3a);
    bytes.push(0x00);

    let offset_bytes = byte_offset.to_ne_bytes();

    // Jump to the end of the loop if equal.
    // je    offset
    bytes.push(0x0f);
    bytes.push(0x84);
    bytes.push(offset_bytes[0]);
    bytes.push(offset_bytes[1]);
    bytes.push(offset_bytes[2]);
    bytes.push(offset_bytes[3]);

    bytes.extend(inner_loop_bytes);

    // Check if the current memory cell equals zero.
    // cmp    BYTE PTR [r10],0x0
    bytes.push(0x41);
    bytes.push(0x80);
    bytes.push(0x3a);
    bytes.push(0x00);

    let offset_bytes = (-byte_offset).to_ne_bytes();

    // Jump back to the beginning of the loop if not equal.
    // jne    offset
    bytes.push(0x0f);
    bytes.push(0x85);
    bytes.push(offset_bytes[0]);
    bytes.push(offset_bytes[1]);
    bytes.push(offset_bytes[2]);
    bytes.push(offset_bytes[3]);
}

pub fn jit_loop(bytes: &mut Vec<u8>, loop_index: JITPromiseID) {
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

    let loop_index_bytes = loop_index.to_ne_bytes();

    // Move target index into the second argument
    // movabs rsi,index
    bytes.push(0x48);
    bytes.push(0xbe);
    bytes.push(loop_index_bytes[0]);
    bytes.push(loop_index_bytes[1]);
    bytes.push(loop_index_bytes[2]);
    bytes.push(loop_index_bytes[3]);
    bytes.push(loop_index_bytes[4]);
    bytes.push(loop_index_bytes[5]);
    bytes.push(loop_index_bytes[6]);
    bytes.push(loop_index_bytes[7]);

    // Move data pointer into the third argument
    // mov rdx,r10
    bytes.push(0x4c);
    bytes.push(0x89);
    bytes.push(0xd2);

    call_vtable_entry(bytes, JITTargetVTable::JITCallback);

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
