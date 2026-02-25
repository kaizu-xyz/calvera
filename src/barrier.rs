use crate::Sequence;

/// As in none, not zero. This is used to indicate that a producer has not yet produced any items.
pub const NONE: Sequence = -1;

// TODO: Document Send + Sync
pub trait Barrier: Send + Sync {
    fn get_after(&self, prev: Sequence) -> Sequence;
}
