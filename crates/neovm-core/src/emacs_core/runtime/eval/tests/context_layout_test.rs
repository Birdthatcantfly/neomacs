//! Round 3w (plan-3w.md section 3): Context is `repr(C)` in the field order
//! rustc chose for main at e5fd507ac9, so moving GNU-global state into the
//! World moves no field. This pins every offset; a failure means a field
//! changed size or was added out of place, which shifts the hot set and the
//! JIT's Context ABI.

use super::Context;
use std::mem::{offset_of, size_of};

#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn every_context_offset_is_mains() {
    assert_eq!(size_of::<Context>(), 44464, "Context size");
    let table: &[(usize, usize, &str)] = &[
        (
            0,
            offset_of!(Context, combine_after_change_buffer),
            "combine_after_change_buffer",
        ),
        (
            16,
            offset_of!(Context, selected_global_map),
            "selected_global_map",
        ),
        (32, offset_of!(Context, current_message), "current_message"),
        (
            80,
            offset_of!(Context, echo_area_buffers),
            "echo_area_buffers",
        ),
        (
            112,
            offset_of!(Context, minibuffer_selected_window),
            "minibuffer_selected_window",
        ),
        (
            128,
            offset_of!(Context, active_minibuffer_window),
            "active_minibuffer_window",
        ),
        (144, offset_of!(Context, threads), "threads"),
        (480, offset_of!(Context, command_loop), "command_loop"),
        (1432, offset_of!(Context, scroll_goal), "scroll_goal"),
        (
            1480,
            offset_of!(Context, materialized_face_table_source),
            "materialized_face_table_source",
        ),
        (
            1504,
            offset_of!(Context, gnu_redisplay_hooks),
            "gnu_redisplay_hooks",
        ),
        (
            1960,
            offset_of!(Context, gc_runtime_settings_cache),
            "gc_runtime_settings_cache",
        ),
        (
            2008,
            offset_of!(Context, interpreted_closure_filter_fn),
            "interpreted_closure_filter_fn",
        ),
        (
            2024,
            offset_of!(Context, in_flight_registry),
            "in_flight_registry",
        ),
        (
            2048,
            offset_of!(Context, cached_standard_case_table),
            "cached_standard_case_table",
        ),
        (2064, offset_of!(Context, buffers), "buffers"),
        (
            2128,
            offset_of!(Context, window_layout_query_adapter),
            "window_layout_query_adapter",
        ),
        (
            2152,
            offset_of!(Context, pending_menu_bar_popup_anchor),
            "pending_menu_bar_popup_anchor",
        ),
        (
            2240,
            offset_of!(Context, code_conversion_workspace),
            "code_conversion_workspace",
        ),
        (
            2256,
            offset_of!(Context, last_redisplay_signature),
            "last_redisplay_signature",
        ),
        (2552, offset_of!(Context, match_data), "match_data"),
        (2784, offset_of!(Context, input_rx), "input_rx"),
        (2800, offset_of!(Context, eval_task_rx), "eval_task_rx"),
        (2816, offset_of!(Context, specpdl), "specpdl"),
        (
            2840,
            offset_of!(Context, suspended_thread_bindings),
            "suspended_thread_bindings",
        ),
        (2864, offset_of!(Context, require_stack), "require_stack"),
        (
            2888,
            offset_of!(Context, loads_in_progress),
            "loads_in_progress",
        ),
        (
            2912,
            offset_of!(Context, load_read_cursors),
            "load_read_cursors",
        ),
        (
            2936,
            offset_of!(Context, last_overlay_modification_hooks),
            "last_overlay_modification_hooks",
        ),
        (
            2960,
            offset_of!(Context, combine_after_change_list),
            "combine_after_change_list",
        ),
        (2984, offset_of!(Context, bookmarks), "bookmarks"),
        (3064, offset_of!(Context, rectangle), "rectangle"),
        (3088, offset_of!(Context, interactive), "interactive"),
        (3376, offset_of!(Context, minibuffers), "minibuffers"),
        (3464, offset_of!(Context, modes), "modes"),
        (3808, offset_of!(Context, kmacro), "kmacro"),
        (3880, offset_of!(Context, visual_config), "visual_config"),
        (8024, offset_of!(Context, vm_root_frames), "vm_root_frames"),
        (
            8048,
            offset_of!(Context, backtrace_args_stack),
            "backtrace_args_stack",
        ),
        (
            8072,
            offset_of!(Context, eval_temp_roots),
            "eval_temp_roots",
        ),
        (
            8096,
            offset_of!(Context, eval_call_roots),
            "eval_call_roots",
        ),
        (
            8120,
            offset_of!(Context, sequence_temp_root_frames),
            "sequence_temp_root_frames",
        ),
        (8144, offset_of!(Context, bc_buf), "bc_buf"),
        (8168, offset_of!(Context, jit_root_stack), "jit_root_stack"),
        (8192, offset_of!(Context, bc_frames), "bc_frames"),
        (
            8216,
            offset_of!(Context, condition_stack),
            "condition_stack",
        ),
        (
            8240,
            offset_of!(Context, pending_safe_funcalls),
            "pending_safe_funcalls",
        ),
        (
            8264,
            offset_of!(Context, interpreter_stacks),
            "interpreter_stacks",
        ),
        (8288, offset_of!(Context, jit_bind_stack), "jit_bind_stack"),
        (8312, offset_of!(Context, cconv_memo), "cconv_memo"),
        (8712, offset_of!(Context, tier_i), "tier_i"),
        (
            8896,
            offset_of!(Context, last_uncaught_signal_backtrace),
            "last_uncaught_signal_backtrace",
        ),
        (8920, offset_of!(Context, daemon), "daemon"),
        (8968, offset_of!(Context, processes), "processes"),
        (9192, offset_of!(Context, abbrevs), "abbrevs"),
        (9248, offset_of!(Context, profiler), "profiler"),
        (
            9416,
            offset_of!(Context, shutdown_request),
            "shutdown_request",
        ),
        (
            9424,
            offset_of!(Context, window_edge_drag),
            "window_edge_drag",
        ),
        (
            9440,
            offset_of!(Context, integer_width_context),
            "integer_width_context",
        ),
        (9448, offset_of!(Context, owned_roots), "owned_roots"),
        (9464, offset_of!(Context, tagged_heap), "tagged_heap"),
        (9480, offset_of!(Context, obarray), "obarray"),
        (
            9520,
            offset_of!(Context, runtime_projection_mask),
            "runtime_projection_mask",
        ),
        (9536, offset_of!(Context, features), "features"),
        (9552, offset_of!(Context, xwidgets), "xwidgets"),
        (9680, offset_of!(Context, watchers), "watchers"),
        (
            9696,
            offset_of!(Context, active_variable_watchers),
            "active_variable_watchers",
        ),
        (9712, offset_of!(Context, registers), "registers"),
        (9760, offset_of!(Context, autoloads), "autoloads"),
        (9776, offset_of!(Context, treesit), "treesit"),
        (9792, offset_of!(Context, frames), "frames"),
        (9808, offset_of!(Context, quit_requested), "quit_requested"),
        (
            9816,
            offset_of!(Context, gstring_shape_cache),
            "gstring_shape_cache",
        ),
        (9864, offset_of!(Context, coding_systems), "coding_systems"),
        (9880, offset_of!(Context, face_table), "face_table"),
        (9904, offset_of!(Context, input_progress), "input_progress"),
        (
            9936,
            offset_of!(Context, body_redisplay_by_window),
            "body_redisplay_by_window",
        ),
        (
            9968,
            offset_of!(Context, body_redisplay_by_buffer),
            "body_redisplay_by_buffer",
        ),
        (10000, offset_of!(Context, chrome_dirty), "chrome_dirty"),
        (
            10104,
            offset_of!(Context, symbol_bytecode_call_cache),
            "symbol_bytecode_call_cache",
        ),
        (
            10112,
            offset_of!(Context, named_call_cache),
            "named_call_cache",
        ),
        (
            10144,
            offset_of!(Context, macro_perf_stats),
            "macro_perf_stats",
        ),
        (10160, offset_of!(Context, fringe_bitmaps), "fringe_bitmaps"),
        (10232, offset_of!(Context, ccl_registry), "ccl_registry"),
        (
            10248,
            offset_of!(Context, charset_registry),
            "charset_registry",
        ),
        (10264, offset_of!(Context, font_registry), "font_registry"),
        (
            10280,
            offset_of!(Context, terminal_registry),
            "terminal_registry",
        ),
        (
            10296,
            offset_of!(Context, hash_table_test_registry),
            "hash_table_test_registry",
        ),
        (
            10312,
            offset_of!(Context, file_notify_registry),
            "file_notify_registry",
        ),
        (
            10328,
            offset_of!(Context, window_configuration_registry),
            "window_configuration_registry",
        ),
        (
            10344,
            offset_of!(Context, dynamic_module_registry),
            "dynamic_module_registry",
        ),
        (10360, offset_of!(Context, pdump_image), "pdump_image"),
        (
            10368,
            offset_of!(Context, cached_system_name),
            "cached_system_name",
        ),
        (10376, offset_of!(Context, lexenv), "lexenv"),
        (10384, offset_of!(Context, quit_flag), "quit_flag"),
        (10392, offset_of!(Context, inhibit_quit), "inhibit_quit"),
        (10400, offset_of!(Context, throw_on_input), "throw_on_input"),
        (
            10408,
            offset_of!(Context, unwind_cleanup_depth),
            "unwind_cleanup_depth",
        ),
        (
            10416,
            offset_of!(Context, interval_insert_behind_hooks),
            "interval_insert_behind_hooks",
        ),
        (
            10424,
            offset_of!(Context, interval_insert_in_front_hooks),
            "interval_insert_in_front_hooks",
        ),
        (
            10432,
            offset_of!(Context, standard_syntax_table),
            "standard_syntax_table",
        ),
        (
            10440,
            offset_of!(Context, syntax_code_objects),
            "syntax_code_objects",
        ),
        (
            10448,
            offset_of!(Context, standard_category_table),
            "standard_category_table",
        ),
        (
            10456,
            offset_of!(Context, current_local_map),
            "current_local_map",
        ),
        (
            10464,
            offset_of!(Context, interactive_minibuffer_read_count),
            "interactive_minibuffer_read_count",
        ),
        (10472, offset_of!(Context, quit_char), "quit_char"),
        (10480, offset_of!(Context, redisplay_fn), "redisplay_fn"),
        (
            10496,
            offset_of!(Context, mode_line_display_flow),
            "mode_line_display_flow",
        ),
        (
            10504,
            offset_of!(Context, redisplay_prepare_fn),
            "redisplay_prepare_fn",
        ),
        (10520, offset_of!(Context, font_shape_fn), "font_shape_fn"),
        (
            10536,
            offset_of!(Context, scroll_preview_fn),
            "scroll_preview_fn",
        ),
        (
            10552,
            offset_of!(Context, display_idle_maintenance_fn),
            "display_idle_maintenance_fn",
        ),
        (
            10568,
            offset_of!(Context, frame_snapshot_fn),
            "frame_snapshot_fn",
        ),
        (10584, offset_of!(Context, display_host), "display_host"),
        (
            10600,
            offset_of!(Context, tty_frame_host_factory),
            "tty_frame_host_factory",
        ),
        (
            10616,
            offset_of!(Context, face_change_count),
            "face_change_count",
        ),
        (
            10624,
            offset_of!(Context, display_var_change_count),
            "display_var_change_count",
        ),
        (
            10632,
            offset_of!(Context, redisplay_generation),
            "redisplay_generation",
        ),
        (
            10640,
            offset_of!(Context, body_redisplay_all),
            "body_redisplay_all",
        ),
        (
            10648,
            offset_of!(Context, menu_bar_rebuild_generation),
            "menu_bar_rebuild_generation",
        ),
        (
            10656,
            offset_of!(Context, context_instance_id),
            "context_instance_id",
        ),
        (
            10664,
            offset_of!(Context, media_generation),
            "media_generation",
        ),
        (10672, offset_of!(Context, depth), "depth"),
        (10680, offset_of!(Context, eval_counter), "eval_counter"),
        (10688, offset_of!(Context, max_depth), "max_depth"),
        (10696, offset_of!(Context, gc_count), "gc_count"),
        (
            10704,
            offset_of!(Context, last_activation_thread),
            "last_activation_thread",
        ),
        (
            10712,
            offset_of!(Context, gc_inhibit_depth),
            "gc_inhibit_depth",
        ),
        (
            10720,
            offset_of!(Context, jit_root_stack_ptr),
            "jit_root_stack_ptr",
        ),
        (
            10728,
            offset_of!(Context, jit_root_stack_top),
            "jit_root_stack_top",
        ),
        (
            10736,
            offset_of!(Context, jit_root_stack_cap),
            "jit_root_stack_cap",
        ),
        (
            10744,
            offset_of!(Context, jit_stack_limit),
            "jit_stack_limit",
        ),
        (
            10752,
            offset_of!(Context, jit_stack_scratch),
            "jit_stack_scratch",
        ),
        (10808, offset_of!(Context, next_resume_id), "next_resume_id"),
        (10816, offset_of!(Context, owner), "owner"),
        (
            10824,
            offset_of!(Context, apply_fast_path_epoch),
            "apply_fast_path_epoch",
        ),
        (
            10832,
            offset_of!(Context, form_head_cache),
            "form_head_cache",
        ),
        (
            43600,
            offset_of!(Context, lexenv_assq_cache),
            "lexenv_assq_cache",
        ),
        (
            44112,
            offset_of!(Context, lexenv_special_cache),
            "lexenv_special_cache",
        ),
        (
            44368,
            offset_of!(Context, macro_expansion_scope_depth),
            "macro_expansion_scope_depth",
        ),
        (
            44376,
            offset_of!(Context, macro_expansion_mutation_epoch),
            "macro_expansion_mutation_epoch",
        ),
        (
            44384,
            offset_of!(Context, macro_expand_calls),
            "macro_expand_calls",
        ),
        (
            44392,
            offset_of!(Context, macro_expand_total_us),
            "macro_expand_total_us",
        ),
        (
            44400,
            offset_of!(Context, internal_interpreter_environment_symbol),
            "internal_interpreter_environment_symbol",
        ),
        (
            44404,
            offset_of!(Context, quit_flag_symbol),
            "quit_flag_symbol",
        ),
        (
            44408,
            offset_of!(Context, inhibit_quit_symbol),
            "inhibit_quit_symbol",
        ),
        (
            44412,
            offset_of!(Context, throw_on_input_symbol),
            "throw_on_input_symbol",
        ),
        (
            44416,
            offset_of!(Context, kill_emacs_symbol),
            "kill_emacs_symbol",
        ),
        (44420, offset_of!(Context, attention), "attention"),
        (
            44424,
            offset_of!(Context, noninteractive_symbol),
            "noninteractive_symbol",
        ),
        (
            44428,
            offset_of!(Context, symbols_with_pos_enabled_symbol),
            "symbols_with_pos_enabled_symbol",
        ),
        (
            44432,
            offset_of!(Context, print_symbols_bare_symbol),
            "print_symbols_bare_symbol",
        ),
        (
            44436,
            offset_of!(Context, load_read_stream_token),
            "load_read_stream_token",
        ),
        (
            44440,
            offset_of!(Context, _reserved_debugging_output_file),
            "_reserved_debugging_output_file",
        ),
        (
            44444,
            offset_of!(Context, compiler_function_overrides_symbol),
            "compiler_function_overrides_symbol",
        ),
        (
            44448,
            offset_of!(Context, after_pdump_load_hook_pending),
            "after_pdump_load_hook_pending",
        ),
        (44449, offset_of!(Context, noninteractive), "noninteractive"),
        (
            44450,
            offset_of!(Context, symbols_with_pos_enabled),
            "symbols_with_pos_enabled",
        ),
        (
            44451,
            offset_of!(Context, print_symbols_bare),
            "print_symbols_bare",
        ),
        (
            44452,
            offset_of!(Context, _reserved_message_log_need_newline),
            "_reserved_message_log_need_newline",
        ),
        (
            44453,
            offset_of!(Context, echo_area_resize_exact_pending),
            "echo_area_resize_exact_pending",
        ),
        (
            44454,
            offset_of!(Context, _reserved_message_buf_print),
            "_reserved_message_buf_print",
        ),
        (
            44455,
            offset_of!(Context, shutdown_in_progress),
            "shutdown_in_progress",
        ),
        (
            44456,
            offset_of!(Context, _reserved_input_mode_interrupt),
            "_reserved_input_mode_interrupt",
        ),
        (
            44457,
            offset_of!(Context, waiting_for_user_input),
            "waiting_for_user_input",
        ),
        (44458, offset_of!(Context, gc_pending), "gc_pending"),
        (
            44459,
            offset_of!(Context, gc_driver_active),
            "gc_driver_active",
        ),
        (44460, offset_of!(Context, gc_stress), "gc_stress"),
        (
            44461,
            offset_of!(Context, compiler_function_overrides_active),
            "compiler_function_overrides_active",
        ),
        (
            44462,
            offset_of!(Context, macro_perf_enabled),
            "macro_perf_enabled",
        ),
        (44463, offset_of!(Context, custom), "custom"),
    ];
    let moved: Vec<String> = table
        .iter()
        .filter(|(expected, actual, _)| expected != actual)
        .map(|(expected, actual, name)| format!("{name}: {expected} -> {actual}"))
        .collect();
    assert!(moved.is_empty(), "Context fields moved: {moved:#?}");
}
