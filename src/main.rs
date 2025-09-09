#[macro_use]
extern crate serde_derive;

mod parser;
mod runnable;

use anyhow::{Context, Result};
use docopt::Docopt;
use std::fs::File;
use std::io::{Read, stdin};

use parser::AstNode;
use runnable::Runnable;
use runnable::int::Interpreter;
#[cfg(feature = "jit")]
use runnable::jit::JITTarget;

const USAGE: &str = "
Fucker

Usage:
  fucker [--int] <program>
  fucker (--ast) <program>
  fucker (-h | --help)

Options:
  -h --help     Show this screen.
  --ast         Display intermediate language.
  --int         Use an interpreter instead of the JIT compiler.
";

#[derive(Debug, Deserialize)]
struct Args {
    arg_program: String,
    flag_ast: bool,
    flag_int: bool,
}

fn main() -> Result<()> {
    let args: Args = Docopt::new(USAGE)
        .and_then(|d| d.deserialize())
        .unwrap_or_else(|e| e.exit());

    let program = read_program(&args.arg_program)
        .and_then(|source| AstNode::parse(&source))
        .with_context(|| format!("Failed to load program: {}", args.arg_program))?;

    if args.flag_ast {
        println!("{program:?}");
        return Ok(());
    }

    let mut runnable: Box<dyn Runnable> = if args.flag_int {
        Box::new(Interpreter::new(program))
    } else {
        #[cfg(not(feature = "jit"))]
        {
            anyhow::bail!("JIT is not supported for this architecture");
        }

        #[cfg(feature = "jit")]
        Box::new(JITTarget::new(program)?)
    };

    runnable
        .run()
        .with_context(|| "Runtime error occurred during program execution")?;
    Ok(())
}

/// Read a brainfuck program's source code.
///
/// When path is "-" this will read from stdin.
fn read_program(path: &str) -> Result<String> {
    let mut buffer: String = String::new();
    let mut source: Box<dyn Read> = {
        if path == "-" {
            Box::new(stdin())
        } else {
            Box::new(File::open(path).with_context(|| format!("Could not open file: {path}"))?)
        }
    };

    source
        .read_to_string(&mut buffer)
        .context("Could not read file content")?;

    Ok(buffer)
}
