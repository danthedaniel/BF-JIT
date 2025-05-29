use std::collections::VecDeque;
use std::ops::{Deref, DerefMut};

use super::JITTarget;
use crate::parser::AstNode;

#[repr(C)]
pub struct JITPromiseID(usize);

impl JITPromiseID {
    pub fn value(&self) -> usize {
        return self.0;
    }
}

/// Holds AstNodes for later compilation.
#[derive(Debug)]
pub enum JITPromise {
    Deferred(VecDeque<AstNode>),
    Compiled(JITTarget),
}

impl JITPromise {
    pub fn source(&self) -> &VecDeque<AstNode> {
        match self {
            JITPromise::Deferred(source) => source,
            JITPromise::Compiled(JITTarget { source, .. }) => source,
        }
    }
}

/// The global set of JITPromises for a program.
#[derive(Debug, Default)]
pub struct PromiseSet(Vec<Option<JITPromise>>);

impl PromiseSet {
    /// By either searching for an equivalent promise, or creating a new one,
    /// return a promise ID for a vector of AstNodes.
    pub fn add(&mut self, nodes: VecDeque<AstNode>) -> JITPromiseID {
        for (index, promise) in self.iter().enumerate() {
            if let Some(promise) = promise {
                if promise.source() == &nodes {
                    return JITPromiseID(index);
                }
            }
            // It's possible for `promise` to be None here. If the call stack
            // look like:
            //
            // * PromisePool::add
            // * JITTarget::defer_loop
            // * JITTarget::shallow_compile
            // * JITTarget::new_fragment
            // * JITTarget::jit_callback
            //
            // then the JITPromise that was plucked from this PromisePool in
            // JITTarget::jit_callback has not been placed back into the pool
            // yet. This won't lead to duplicates and thus is not a problem
            // since it is not possible for a loop to contain itself.
            // (i.e. BrainFuck does not support recursion).
        }

        // If this is a new promise, add it to the pool.
        self.push(Some(JITPromise::Deferred(nodes)));

        JITPromiseID(self.len() - 1)
    }
}

impl Deref for PromiseSet {
    type Target = Vec<Option<JITPromise>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for PromiseSet {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
