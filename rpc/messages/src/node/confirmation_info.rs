use indexmap::IndexMap;
use rsnano_types::QualifiedRoot;
use rsnano_types::{Account, Amount, BlockHash, JsonBlock};
use serde::{Deserialize, Serialize};

use crate::{RpcBool, RpcU32};

impl From<QualifiedRoot> for ConfirmationInfoArgs {
    fn from(value: QualifiedRoot) -> Self {
        Self::build(value).finish()
    }
}

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ConfirmationInfoArgs {
    pub root: QualifiedRoot,
    pub contents: Option<RpcBool>,
    pub representatives: Option<RpcBool>,
}

impl ConfirmationInfoArgs {
    pub fn build(root: QualifiedRoot) -> ConfirmationInfoArgsBuilder {
        ConfirmationInfoArgsBuilder {
            args: ConfirmationInfoArgs {
                root,
                contents: None,
                representatives: None,
            },
        }
    }
}

pub struct ConfirmationInfoArgsBuilder {
    args: ConfirmationInfoArgs,
}

impl ConfirmationInfoArgsBuilder {
    pub fn without_contents(mut self) -> Self {
        self.args.contents = Some(false.into());
        self
    }

    pub fn include_representatives(mut self) -> Self {
        self.args.representatives = Some(true.into());
        self
    }

    pub fn finish(self) -> ConfirmationInfoArgs {
        self.args
    }
}

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ConfirmationInfoResponse {
    pub announcements: RpcU32,
    pub voters: RpcU32,
    pub last_winner: BlockHash,
    pub total_tally: Amount,
    pub final_tally: Amount,
    pub blocks: IndexMap<BlockHash, ConfirmationBlockInfoDto>,

    /// Kudzu election state (rai_protocol builds only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,

    /// Kudzu certificates collected so far (rai_protocol builds only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub certificates: Option<KudzuCertificatesDto>,
}

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct KudzuCertificatesDto {
    pub notarized: Vec<BlockHash>,
    pub timeout: bool,
    pub timeout_tally: Amount,
    pub certificate_threshold: Amount,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fast: Option<BlockHash>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "final")]
    pub final_: Option<BlockHash>,
    /// Whether a finalization certificate can still form
    pub finalizable: bool,
}

#[derive(PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ConfirmationBlockInfoDto {
    pub tally: Amount,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub contents: Option<JsonBlock>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub representatives: Option<IndexMap<Account, Amount>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub representatives_final: Option<IndexMap<Account, Amount>>,

    /// Kudzu vote kinds each representative cast for this block, e.g. "first,final"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub representatives_kudzu: Option<IndexMap<Account, String>>,

    /// Kudzu first-vote tally for this block
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_tally: Option<Amount>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_json() {
        let response = ConfirmationInfoResponse {
            announcements: 1.into(),
            voters: 2.into(),
            last_winner: 3.into(),
            total_tally: 4.into(),
            final_tally: 5.into(),
            blocks: [(
                BlockHash::from(6),
                ConfirmationBlockInfoDto {
                    tally: 7.into(),
                    contents: None,
                    representatives: None,
                    representatives_final: None,
                    representatives_kudzu: None,
                    first_tally: None,
                },
            )]
            .into(),
            state: None,
            certificates: None,
        };

        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(
            json,
            "{\"announcements\":\"1\",\"voters\":\"2\",\
            \"last_winner\":\"0000000000000000000000000000000000000000000000000000000000000003\",\
            \"total_tally\":\"4\",\"final_tally\":\"5\",\
            \"blocks\":{\
            \"0000000000000000000000000000000000000000000000000000000000000006\":{\
            \"tally\":\"7\"}}}"
        );
    }
}
