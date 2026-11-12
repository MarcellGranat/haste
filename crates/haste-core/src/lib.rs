//! Core types and pure logic for haste: data model, policy gate, proposals,
//! audit log, token handling and the connector trait.

pub mod audit;
pub mod auth;
pub mod connector;
pub mod herdr;
pub mod ignore;
pub mod log;
pub mod model;
pub mod notes;
pub mod policy;
pub mod proposal;
pub mod send;
pub mod time;

pub use model::{
    Identifier, IdentifierKind, Link, LinkType, Message, ObjectKind, Person, Source, Thread, Triage,
};
pub use policy::{evaluate, Action, Decision, Matrix, Outcome, Principal, PrincipalKind, Target};
pub use proposal::{Payload, Proposal, ProposalState};
