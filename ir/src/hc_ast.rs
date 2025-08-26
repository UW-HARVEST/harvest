/*
 *  This file contains the data structures for
 *  HARVEST-IR's version of C ASTs.
 *
 *  Original Intentions when creating this file:
 *  - Code for marshalling data into and out of this representation
 *    should be located in other modules as much as possible
 *  - Code for validating invariants of the IR belongs here.
 *  - Some light amount of support/helper functions for the IR is
 *    reasonable to include here, but if it becomes much larger than
 *    the data structure definitions, then we should consider moving
 *    it into a separate file.
 *
 *  Initial conventions on IR encoding in Rust have mostly been inherited
 *  from C2Rust.  Thus such choices were made for expediency, and should be
 *  revisited in the future by someone with a better sense of the best
 *  way to encode IRs in Rust
 *
 */

//#[derive(Debug, Clone)]
//pub struct

//pub type Decl
