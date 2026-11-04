//! A proxy between aegis and the Docker daemon (#321). Whoever holds the
//! daemon's socket is root of the machine; aegis holds this proxy's
//! instead, which passes on only the calls the gate, the egress proxy and
//! the ACI make, and creates only containers hardened as #320 makes them,
//! with only the workspace mounted. See ADR 0010.

pub mod policy;
pub mod proxy;

pub use policy::Policy;
pub use proxy::{Proxy, bind};
