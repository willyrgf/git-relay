mod p01;
mod p02;
mod p03;
mod p04;
mod p05;
mod p06;
mod p07;
mod p08;
mod p09;
mod p10;
mod p11;

use serde_json::Value;

use crate::proof_support::lab::{CaseReport, ProofLab};
use crate::proof_support::schema::ProofMode;

pub type CaseRunner = fn(&mut ProofLab, ProofMode) -> Result<CaseReport, String>;

#[derive(Clone)]
pub struct CaseDefinition {
    pub case_id: &'static str,
    pub setup: &'static str,
    pub action: &'static str,
    pub pass_criteria: &'static [&'static str],
    pub fail_criteria: &'static [&'static str],
    pub contract_refs: &'static [&'static str],
    pub runner: CaseRunner,
}

impl CaseDefinition {
    pub fn run(&self, lab: &mut ProofLab, mode: ProofMode) -> Result<CaseReport, String> {
        (self.runner)(lab, mode)
    }

    pub fn base_case_json(&self) -> Value {
        serde_json::json!({
            "case_id": self.case_id,
            "setup": self.setup,
            "action": self.action,
            "pass_criteria": self.pass_criteria,
            "fail_criteria": self.fail_criteria,
            "contract_refs": self.contract_refs,
        })
    }
}

pub fn all_cases() -> Vec<CaseDefinition> {
    vec![
        p01::definition(),
        p02::definition(),
        p03::definition(),
        p04::definition(),
        p05::definition(),
        p06::definition(),
        p07::definition(),
        p08::definition(),
        p09::definition(),
        p10::definition(),
        p11::definition(),
    ]
}
