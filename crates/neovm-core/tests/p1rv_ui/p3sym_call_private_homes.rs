use neovm_core::tagged::gc::{FunctionActivation, FunctionCallCache, OwnerRootSlot};

fn alias_cache<'era>(cache: &FunctionCallCache<'era>) -> OwnerRootSlot<'era> {
    cache.home.clone()
}

fn alias_activation<'era>(activation: &FunctionActivation<'era>) -> OwnerRootSlot<'era> {
    activation.home.clone()
}

fn main() {}
