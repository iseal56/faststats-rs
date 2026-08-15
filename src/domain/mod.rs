//! Core domain types: config, attributes, and SDK identification.

pub mod attributes;
pub mod config;
pub mod sdk_info;

pub use attributes::Attributes;
pub use config::Config;
pub use sdk_info::SdkInfo;
