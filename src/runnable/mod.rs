/// Simple interface for an type that can be invoked without any arguments and
/// with no return value.
///
/// Rather than this trait FnMut would have been used were it a stable feature.
pub trait Runnable {
    /// Invoke this type.
    fn run(&mut self);
}

mod fucker;
mod immutable;
mod jit_helpers;
mod jit_promise;
mod jit_target;
#[cfg(test)]
mod test_buffer;
#[cfg(test)]
pub use self::test_buffer::SharedBuffer;

pub use self::fucker::Fucker;
pub use self::jit_promise::{JITPromise, JITPromiseID, PromiseSet};
pub use self::jit_target::{JITTarget, JITTargetVTable};
