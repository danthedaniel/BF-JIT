BrainFuck
===

A very over-engineered BrainFuck interpreter/optimizing JIT compiler written in
rust.

## Usage

```
  fucker [--jit] <program>
  fucker (-d | --debug) <program>
  fucker (-h | --help)

Options:
  -h --help     Show this screen.
  -d --debug    Display intermediate language.
  --jit         JIT compile the program before running (x86-64 only).
```

## What is BrainFuck?

[BrainFuck](https://en.wikipedia.org/wiki/Brainfuck) is an esoteric programming
language designed as a joke. The environment provides the programmer with an
"infinite" array of unsigned bytes and a data pointer. There are only 8 single
character commands:

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
    BeginLoop(Option<usize>),
    EndLoop(Option<usize>),
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
implement in x86-64.

---

`+`:

```asm
add    BYTE PTR [r10],n
```

Where:

* `r10` is used as the data pointer
* `n` is the same value that is held by `Incr` in the `Instr` enum

`-`, `>` and `<` are equally simple.

---

`Print` and `Read` are slightly more complex but don't require us to do any
control flow ourselves.

---

Where we start to get into trouble is with `[` and `]`. To avoid the difficulty
of tracking labels and linking them together before execution, all instructions'
x86-64 machine code is padded with `nop`s.

```rust
while bytes.len() < BF_INSTR_SIZE as usize {
    // nop
    bytes.push(0x90);
}
```

This means that the jump targets can be easily found as long as you know the
target position (in the `Program` data vector), current position, and unpadded
size of the current instruction:

```rust
let begin_loop_size: i32 = 10; // Bytes

let offset = (*pos as i32 - this_pos as i32) * BF_INSTR_SIZE - begin_loop_size;
let offset_bytes: [u8; mem::size_of::<i32>()] = unsafe { mem::transmute(offset) };

// Check if the current memory cell equals zero.
// cmp    BYTE PTR [r10],0x0
bytes.push(0x41);
bytes.push(0x80);
bytes.push(0x3a);
bytes.push(0x00);

// Jump to the end of the loop if equal.
// je    offset
bytes.push(0x0f);
bytes.push(0x84);
bytes.push(offset_bytes[0]);
bytes.push(offset_bytes[1]);
bytes.push(offset_bytes[2]);
bytes.push(offset_bytes[3]);
```

## Benchmarks

Ran on [mandelbrot.bf](https://github.com/erikdubbelboer/brainfuck-jit/blob/919df502dc8a0441572180700de86be405387fcc/mandelbrot.bf)

| Version | Runtime |
|---|--:|
| Naive Interpreter | 56.824s |
| Optimized Interpreter | 19.055s |
| Optimized JIT | 5.484s |
