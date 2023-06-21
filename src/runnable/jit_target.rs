use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt;
use std::io::{self, Read, Write};
use std::mem;
use std::ops::{Deref, DerefMut};
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
    use super::JITTarget;
    use libc::c_void;
    use std::io::{self, Read, Write};

    /// Print a single byte
    pub extern "C" fn print(jit_target: *mut c_void, byte: u8) {
        let io_write = unsafe {
            &mut jit_target
                .cast::<JITTarget>()
                .as_mut()
                .expect("jit_target was NULL")
                .context
                .borrow_mut()
                .io_write
        };

        let buffer = [byte];
        let write_result = io_write.write_all(&buffer);

        if let Err(error) = write_result {
            panic!("Failed to write to stdout: {}", error);
        }
    }

    /// Read a single byte
    pub extern "C" fn read(jit_target: *mut c_void) -> u8 {
        let io_read = unsafe {
            &mut jit_target
                .cast::<JITTarget>()
                .as_mut()
                .expect("jit_target was NULL")
                .context
                .borrow_mut()
                .io_read
        };

        let mut buffer = [0];
        let read_result = io_read.read_exact(&mut buffer);

        if let Err(error) = read_result {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                // Just send out newlines forever if the read stream has ended.
                return b'\n';
            }

            panic!("Failed to read from stdin: {}", error);
        }

        buffer[0]
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
        let mut buffer = mem::MaybeUninit::<*mut libc::c_void>::uninit();
        let buffer_ptr = buffer.as_mut_ptr();

        libc::posix_memalign(buffer_ptr, *PAGE_SIZE, size);
        libc::mprotect(
            *buffer_ptr,
            size,
            libc::PROT_EXEC | libc::PROT_READ | libc::PROT_WRITE,
        );
        // for now, prepopulate with 'RET'
        libc::memset(*buffer_ptr, code_gen::RET as i32, size);

        data = Vec::from_raw_parts(buffer.assume_init() as *mut u8, source.len(), size);
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

/// The global set of JITPromises for a program.
#[derive(Debug, Default)]
struct PromiseSet(Vec<Option<JITPromise>>);

impl PromiseSet {
    /// By either searching for an equivalent promise, or creating a new one,
    /// return a promise ID for a vector of ASTNodes.
    pub fn add(&mut self, nodes: VecDeque<ASTNode>) -> JITPromiseID {
        for (index, promise) in self.iter().enumerate() {
            if let Some(promise) = promise {
                if promise.source() == &nodes {
                    return index;
                }
            }
            // It's possible for `promise` to be None here. If the call stack
            // look like:
            //
            // * PromisePool::add
            // * JITTarget::defer_loop
            // * JITTarget::shallow_compile
            // * JITTarget::new_fragment
            // * JITTarget::jit_callback
            //
            // then the JITPromise that was plucked from this PromisePool in
            // JITTarget::jit_callback has not been placed back into the pool
            // yet. This won't lead to duplicates and thus is not a problem
            // since it is not possible for a loop to contain itself.
            // (i.e. BrainFuck does not support recursion).
        }

        // If this is a new promise, add it to the pool.
        self.push(Some(JITPromise::Deferred(nodes)));
        return self.len() - 1;
    }
}

impl Deref for PromiseSet {
    type Target = Vec<Option<JITPromise>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for PromiseSet {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

struct JITContext {
    /// All non-root JITTargets in the program
    promises: PromiseSet,
    /// Reader that can be overridden to allow for input from a source other than stdin
    io_read: Box<dyn Read>,
    /// Writer that can be overriden to allow for output to a location other than stdout
    io_write: Box<dyn Write>,
}

/// Container for executable bytes.
pub struct JITTarget {
    /// Original AST
    source: VecDeque<ASTNode>,
    /// Executable bytes buffer
    bytes: Immutable<Vec<u8>>,
    /// Globals for the whole program
    context: Rc<RefCell<JITContext>>,
}

impl fmt::Debug for JITTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JITTarget")
            .field("source", &self.source)
            .field("bytes", &self.bytes)
            .field("promises", &self.context.borrow().promises)
            .finish()
    }
}

impl JITTarget {
    /// Initialize a JIT compiled version of a program.
    #[cfg(target_arch = "x86_64")]
    pub fn new(nodes: VecDeque<ASTNode>) -> Result<Self, String> {
        let mut bytes = Vec::new();
        let context = Rc::new(RefCell::new(JITContext {
            promises: PromiseSet::default(),
            io_read: Box::new(io::stdin()),
            io_write: Box::new(io::stdout()),
        }));

        code_gen::wrapper(
            &mut bytes,
            Self::shallow_compile(nodes.clone(), context.clone()),
        );

        Ok(Self {
            source: nodes,
            bytes: make_executable(&bytes),
            context: context.clone(),
        })
    }

    /// No-op version for unsupported architectures.
    #[cfg(not(target_arch = "x86_64"))]
    pub fn new(nodes: VecDeque<ASTNode>) -> Result<Self, String> {
        Err(format!("Unsupported JIT architecture."))
    }

    #[cfg(target_arch = "x86_64")]
    fn new_fragment(context: Rc<RefCell<JITContext>>, nodes: VecDeque<ASTNode>) -> Self {
        let mut bytes = Vec::new();

        code_gen::wrapper(
            &mut bytes,
            Self::compile_loop(nodes.clone(), context.clone()),
        );

        Self {
            source: nodes,
            bytes: make_executable(&bytes),
            context: context.clone(),
        }
    }

    /// Compile a vector of ASTNodes into executable bytes.
    #[cfg(target_arch = "x86_64")]
    fn shallow_compile(nodes: VecDeque<ASTNode>, context: Rc<RefCell<JITContext>>) -> Vec<u8> {
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
                ASTNode::AddTo(n) => code_gen::add(&mut bytes, n),
                ASTNode::SubFrom(n) => code_gen::sub(&mut bytes, n),
                ASTNode::Loop(nodes) if nodes.len() < INLINE_THRESHOLD => {
                    bytes.extend(Self::compile_loop(nodes, context.clone()))
                }
                ASTNode::Loop(nodes) => bytes.extend(Self::defer_loop(nodes, context.clone())),
            };
        }

        bytes
    }

    /// Perform AOT compilation on a loop.
    #[cfg(target_arch = "x86_64")]
    fn compile_loop(nodes: VecDeque<ASTNode>, context: Rc<RefCell<JITContext>>) -> Vec<u8> {
        let mut bytes = Vec::new();

        code_gen::aot_loop(&mut bytes, Self::shallow_compile(nodes, context));

        bytes
    }

    /// Perform JIT compilation on a loop.
    #[cfg(target_arch = "x86_64")]
    fn defer_loop(nodes: VecDeque<ASTNode>, context: Rc<RefCell<JITContext>>) -> Vec<u8> {
        let mut bytes = Vec::new();

        code_gen::jit_loop(&mut bytes, context.borrow_mut().promises.add(nodes));

        bytes
    }

    /// Execute the bytes buffer as a function.
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
        let mut promise = self.context.borrow_mut().promises[promise_id]
            .take()
            .expect("Someone forgot to put a promise back");
        let return_ptr;
        let new_promise;

        match promise {
            JITPromise::Deferred(nodes) => {
                let mut new_target = Self::new_fragment(self.context.clone(), nodes);
                return_ptr = new_target.exec(mem_ptr);
                new_promise = Some(JITPromise::Compiled(new_target));
            }
            JITPromise::Compiled(ref mut jit_target) => {
                return_ptr = jit_target.exec(mem_ptr);
                new_promise = Some(promise);
            }
        };

        self.context.borrow_mut().promises[promise_id] = new_promise;

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
    use crate::runnable::SharedBuffer;
    use std::io::Cursor;

    #[test]
    fn run_hello_world() {
        let ast = AST::parse(include_str!("../../test/programs/hello_world.bf")).unwrap();
        let mut jit_target = JITTarget::new(ast.data).unwrap();
        let shared_buffer = SharedBuffer::new();
        jit_target.context.borrow_mut().io_write = Box::new(shared_buffer.clone());

        jit_target.run();

        let output_string = shared_buffer.get_string_content();
        assert_eq!(output_string, "Hello World!\n");
    }

    #[test]
    fn run_mandelbrot() {
        let ast = AST::parse(include_str!("../../test/programs/mandelbrot.bf")).unwrap();
        let mut jit_target = JITTarget::new(ast.data).unwrap();
        let shared_buffer = SharedBuffer::new();
        jit_target.context.borrow_mut().io_write = Box::new(shared_buffer.clone());

        jit_target.run();

        let output_string = shared_buffer.get_string_content();
        let expected_output = include_str!("../../test/programs/mandelbrot.out");
        assert_eq!(output_string, expected_output);
    }

    #[test]
    fn run_rot13() {
        // This rot13 program terminates after 16 characters so we can test it. Otherwise it would
        // wait on input forever.
        let ast = AST::parse(include_str!("../../test/programs/rot13-16char.bf")).unwrap();
        let mut jit_target = JITTarget::new(ast.data).unwrap();
        let shared_buffer = SharedBuffer::new();
        jit_target.context.borrow_mut().io_write = Box::new(shared_buffer.clone());
        let in_cursor = Box::new(Cursor::new("Hello World! 123".as_bytes().to_vec()));
        jit_target.context.borrow_mut().io_read = in_cursor;

        jit_target.run();

        let output_string = shared_buffer.get_string_content();
        assert_eq!(output_string, "Uryyb Jbeyq! 123");
    }
}
