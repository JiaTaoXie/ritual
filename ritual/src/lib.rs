//! Implementation of `cpp_to_rust` generator that
//! analyzes a C++ library and produces a Rust crate for it.
//! See [README]
//! (https://github.com/rust-qt/cpp_to_rust/tree/master/cpp_to_rust/cpp_to_rust_generator)
//! for more information.

#![deny(unused_must_use)]

pub mod config;
mod cpp_checker;
mod cpp_code_generator;
pub mod cpp_data;
mod cpp_explicit_destructors;
mod cpp_ffi_data;
mod cpp_ffi_generator;
pub mod cpp_function;
mod cpp_inheritance; // TODO: deal with inheritance for subclassing support
mod cpp_operator;
mod cpp_parser;
mod cpp_template_instantiator;
pub mod cpp_type;
mod crate_writer;
pub mod database;
mod doc_formatter;
pub mod processor;
mod rust_code_generator;
mod rust_info;
mod rust_name_resolver;
mod rust_type;
#[cfg(test)]
mod tests;
mod type_allocation_places;
mod versions;
pub mod workspace;

//mod rust_generator;
