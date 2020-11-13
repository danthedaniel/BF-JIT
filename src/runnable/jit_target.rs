use std::cell::RefCell;
use std::collections::VecDeque;
use std::mem;
use std::ops::Deref;
use std::rc::Rc;

use crate::code_gen;
use crate::parser::ASTNode;
use crate::runnable::immutable::Immutable;

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

/// Clone a slice of bytes into new executable memory pages.
///
/// The returned vector is immutable because re-allocation could result in lost
/// memory protection settings.
fn make_executable(source: &[u8]) -> Immutable<Vec<u8>> {
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

impl JITPromise {
    pub fn source(&self) -> &VecDeque<ASTNode> {
        match self {
            JITPromise::Deferred(source) => source,
            JITPromise::Compiled(JITTarget { source, .. }) => source,
        }
    }
}

#[derive(Debug, Clone)]
struct PromisePool(Rc<RefCell<Vec<Option<JITPromise>>>>);

impl PromisePool {
    pub fn new() -> Self {
        PromisePool(Rc::new(RefCell::new(Vec::new())))
    }

    /// By either searching for an equivalent promise, or creating a new one,
    /// return a promise ID for a vector of ASTNodes.
    pub fn add(&mut self, nodes: VecDeque<ASTNode>) -> JITPromiseID {
        let mut promises_inner = self.0.borrow_mut();

        for (index, promise) in promises_inner.iter().enumerate() {
            if let Some(promise) = promise {
                if promise.source() == &nodes {
                    return index;
                }
            }
        }

        // If this is a new promise, add it to the pool.
        promises_inner.push(Some(JITPromise::Deferred(nodes)));
        return promises_inner.len() - 1;
    }
}

impl Deref for PromisePool {
    type Target = Rc<RefCell<Vec<Option<JITPromise>>>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Container for executable bytes.
#[derive(Debug)]
pub struct JITTarget {
    source: VecDeque<ASTNode>,
    bytes: Immutable<Vec<u8>>,
    promises: PromisePool,
}

impl JITTarget {
    /// Initialize a JIT compiled version of a program.
    #[cfg(target_arch = "x86_64")]
    pub fn new(nodes: VecDeque<ASTNode>) -> Result<Self, String> {
        let mut bytes = Vec::new();
        let promises = PromisePool::new();

        code_gen::wrapper(
            &mut bytes,
            Self::shallow_compile(nodes.clone(), promises.clone()),
        );

        Ok(Self {
            source: nodes,
            bytes: make_executable(&bytes),
            promises,
        })
    }

    /// No-op version for unsupported architectures.
    #[cfg(not(target_arch = "x86_64"))]
    pub fn new(nodes: VecDeque<ASTNode>) -> Result<Self, String> {
        Err(format!("Unsupported JIT architecture."))
    }

    #[cfg(target_arch = "x86_64")]
    fn new_fragment(nodes: VecDeque<ASTNode>, promises: PromisePool) -> Self {
        let mut bytes = Vec::new();

        code_gen::wrapper(
            &mut bytes,
            Self::compile_loop(nodes.clone(), promises.clone()),
        );

        Self {
            source: nodes,
            bytes: make_executable(&bytes),
            promises,
        }
    }

    /// Compile a vector of ASTNodes into executable bytes.
    #[cfg(target_arch = "x86_64")]
    fn shallow_compile(nodes: VecDeque<ASTNode>, promises: PromisePool) -> Vec<u8> {
        let mut bytes = Vec::new();

        for node in nodes {
            match node {
                ASTNode::Incr(n) => code_gen::incr(&mut bytes, n),
                ASTNode::Decr(n) => code_gen::decr(&mut bytes, n),
                ASTNode::Next(n) => code_gen::next(&mut bytes, n),
                ASTNode::Prev(n) => code_gen::prev(&mut bytes, n),
                ASTNode::Print => code_gen::print(&mut bytes, jit_functions::print),
                ASTNode::Read => code_gen::read(&mut bytes, jit_functions::read),
                ASTNode::Set(n) => code_gen::set(&mut bytes, n),
                ASTNode::Move(n) => code_gen::move_cell(&mut bytes, n),
                ASTNode::Loop(nodes) if nodes.len() < INLINE_THRESHOLD => {
                    bytes.extend(Self::compile_loop(nodes, promises.clone()))
                }
                ASTNode::Loop(nodes) => bytes.extend(Self::defer_loop(nodes, promises.clone())),
            };
        }

        bytes
    }

    /// Perform AOT compilation on a loop.
    #[cfg(target_arch = "x86_64")]
    fn compile_loop(nodes: VecDeque<ASTNode>, promises: PromisePool) -> Vec<u8> {
        let mut bytes = Vec::new();

        code_gen::aot_loop(&mut bytes, Self::shallow_compile(nodes, promises));

        bytes
    }

    /// Perform JIT compilation on a loop.
    #[cfg(target_arch = "x86_64")]
    fn defer_loop(nodes: VecDeque<ASTNode>, mut promises: PromisePool) -> Vec<u8> {
        let mut bytes = Vec::new();

        code_gen::jit_loop(&mut bytes, promises.add(nodes));

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
    extern "C" fn jit_callback(&mut self, promise_id: JITPromiseID, mem_ptr: *mut u8) -> *mut u8 {
        let mut promise = self.promises.borrow_mut()[promise_id]
            .take()
            .expect("Someone forgot to put a promise back");
        let return_ptr;
        let new_promise;

        match promise {
            JITPromise::Deferred(nodes) => {
                let mut new_target = Self::new_fragment(nodes, self.promises.clone());
                return_ptr = new_target.exec(mem_ptr);
                new_promise = Some(JITPromise::Compiled(new_target));
            }
            JITPromise::Compiled(ref mut jit_target) => {
                return_ptr = jit_target.exec(mem_ptr);
                new_promise = Some(promise);
            }
        };

        self.promises.borrow_mut()[promise_id] = new_promise;

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
    use super::*;
    use crate::parser::AST;

    #[test]
    fn run_hello_world() {
        let ast = AST::parse(include_str!("../../test/programs/hello_world.bf")).unwrap();
        let mut jit_target = JITTarget::new(ast.data).unwrap();
        jit_target.run();
    }

    #[test]
    fn run_mandelbrot() {
        let ast = AST::parse(include_str!("../../test/programs/mandelbrot.bf")).unwrap();
        let mut jit_target = JITTarget::new(ast.data).unwrap();
        jit_target.run();
    }
}
