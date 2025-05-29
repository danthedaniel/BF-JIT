use libc::{_SC_PAGESIZE, sysconf};
use std::ops::Deref;
use std::sync::OnceLock;

use super::code_gen::RET;

// macos needs an extra flag
#[cfg(target_os = "macos")]
const MMAP_FLAGS: i32 = libc::MAP_ANON | libc::MAP_PRIVATE | libc::MAP_JIT;
#[cfg(target_os = "linux")]
const MMAP_FLAGS: i32 = libc::MAP_ANON | libc::MAP_PRIVATE;

static PAGE_SIZE: OnceLock<usize> = OnceLock::new();

/// A buffer of executable memory that properly handles platform-specific allocation
#[derive(Debug)]
pub struct ExecutableMemory {
    ptr: *mut u8,
    len: usize,
    capacity: usize,
}

impl ExecutableMemory {
    pub fn new(source: &[u8]) -> Self {
        let page_size = *PAGE_SIZE.get_or_init(|| unsafe { sysconf(_SC_PAGESIZE) as usize });
        let buffer_size_pages = source.len().div_ceil(page_size);
        let buffer_size_bytes = buffer_size_pages * page_size;

        let buffer_ptr = Self::allocate_memory(buffer_size_bytes);
        Self::fill_with_ret(buffer_ptr, buffer_size_bytes);

        let mprotect_result;
        unsafe {
            // Copy the source data
            std::ptr::copy_nonoverlapping(source.as_ptr(), buffer_ptr, source.len());
            // Make the memory executable
            mprotect_result = libc::mprotect(
                buffer_ptr as *mut libc::c_void,
                buffer_size_bytes,
                libc::PROT_READ | libc::PROT_EXEC,
            );
        }

        if mprotect_result != 0 {
            panic!(
                "Failed to make memory executable: {}",
                std::io::Error::last_os_error()
            );
        }

        Self {
            ptr: buffer_ptr,
            len: source.len(),
            capacity: buffer_size_bytes,
        }
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    fn allocate_memory(buffer_size_bytes: usize) -> *mut u8 {
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                buffer_size_bytes,
                libc::PROT_READ | libc::PROT_WRITE,
                MMAP_FLAGS,
                -1,
                0,
            )
        };

        if ptr == libc::MAP_FAILED {
            panic!(
                "Failed to allocate JIT memory: {}",
                std::io::Error::last_os_error()
            );
        }

        ptr as *mut u8
    }

    fn fill_with_ret(ptr: *mut u8, len: usize) {
        let ret_bytes = RET.to_ne_bytes();

        for word in 0..(len / ret_bytes.len()) {
            for (offset, byte) in ret_bytes.iter().enumerate() {
                unsafe {
                    *ptr.add(word + offset) = *byte;
                }
            }
        }
    }
}

impl Drop for ExecutableMemory {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.capacity);
        }
    }
}

impl Deref for ExecutableMemory {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}
