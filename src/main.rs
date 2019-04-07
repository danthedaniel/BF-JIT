extern crate libc;

#[macro_use]
extern crate serde_derive;
extern crate docopt;

mod fucker;

use docopt::Docopt;
use fucker::{Fucker, Instr};
use std::fs::File;
use std::io;
use std::io::Read;
use std::process::exit;

const USAGE: &'static str = "
Fucker

Usage:
  fucker <program>
  fucker (-d | --debug) <program>
  fucker (-h | --help)

Options:
  -h --help     Show this screen.
  -d --debug    Display intermediate language.
";

#[derive(Debug, Deserialize)]
struct Args {
    arg_program: String,
    flag_debug: bool,
}

fn main() {
    let args: Args = Docopt::new(USAGE)
        .and_then(|d| d.deserialize())
        .unwrap_or_else(|e| e.exit());

    let program = read_file(&args.arg_program)
        .map(|chars| Instr::parse(chars))
        .unwrap_or_else(|e| {
            eprintln!("Could not read file: {}", e);
            exit(1)
        });

    if args.flag_debug {
        display_program(program);
        return;
    }

    let mut fucker = Fucker::new(program);
    fucker.run(&mut std::io::stdout());
}

fn display_program(program: Vec<fucker::Instr>) {
    println!("Addr\tInstr\tOperands");

    for (pos, instr) in program.iter().enumerate() {
        println!("0x{:04X}\t{}", pos, instr);
    }
}

fn read_file(path: &str) -> io::Result<Vec<char>> {
    let mut file = File::open(path)?;
    let mut buffer: String = String::new();
    file.read_to_string(&mut buffer)?;

    Ok(buffer.chars().collect())
}
