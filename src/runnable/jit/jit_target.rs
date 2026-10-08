use anyhow::{Context, Result};
use std::cell::RefCell;
use std::io::{self, Read, Write};
use std::rc::Rc;
use std::{fmt, slice};

use super::code_gen;
use super::executable_memory::{ExecutableMemory, VoidPtr};
use super::jit_promise::{JITPromise, JITPromiseID, PromiseSet};
use crate::parser::{AstNode, max_offset};
use crate::runnable::jit::executable_memory::VTable;
use crate::runnable::syscall::{execute_syscall, parse_syscall_args};
use crate::runnable::{BF_MEMORY_SIZE, Runnable};

/// Loops with fewer nodes than this (not counting nested loops' bodies) are
/// compiled inline rather than deferred. Calling a deferred loop costs
/// saving and restoring registers, which adds up for short hot loops.
const INLINE_THRESHOLD: usize = 0x100;

pub struct JITContext {
    /// All non-root `JITTargets` in the program
    promises: PromiseSet,
    /// Entry points of compiled promises, or null. Compiled code calls these
    /// directly. Allocated once with room for every promise ID so it never moves.
    fragments: Box<[VoidPtr]>,
    /// Reader that can be overridden to allow for input from a source other than stdin
    pub io_read: Box<dyn Read>,
    /// Writer that can be overriden to allow for output to a location other than stdout
    pub io_write: Box<dyn Write>,
}

impl Default for JITContext {
    fn default() -> Self {
        Self {
            promises: PromiseSet::default(),
            fragments: vec![std::ptr::null(); 1 << 16].into_boxed_slice(),
            io_read: Box::new(io::stdin()),
            io_write: Box::new(io::stdout()),
        }
    }
}

/// Container for executable bytes.
pub struct JITTarget {
    /// Original AST
    pub source: Vec<AstNode>,
    /// Executable bytes buffer
    executable: ExecutableMemory,
    /// Globals for the whole program
    pub(crate) context: Rc<RefCell<JITContext>>,
}

impl fmt::Debug for JITTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JITTarget")
            .field("source", &self.source)
            .field("executable", &self.executable)
            .field("promises", &self.context.borrow().promises)
            .finish()
    }
}

impl JITTarget {
    /// Initialize a JIT compiled version of a program.
    pub fn new(ast: Vec<AstNode>) -> Result<Self> {
        let mut bytes = Vec::new();
        let context = Rc::new(RefCell::new(JITContext::default()));

        code_gen::wrapper(&mut bytes, Self::shallow_compile(ast.clone(), &context));

        let executable = ExecutableMemory::new(&bytes)
            .context("Failed to create executable memory for JIT target")?;

        Ok(Self {
            source: ast,
            executable,
            context,
        })
    }

    fn new_fragment(context: Rc<RefCell<JITContext>>, nodes: Vec<AstNode>) -> Result<Self> {
        let mut bytes = Vec::new();

        code_gen::wrapper_fragment(&mut bytes, Self::compile_loop(nodes.clone(), &context));

        let executable = ExecutableMemory::new(&bytes)
            .context("Failed to create executable memory for JIT fragment")?;

        Ok(Self {
            source: nodes,
            executable,
            context,
        })
    }

    /// Compile a vector of `AstNodes` into executable bytes.
    fn shallow_compile(nodes: Vec<AstNode>, context: &Rc<RefCell<JITContext>>) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut nodes = nodes.into_iter().peekable();

        while let Some(node) = nodes.next() {
            match node {
                AstNode::Add(..)
                | AstNode::Set(..)
                | AstNode::MulAdd { .. }
                | AstNode::CondAdd { .. }
                | AstNode::ProductAdd { .. } => {
                    let mut run = vec![node];
                    while let Some(next) = nodes.next_if(Self::is_straight_line) {
                        run.push(next);
                    }
                    code_gen::straight_line(&mut bytes, &run);
                }
                AstNode::Move(n) => code_gen::move_pointer(&mut bytes, n),
                AstNode::Print(offset) => code_gen::print(&mut bytes, offset),
                AstNode::Read(offset) => code_gen::read(&mut bytes, offset),
                AstNode::Scan(stride) => code_gen::scan(&mut bytes, stride),
                AstNode::Loop(nodes) if nodes.len() < INLINE_THRESHOLD => {
                    bytes.extend(Self::compile_loop(nodes, context));
                }
                AstNode::Loop(nodes) => bytes.extend(Self::defer_loop(nodes, context)),
                AstNode::Syscall => code_gen::syscall(&mut bytes),
            }
        }

        bytes
    }

    const fn is_straight_line(node: &AstNode) -> bool {
        matches!(
            node,
            AstNode::Add(..)
                | AstNode::Set(..)
                | AstNode::MulAdd { .. }
                | AstNode::CondAdd { .. }
                | AstNode::ProductAdd { .. }
        )
    }

    /// Perform AOT compilation on a loop.
    fn compile_loop(mut nodes: Vec<AstNode>, context: &Rc<RefCell<JITContext>>) -> Vec<u8> {
        let mut bytes = Vec::new();

        if nodes.iter().all(Self::is_straight_line) && code_gen::register_loop(&mut bytes, &nodes) {
            return bytes;
        }

        // Fold the pointer movement at the end of the body into the loop condition.
        let trailing_move = match nodes.last() {
            Some(&AstNode::Move(n)) => {
                nodes.pop();
                n
            }
            _ => 0,
        };

        code_gen::aot_loop(
            &mut bytes,
            Self::shallow_compile(nodes, context),
            trailing_move,
        );

        bytes
    }

    /// Perform JIT compilation on a loop.
    fn defer_loop(nodes: Vec<AstNode>, context: &Rc<RefCell<JITContext>>) -> Vec<u8> {
        let mut bytes = Vec::new();

        code_gen::jit_loop(&mut bytes, context.borrow_mut().promises.add(nodes));

        bytes
    }

    /// Callback passed into compiled code. Allows for deferred compilation
    /// targets to be compiled, ran, and later re-ran.
    extern "C" fn jit_callback(&mut self, promise_id: JITPromiseID, mem_ptr: *mut u8) -> *mut u8 {
        let promise_index = usize::from(promise_id.value());
        let mut promise = self.context.borrow_mut().promises[promise_index]
            .take()
            .expect("Someone forgot to put a promise back");

        let return_ptr;
        let new_promise;

        match promise {
            JITPromise::Deferred(nodes) => {
                let mut new_target = Self::new_fragment(self.context.clone(), nodes)
                    .expect("Failed to create JIT fragment during callback");
                self.context.borrow_mut().fragments[promise_index] =
                    new_target.executable.as_fn() as VoidPtr;
                return_ptr = new_target.exec(mem_ptr);
                new_promise = Some(JITPromise::Compiled(new_target));
            }
            JITPromise::Compiled(ref mut jit_target) => {
                return_ptr = jit_target.exec(mem_ptr);
                new_promise = Some(promise);
            }
        }

        self.context.borrow_mut().promises[promise_index] = new_promise;

        return_ptr
    }

    /// Print a single byte (called by JIT compiled code)
    extern "C" fn print(&mut self, byte: u8) {
        let buffer = [byte];
        let write_result = self.context.borrow_mut().io_write.write_all(&buffer);

        if let Err(error) = write_result {
            panic!("Failed to write to output: {error}");
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

            panic!("Failed to read from input: {error}");
        }

        buffer[0]
    }

    /// Execute a syscall using the systemf convention.
    /// Called by JIT compiled code.
    ///
    /// Arguments:
    /// - `mem_ptr`: Pointer to the current cell in the brainfuck memory
    /// - `bf_mem_base`: Base pointer to the brainfuck memory
    ///
    /// Returns the low byte of the syscall return value.
    #[allow(clippy::unused_self)]
    extern "C" fn syscall(&mut self, mem_ptr: *mut u8, bf_mem_base: *mut u8) -> u8 {
        // Get memory from current cell to end of the full memory segment
        let memory = unsafe {
            let cell = usize::try_from(mem_ptr.offset_from(bf_mem_base))
                .expect("mem_ptr is below the base memory location");
            slice::from_raw_parts(mem_ptr, BF_MEMORY_SIZE - cell)
        };

        let syscall_args =
            parse_syscall_args(memory, bf_mem_base).expect("Invalid syscall argument type");

        #[allow(clippy::cast_possible_truncation)]
        {
            execute_syscall(&syscall_args) as u8
        }
    }

    /// Execute the bytes buffer as a function.
    pub(crate) fn exec(&mut self, mem_ptr: *mut u8) -> *mut u8 {
        let vtable: VTable<5> = [
            Self::jit_callback as VoidPtr,
            Self::read as VoidPtr,
            Self::print as VoidPtr,
            Self::syscall as VoidPtr,
            self.context.borrow().fragments.as_ptr().cast(),
        ];

        self.executable.as_fn()(mem_ptr, self, &vtable)
    }
}

impl Runnable for JITTarget {
    fn run(&mut self) -> Result<()> {
        let padding = max_offset(&self.source) as usize;
        // Memory space used by BrainFuck
        let mut bf_mem = vec![0u8; padding + BF_MEMORY_SIZE + padding];
        self.exec(bf_mem[padding..].as_mut_ptr());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::test_buffer::TestBuffer;
    use super::JITTarget;
    use crate::parser::AstNode;
    use crate::runnable::BF_MEMORY_SIZE;
    use crate::runnable::Runnable;
    use std::io::Cursor;

    #[test]
    fn run_hello_world() {
        let ast = AstNode::parse(
            include_str!("../../../tests/programs/hello_world.bf"),
            false,
        )
        .unwrap();
        let mut jit_target = JITTarget::new(ast).unwrap();
        let shared_buffer = TestBuffer::new();
        jit_target.context.borrow_mut().io_write = Box::new(shared_buffer.clone());

        jit_target.run().unwrap();

        let output_string = shared_buffer.get_string_content();
        assert_eq!(output_string, "Hello World!\n");
    }

    #[test]
    fn run_mandelbrot() {
        let ast =
            AstNode::parse(include_str!("../../../tests/programs/mandelbrot.bf"), false).unwrap();
        let mut jit_target = JITTarget::new(ast).unwrap();
        let shared_buffer = TestBuffer::new();
        jit_target.context.borrow_mut().io_write = Box::new(shared_buffer.clone());

        jit_target.run().unwrap();

        let output_string = shared_buffer.get_string_content();
        let expected_output = include_str!("../../../tests/programs/mandelbrot.out");
        assert_eq!(output_string, expected_output);
    }

    #[test]
    fn run_rot13() {
        // This rot13 program terminates after 16 characters so we can test it. Otherwise it would
        // wait on input forever.
        let ast = AstNode::parse(
            include_str!("../../../tests/programs/rot13-16char.bf"),
            false,
        )
        .unwrap();
        let mut jit_target = JITTarget::new(ast).unwrap();
        let shared_buffer = TestBuffer::new();
        jit_target.context.borrow_mut().io_write = Box::new(shared_buffer.clone());
        let in_cursor = Box::new(Cursor::new(b"Hello World! 123".to_vec()));
        jit_target.context.borrow_mut().io_read = in_cursor;

        jit_target.run().unwrap();

        let output_string = shared_buffer.get_string_content();
        assert_eq!(output_string, "Uryyb Jbeyq! 123");
    }

    #[test]
    fn test_multiply_add_to() {
        // Set cell 0 to 5, then multiply by 3 and add to cell 2
        let nodes = vec![
            AstNode::Set(0, 5),
            AstNode::MulAdd {
                src: 0,
                dst: 2,
                factor: 3,
            },
        ];

        let mut jit_target = JITTarget::new(nodes).unwrap();

        // Create a custom memory to inspect results
        let mut bf_mem = vec![0u8; BF_MEMORY_SIZE];
        jit_target.exec(bf_mem.as_mut_ptr());

        assert_eq!(bf_mem[0], 5);
        assert_eq!(bf_mem[2], 15);
    }
}
