BF Just-in-Time Compiler
===

[On crates.io](https://crates.io/crates/fucker)

A very over-engineered [BrainFuck](https://en.wikipedia.org/wiki/Brainfuck) interpreter/optimizing JIT compiler written in
rust. Done from first-principles without any research or examination of prior art.

## Supported Architectures

The JIT compiler supports the following architectures:
- x86-64 (Linux and MacOS)
- AArch64 (Linux and MacOS)

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
* `-` : Decrememt the current memory cell by 1 (with wrapping underflow)
* `>` : Shift the data pointer to the next memory cell
* `<` : Shift the data pointer to the previous memory cell
* `.` : Output the current memory cell as an ASCII character
* `,` : Read one ASCII character from stdin
* `[` : Jump to the matching `]` if the current memory cell is `0`
* `]` : Jump to the matching `[` if the current memory cell is not `0`

## Implementation

### Optimization

The lowest hanging fruit here is to perform run-length encoding on the
instructions. Sequential `+`, `-`, `>` and `<` commands can be combined before
they are executed. Internally this is done by compiling to an intermediate
language - which is stored as a vector of `Instr`s:

```rust
pub struct Program {
    pub data: Vec<Instr>,
}

pub enum Instr {
    Incr(u8),
    Decr(u8),
    Next(usize),
    Prev(usize),
    Print,
    Read,
    BeginLoop(usize),
    EndLoop(usize),
}
```

Without any other optimizations performed (unless you count stripping out
comments before execution) this alone results in a ~3x speedup when benchmarked
against a BrainFuck Mandelbrot set renderer.

What's next? The more complicated BrainFuck programs are generated from a high
level macro language. Decompiling from BrainFuck back to this language could
allow me to do more intelligent code execution.

### JIT Compiling

While impossible to read BrainFuck code itself, BrainFuck is probably the
simplest turing-complete language. This makes it an ideal candidate for
exploring JIT compilation.

The first six of our instructions defined in `Instr` are pretty straitforward to
implement in x86-64 or AArch64.

---

`+` (x86-64):

```asm
add    BYTE PTR [r10],n
```

`+` (AArch64):

```asm
ldrb   w8, [x19]
add    w8, w8, #n
strb   w8, [x19]
```

Where:

* `r10` (x86-64) or `x19` (AArch64) is used as the data pointer
* `n` is the same value that is held by `Incr` in the `Instr` enum

`-`, `>` and `<` are equally simple.

---

`Print` and `Read` are slightly more complex but don't require us to do any
control flow ourselves.

## Benchmarks

Ran on [mandelbrot.bf](https://github.com/erikdubbelboer/brainfuck-jit/blob/919df502dc8a0441572180700de86be405387fcc/mandelbrot.bf).

Tested with a Intel Core i5-3230M.

| Version | Runtime |
|---|--:|
| Naive Interpreter | 56.824s |
| Optimized Interpreter | 19.055s |
| Optimized JIT | 1.450s |
