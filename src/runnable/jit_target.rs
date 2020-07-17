use std::collections::VecDeque;
use std::mem;

use super::super::code_gen;
use super::super::parser::ASTNode;
use super::immutable::Immutable;

use libc::{sysconf, _SC_PAGESIZE};

use runnable::Runnable;

const INLINE_THRESHOLD: usize = 0x16;

lazy_static! {
    static ref PAGE_SIZE: usize = unsafe { sysconf(_SC_PAGESIZE) as usize };
}

/// Functions called by JIT-compiled code.
mod jit_functions {
    use libc::{c_int, getchar, putchar};

    /// Print a single byte to stdout.
    pub extern "C" fn print(byte: u8) {
        unsafe {
            putchar(byte as c_int);
        }
    }

    /// Read a single byte from stdin.
    pub extern "C" fn read() -> u8 {
        unsafe { getchar() as u8 }
    }
}

/// Round up an integer division.
///
/// * `numerator` - The upper component of a division
/// * `denominator` - The lower component of a division
fn int_ceil(numerator: usize, denominator: usize) -> usize {
    (numerator / denominator + 1) * denominator
}

/// Clone a vector of bytes into new executable memory pages.
///
/// The returned vector is immutable because re-allocation could result in lost
/// memory protection settings.
fn make_executable(source: &Vec<u8>) -> Immutable<Vec<u8>> {
    let size = int_ceil(source.len(), *PAGE_SIZE);
    let mut data: Vec<u8>;

    unsafe {
        let mut ptr: *mut libc::c_void = mem::MaybeUninit::uninit().assume_init();

        libc::posix_memalign(&mut ptr, *PAGE_SIZE, size);
        libc::mprotect(
            ptr,
            size,
            libc::PROT_EXEC | libc::PROT_READ | libc::PROT_WRITE,
        );
        libc::memset(ptr, 0xc3, size); // for now, prepopulate with 'RET'

        data = Vec::from_raw_parts(ptr as *mut u8, source.len(), size);
    }

    for (index, &byte) in source.iter().enumerate() {
        data[index] = byte;
    }

    Immutable::new(data)
}

pub type JITPromiseID = usize;

/// Holds ASTNodes for later compilation.
#[derive(Debug)]
pub enum JITPromise {
    Deferred(VecDeque<ASTNode>),
    Compiled(JITTarget),
}

/// Container for executable bytes.
#[derive(Debug)]
pub struct JITTarget {
    bytes: Immutable<Vec<u8>>,
    promises: Vec<JITPromise>,
}

impl JITTarget {
    /// Initialize a JIT compiled version of a program.
    #[cfg(target_arch = "x86_64")]
    pub fn new(nodes: &VecDeque<ASTNode>) -> Result<Self, String> {
        let mut bytes = Vec::new();
        let mut promises = Vec::new();

        code_gen::wrapper(&mut bytes, Self::shallow_compile(nodes, &mut promises));

        Ok(Self {
            bytes: make_executable(&bytes),
            promises,
        })
    }

    /// No-op version for unsupported architectures.
    #[cfg(not(target_arch = "x86_64"))]
    pub fn new(nodes: &VecDeque<ASTNode>) -> Result<Self, String> {
        Err(format!("Unsupported JIT architecture."))
    }

    #[cfg(target_arch = "x86_64")]
    fn new_fragment(nodes: &VecDeque<ASTNode>) -> Self {
        let mut bytes = Vec::new();
        let mut promises = Vec::new();

        code_gen::wrapper(&mut bytes, Self::compile_loop(nodes, &mut promises));

        Self {
            bytes: make_executable(&bytes),
            promises,
        }
    }

    /// Compile a vector of ASTNodes into executable bytes.
    #[cfg(target_arch = "x86_64")]
    fn shallow_compile(nodes: &VecDeque<ASTNode>, promises: &mut Vec<JITPromise>) -> Vec<u8> {
        let mut bytes = Vec::new();

        for node in nodes {
            match node {
                ASTNode::Incr(n) => code_gen::incr(&mut bytes, *n),
                ASTNode::Decr(n) => code_gen::decr(&mut bytes, *n),
                ASTNode::Next(n) => code_gen::next(&mut bytes, *n),
                ASTNode::Prev(n) => code_gen::prev(&mut bytes, *n),
                ASTNode::Print => code_gen::print(&mut bytes, jit_functions::print),
                ASTNode::Read => code_gen::read(&mut bytes, jit_functions::read),
                ASTNode::Loop(nodes) if nodes.len() >= INLINE_THRESHOLD => {
                    bytes.extend(Self::defer_loop(nodes, promises))
                }
                ASTNode::Loop(nodes) => bytes.extend(Self::compile_loop(nodes, promises)),
            };
        }

        bytes
    }

    /// Perform AOT compilation on a loop.
    #[cfg(target_arch = "x86_64")]
    fn compile_loop(nodes: &VecDeque<ASTNode>, promises: &mut Vec<JITPromise>) -> Vec<u8> {
        let mut bytes = Vec::new();

        code_gen::aot_loop(&mut bytes, Self::shallow_compile(nodes, promises));

        bytes
    }

    /// Perform JIT compilation on a loop.
    #[cfg(target_arch = "x86_64")]
    fn defer_loop(nodes: &VecDeque<ASTNode>, promises: &mut Vec<JITPromise>) -> Vec<u8> {
        let mut bytes = Vec::new();

        promises.push(JITPromise::Deferred(nodes.clone()));

        code_gen::jit_loop(&mut bytes, promises.len() - 1);

        bytes
    }

    /// Execute the bytes buffer as a function with context.
    #[cfg(target_arch = "x86_64")]
    fn exec(&mut self, mem_ptr: *mut u8) -> *mut u8 {
        type JITCallbackType = extern "C" fn(&mut JITTarget, JITPromiseID, *mut u8) -> *mut u8;
        let func: fn(*mut u8, &mut JITTarget, JITCallbackType) -> *mut u8 =
            unsafe { mem::transmute(self.bytes.as_ptr()) };

        func(mem_ptr, self, Self::jit_callback)
    }

    /// Callback passed into compiled code. Allows for deferred compilation
    /// targets to be compiled, ran, and later re-ran.
    #[cfg(target_arch = "x86_64")]
    extern "C" fn jit_callback(&mut self, loop_index: JITPromiseID, mem_ptr: *mut u8) -> *mut u8 {
        let promise = &mut self.promises[loop_index];
        let return_ptr;

        match promise {
            JITPromise::Deferred(nodes) => {
                let mut new_target = Self::new_fragment(nodes);
                return_ptr = new_target.exec(mem_ptr);
                *promise = JITPromise::Compiled(new_target);
            }
            JITPromise::Compiled(jit_target) => {
                return_ptr = jit_target.exec(mem_ptr);
            }
        };

        return_ptr
    }
}

impl Runnable for JITTarget {
    #[cfg(target_arch = "x86_64")]
    fn run(&mut self) {
        let mut bf_mem = vec![0u8; 30_000]; // Memory space used by BrainFuck
        let mem_ptr = bf_mem.as_mut_ptr();

        self.exec(mem_ptr);
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn run(&mut self) {}
}

#[cfg(target_arch = "x86_64")]
#[cfg(test)]
mod tests {
    use super::super::super::parser::AST;
    use super::*;

    #[test]
    fn run_hello_world() {
        let ast = AST::parse(include_str!("../../test/programs/hello_world.bf")).unwrap();
        let mut jit_target = JITTarget::new(&ast.data).unwrap();
        jit_target.run();
    }

    #[test]
    fn run_mandelbrot() {
        let ast = AST::parse(include_str!("../../test/programs/mandelbrot.bf")).unwrap();
        let mut jit_target = JITTarget::new(&ast.data).unwrap();
        jit_target.run();
    }
}
