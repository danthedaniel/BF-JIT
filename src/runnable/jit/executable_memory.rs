use anyhow::Result;
use libc::{_SC_PAGESIZE, sysconf};
use std::slice;
use std::sync::OnceLock;

use crate::runnable::jit::JITTarget;

use super::code_gen::RET;

// macos needs an extra flag
#[cfg(target_os = "macos")]
const MMAP_FLAGS: i32 = libc::MAP_ANON | libc::MAP_PRIVATE | libc::MAP_JIT;
#[cfg(target_os = "linux")]
const MMAP_FLAGS: i32 = libc::MAP_ANON | libc::MAP_PRIVATE;

static PAGE_SIZE: OnceLock<usize> = OnceLock::new();

/// A type to unify all function pointers behind. Because the vtable is not used in the
/// Rust code at all, the type is not important.
pub type VoidPtr = *const ();
/// VTable for JIT compiled code
type VTable<const SIZE: usize> = [VoidPtr; SIZE];

/// A buffer of executable memory that properly handles platform-specific allocation
#[derive(Debug)]
pub struct ExecutableMemory {
    ptr: *const u8,
    len: usize,
}

impl ExecutableMemory {
    pub fn new(source: &[u8]) -> Result<Self> {
        let len = Self::calculate_length(source.len());
        let ptr = Self::allocate_memory(len)?;
        let buffer = unsafe { slice::from_raw_parts_mut(ptr, len) };
        Self::fill_with_ret(buffer);
        Self::copy_source(buffer, source);
        Self::make_executable(buffer)?;

        Ok(Self { ptr, len })
    }

    pub fn as_fn(&self) -> fn(*mut u8, &mut JITTarget, &VTable<3>) -> *mut u8 {
        unsafe { std::mem::transmute(self.ptr) }
    }

    /// Length is an integer number of pages at least as large as the source.
    fn calculate_length(source_length: usize) -> usize {
        let page_size = *PAGE_SIZE.get_or_init(|| unsafe { sysconf(_SC_PAGESIZE) as usize });
        let buffer_size_pages = source_length.div_ceil(page_size);
        buffer_size_pages * page_size
    }

    fn allocate_memory(len: usize) -> Result<*mut u8> {
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                MMAP_FLAGS,
                -1,
                0,
            )
        };

        if ptr == libc::MAP_FAILED {
            anyhow::bail!(
                "Failed to allocate JIT memory: {}",
                std::io::Error::last_os_error()
            );
        }

        Ok(ptr as *mut u8)
    }

    /// In case of a bad jump we want unpopulated areas of memory to return.
    fn fill_with_ret(buffer: &mut [u8]) {
        let ret_bytes = RET.to_ne_bytes();
        assert_eq!(
            buffer.len() % ret_bytes.len(),
            0,
            "Buffer length must evenly divide by the size of RET"
        );

        for word in 0..(buffer.len() / ret_bytes.len()) {
            for (offset, &byte) in ret_bytes.iter().enumerate() {
                buffer[word + offset] = byte;
            }
        }
    }

    fn copy_source(buffer: &mut [u8], source: &[u8]) {
        assert!(
            buffer.len() >= source.len(),
            "Buffer must be at least as long as source"
        );

        for (index, &byte) in source.iter().enumerate() {
            buffer[index] = byte;
        }
    }

    fn make_executable(buffer: &mut [u8]) -> Result<()> {
        let mprotect_result = unsafe {
            libc::mprotect(
                buffer.as_mut_ptr() as *mut libc::c_void,
                buffer.len(),
                libc::PROT_READ | libc::PROT_EXEC,
            )
        };

        if mprotect_result != 0 {
            anyhow::bail!(
                "Failed to make memory executable: {}",
                std::io::Error::last_os_error()
            );
        }

        Ok(())
    }
}

impl Drop for ExecutableMemory {
    fn drop(&mut self) {
        let munmap_result = unsafe { libc::munmap(self.ptr as *mut libc::c_void, self.len) };

        if munmap_result != 0 {
            panic!(
                "Failed to unmap memory: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}
