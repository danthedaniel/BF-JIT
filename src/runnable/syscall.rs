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
static SYSCALL_NUM_OFFSET: u64 = 0x0200_0000;
#[cfg(target_os = "linux")]
static SYSCALL_NUM_OFFSET: u64 = 0;

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
pub struct SyscallArgs {
    /// The syscall number
    pub syscall_num: u64,
    /// The argument values
    pub args: [u64; MAX_ARGS],
}

/// Parse syscall arguments from memory.
///
/// # Arguments
/// * `memory` - Slice of memory starting at the current data pointer
/// * `mem_base_ptr` - Base pointer to the brainfuck memory (for cell pointer arguments)
pub fn parse_syscall_args(
    memory: &[u8],
    mem_base_ptr: *const u8,
) -> Result<SyscallArgs> {
    let syscall_num = u64::from(memory[0]) | SYSCALL_NUM_OFFSET;
    let arg_count = memory[1] as usize;

    let mut args: [u64; MAX_ARGS] = [0; MAX_ARGS];
    let mut pos = 2usize;

    for arg in args.iter_mut().take(arg_count.min(MAX_ARGS)) {
        let arg_type = memory[pos];
        pos += 1;
        let arg_len = memory[pos] as usize;
        pos += 1;

        // Position where the argument data starts
        let data_pos = pos;

        // Read argument value (big-endian)
        let mut value: u64 = 0;
        for i in 0..arg_len {
            value = (value << 8) | u64::from(memory[pos + i]);
        }
        pos += arg_len;

        *arg = match arg_type {
            arg_type::NORMAL => value,
            arg_type::BUFFER_PTR => memory[data_pos..].as_ptr() as u64,
            #[allow(clippy::cast_possible_truncation)]
            arg_type::CELL_PTR => mem_base_ptr.wrapping_add(value as usize) as u64,
            _ => bail!("Invalid syscall argument type"),
        };
    }

    Ok(SyscallArgs { syscall_num, args })
}

/// Execute a syscall with the given arguments.
#[cfg(any(
    all(target_os = "linux", target_arch = "x86_64"),
    all(target_os = "macos", target_arch = "x86_64")
))]
pub fn execute_syscall(args: &SyscallArgs) -> u64 {
    use std::arch::asm;

    let result: u64;
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") args.syscall_num => result,
            in("rdi") args.args[0],
            in("rsi") args.args[1],
            in("rdx") args.args[2],
            in("r10") args.args[3],
            in("r8") args.args[4],
            in("r9") args.args[5],
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack)
        );
    }
    result
}

/// Execute a syscall with the given arguments.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub fn execute_syscall(args: &SyscallArgs) -> u64 {
    use std::arch::asm;

    let result: u64;
    unsafe {
        asm!(
            "svc #0x80",
            inlateout("x16") args.syscall_num => _,
            inlateout("x0") args.args[0] => result,
            in("x1") args.args[1],
            in("x2") args.args[2],
            in("x3") args.args[3],
            in("x4") args.args[4],
            in("x5") args.args[5],
            options(nostack)
        );
    }
    result
}

/// Execute a syscall with the given arguments.
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub fn execute_syscall(args: &SyscallArgs) -> u64 {
    use std::arch::asm;

    let result: u64;
    unsafe {
        asm!(
            "svc #0",
            inlateout("x8") args.syscall_num => _,
            inlateout("x0") args.args[0] => result,
            in("x1") args.args[1],
            in("x2") args.args[2],
            in("x3") args.args[3],
            in("x4") args.args[4],
            in("x5") args.args[5],
            options(nostack)
        );
    }
    result
}

/// Execute a syscall with the given arguments.
#[cfg(all(target_os = "linux", target_arch = "x86"))]
pub fn execute_syscall(args: &SyscallArgs) -> u64 {
    use std::arch::asm;

    let result: u32;
    // Prepare args as u32 array for easier assembly access
    let args32: [u32; MAX_ARGS] = [
        args.args[0] as u32,
        args.args[1] as u32,
        args.args[2] as u32,
        args.args[3] as u32,
        args.args[4] as u32,
        args.args[5] as u32,
    ];
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
            arr = in(reg) args32.as_ptr(),
            inlateout("eax") args.syscall_num as u32 => result,
            in("ebx") args32[0],
            in("ecx") args32[1],
            in("edx") args32[2],
        );
    }
    u64::from(result)
}
