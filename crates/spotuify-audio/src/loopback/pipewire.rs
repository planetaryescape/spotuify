//! Reserved native PipeWire loopback capture module.
//!
//! The cpal loopback backend is the default. This module exists so the
//! opt-in `loopback-pipewire` feature has a concrete module boundary. It
//! intentionally exposes no capture manager until the native PipeWire
//! stream path is implemented and tested.
