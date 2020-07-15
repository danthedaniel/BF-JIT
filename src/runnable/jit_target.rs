use std::collections::VecDeque;
use std::{
    cell::RefCell,
    mem,
    ops::{Index, IndexMut},
    rc::Rc,
};

use super::super::code_gen;
use super::super::parser::ASTNode;

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
fn make_executable(source: &Vec<u8>) -> Vec<u8> {
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

    data
}

pub type JITPromiseID = usize;

/// Holds ASTNodes for later compilation.
#[derive(Debug)]
enum JITPromise {
    Deferred(VecDeque<ASTNode>),
    Compiled(JITTarget),
}

impl JITPromise {
    pub fn source(&self) -> &VecDeque<ASTNode> {
        match self {
            JITPromise::Deferred(source) => source,
            JITPromise::Compiled(JITTarget { source, .. }) => source,
        }
    }
}

impl PartialEq for JITPromise {
    fn eq(&self, other: &Self) -> bool {
        self.source() == other.source()
    }
}

#[derive(Clone, Debug)]
struct PromisePool {
    promises: Rc<RefCell<Vec<JITPromise>>>,
}

impl PromisePool {
    pub fn new() -> Self {
        PromisePool {
            promises: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub fn len(&self) -> usize {
        self.promises.borrow().len()
    }

    pub fn find(&self, new_promise: &JITPromise) -> Option<(usize, &JITPromise)> {
        self.promises
            .borrow()
            .iter()
            .enumerate()
            .find(|&(_index, promise)| promise == new_promise)
    }

    pub fn add(&mut self, new_promise: &JITPromise) -> JITPromiseID {
        let vec = self.promises.borrow_mut();

        if let Some((index, _)) = vec
            .iter()
            .enumerate()
            .find(|&(_index, promise)| promise == new_promise)
        {
            return index;
        }

        vec.push(*new_promise);
        return vec.len() - 1;
    }
}

impl Index<usize> for PromisePool {
    type Output = JITPromise;

    fn index(&self, id: JITPromiseID) -> &Self::Output {
        &self.promises.borrow()[id]
    }
}

impl IndexMut<usize> for PromisePool {
    fn index_mut(&mut self, id: JITPromiseID) -> &mut Self::Output {
        &mut self.promises.borrow_mut()[id]
    }
}

/// Container for executable bytes.
#[derive(Debug)]
pub struct JITTarget {
    source: VecDeque<ASTNode>,
    bytes: Vec<u8>,
    promise_pool: PromisePool,
}

impl JITTarget {
    /// Initialize a JIT compiled version of a program.
    #[cfg(target_arch = "x86_64")]
    pub fn new(nodes: &VecDeque<ASTNode>) -> Result<Self, String> {
        let mut bytes = Vec::new();
        let promise_pool = PromisePool::new();

        code_gen::wrapper(
            &mut bytes,
            Self::shallow_compile(nodes, &mut promise_pool.clone()),
        );

        Ok(Self {
            source: nodes.clone(),
            bytes: make_executable(&bytes),
            promise_pool,
        })
    }

    /// No-op version for unsupported architectures.
    #[cfg(not(target_arch = "x86_64"))]
    pub fn new(nodes: &VecDeque<ASTNode>) -> Result<Self, String> {
        Err(format!("Unsupported JIT architecture."))
    }

    #[cfg(target_arch = "x86_64")]
    fn new_fragment(nodes: &VecDeque<ASTNode>, promise_pool: PromisePool) -> Self {
        let mut bytes = Vec::new();

        code_gen::wrapper(
            &mut bytes,
            Self::compile_loop(nodes, &mut promise_pool.clone()),
        );

        Self {
            source: nodes.clone(),
            bytes: make_executable(&bytes),
            promise_pool,
        }
    }

    /// Convert a vector of ASTNodes into a sequence of executable bytes.
    ///
    /// r10 is used to hold the data pointer.
    #[cfg(target_arch = "x86_64")]
    fn shallow_compile(nodes: &VecDeque<ASTNode>, promise_pool: &mut PromisePool) -> Vec<u8> {
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
                    bytes.extend(Self::defer_loop(nodes, promise_pool))
                }
                ASTNode::Loop(nodes) => bytes.extend(Self::compile_loop(nodes, promise_pool)),
            };
        }

        bytes
    }

    /// Perform AOT compilation on a loop.
    #[cfg(target_arch = "x86_64")]
    fn compile_loop(nodes: &VecDeque<ASTNode>, promise_pool: &mut PromisePool) -> Vec<u8> {
        let mut bytes = Vec::new();

        code_gen::aot_loop(&mut bytes, Self::shallow_compile(nodes, promise_pool));

        bytes
    }

    /// Perform JIT compilation on a loop.
    #[cfg(target_arch = "x86_64")]
    fn defer_loop(nodes: &VecDeque<ASTNode>, promise_pool: &mut PromisePool) -> Vec<u8> {
        let mut bytes = Vec::new();
        let new_promise = JITPromise::Deferred(nodes.clone());

        // If an identical promise is already in the pool, use that existing
        // promise instead of adding the new one.
        if let Some((id, promise)) = promise_pool.find(&new_promise) {
            match promise {
                JITPromise::Compiled(jit_target) => {
                    code_gen::aot_loop(&mut bytes, jit_target.bytes.clone());
                }
                JITPromise::Deferred(_source) => {
                    code_gen::jit_loop(&mut bytes, id);
                }
            };

            return bytes;
        }

        code_gen::jit_loop(&mut bytes, promise_pool.add(&new_promise));

        bytes
    }

    /// Execute the bytes buffer as a function with context.
    #[cfg(target_arch = "x86_64")]
    fn exec(&mut self, mem_ptr: *mut u8) -> *mut u8 {
        let jit_callback_ptr = Self::jit_callback;

        type JITCallbackType = extern "C" fn(&mut JITTarget, JITPromiseID, *mut u8) -> *mut u8;
        let func: fn(*mut u8, &mut JITTarget, JITCallbackType) -> *mut u8 =
            unsafe { mem::transmute(self.bytes.as_ptr()) };

        func(mem_ptr, self, jit_callback_ptr)
    }

    /// Callback passed into compiled code. Allows for deferred compilation
    /// targets to be compiled, ran, and later re-ran.
    #[cfg(target_arch = "x86_64")]
    extern "C" fn jit_callback(&mut self, loop_index: JITPromiseID, mem_ptr: *mut u8) -> *mut u8 {
        let mut return_ptr = mem_ptr;
        let mut new_target = None;

        if let JITPromise::Deferred(nodes) = &self.promise_pool[loop_index] {
            new_target = Some(Self::new_fragment(&nodes, self.promise_pool.clone()));
            return_ptr = new_target.unwrap().exec(mem_ptr);
        }

        if let JITPromise::Compiled(jit_target) = &mut self.promise_pool[loop_index] {
            return_ptr = jit_target.exec(mem_ptr);
        }

        // if let Some(jit_target) = &new_target {
        //     self.promise_pool[loop_index] = JITPromise::Compiled(jit_target);
        // }

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
