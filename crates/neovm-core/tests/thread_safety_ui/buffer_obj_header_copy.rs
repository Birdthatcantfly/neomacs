use neovm_core::emacs_core::forward::LispBufferObjFwd;

// A per-buffer header is never lifted out of the body it heads: a thread
// projection's header must only ever exist inside its descriptor.
fn duplicate(fwd: &LispBufferObjFwd) -> LispBufferObjFwd {
    *fwd
}

fn main() {
    let _ = duplicate;
}
