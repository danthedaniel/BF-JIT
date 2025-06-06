BF Just-in-Time Compiler
===

[On crates.io](https://crates.io/crates/fucker)

A very over-engineered [BrainFuck](https://en.wikipedia.org/wiki/Brainfuck)
interpreter/optimizing JIT compiler written in rust. Done from first-principles
without any research or examination of prior art\*.

**\*Update**:
The aarch64 implementation in `src/runnable/jit/code_gen/aarch64.rs` was written
almost entirely by Claude 4 Opus.

## Supports

- Linux x86-64
- Linux aarch64
- MacOS x86-64
- MacOS aarch64

## Usage

```
  fucker [--int] <program>
  fucker (-d | --debug) <program>
  fucker (-h | --help)

Options:
  -h --help     Show this screen.
  -d --debug    Display intermediate language.
  --int         Use an interpreter instead of the JIT compiler.
```

## What is BrainFuck?

[BrainFuck](https://en.wikipedia.org/wiki/Brainfuck) is an esoteric programming
language designed to be both turing complete and easy to compile. The environment
provides the programmer with an a 30,000 cell array of unsigned bytes and a data
pointer. There are only 8 single character commands:

* `+` : Increment the current memory cell by 1 (with wrapping overflow)
* `-` : Decrement the current memory cell by 1 (with wrapping underflow)
* `>` : Shift the data pointer to the next memory cell
* `<` : Shift the data pointer to the previous memory cell
* `.` : Output the current memory cell as an ASCII character
* `,` : Read one ASCII character from stdin
* `[` : Jump to the matching `]` if the current memory cell is `0`
* `]` : Jump to the matching `[` if the current memory cell is not `0`

## Implementation

### Parser and AST

The compiler first parses BrainFuck source code into an Abstract Syntax Tree
(AST) representation. This intermediate representation enables optimizations
before execution:

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum AstNode {
    Incr(u8), // Add to current cell
    Decr(u8), // Subtract from current cell
    Next(usize), // Move data pointer right
    Prev(usize), // Move data pointer left
    Print, // Output current cell as ASCII
    Read, // Read ASCII input to current cell
    Set(u8), // Set current cell to literal value
    AddTo(isize), // Add current cell to offset cell, zero current
    SubFrom(isize),// Subtract current cell from offset cell, zero current
    MultiplyAddTo(isize, u8), // Multiply current cell and add to cell at offset, zero current
    CopyTo(Vec<isize>), // Copy current cell to multiple offsets, zero current
    Loop(VecDeque<AstNode>), // Loop while current cell != 0
}
```

### Optimization Techniques

The compiler implements several optimization passes during AST construction:

#### 1. Run-Length Encoding
Sequential identical operations are combined into single instructions with
counts:
- `++++` becomes `Incr(4)`
- `>>>>` becomes `Next(4)`
- `----` becomes `Decr(4)`
- `<<<<` becomes `Prev(4)`

This optimization alone provides an approximately 3x speedup on typical
BrainFuck programs.

#### 2. Loop Pattern Recognition
Common BrainFuck idioms are detected and replaced with optimized operations:

**Zero loops**: `[-]` or `[+]` → `Set(0)`
- Replaces loops that simply zero the current cell

**Move loops**: `[-<+>]` → `AddTo(-1)`
- Detects patterns that move the current cell's value to another location
- Supports both addition (`AddTo`) and subtraction (`SubFrom`) variants
- Works with arbitrary offsets in either direction

#### 3. Constant Folding
Operations on literal values are computed at compile time:
- `Set(5)` followed by `Incr(3)` becomes `Set(8)`

### Execution Backends

The compiler supports two execution modes:

#### Interpreter Backend
A traditional bytecode interpreter that executes the optimized AST directly.
This provides:
- Guaranteed compatibility across all architectures
- Fallback when JIT compilation is unavailable

The interpreter uses a simple virtual machine with:
- 30,000+ cell memory array (dynamically expandable)
- Program counter and data pointer
- Stack-based loop handling with pre-computed jump offsets

#### JIT Compiler Backend
A Just-In-Time compiler that generates native machine code for maximum
performance.

**Supported Architectures:**
- x86-64
- AArch64

**JIT Compilation Strategy:**
The JIT uses a hybrid approach combining Ahead-of-Time (AOT) and Just-in-Time
compilation:

1. **Small loops** (< 22 instructions): Compiled immediately (AOT)
2. **Large loops**: Deferred compilation using a promise system
3. **Hot code paths**: Compiled on first execution, cached for subsequent runs

**Code Generation:**
- Direct assembly generation without external assemblers
- Register allocation optimized for BrainFuck's memory model:
  - `r10`/`x19`: Data pointer register
  - `r11`/`x20`: JIT context pointer
  - `r12`/`x21`: Virtual function table pointer
- Efficient calling conventions for I/O operations
- Proper stack frame management and callee-saved register preservation

**Assembly Code Examples:**

The JIT compiler generates native assembly for each BrainFuck operation:

*Increment (`++++` → `Incr(4)`):*
```asm
; x86-64
add    BYTE PTR [r10], 4

; AArch64
ldrb   w8, [x19]
add    w8, w8, #4
strb   w8, [x19]
```

*Pointer movement (`>>>>` → `Next(4)`):*
```asm
; x86-64
add    r10, 4

; AArch64
add    x19, x19, #4
```

*Cell zeroing (`[-]` → `Set(0)`):*
```asm
; x86-64
mov    BYTE PTR [r10], 0

; AArch64
mov    w8, #0
strb   w8, [x19]
```

*Move operation (`[-<+>]` → `AddTo(-1)`):*
```asm
; x86-64
movzx  eax, BYTE PTR [r10]    ; Load current cell
add    BYTE PTR [r10-1], al   ; Add to target cell
mov    BYTE PTR [r10], 0      ; Zero current cell

; AArch64
ldrb   w8, [x19]              ; Load current cell
ldrb   w9, [x19, #-1]         ; Load target cell
add    w9, w9, w8             ; Add values
strb   w9, [x19, #-1]         ; Store to target
mov    w8, #0                 ; Zero current cell
strb   w8, [x19]
```

**Memory Management:**
- Executable memory pages allocated with proper permissions
- Automatic cleanup of compiled code fragments
- Promise-based deferred compilation for memory efficiency

### Control Flow Handling

**Loops** are the most complex aspect of BrainFuck compilation:

- **AOT loops**: Small loops are compiled inline with conditional jumps
- **JIT loops**: Large loops use a callback mechanism:
  1. First execution triggers compilation via callback
  2. Compiled code is cached in a promise table
  3. Subsequent executions call the cached native code directly

**Jump Resolution:**
- Forward jumps (`[`) use conditional branches that skip the loop body
- Backward jumps (`]`) use conditional branches that return to loop start
- Jump distances are computed during compilation for optimal instruction
  selection

### I/O System

Both backends use an I/O system supporting:
- Standard input/output (default)
- Custom readers/writers for testing
- Proper error handling for EOF conditions
- UTF-8/ASCII character handling

The JIT compiler implements I/O through a virtual function table, allowing:
- Efficient native code calls to Rust I/O functions
- Consistent behavior between interpreter and JIT modes
- Easy testing with mock I/O streams

## Benchmarks

Ran on [mandelbrot.bf](https://github.com/erikdubbelboer/brainfuck-jit/blob/919df502dc8a0441572180700de86be405387fcc/mandelbrot.bf).

### Intel Core i5-3230M

| Version | Runtime |
|---|--:|
| Naive Interpreter | 56.824s |
| Optimized Interpreter | 19.055s |
| Optimized JIT | 1.06s |

### Apple M3

| Version | Runtime |
|---|--:|
| Optimized Interpreter | 8.18s |
| Optimized JIT | 0.39s |
