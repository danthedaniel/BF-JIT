extern crate libc;

mod fucker;

use fucker::{Fucker, Instr};
use std::fs::File;
use std::io;
use std::io::Read;

fn main() {
    if let Ok(program) = read_file("hello_world.bf").map(|chars| Instr::parse(chars)) {
        println!("{:?} {}", program, program.len());
        let mut fucker = Fucker::new(program);
        fucker.run();
    } else {
        eprintln!("Could no read file!");
    }
}

fn read_file(path: &str) -> io::Result<Vec<char>> {
    let mut file = File::open(path)?;
    let mut buffer: String = String::new();
    let file_size = file.read_to_string(&mut buffer)?;

    Ok(buffer.chars().collect())
}
