use std::ops::Deref;
use std::sync::OnceLock;
use libc::{sysconf, _SC_PAGESIZE};

// MAP_JIT is macOS-specific
#[cfg(all(target_os = "macos"))]
const MAP_JIT: i32 = 0x0800;

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
            std::ptr::copy_nonoverlapping(
                source.as_ptr(),
                buffer_ptr,
                source.len(),
            );
            // Make the memory executable
            mprotect_result = libc::mprotect(
                buffer_ptr as *mut libc::c_void,
                buffer_size_bytes,
                libc::PROT_READ | libc::PROT_EXEC,
            );
        }

        if mprotect_result != 0 {
            panic!("Failed to make memory executable: {}", std::io::Error::last_os_error());
        }

        return Self {
            ptr: buffer_ptr,
            len: source.len(),
            capacity: buffer_size_bytes,
        };
    }
    
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    #[cfg(target_os = "macos")]
    fn allocate_memory(buffer_size_bytes: usize) -> *mut u8 {
        // On macOS ARM64, use mmap with MAP_JIT
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                buffer_size_bytes,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_ANON | libc::MAP_PRIVATE | MAP_JIT,
                -1,
                0,
            )
        };
        
        if ptr == libc::MAP_FAILED {
            panic!("Failed to allocate JIT memory: {}", std::io::Error::last_os_error());
        }
        
        ptr as *mut u8
    }

    #[cfg(target_os = "linux")]
    fn allocate_memory(buffer_size_bytes: usize) -> *mut u8 {
        let mut buffer = std::mem::MaybeUninit::<*mut libc::c_void>::uninit();
        let buffer_ptr_ptr = buffer.as_mut_ptr();

        let memalign_result = unsafe {
            libc::posix_memalign(
                buffer_ptr_ptr,
                *(PAGE_SIZE.get().unwrap()), 
                buffer_size_bytes,
            )
        };
        
        if memalign_result != 0 {
            panic!("Failed to allocate aligned memory");
        }
        
        unsafe { buffer.assume_init() as *mut u8 }
    }

    #[cfg(target_arch = "aarch64")]
    fn fill_with_ret(ptr: *mut u8, len: usize) {
        let ret_instruction: u32 = 0xd65f03c0;
        let buffer_as_u32 = ptr as *mut u32;
        for i in 0..(len / 4) {
            unsafe {
                *buffer_as_u32.add(i) = ret_instruction;
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    fn fill_with_ret(ptr: *mut u8, len: usize) {
        let ret_instruction: u8 = 0xc3;
        for i in 0..len {
            unsafe {
                *ptr.add(i) = ret_instruction;
            }
        }
    }
}

impl Drop for ExecutableMemory {
    #[cfg(target_os = "macos")]
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.capacity);
        }
    }

    #[cfg(target_os = "linux")]
    fn drop(&mut self) {
        unsafe {
            libc::free(self.ptr as *mut libc::c_void);
        }
    }
}

impl Deref for ExecutableMemory {
    type Target = [u8];
    
    fn deref(&self) -> &Self::Target {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

// Safety: ExecutableMemory owns its memory exclusively
unsafe impl Send for ExecutableMemory {}
unsafe impl Sync for ExecutableMemory {}
