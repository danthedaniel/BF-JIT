use super::code_gen;

use super::immutable::Immutable;
use std::mem;
use std::sync::OnceLock;

use libc::{sysconf, _SC_PAGESIZE};

static PAGE_SIZE: OnceLock<usize> = OnceLock::new();

/// Round up an integer division.
///
/// * `numerator` - The upper component of a division
/// * `denominator` - The lower component of a division
fn int_div_ceil(numerator: usize, denominator: usize) -> usize {
    (numerator / denominator + 1) * denominator
}

/// Clone a slice of bytes into new executable memory pages.
///
/// The returned vector is immutable because re-allocation could result in lost
/// memory protection settings.
pub fn make_executable(source: &[u8]) -> Immutable<Vec<u8>> {
    let mut executable: Vec<u8>;

    {
        let mut buffer = mem::MaybeUninit::<*mut libc::c_void>::uninit();
        let buffer_ptr = buffer.as_mut_ptr();

        let page_size = *PAGE_SIZE.get_or_init(|| unsafe { sysconf(_SC_PAGESIZE) as usize });
        let buffer_size = int_div_ceil(source.len(), page_size);

        unsafe {
            libc::posix_memalign(buffer_ptr, page_size, buffer_size);
            libc::mprotect(
                *buffer_ptr,
                buffer_size,
                libc::PROT_EXEC | libc::PROT_WRITE | libc::PROT_READ,
            );
            // for now, prepopulate with 'RET'
            libc::memset(*buffer_ptr, code_gen::RET as i32, buffer_size);

            executable =
                Vec::from_raw_parts(buffer.assume_init() as *mut u8, source.len(), buffer_size);
        }
    }

    for (index, &byte) in source.iter().enumerate() {
        executable[index] = byte;
    }

    Immutable::new(executable)
}
