//! Reductions that only show before the 2024 edition. Each is a case of `breakers` by the
//! same name, which calls the `run` here.

pub mod if_branch_temporaries;
pub mod for_tail_temporaries;
pub mod if_let_temporaries;
pub mod unwind_operand_tail;
