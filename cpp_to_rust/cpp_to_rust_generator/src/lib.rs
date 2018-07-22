//! Implementation of `cpp_to_rust` generator that
//! analyzes a C++ library and produces a Rust crate for it.
//! See [README]
//! (https://github.com/rust-qt/cpp_to_rust/tree/master/cpp_to_rust/cpp_to_rust_generator)
//! for more information.

#![cfg_attr(feature = "clippy", feature(plugin))]
#![cfg_attr(feature = "clippy", plugin(clippy))]
#![cfg_attr(feature = "clippy", warn(nonminimal_bool))]
#![cfg_attr(feature = "clippy", warn(if_not_else))]
#![cfg_attr(feature = "clippy", warn(shadow_same))]
#![cfg_attr(feature = "clippy", warn(shadow_unrelated))]
#![cfg_attr(feature = "clippy", warn(single_match_else))]
// some time in the future...
// #![warn(option_unwrap_used)]
// #![warn(result_unwrap_used)]
// #![warn(print_stdout)]

extern crate clang;
extern crate regex;
extern crate rustfmt;
extern crate tempdir;

#[macro_use]
extern crate serde_derive;

extern crate serde;

pub extern crate cpp_to_rust_common as common;

pub mod config;
mod cpp_code_generator;
pub mod cpp_data;
pub mod cpp_ffi_generator;
//mod cpp_post_processor;
mod cpp_ffi_data;
pub mod cpp_function;
mod cpp_operator;
pub mod cpp_type;
//mod doc_formatter;
//mod launcher;
//mod rust_generator;
//mod rust_code_generator;
mod cpp_parser;
//mod rust_info;
//mod rust_type;
//mod versions;
mod cpp_inheritance;
mod cpp_template_instantiator;

#[cfg(test)]
mod tests;

pub mod new_impl;
