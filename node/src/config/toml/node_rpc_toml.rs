use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
pub struct NodeRpcToml {
    pub enable: Option<bool>,
}
