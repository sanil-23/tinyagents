//! Reasoning fallback after a dead call.
//!
//! A hosted reasoning model can spend its whole output cap on the hidden
//! reasoning channel and return `finish_reason = "length"` with no text and
//! no tool call. Measured on deepseek-v4.1-flash through OpenRouter's routable
//! providers, neither a smaller output cap nor a lower effort label stops it:
//! one task died nine times in a row at caps from 65k down to 2k, and at
//! `medium` effort 11 of 25 calls still died. The one control that reliably
//! produced zero reasoning tokens was `effort = none`.
//!
//! So after a dead call the loop re-issues the step with reasoning switched
//! off. The model then has to act from what it already knows, and the nudge
//! that accompanies a repeated death tells it to do its working-out in the
//! workspace (a scratch file, a small experiment) instead of in its head. The
//! hold-off backs off: the first death costs one call without reasoning, the
//! next two, then four, up to [`REASONING_FALLBACK_MAX_HOLDOFF`], so a model
//! that keeps dying on this transcript spends less of the run proving it.
//!
//! The state lives on [`super::types::TurnRecovery`] but, unlike the
//! counters there, is run-wide: the hold-off outlives the turn that set it,
//! and none of the turn-boundary resets touch it.

use tinyinference_llm::model::{ModelRequest, ReasoningConfig, ReasoningEffort};

/// Most consecutive live calls one dead call can switch reasoning off for.
pub(super) const REASONING_FALLBACK_MAX_HOLDOFF: u32 = 8;

/// Per-run state of the fallback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ReasoningFallback {
    /// Live calls still to go out without reasoning.
    holdoff: u32,
    /// Hold-off the next dead call will set; doubles per death, clamped.
    scale: u32,
}

impl ReasoningFallback {
    /// Records a dead call: the next `holdoff` calls go out without reasoning.
    /// Returns the hold-off just set.
    pub(super) fn on_dead_call(&mut self) -> u32 {
        let scale = self.scale.max(1);
        self.holdoff = scale;
        self.scale = scale.saturating_mul(2).min(REASONING_FALLBACK_MAX_HOLDOFF);
        self.holdoff
    }

    /// Records a reply with content or a tool call: one hold-off call spent.
    pub(super) fn on_live_reply(&mut self) {
        self.holdoff = self.holdoff.saturating_sub(1);
    }

    /// Whether the next call goes out without reasoning.
    pub(super) fn active(&self) -> bool {
        self.holdoff > 0
    }

    /// Live calls still to go out without reasoning.
    pub(super) fn holdoff(&self) -> u32 {
        self.holdoff
    }

    /// Switches reasoning off on `request` while the fallback is active.
    /// Returns what the request asked for before, when it was changed: a
    /// request that already had reasoning off is left alone.
    pub(super) fn apply(&self, request: &mut ModelRequest) -> Option<Option<ReasoningConfig>> {
        if !self.active() {
            return None;
        }
        if matches!(
            request.reasoning.as_ref().and_then(|r| r.effort),
            Some(ReasoningEffort::None)
        ) {
            return None;
        }
        let previous = request
            .reasoning
            .replace(ReasoningConfig::effort(ReasoningEffort::None));
        Some(previous)
    }
}

#[cfg(test)]
#[path = "reasoning_fallback_tests.rs"]
mod tests;
