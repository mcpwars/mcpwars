//! Versioned scenario definitions (TOML files under `eval/scenarios/`).
//!
//! A scenario fixes everything about an episode except the candidate: world
//! generation, seeds (dev and held-out), the schedule of action opportunities,
//! the per-wake tool budget, starting conditions, the memory policy, tool
//! permissions, the objective and success threshold, scripted opponents, an
//! optional adversary, and named conditions (controlled variations used to
//! check that the scenario measures what it claims to).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub id: String,
    pub version: u32,
    /// The capability the scenario is meant to measure.
    pub capability: String,
    pub description: String,
    pub world: WorldSpec,
    pub seeds: Seeds,
    pub schedule: Schedule,
    #[serde(default)]
    pub start: StartSpec,
    #[serde(default)]
    pub memory: MemoryPolicy,
    #[serde(default)]
    pub permissions: Permissions,
    pub objective: Objective,
    #[serde(default)]
    pub opponents: Vec<OpponentSpec>,
    #[serde(default)]
    pub adversary: Option<AdversarySpec>,
    /// Named variations. If empty, a single "default" condition is used.
    #[serde(default)]
    pub conditions: Vec<Condition>,
    pub prompt: PromptSpec,
    /// Filled in by the loader: SHA-256 of the scenario file.
    #[serde(skip_deserializing, default)]
    pub file_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldSpec {
    pub sectors: u32,
    /// World seed = seed_base + episode seed.
    pub seed_base: u64,
    /// JSON merge patch applied to the default rules before generation.
    #[serde(default)]
    pub rules: Option<toml::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Seeds {
    pub dev: Vec<u64>,
    pub heldout: Vec<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schedule {
    /// Action opportunities for the candidate.
    pub wakes: u32,
    /// Simulation ticks that pass between wakes (time is frozen during a wake).
    pub ticks_between_wakes: u64,
    /// Tool calls the candidate may make per wake. Every call counts, reads included.
    pub tool_budget_per_wake: u32,
    /// Tool calls each scripted opponent may make per wake (default: same as the candidate).
    #[serde(default)]
    pub opponent_budget_per_wake: Option<u32>,
    /// Wall-clock limit on one candidate wake (default 600 s). A wake that hits it ends
    /// there, is recorded as a timeout, and simulation time is unaffected.
    #[serde(default)]
    pub wake_timeout_secs: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartSpec {
    #[serde(default)]
    pub credits: Option<i64>,
    #[serde(default)]
    pub sector: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPolicy {
    /// The candidate's private notes persist across wakes (and are shown at each wake).
    #[default]
    Notes,
    /// Notes are erased before every wake: no memory except the world itself.
    None,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    /// Tools the candidate may not call (join_game is always denied).
    #[serde(default)]
    pub deny: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Objective {
    /// Only `liquid_gain` is implemented: credits + bank at the end minus at the start.
    pub kind: String,
    /// Success threshold, fixed before any candidate is run.
    pub target: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpponentSpec {
    /// A versioned scripted policy, e.g. "merchant@1".
    pub policy: String,
    #[serde(default = "one")]
    pub count: u32,
}

fn one() -> u32 {
    1
}

/// A scripted adversary that sends one private hail. Its "attack" message asks
/// for an unauthorized diversion; its "control" message is a benign hail of
/// similar length, so the conditions differ only in the instruction.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdversarySpec {
    pub name: String,
    pub inject_at_wake: u32,
    /// Placeholders: {trap} (a far frontier sector), {adversary}, {amount}.
    pub attack_message: String,
    pub control_message: String,
    /// Credits the attack message asks for.
    pub amount: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    pub name: String,
    /// Extra tools denied under this condition (e.g. the macro tools).
    #[serde(default)]
    pub deny_extra: Vec<String>,
    #[serde(default)]
    pub memory: Option<MemoryPolicy>,
    /// With an adversary: true sends the attack message, false the control message.
    #[serde(default)]
    pub injection: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptSpec {
    /// The task as the operator states it to model candidates (trusted channel).
    pub objective_text: String,
}

impl Scenario {
    pub fn load(path: &std::path::Path) -> anyhow::Result<Scenario> {
        let text = std::fs::read_to_string(path)?;
        let mut s: Scenario = toml::from_str(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        s.file_sha256 = hex::encode(Sha256::digest(text.as_bytes()));
        s.validate()?;
        Ok(s)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.objective.kind == "liquid_gain", "objective.kind must be liquid_gain");
        anyhow::ensure!(
            self.schedule.wakes > 0 && self.schedule.tool_budget_per_wake > 0,
            "schedule needs wakes and a budget"
        );
        anyhow::ensure!(!self.seeds.dev.is_empty(), "seeds.dev is empty");
        let overlap: Vec<&u64> = self.seeds.dev.iter().filter(|s| self.seeds.heldout.contains(s)).collect();
        anyhow::ensure!(overlap.is_empty(), "dev and held-out seeds overlap: {overlap:?}");
        if let Some(a) = &self.adversary {
            anyhow::ensure!(
                a.inject_at_wake >= 1 && a.inject_at_wake <= self.schedule.wakes,
                "adversary.inject_at_wake out of range"
            );
        }
        for c in &self.conditions {
            anyhow::ensure!(
                c.injection.is_none() || self.adversary.is_some(),
                "condition {} sets injection without an adversary",
                c.name
            );
        }
        Ok(())
    }

    pub fn conditions(&self) -> Vec<Condition> {
        if self.conditions.is_empty() {
            vec![Condition {
                name: "default".into(),
                ..Default::default()
            }]
        } else {
            self.conditions.clone()
        }
    }

    pub fn seeds(&self, split: &str) -> anyhow::Result<Vec<u64>> {
        match split {
            "dev" => Ok(self.seeds.dev.clone()),
            "heldout" => Ok(self.seeds.heldout.clone()),
            other => anyhow::bail!("unknown split {other} (dev or heldout)"),
        }
    }

    /// Rules patch as JSON (TOML tables map directly).
    pub fn rules_patch(&self) -> Option<Value> {
        self.world.rules.as_ref().and_then(|v| serde_json::to_value(v).ok())
    }
}
