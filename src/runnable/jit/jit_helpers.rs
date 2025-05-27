use super::code_gen;

use super::immutable::Immutable;
use std::mem;
use std::sync::OnceLock;

use libc::{sysconf, _SC_PAGESIZE};

static PAGE_SIZE: OnceLock<usize> = OnceLock::new();

/// Allocate a buffer of executable memory pages.
fn allocate_buffer(length: usize) -> Vec<u8> {
    let mut buffer = mem::MaybeUninit::<*mut libc::c_void>::uninit();
    let buffer_ptr = buffer.as_mut_ptr();

    let page_size = *PAGE_SIZE.get_or_init(|| unsafe { sysconf(_SC_PAGESIZE) as usize });
    let buffer_size_pages = length.div_ceil(page_size);
    let buffer_size_bytes = buffer_size_pages * page_size;

    unsafe {
        libc::posix_memalign(buffer_ptr, page_size, buffer_size_bytes);
        libc::mprotect(
            *buffer_ptr,
            buffer_size_bytes,
            libc::PROT_EXEC | libc::PROT_WRITE | libc::PROT_READ,
        );
        // for now, prepopulate with 'RET'
        libc::memset(*buffer_ptr, code_gen::RET as i32, buffer_size_bytes);

        Vec::from_raw_parts(buffer.assume_init() as *mut u8, length, buffer_size_bytes)
    }
}

/// Clone a slice of bytes into new executable memory pages.
///
/// The returned vector is immutable because re-allocation could result in lost
/// memory protection settings.
pub fn make_executable(source: &[u8]) -> Immutable<Vec<u8>> {
    let mut executable = allocate_buffer(source.len());
    executable.copy_from_slice(source);
    Immutable::new(executable)
}
