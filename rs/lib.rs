pub mod cache;
pub mod graph;
pub mod model;
pub mod noncode;
pub mod outline;
pub mod oxc_index;
pub mod parser;
pub mod py_index;
pub mod query;
pub mod walk;

pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
