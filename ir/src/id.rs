use std::fmt::{self, Display, Formatter};
use std::num::NonZeroU64;
use std::process::abort;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// An opaque type that refers to a particular representation instance in
/// HarvestIR.
// Because IDs can be generated and dropped, it is possible (on 32-bit systems)
// for the ID counter to exceed usize::MAX. Therefore, we use 64-bit IDs (and in
// practice, we run on 64-bit systems, so that matches usize anyway). NonZeroU64
// is used to make Option<Id> smaller, because it's easy and doesn't have a
// downside.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Id(NonZeroU64);

impl Id {
    /// Returns a new ID that has not been seen before.
    #[allow(clippy::new_without_default, reason = "Id has no single default value")]
    pub fn new() -> Id {
        let [out] = Id::new_array();
        out
    }

    /// Returns an array of new, unique IDs.
    ///
    /// # Example
    /// ```
    /// # use harvest_core::Id;
    /// # fn main() {
    ///     // Allocate two new IDs.
    ///     let [c_ast, rust_ast] = Id::new_array();
    /// # }
    /// ```
    pub fn new_array<const LEN: usize>() -> [Id; LEN] {
        // The highest ID allocated so far. Each new_array() call starts
        // allocating IDs at HIGHEST_ID + 1.
        static HIGHEST_ID: AtomicU64 = AtomicU64::new(0);
        new_array_testable(&HIGHEST_ID)
    }
}

impl Display for Id {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "Id({})", self.0)
    }
}

impl From<Id> for u64 {
    fn from(value: Id) -> u64 {
        value.0.into()
    }
}

// `Id::new_array`, but with an injected AtomicU64. This allows `tests::new` to
// use its own AtomicU64, which prevents other tests that are run in parallel
// from interfering with it.
fn new_array_testable<const LEN: usize>(highest_id: &AtomicU64) -> [Id; LEN] {
    // prev is the ID number immediately before the ID we are currently trying
    // to construct.
    let mut prev = highest_id.fetch_add(LEN.try_into().expect("LEN > u64::MAX"), Relaxed);
    [(); LEN].map(|_| {
        let Some(num) = prev.checked_add(1).and_then(NonZeroU64::new) else {
            // We don't have any way to continue execution on overflow. If we
            // try to panic, this tool invocation will fail, but the panic will
            // be caught and we'll just run into this again. Fortunately, it's
            // basically impossible for this to overflow, so we won't hit this
            // case in any useful harvest_translate execution.
            eprintln!("IR ID allocation overflow, cannot continue");
            abort();
        };
        prev = num.get();
        Id(num)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::thread::{scope, spawn};

    #[test]
    fn display() {
        assert_eq!(
            format!("{}", Id(NonZeroU64::new(1234).unwrap())),
            "Id(1234)"
        );
    }

    // Verifies that Id::new and Id::new_array return unique IDs (to verify they
    // correctly use new_array_testable).
    #[test]
    fn new() {
        let one_at_a_time = spawn(|| (0..100).map(|_| Id::new()).collect());
        let all_at_once = Id::new_array::<100>();
        let mut ids: Vec<_> = one_at_a_time.join().unwrap();
        ids.extend(all_at_once);
        let deduplicated: HashSet<Id> = ids.iter().copied().collect();
        assert_eq!(ids.len(), deduplicated.len(), "duplicate ID");
    }

    // Verifies that new_array_testable works as designed. The contract of Id is
    // simply that each generated ID is unique, but if we simply generate N
    // random u64s (for a reasonably-sized N) then uniqueness is likely to
    // happen by accident. Instead, this specifically tests that the IDs are
    // sequential and start at 1 to verify the implementation is behaving as
    // intended.
    #[test]
    fn new_implementation() {
        // Use a smaller test size in Miri to keep the test fast and a larger test size for native
        // execution to make the test more effective.
        #[cfg(not(miri))]
        const CHUNK_SIZE: usize = 100;
        #[cfg(miri)]
        const CHUNK_SIZE: usize = 10;
        let highest_id = &AtomicU64::new(0);
        // Generate IDs from three unsynchronized threads, hoping they run at
        // the same time. Each thread uses a different strategy, but each thread
        // generates exactly 1000 IDs.
        let id_vecs: [Vec<Id>; 3] = scope(|s| {
            let one_at_a_time = s.spawn(|| {
                (0..10 * CHUNK_SIZE)
                    .map(|_| new_array_testable::<1>(highest_id)[0])
                    .collect()
            });
            let chunks = s.spawn(|| {
                (0..10)
                    .map(|_| new_array_testable::<CHUNK_SIZE>(highest_id))
                    .flatten()
                    .collect()
            });
            let all_at_once = new_array_testable::<{ 10 * CHUNK_SIZE }>(highest_id).into();
            [
                all_at_once,
                chunks.join().unwrap(),
                one_at_a_time.join().unwrap(),
            ]
        });
        // This loop verifies the IDs generated are exactly 1..=3000.
        let mut found = [false; 30 * CHUNK_SIZE];
        for Id(n) in id_vecs.iter().flatten() {
            let entry = found.get_mut(n.get() as usize - 1).expect("too-large ID");
            assert!(!*entry, "duplicate ID {}", n);
            *entry = true;
        }
        assert_eq!(found, [true; 30 * CHUNK_SIZE], "missing ID");
    }
}
