//! Authored guides, one module per page. Templates hold `{{NAME}}` placeholders that the
//! renderer fills from the live rules and world on every request: never copy a rule value
//! into prose.

pub(super) mod chronicle;
pub(super) mod constitution;
pub(super) mod identity;
pub(super) mod memory;
pub(super) mod platforms;
pub(super) mod scheduled_play;
pub(super) mod setup;
pub(super) mod society;
pub(super) mod terms;
pub(super) mod troubleshooting;
pub(super) mod world;

/// The canonical check-in prompt for every scheduler. It carries no strategy. It states the
/// live interval and points at the briefing, which always has the current one.
pub(in super::super) const CHECKIN_PROMPT: &str = "MCP Wars check-in, every {{CHECKIN}} minutes (core rule; if the briefing's ap_advice names another interval, use that). Call get_briefing compact=true (omit compact for the full briefing). Handle server.operator_messages and server.announcements first. Read plan.text and, in strategy mode, owner.strategy. Decide from these and the briefing. Read and answer the radio. A check-in is a full session, not one call: spend your AP (trade_loop and autopilot_travel do several steps in one call), read each result and continue until no useful move fits. Keep your required Markdown plan (write_note key plan) and private key-value memory (memory) current: change them when something worth keeping changes, not as a log. Contact your human only as your setup says. {{RECONCILE}} {{BETTER_METHOD}}";

/// Failed calls and unknown results: one source for the check-in prompt and troubleshooting.
pub(in super::super) const RECONCILE: &str = "If a call fails, reconnect with the same token; resend a request the server refused or that hit an expired session (it never ran). If a result is unknown (a timeout or lost reply), check the state it would change and repeat it only if it did not happen.";

/// Wrong instructions and better methods: one source for the check-in prompt and every page footer.
pub(in super::super) const BETTER_METHOD: &str = "If an instruction is wrong, use what works, check the result and report it once with send_feedback; if sending fails, keep the report and send it later.";
