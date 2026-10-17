//! Reductions of what broke rustd in the crate sweep, one module each, with the cause on top.
//! `breakers.py` runs every one on its own, so that a crash in one does not hide the others.

macro_rules! cases {
    ($($name:ident),* $(,)?) => {
        $(pub mod $name;)*
        pub const NAMES: &[&str] = &[$(stringify!($name)),*];
        pub fn run(name: &str) -> Option<String> {
            match name {
                $(stringify!($name) => Some(format!("{:?}", $name::run())),)*
                _ => None,
            }
        }
    };
}

cases![
    niche_eq,
    simd_cast,
    big_frame,
    big_promoted,
    future_size,
    future_states,
    macro_item_location,
    clmul,
    cast_then_shift,
    xgetbv,
    unsize_assoc_field,
    const_bswap,
    format_blanket_new,
    f16c,
    unsized_tail_cmp,
    named_const_impl,
    macro_same_name_vars,
    safe_macro,
    path_where_clause,
    impl_in_fn,
    fragment_token,
    blanket_in_fn,
    lifetime_only_callee,
    arg_frames,
    panic_columns,
    panic_column_width,
    panic_index_operand,
    unwind_operand_order,
    assert_message,
    stack_aggregate_odd_size,
    repeat_rows,
    if_branch_temporaries,
    uncaptured_scrutinee,
    unread_index,
    let_else_temporaries,
    unmatched_array_drop,
    for_tail_temporaries,
    if_let_temporaries,
    guard_subslice_restored,
    unreachable_arm_move,
    unwind_operand_tail,
    non_exhaustive_capture,
    allocator_static,
    negative_literal_token,
    constructor_as_pointer,
    nested_binder_type_id,
    if_let_outlives,
    foreign_of_dependency,
    repeat_moved,
    raw_write_then_match,
    guard_let_drops,
    future_unwind_flags,
    caller_in_constant,
    numbered_empty_slice,
    null_by_default,
    promoted_const_parameter,
    list_by_address,
    shared_constant_address,
    aligned_constant_field,
    negative_literal_operand,
    empty_array_by_impl,
    fragment_edition_through_macro,
    let_else_drops,
    tail_tuple_drops,
    promoted_constructor,
    renamed_variant_pattern,
    alias_under_binder,
];
