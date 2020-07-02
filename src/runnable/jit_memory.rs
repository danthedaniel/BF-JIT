use std::fmt;
use std::mem;
use std::ops::{Index, IndexMut};

use libc::{sysconf, _SC_PAGESIZE};

use runnable::Runnable;

lazy_static! {
    static ref PAGE_SIZE: usize = unsafe { sysconf(_SC_PAGESIZE) as usize };
}

/// Round up an integer division.
///
/// * `numerator` - The upper component of a division
/// * `denominator` - The lower component of a division
fn int_ceil(numerator: usize, denominator: usize) -> usize {
    (numerator / denominator + 1) * denominator
}

/// Container for executable bytes.
pub struct JITMemory {
    contents: Vec<u8>,
}

impl JITMemory {
    /// Clone a vector of bytes into new executable memory pages.
    pub fn new(source: Vec<u8>) -> JITMemory {
        let size = int_ceil(source.len(), *PAGE_SIZE);
        let contents: Vec<u8>;

        unsafe {
            let mut ptr: *mut libc::c_void = mem::MaybeUninit::uninit().assume_init();

            libc::posix_memalign(&mut ptr, *PAGE_SIZE, size);
            libc::mprotect(
                ptr,
                size,
                libc::PROT_EXEC | libc::PROT_READ | libc::PROT_WRITE,
            );
            libc::memset(ptr, 0xc3, size); // for now, prepopulate with 'RET'

            contents = Vec::from_raw_parts(ptr as *mut u8, source.len(), size);
        }

        let mut jit = JITMemory { contents };

        // Copy source into JIT memory.
        for (index, &byte) in source.iter().enumerate() {
            jit[index] = byte;
        }

        jit
    }
}

impl Runnable for JITMemory {
    fn run(&mut self) {
        let mut bf_mem = vec![0u8; 30_000]; // Memory space used by BrainFuck
        let mem_ptr = bf_mem.as_mut_ptr();
        let func: fn(*mut u8) -> () = unsafe { mem::transmute(self.contents.as_mut_ptr()) };

        func(mem_ptr);
    }
}

impl Index<usize> for JITMemory {
    type Output = u8;

    fn index(&self, index: usize) -> &u8 {
        &self.contents[index]
    }
}

impl IndexMut<usize> for JITMemory {
    fn index_mut(&mut self, index: usize) -> &mut u8 {
        &mut self.contents[index]
    }
}

/// Display hexadecimal values for contents.
impl fmt::Debug for JITMemory {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        for byte in self.contents.iter() {
            write!(f, "{:02X}", byte)?;
        }

        writeln!(f)
    }
}
