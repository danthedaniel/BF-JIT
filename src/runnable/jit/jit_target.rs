use super::super::Runnable;
use super::code_gen;
use super::immutable::Immutable;
use super::jit_helpers::make_executable;
use super::jit_promise::{JITPromise, JITPromiseID, PromiseSet};
use crate::parser::AstNode;
use crate::runnable::BF_MEMORY_SIZE;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt;
use std::io::{self, Read, Write};
use std::mem;
use std::rc::Rc;

/// Set arbitrarily
const INLINE_THRESHOLD: usize = 0x16;

/// Indexes into the vtable passed into JIT compiled code
pub enum VTableEntry {
    JITCallback = 0,
    Read = 1,
    Print = 2,
}

/// A type to unify all function pointers behind. Because the vtable is not used in the
/// Rust code at all, the type is not important.
type VoidPtr = *mut ();
/// VTable for JIT compiled code
type VTable<const SIZE: usize> = [VoidPtr; SIZE];

pub struct JITContext {
    /// All non-root JITTargets in the program
    promises: PromiseSet,
    /// Reader that can be overridden to allow for input from a source other than stdin
    pub io_read: Box<dyn Read>,
    /// Writer that can be overriden to allow for output to a location other than stdout
    pub io_write: Box<dyn Write>,
}

/// Container for executable bytes.
pub struct JITTarget {
    /// Original AST
    pub source: VecDeque<AstNode>,
    /// Executable bytes buffer
    bytes: Immutable<Vec<u8>>,
    /// Globals for the whole program
    pub context: Rc<RefCell<JITContext>>,
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
    pub fn new(nodes: VecDeque<AstNode>) -> Self {
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

        Self {
            source: nodes,
            bytes: make_executable(&bytes),
            context,
        }
    }

    fn new_fragment(context: Rc<RefCell<JITContext>>, nodes: VecDeque<AstNode>) -> Self {
        let mut bytes = Vec::new();

        code_gen::wrapper(
            &mut bytes,
            Self::compile_loop(nodes.clone(), context.clone()),
        );

        Self {
            source: nodes,
            bytes: make_executable(&bytes),
            context,
        }
    }

    /// Compile a vector of AstNodes into executable bytes.
    fn shallow_compile(nodes: VecDeque<AstNode>, context: Rc<RefCell<JITContext>>) -> Vec<u8> {
        let mut bytes = Vec::new();

        for node in nodes {
            match node {
                AstNode::Incr(n) => code_gen::incr(&mut bytes, n),
                AstNode::Decr(n) => code_gen::decr(&mut bytes, n),
                AstNode::Next(n) => code_gen::next(&mut bytes, n),
                AstNode::Prev(n) => code_gen::prev(&mut bytes, n),
                AstNode::Print => code_gen::print(&mut bytes),
                AstNode::Read => code_gen::read(&mut bytes),
                AstNode::Set(n) => code_gen::set(&mut bytes, n),
                AstNode::AddTo(n) => code_gen::add(&mut bytes, n),
                AstNode::SubFrom(n) => code_gen::sub(&mut bytes, n),
                AstNode::Loop(nodes) if nodes.len() < INLINE_THRESHOLD => {
                    bytes.extend(Self::compile_loop(nodes, context.clone()))
                }
                AstNode::Loop(nodes) => bytes.extend(Self::defer_loop(nodes, context.clone())),
            };
        }

        bytes
    }

    /// Perform AOT compilation on a loop.
    fn compile_loop(nodes: VecDeque<AstNode>, context: Rc<RefCell<JITContext>>) -> Vec<u8> {
        let mut bytes = Vec::new();

        code_gen::aot_loop(&mut bytes, Self::shallow_compile(nodes, context));

        bytes
    }

    /// Perform JIT compilation on a loop.
    fn defer_loop(nodes: VecDeque<AstNode>, context: Rc<RefCell<JITContext>>) -> Vec<u8> {
        let mut bytes = Vec::new();

        code_gen::jit_loop(&mut bytes, context.borrow_mut().promises.add(nodes));

        bytes
    }

    /// Callback passed into compiled code. Allows for deferred compilation
    /// targets to be compiled, ran, and later re-ran.
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

    /// Print a single byte (called by JIT compiled code)
    extern "C" fn print(&mut self, byte: u8) {
        let buffer = [byte];
        let write_result = self.context.borrow_mut().io_write.write_all(&buffer);

        if let Err(error) = write_result {
            panic!("Failed to write to stdout: {}", error);
        }
    }

    /// Read a single byte (called by JIT compiled code)
    extern "C" fn read(&mut self) -> u8 {
        let mut buffer = [0];
        let read_result = self.context.borrow_mut().io_read.read_exact(&mut buffer);

        if let Err(error) = read_result {
            if error.kind() == io::ErrorKind::UnexpectedEof {
                // Just send out newlines forever if the read stream has ended.
                return b'\n';
            }

            panic!("Failed to read from stdin: {}", error);
        }

        buffer[0]
    }

    /// Execute the bytes buffer as a function.
    fn exec(&mut self, mem_ptr: *mut u8) -> *mut u8 {
        let vtable: VTable<3> = [
            Self::jit_callback as VoidPtr,
            Self::read as VoidPtr,
            Self::print as VoidPtr,
        ];

        type JitFunc = extern "C" fn(*mut u8, &mut JITTarget, &VTable<3>) -> *mut u8;
        let func: JitFunc = unsafe { mem::transmute(self.bytes.as_ptr()) };

        func(mem_ptr, self, &vtable)
    }
}

impl Runnable for JITTarget {
    fn run(&mut self) {
        let mut bf_mem = vec![0u8; BF_MEMORY_SIZE]; // Memory space used by BrainFuck
        self.exec(bf_mem.as_mut_ptr());
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::test_buffer::SharedBuffer;
    use super::JITTarget;
    use crate::parser::Ast;
    use crate::runnable::Runnable;
    use std::io::Cursor;

    #[test]
    fn run_hello_world() {
        let ast = Ast::parse(include_str!("../../../test/programs/hello_world.bf")).unwrap();
        let mut jit_target = JITTarget::new(ast.data);
        let shared_buffer = SharedBuffer::new();
        jit_target.context.borrow_mut().io_write = Box::new(shared_buffer.clone());

        jit_target.run();

        let output_string = shared_buffer.get_string_content();
        assert_eq!(output_string, "Hello World!\n");
    }

    #[test]
    fn run_mandelbrot() {
        let ast = Ast::parse(include_str!("../../../test/programs/mandelbrot.bf")).unwrap();
        let mut jit_target = JITTarget::new(ast.data);
        let shared_buffer = SharedBuffer::new();
        jit_target.context.borrow_mut().io_write = Box::new(shared_buffer.clone());

        jit_target.run();

        let output_string = shared_buffer.get_string_content();
        let expected_output = include_str!("../../../test/programs/mandelbrot.out");
        assert_eq!(output_string, expected_output);
    }

    #[test]
    fn run_rot13() {
        // This rot13 program terminates after 16 characters so we can test it. Otherwise it would
        // wait on input forever.
        let ast = Ast::parse(include_str!("../../../test/programs/rot13-16char.bf")).unwrap();
        let mut jit_target = JITTarget::new(ast.data);
        let shared_buffer = SharedBuffer::new();
        jit_target.context.borrow_mut().io_write = Box::new(shared_buffer.clone());
        let in_cursor = Box::new(Cursor::new("Hello World! 123".as_bytes().to_vec()));
        jit_target.context.borrow_mut().io_read = in_cursor;

        jit_target.run();

        let output_string = shared_buffer.get_string_content();
        assert_eq!(output_string, "Uryyb Jbeyq! 123");
    }
}
