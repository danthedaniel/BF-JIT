use std::fmt;
use std::ops::Index;

use super::super::runnable::{Fucker, JITMemory, Runnable};
use super::Instr;

#[derive(Clone)]
pub struct Program {
    pub data: Vec<Instr>,
}

impl Program {
    pub fn new(data: Vec<Instr>) -> Self {
        Program { data }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Initialize a JIT compiled version of this program.
    #[cfg(target_arch = "x86_64")]
    pub fn jit(&self) -> Result<Box<dyn Runnable>, String> {
        let mut bytes = Vec::new();

        // push   rbp
        bytes.push(0x55);

        // mov    rbp,rsp
        bytes.push(0x48);
        bytes.push(0x89);
        bytes.push(0xe5);

        // Store pointer to brainfuck memory (first argument) in r10
        // mov    r10,rdi
        bytes.push(0x49);
        bytes.push(0x89);
        bytes.push(0xfa);

        for instr in self.data.iter() {
            bytes.extend(instr.jit());
        }

        // mov    rsp,rbp
        bytes.push(0x48);
        bytes.push(0x89);
        bytes.push(0xec);

        // pop    rbp
        bytes.push(0x5d);

        // ret
        bytes.push(0xc3);

        Ok(Box::new(JITMemory::new(bytes)))
    }

    /// No-op version of jit() for unsupported architectures.
    #[cfg(not(target_arch = "x86_64"))]
    pub fn jit(&self) -> Result<Box<dyn Runnable>, String> {
        Err(format!("Unsupported JIT architecture."))
    }

    /// Initialize a BrainFuck interpreter that will use this program.
    pub fn int(&self) -> Box<dyn Runnable> {
        Box::new(Fucker::new(self.clone()))
    }
}

impl Index<usize> for Program {
    type Output = Instr;

    fn index(&self, index: usize) -> &Instr {
        &self.data[index]
    }
}

impl fmt::Debug for Program {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "Addr\tInstr\tOperands")?;

        for (pos, instr) in self.data.iter().enumerate() {
            writeln!(f, "0x{:04X}\t{:?}", pos, instr)?;
        }

        writeln!(f)
    }
}
