pub mod manifest;
pub mod parse;
pub mod prelude;
pub mod semver;
pub mod subsumption;
pub mod ty;

use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Re-export for convenience.
pub use semver::{Version, VersionReq};
pub use ty::{
    Constraint, Effect, EffectSet, Field, FunctionSig, Kind, ModuleSignature, Prim, Ty, TyVar,
    TyVarBinding,
};

/// A human or org that sponsors agents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sponsor {
    pub id: Uuid,
    pub name: String,
}

/// A registered agent identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentId {
    pub id: Uuid,
    pub name: String,
    pub sponsor_id: Uuid,
    #[serde(skip)]
    pub public_key: Option<VerifyingKey>,
}

/// A repository on AgentCodeRepo.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repo {
    pub id: Uuid,
    pub owner: Uuid,
    pub name: String,
    pub description: Option<String>,
}
