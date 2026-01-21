//! Syscall implementation for BrainFuck.
//!
//! This module provides a shared syscall implementation used by both the
//! interpreter and JIT compiler. It supports the "systemf" convention for
//! encoding syscall parameters in brainfuck memory.
//!
//! # Syscall Format (systemf convention)
//!
//! The syscall format from the current cell position:
//! - Cell 0: syscall number
//! - Cell 1: number of arguments (0-6)
//! - For each argument:
//!   - 1 cell: argument type (0=normal value, 1=pointer to buffer, 2=cell pointer)
//!   - 1 cell: argument length in cells
//!   - N cells: argument data (big-endian for multi-cell values)
//!
//! Returns the low byte of the syscall return value.
use anyhow::{Result, bail};

/// Get the platform-specific syscall number offset.
#[cfg(target_os = "macos")]
static SYSCALL_NUM_OFFSET: usize = 0x0200_0000;
#[cfg(target_os = "linux")]
static SYSCALL_NUM_OFFSET: usize = 0;

/// Argument type constants for syscall arguments
mod arg_type {
    /// Normal value - the argument is passed directly
    pub const NORMAL: u8 = 0;
    /// Pointer to buffer - a pointer to the argument data in memory is passed
    pub const BUFFER_PTR: u8 = 1;
    /// Cell pointer - the value is treated as an offset from the base memory pointer
    pub const CELL_PTR: u8 = 2;
}

/// Maximum number of arguments supported by syscalls
const MAX_ARGS: usize = 6;

/// Parsed syscall arguments ready for execution
#[derive(Default)]
pub struct SyscallArgs {
    /// The syscall number
    pub syscall_num: usize,
    /// The argument values
    pub args: [usize; MAX_ARGS],
}

/// Parse syscall arguments from memory.
///
/// # Arguments
/// * `memory` - Slice of memory starting at the current data pointer
/// * `mem_base_ptr` - Base pointer to the brainfuck memory (for cell pointer arguments)
pub fn parse_syscall_args(memory: &[u8], mem_base_ptr: *const u8) -> Result<SyscallArgs> {
    let mut syscall_args = SyscallArgs::default();
    let mut pos = 0usize;

    syscall_args.syscall_num = usize::from(memory[pos]) | SYSCALL_NUM_OFFSET;
    pos += 1;

    let arg_count = memory[pos] as usize;
    pos += 1;

    for arg in syscall_args.args.iter_mut().take(arg_count.min(MAX_ARGS)) {
        let arg_type = memory[pos];
        pos += 1;

        let arg_len = memory[pos] as usize;
        pos += 1;

        // Position where the argument data starts
        let data_pos = pos;

        // Read argument value (big-endian)
        let mut value: usize = 0;
        for i in 0..arg_len {
            value = (value << 8) | usize::from(memory[pos + i]);
        }
        pos += arg_len;

        *arg = match arg_type {
            arg_type::NORMAL => value,
            arg_type::BUFFER_PTR => memory[data_pos..].as_ptr() as usize,
            #[allow(clippy::cast_possible_truncation)]
            arg_type::CELL_PTR => mem_base_ptr.wrapping_add(value as usize) as usize,
            _ => bail!("Invalid syscall argument type"),
        };
    }

    Ok(syscall_args)
}

/// Execute a syscall with the given arguments.
#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "x86_64")
))]
pub fn execute_syscall(syscall_args: &SyscallArgs) -> usize {
    use std::arch::asm;

    let result: usize;
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") syscall_args.syscall_num => result,
            in("rdi") syscall_args.args[0],
            in("rsi") syscall_args.args[1],
            in("rdx") syscall_args.args[2],
            in("r10") syscall_args.args[3],
            in("r8") syscall_args.args[4],
            in("r9") syscall_args.args[5],
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack)
        );
    }

    result
}

/// Execute a syscall with the given arguments.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub fn execute_syscall(syscall_args: &SyscallArgs) -> usize {
    use std::arch::asm;

    let result: usize;
    unsafe {
        asm!(
            "svc #0x80",
            inlateout("x16") syscall_args.syscall_num => _,
            inlateout("x0") syscall_args.args[0] => result,
            in("x1") syscall_args.args[1],
            in("x2") syscall_args.args[2],
            in("x3") syscall_args.args[3],
            in("x4") syscall_args.args[4],
            in("x5") syscall_args.args[5],
            options(nostack)
        );
    }

    result
}

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub fn execute_syscall(syscall_args: &SyscallArgs) -> usize {
    use std::arch::asm;

    let result: usize;
    unsafe {
        asm!(
            "svc #0",
            in("x8") syscall_args.syscall_num,
            inlateout("x0") syscall_args.args[0] => result,
            in("x1") syscall_args.args[1],
            in("x2") syscall_args.args[2],
            in("x3") syscall_args.args[3],
            in("x4") syscall_args.args[4],
            in("x5") syscall_args.args[5],
            options(nostack)
        );
    }

    result
}

/// Execute a syscall with the given arguments.
#[cfg(all(target_os = "linux", target_arch = "x86"))]
pub fn execute_syscall(syscall_args: &SyscallArgs) -> usize {
    use std::arch::asm;

    let result: usize;
    unsafe {
        asm!(
            "push ebp",
            "push esi",
            "push edi",
            "mov edi, [{arr} + 16]",
            "mov esi, [{arr} + 12]",
            "mov ebp, [{arr} + 20]",
            "int 0x80",
            "pop edi",
            "pop esi",
            "pop ebp",
            arr = in(reg) syscall_args.args.as_ptr(),
            inlateout("eax") syscall_args.syscall_num => result,
            in("ebx") syscall_args.args[0],
            in("ecx") syscall_args.args[1],
            in("edx") syscall_args.args[2],
        );
    }

    result
}
