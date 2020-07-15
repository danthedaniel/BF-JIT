extern crate libc;

#[macro_use]
extern crate serde_derive;
extern crate docopt;

#[macro_use]
extern crate lazy_static;

mod code_gen;
mod parser;
mod runnable;

use std::fs::File;
use std::io::{stdin, Read};
use std::process::exit;

use docopt::Docopt;

use parser::AST;
use runnable::{Fucker, JITTarget, Runnable};

const USAGE: &str = "
Fucker

Usage:
  fucker [--jit] <program>
  fucker (-d | --debug) <program>
  fucker (-h | --help)

Options:
  -h --help     Show this screen.
  -d --debug    Display intermediate language.
  --jit         JIT compile the program before running (x86-64 only).
";

#[derive(Debug, Deserialize)]
struct Args {
    arg_program: String,
    flag_debug: bool,
    flag_jit: bool,
}

fn main() {
    let args: Args = Docopt::new(USAGE)
        .and_then(|d| d.deserialize())
        .unwrap_or_else(|e| e.exit());

    let program = read_program(&args.arg_program)
        .and_then(|source| AST::parse(&source))
        .unwrap_or_else(|e| {
            eprintln!("Error occurred while loading program: {}", e);
            exit(1)
        });

    if args.flag_debug {
        println!("{:?}", program);

        return;
    }

    let mut runnable: Box<dyn Runnable> = if args.flag_jit {
        let jit_target = JITTarget::new(&program.data).unwrap_or_else(|msg| {
            eprintln!("Error occurred while compiling program: {}", msg);
            exit(1)
        });
        Box::new(jit_target)
    } else {
        Box::new(Fucker::new(&program.data))
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
