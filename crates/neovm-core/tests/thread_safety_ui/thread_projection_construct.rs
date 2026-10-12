use neovm_core::emacs_core::forward::{CanonicalArm, ThreadProjectionDescriptor, TlsIndex};

fn forge(index: TlsIndex, arm: CanonicalArm) -> ThreadProjectionDescriptor {
    ThreadProjectionDescriptor {
        index,
        arm: std::cell::Cell::new(arm),
        host_projected: false,
    }
}

fn main() {
    let _ = forge;
}
