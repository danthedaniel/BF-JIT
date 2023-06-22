use std::{
    collections::VecDeque,
    ops::{Deref, DerefMut},
};

use crate::parser::ASTNode;

use super::JITTarget;

pub type JITPromiseID = usize;

/// Holds ASTNodes for later compilation.
#[derive(Debug)]
pub enum JITPromise {
    Deferred(VecDeque<ASTNode>),
    Compiled(JITTarget),
}

impl JITPromise {
    pub fn source(&self) -> &VecDeque<ASTNode> {
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
    /// return a promise ID for a vector of ASTNodes.
    pub fn add(&mut self, nodes: VecDeque<ASTNode>) -> JITPromiseID {
        for (index, promise) in self.iter().enumerate() {
            if let Some(promise) = promise {
                if promise.source() == &nodes {
                    return index;
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
        return self.len() - 1;
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
