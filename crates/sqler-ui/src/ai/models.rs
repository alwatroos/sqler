// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: The SQLer Authors

//! The models per use case: the one place that maps a use case (or the default) to its slot in
//! the configuration, and the rows the settings page and the side view show.

use sqler_config::{AiConfig, ModelAssignments, ModelChoice, UseCase};

/// The default and every use case, in the order the UI lists them (`None` is the default).
pub(crate) const SLOTS: [Option<UseCase>; 5] = [
    None,
    Some(UseCase::NlToSql),
    Some(UseCase::Explain),
    Some(UseCase::Fix),
    Some(UseCase::Exploration),
];

/// The title of a use case in the models lists.
#[must_use]
pub fn use_case_title(use_case: Option<UseCase>) -> &'static str {
    match use_case {
        None => "Default",
        Some(UseCase::NlToSql) => "NL to SQL",
        Some(UseCase::Explain) => "Explain",
        Some(UseCase::Fix) => "Fix",
        Some(UseCase::Exploration) => "Exploration",
        Some(_) => "Other",
    }
}

/// The slot of `use_case` (`None` = the default).
#[must_use]
pub(crate) fn slot(models: &ModelAssignments, use_case: Option<UseCase>) -> Option<&ModelChoice> {
    match use_case {
        None => models.default.as_ref(),
        Some(UseCase::NlToSql) => models.nl_to_sql.as_ref(),
        Some(UseCase::Explain) => models.explain.as_ref(),
        Some(UseCase::Fix) => models.fix.as_ref(),
        Some(_) => models.exploration.as_ref(),
    }
}

/// The slot of `use_case` (`None` = the default), to write.
pub(crate) fn slot_mut(
    models: &mut ModelAssignments,
    use_case: Option<UseCase>,
) -> &mut Option<ModelChoice> {
    match use_case {
        None => &mut models.default,
        Some(UseCase::NlToSql) => &mut models.nl_to_sql,
        Some(UseCase::Explain) => &mut models.explain,
        Some(UseCase::Fix) => &mut models.fix,
        Some(_) => &mut models.exploration,
    }
}

/// Every slot, to write.
pub(crate) fn slots_mut(models: &mut ModelAssignments) -> [&mut Option<ModelChoice>; 5] {
    [
        &mut models.default,
        &mut models.nl_to_sql,
        &mut models.explain,
        &mut models.fix,
        &mut models.exploration,
    ]
}

/// One line of the models list in Settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRow {
    /// The use case; `None` for the default.
    pub use_case: Option<UseCase>,
    /// Its title.
    pub title: &'static str,
    /// What the "no choice" chip says: the default has none, a use case falls back to it.
    pub none_label: &'static str,
    /// The model chosen for it, if any.
    pub current: Option<ModelChoice>,
    /// Every provider and model pair the configuration offers.
    pub choices: Vec<ModelChoice>,
}

/// Every provider and model pair the configuration offers, in file order.
#[must_use]
pub fn model_choices(config: &AiConfig) -> Vec<ModelChoice> {
    config
        .providers
        .iter()
        .flat_map(|p| {
            p.models
                .iter()
                .map(|m| ModelChoice::new(p.id.clone(), m.clone()))
        })
        .collect()
}

/// The rows of the models list; empty while no provider lists a model.
#[must_use]
pub fn model_rows(config: &AiConfig) -> Vec<ModelRow> {
    let choices = model_choices(config);
    if choices.is_empty() {
        return Vec::new();
    }
    SLOTS
        .into_iter()
        .map(|use_case| ModelRow {
            use_case,
            title: use_case_title(use_case),
            none_label: if use_case.is_none() {
                "None"
            } else {
                "Default"
            },
            current: slot(&config.models, use_case).cloned(),
            choices: choices.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqler_config::{ProviderConfig, ProviderId, ProviderKind};

    fn config() -> AiConfig {
        let id = |s| ProviderId::new(s).unwrap();
        let mut c = AiConfig::default();
        c.providers
            .push(ProviderConfig::new(id("a"), ProviderKind::Ollama).with_models(["m1", "m2"]));
        c.providers
            .push(ProviderConfig::new(id("b"), ProviderKind::Anthropic).with_models(["c"]));
        c
    }

    #[test]
    fn slot_and_slot_mut_agree_for_every_slot() {
        let mut models = ModelAssignments::default();
        let id = ProviderId::new("a").unwrap();
        for (i, use_case) in SLOTS.into_iter().enumerate() {
            let choice = ModelChoice::new(id.clone(), format!("m{i}"));
            *slot_mut(&mut models, use_case) = Some(choice.clone());
            assert_eq!(slot(&models, use_case), Some(&choice));
        }
        for (i, s) in slots_mut(&mut models).into_iter().enumerate() {
            assert_eq!(s.as_ref().unwrap().model, format!("m{i}"));
        }
    }

    #[test]
    fn no_rows_without_a_listed_model() {
        assert_eq!(model_rows(&AiConfig::default()).len(), 0);
    }

    #[test]
    fn rows_list_the_default_then_each_use_case() {
        let mut c = config();
        let id = ProviderId::new("b").unwrap();
        c.models.fix = Some(ModelChoice::new(id.clone(), "c"));
        let rows = model_rows(&c);
        let titles: Vec<_> = rows.iter().map(|r| r.title).collect();
        assert_eq!(
            titles,
            ["Default", "NL to SQL", "Explain", "Fix", "Exploration"]
        );
        assert_eq!(rows[0].none_label, "None");
        assert!(rows[1..].iter().all(|r| r.none_label == "Default"));
        assert_eq!(rows[0].choices.len(), 3);
        assert_eq!(rows[0].current, None);
        assert_eq!(rows[3].current, Some(ModelChoice::new(id, "c")));
        assert_eq!(rows[3].use_case, Some(UseCase::Fix));
    }
}
