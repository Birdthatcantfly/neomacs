//! Accessors for GNU-global Context state too small to hold a
//! [`super::WorldSlot`] in place (round 3w, plan-3w.md section 8): step 2
//! moves these values into a World group, behind these same accessors.

use super::Context;

impl Context {
    /// xdisp.c `message_log_need_newline`.
    #[inline]
    pub(crate) fn message_log_need_newline(&self) -> bool {
        self.message_log_need_newline
    }

    #[inline]
    pub(crate) fn set_message_log_need_newline(&mut self, value: bool) {
        self.message_log_need_newline = value;
    }

    /// print.c `external_debugging_output`'s stream.
    #[inline]
    pub(crate) fn debugging_output_file_mut(&mut self) -> &mut Option<std::fs::File> {
        &mut self.debugging_output_file
    }
}
