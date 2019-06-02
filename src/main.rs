extern crate libc;

#[macro_use]
extern crate serde_derive;
extern crate docopt;

mod fucker;
mod jit_memory;
mod runnable;

use std::fs::File;
use std::io::Read;
use std::process::exit;

use docopt::Docopt;

use fucker::Program;

const USAGE: &'static str = "
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

    let program = read_file(&args.arg_program)
        .and_then(|chars| Program::parse(chars))
        .unwrap_or_else(|e| {
            eprintln!("Error occurred while loading program: {}", e);
            exit(1)
        });

    if args.flag_debug {
        println!("{:?}", program);

        return;
    }

    let mut runnable = if args.flag_jit {
        program.jit().unwrap_or_else(|msg| {
            eprintln!("Error occurred while compiling program: {}", msg);
            exit(1)
        })
    } else {
        program.int()
    };

    runnable.run();
}

fn read_file(path: &str) -> Result<Vec<char>, String> {
    let mut file = File::open(path).map_err(|e| format!("Could not open file {:?}", e))?;
    let mut buffer: String = String::new();

    file.read_to_string(&mut buffer)
        .map_err(|e| format!("Could not read file {:?}", e))?;

    Ok(buffer.chars().collect())
}
