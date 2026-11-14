pub mod runtime_state;

pub use runtime_state::{
    RuntimeState, RuntimeStateManager, default_state_root, is_process_alive, state_roots_overlap,
};
