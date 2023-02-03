//! # Client Classes

use serde::{Deserialize, Serialize};

use crate::wire::v4::Options;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ClientClasses {
    pub(crate) v4: Vec<ClientClass>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ClientClass {
    pub(crate) name: String,
    pub(crate) assert: String,
    pub(crate) options: Options,
}
