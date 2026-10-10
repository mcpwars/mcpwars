//! Results report: per scenario x condition x candidate configuration.
//!
//! Availability (episodes attempted, completed, infrastructure errors) is
//! reported separately from capability. Capability statistics use completed
//! episodes only. Paired comparisons match on scenario, condition, seed and
//! repetition. There is no composite score.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use serde_json::json;

use super::runner::Provenance;
use super::score::Metrics;

#[derive(Clone, Debug, Serialize)]
pub struct Group {
    pub scenario: String,
    pub scenario_version: u32,
    pub condition: String,
    pub candidate: String,
    pub candidate_config_sha: String,
    /// Availability: episodes attempted, completed, lost to infrastructure errors.
    pub attempted: usize,
    pub completed: usize,
    pub infrastructure_errors: usize,
    /// Capability (completed episodes only).
    pub successes: usize,
    pub success_rate: f64,
    /// 95% Wilson score interval for the success rate.
    pub success_ci95: (f64, f64),
    pub gain_mean: f64,
    pub gain_sd: f64,
    pub gain_min: i64,
    pub gain_max: i64,
    pub diverted: usize,
    pub executed_mean: f64,
    pub refusals_mean: f64,
    pub repeated_invalid_total: u32,
    pub paired: Option<Paired>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Paired {
    pub baseline: String,
    /// Pairs matched on (seed, repetition) where both episodes completed.
    pub n: usize,
    pub gain_diff_mean: f64,
    pub gain_diff_sd: f64,
    pub wins: usize,
    pub losses: usize,
    pub ties: usize,
    pub success_only_candidate: usize,
    pub success_only_baseline: usize,
}

fn mean_sd(xs: &[f64]) -> (f64, f64) {
    if xs.is_empty() {
        return (0.0, 0.0);
    }
    let n = xs.len() as f64;
    let m = xs.iter().sum::<f64>() / n;
    let sd = if xs.len() > 1 {
        (xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n - 1.0)).sqrt()
    } else {
        0.0
    };
    (m, sd)
}

pub fn wilson(k: usize, n: usize) -> (f64, f64) {
    if n == 0 {
        return (0.0, 0.0);
    }
    let (k, n, z) = (k as f64, n as f64, 1.96f64);
    let p = k / n;
    let denom = 1.0 + z * z / n;
    let centre = (p + z * z / (2.0 * n)) / denom;
    let half = z * ((p * (1.0 - p) + z * z / (4.0 * n)) / n).sqrt() / denom;
    ((centre - half).max(0.0), (centre + half).min(1.0))
}

type Key = (String, u32, String, String, String);

pub fn aggregate(rows: &[Metrics], baseline: Option<&str>) -> Vec<Group> {
    let mut groups: BTreeMap<Key, Vec<&Metrics>> = BTreeMap::new();
    for r in rows {
        groups
            .entry((
                r.scenario.clone(),
                r.scenario_version,
                r.condition.clone(),
                r.candidate.clone(),
                r.candidate_config_sha.clone(),
            ))
            .or_default()
            .push(r);
    }
    let mut out = vec![];
    for ((scenario, version, condition, candidate, cfg), all) in &groups {
        let done: Vec<&&Metrics> = all.iter().filter(|r| r.status == "completed").collect();
        let gains: Vec<f64> = done.iter().map(|r| r.liquid_gain as f64).collect();
        let (gm, gsd) = mean_sd(&gains);
        let successes = done.iter().filter(|r| r.success).count();
        let paired = baseline.filter(|b| *b != candidate.as_str()).and_then(|b| {
            let base = groups
                .iter()
                .find(|(k, _)| k.0 == *scenario && k.1 == *version && k.2 == *condition && k.3 == b)?
                .1;
            let by_key: BTreeMap<(u64, u32), &&Metrics> = base
                .iter()
                .filter(|m| m.status == "completed")
                .map(|m| ((m.seed, m.repetition), m))
                .collect();
            let pairs: Vec<(&Metrics, &Metrics)> = done
                .iter()
                .filter_map(|m| by_key.get(&(m.seed, m.repetition)).map(|b| (**m, **b)))
                .collect();
            if pairs.is_empty() {
                return None;
            }
            let diffs: Vec<f64> = pairs.iter().map(|(c, b)| (c.liquid_gain - b.liquid_gain) as f64).collect();
            let (dm, dsd) = mean_sd(&diffs);
            Some(Paired {
                baseline: b.to_string(),
                n: pairs.len(),
                gain_diff_mean: dm,
                gain_diff_sd: dsd,
                wins: diffs.iter().filter(|d| **d > 0.0).count(),
                losses: diffs.iter().filter(|d| **d < 0.0).count(),
                ties: diffs.iter().filter(|d| **d == 0.0).count(),
                success_only_candidate: pairs.iter().filter(|(c, b)| c.success && !b.success).count(),
                success_only_baseline: pairs.iter().filter(|(c, b)| !c.success && b.success).count(),
            })
        });
        out.push(Group {
            scenario: scenario.clone(),
            scenario_version: *version,
            condition: condition.clone(),
            candidate: candidate.clone(),
            candidate_config_sha: cfg.clone(),
            attempted: all.len(),
            completed: done.len(),
            infrastructure_errors: all.iter().filter(|r| r.status == "infrastructure_error").count(),
            successes,
            success_rate: successes as f64 / done.len().max(1) as f64,
            success_ci95: wilson(successes, done.len()),
            gain_mean: gm,
            gain_sd: gsd,
            gain_min: done.iter().map(|r| r.liquid_gain).min().unwrap_or(0),
            gain_max: done.iter().map(|r| r.liquid_gain).max().unwrap_or(0),
            diverted: done.iter().filter(|r| r.diverted).count(),
            executed_mean: mean_sd(&done.iter().map(|r| r.executed as f64).collect::<Vec<_>>()).0,
            refusals_mean: mean_sd(
                &done
                    .iter()
                    .map(|r| (r.engine_refusals + r.budget_refusals + r.permission_refusals + r.invalid_argument_refusals) as f64)
                    .collect::<Vec<_>>(),
            )
            .0,
            repeated_invalid_total: done.iter().map(|r| r.repeated_invalid).sum(),
            paired,
        });
    }
    out
}

pub fn markdown(groups: &[Group]) -> String {
    let mut s = String::from(
        "| scenario | condition | candidate (config) | attempted / completed / infra errors | success (95% CI) | liquid gain mean (sd) [min, max] | diverted | calls/episode | refusals/episode | paired vs baseline: mean diff (sd), W/L/T (n) |\n|---|---|---|---|---|---|---|---|---|---|\n",
    );
    for g in groups {
        let paired = g
            .paired
            .as_ref()
            .map(|p| {
                format!(
                    "{:+.0} ({:.0}), {}/{}/{} (n={}) vs {}",
                    p.gain_diff_mean, p.gain_diff_sd, p.wins, p.losses, p.ties, p.n, p.baseline
                )
            })
            .unwrap_or_else(|| "baseline or n/a".into());
        s.push_str(&format!(
            "| {} v{} | {} | {} ({}) | {} / {} / {} | {}/{} ({:.2}-{:.2}) | {:.0} ({:.0}) [{}, {}] | {} | {:.1} | {:.1} | {} |\n",
            g.scenario,
            g.scenario_version,
            g.condition,
            g.candidate,
            g.candidate_config_sha,
            g.attempted,
            g.completed,
            g.infrastructure_errors,
            g.successes,
            g.completed,
            g.success_ci95.0,
            g.success_ci95.1,
            g.gain_mean,
            g.gain_sd,
            g.gain_min,
            g.gain_max,
            g.diverted,
            g.executed_mean,
            g.refusals_mean,
            paired
        ));
    }
    s
}

pub fn write(out: &Path, rows: &[Metrics], baseline: Option<&str>, prov: &Provenance) -> anyhow::Result<()> {
    let groups = aggregate(rows, baseline);
    std::fs::write(
        out.join("results.json"),
        serde_json::to_string_pretty(&json!({"provenance": prov, "baseline": baseline, "groups": groups}))?,
    )?;
    let md = format!(
        "# Evaluation results\n\nCommit `{}`{}, engine `{}`. Baseline: `{}`. Capability columns use completed episodes only; infrastructure errors are counted separately. Per-episode rows: `episodes.jsonl`; traces under `traces/`; archived prompts, catalogs and scenario files under `artifacts/`.\n\n{}",
        prov.git_commit,
        if prov.git_dirty {
            " (uncommitted changes; diff in artifacts/source.diff)"
        } else {
            ""
        },
        prov.engine_version,
        baseline.unwrap_or("none"),
        markdown(&groups)
    );
    std::fs::write(out.join("results.md"), md)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wilson_interval_is_sane() {
        let (lo, hi) = wilson(8, 8);
        assert!(lo > 0.6 && hi == 1.0);
        let (lo, hi) = wilson(0, 8);
        assert!(lo == 0.0 && hi < 0.4);
        assert_eq!(wilson(0, 0), (0.0, 0.0));
    }

    fn row(cand: &str, seed: u64, rep: u32, gain: i64, status: &str) -> Metrics {
        Metrics {
            scenario: "s".into(),
            scenario_version: 1,
            condition: "c".into(),
            candidate: cand.into(),
            candidate_config_sha: format!("{cand}-sha"),
            seed,
            repetition: rep,
            status: status.into(),
            liquid_gain: gain,
            success: gain >= 10,
            ..Default::default()
        }
    }

    #[test]
    fn infrastructure_errors_are_availability_not_capability_and_pairs_use_repetitions() {
        let rows = vec![
            row("base", 1, 1, 10, "completed"),
            row("base", 1, 2, 20, "completed"),
            row("cand", 1, 1, 15, "completed"),
            row("cand", 1, 2, 0, "infrastructure_error"),
        ];
        let g = aggregate(&rows, Some("base"));
        let c = g.iter().find(|g| g.candidate == "cand").unwrap();
        assert_eq!((c.attempted, c.completed, c.infrastructure_errors), (2, 1, 1));
        assert_eq!(c.successes, 1);
        assert_eq!(c.success_rate, 1.0, "the infrastructure error is not a failure");
        let p = c.paired.as_ref().unwrap();
        assert_eq!(p.n, 1, "rep 2 is not paired: the candidate's episode did not complete");
        assert_eq!(p.gain_diff_mean, 5.0, "paired with rep 1 (gain 10), not rep 2");
    }
}
