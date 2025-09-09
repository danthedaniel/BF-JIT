pub mod int;
#[cfg(feature = "jit")]
pub mod jit;

use anyhow::Result;

const BF_MEMORY_SIZE: usize = 30_000;

/// Simple interface for an type that can be invoked without any arguments and
/// with no return value.
///
/// Rather than this trait `FnMut` would have been used were it a stable feature.
pub trait Runnable {
    /// Invoke this type.
    fn run(&mut self) -> Result<()>;
}

#[cfg(test)]
mod test_buffer;
