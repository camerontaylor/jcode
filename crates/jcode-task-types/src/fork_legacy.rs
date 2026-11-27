//! Fork-owned read compatibility for retired semantic-state labels.
//!
//! Upstream `7e6b21927` removed `IntentUnderstanding::Complete`. Builds up to
//! v1.1.2001 still wrote `"complete"` into session plan files, and a plan that
//! fails to parse loads as an empty `TodoPlan`, so a resumed or live session
//! would lose its title and stated user intention on upgrade. Map the retired
//! label to the nearest remaining state when reading; the next save writes the
//! current label. Candidate to send upstream.

/// Return the current label for a retired `label` of semantic state `state`.
pub(crate) fn upgrade(state: &str, label: &str) -> Option<&'static str> {
    match (state, label.trim().to_ascii_lowercase().as_str()) {
        ("IntentUnderstanding", "complete") => Some("clear"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::{ConfidenceState, IntentUnderstanding, TodoPlan};

    #[test]
    fn retired_complete_intent_reads_as_clear() {
        for raw in ["\"complete\"", "\" Complete \""] {
            assert_eq!(
                serde_json::from_str::<IntentUnderstanding>(raw).unwrap(),
                IntentUnderstanding::Clear
            );
        }
        assert_eq!(
            serde_json::to_string(&IntentUnderstanding::Clear).unwrap(),
            "\"clear\""
        );
    }

    #[test]
    fn plan_written_by_v1_1_2001_keeps_title_and_intent() {
        let stored = r#"{
            "title": "Fleet rollout",
            "user_intention": "Ship the release",
            "understands_user_intent": "complete",
            "understands_user_intent_history": ["partial", "complete"]
        }"#;
        let plan: TodoPlan = serde_json::from_str(stored).expect("legacy plan parses");
        assert_eq!(plan.title.as_deref(), Some("Fleet rollout"));
        assert_eq!(
            plan.understands_user_intent,
            Some(IntentUnderstanding::Clear)
        );
        assert_eq!(
            plan.understands_user_intent_history,
            vec![IntentUnderstanding::Partial, IntentUnderstanding::Clear]
        );
    }

    #[test]
    fn other_states_and_numbers_stay_rejected() {
        assert!(serde_json::from_str::<ConfidenceState>("\"complete\"").is_err());
        assert!(serde_json::from_str::<IntentUnderstanding>("100").is_err());
        assert!(serde_json::from_str::<IntentUnderstanding>("\"bogus\"").is_err());
    }
}
