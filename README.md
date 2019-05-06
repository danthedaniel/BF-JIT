BrainFuck
===

BrainFuck interpreter/optimizing JIT compiler written in rust.

# Usage

```
  fucker [--jit] <program>
  fucker (-d | --debug) <program>
  fucker (-h | --help)

Options:
  -h --help     Show this screen.
  -d --debug    Display intermediate language.
  --jit         JIT compile the program before running (x86-64 only).
```
