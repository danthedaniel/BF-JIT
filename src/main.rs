extern crate libc;

#[macro_use]
extern crate serde_derive;
extern crate docopt;

mod parser;
mod runnable;

use docopt::Docopt;
use std::fs::File;
use std::io::{Read, stdin};
use std::process::exit;

use parser::Ast;
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

fn main() {
    let args: Args = Docopt::new(USAGE)
        .and_then(|d| d.deserialize())
        .unwrap_or_else(|e| e.exit());

    let program = read_program(&args.arg_program)
        .and_then(|source| Ast::parse(&source))
        .unwrap_or_else(|e| {
            eprintln!("Error occurred while loading program: {}", e);
            exit(1)
        });

    if args.flag_ast {
        println!("{:?}", program);
        return;
    }

    let mut runnable: Box<dyn Runnable> = if args.flag_int {
        Box::new(Interpreter::new(program.data))
    } else {
        #[cfg(not(feature = "jit"))]
        {
            eprintln!("JIT is not supported for this architecture");
            exit(1);
        }

        #[cfg(feature = "jit")]
        Box::new(JITTarget::new(program.data))
    };

    runnable.run();
}

/// Read a BrainFuck program's source code.
///
/// When path is "-" this will read from stdin.
fn read_program(path: &str) -> Result<String, String> {
    let mut buffer: String = String::new();
    let mut source: Box<dyn Read> = {
        if path == "-" {
            Box::new(stdin())
        } else {
            Box::new(File::open(path).map_err(|e| format!("Could not open file: {:?}", e))?)
        }
    };

    source
        .read_to_string(&mut buffer)
        .map_err(|e| format!("Could not read file: {:?}", e))?;

    Ok(buffer)
}
