//! Input planning — `internal/question/input.go`. `plan_input` translates
//! the shared question protocol into the keyboard contract of the detected
//! terminal form; steps preserve the dispatch-uncertainty boundary.

use super::model::*;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct InputStep {
    pub(crate) keys: Vec<String>,
    pub(crate) text: String,
}

impl InputStep {
    pub(crate) fn keys(keys: Vec<String>) -> Self {
        Self {
            keys,
            text: String::new(),
        }
    }

    pub(crate) fn text(text: impl Into<String>) -> Self {
        Self {
            keys: Vec::new(),
            text: text.into(),
        }
    }
}

/// `question.PlanInput` — navigation and clarification short-circuit the
/// per-agent planners.
pub(crate) fn plan_input(interaction: &Interaction, payload: &QuestionPayload) -> Vec<InputStep> {
    match payload.navigation.as_str() {
        "previous" => {
            if interaction.agent == "opencode" {
                return vec![InputStep::keys(vec!["Shift+Tab".to_owned()])];
            }
            return vec![InputStep::keys(vec!["Left".to_owned()])];
        }
        "next" => {
            if interaction.agent == "opencode" {
                return vec![InputStep::keys(vec!["Tab".to_owned()])];
            }
            return vec![InputStep::keys(vec!["Right".to_owned()])];
        }
        _ => {}
    }
    if payload.clarify {
        let mut keys = navigation_keys(
            interaction,
            &QuestionFocus {
                kind: FocusKind::Chat,
                index: 0,
            },
        );
        keys.push("Enter".to_owned());
        return vec![InputStep::keys(keys)];
    }

    match interaction.agent.as_str() {
        "codex" => plan_codex_input(interaction, payload),
        "qoder" => plan_qoder_input(interaction, payload),
        "opencode" => plan_opencode_input(interaction, payload),
        "omp" => plan_omp_input(interaction, payload),
        _ => plan_claude_input(interaction, payload),
    }
}

/// `planCodexInput` — single-select only; `selected[0]` navigates+Enters,
/// otherwise the notes row gets the text + Enter.
pub(crate) fn plan_codex_input(
    interaction: &Interaction,
    payload: &QuestionPayload,
) -> Vec<InputStep> {
    if !payload.selected.is_empty() {
        let target = QuestionFocus {
            kind: FocusKind::Option,
            index: payload.selected[0] as usize,
        };
        let mut keys = navigation_keys(interaction, &target);
        keys.push("Enter".to_owned());
        return vec![InputStep::keys(keys)];
    }
    let target = QuestionFocus {
        kind: FocusKind::Option,
        index: interaction.all_option_count - 1,
    };
    let keys = navigation_keys(interaction, &target);
    if payload.other_text.is_empty() {
        let mut keys = keys;
        keys.push("Enter".to_owned());
        return vec![InputStep::keys(keys)];
    }
    let keys = if interaction.notes_active {
        vec!["Ctrl+U".to_owned()]
    } else {
        let mut keys = keys;
        keys.push("Tab".to_owned());
        keys
    };
    let mut steps = Vec::with_capacity(3);
    if !keys.is_empty() {
        steps.push(InputStep::keys(keys));
    }
    steps.push(InputStep::text(payload.other_text.clone()));
    steps.push(InputStep::keys(vec!["Enter".to_owned()]));
    steps
}

/// `planQoderInput`.
pub(crate) fn plan_qoder_input(
    interaction: &Interaction,
    payload: &QuestionPayload,
) -> Vec<InputStep> {
    if interaction.kind == "single_select" {
        if interaction.notes_active {
            let mut steps = vec![InputStep::keys(vec!["Ctrl+U".to_owned()])];
            if payload.other_selected && !payload.other_text.is_empty() {
                steps.push(InputStep::text(payload.other_text.clone()));
            }
            steps.push(InputStep::keys(vec!["Enter".to_owned()]));
            if payload.other_selected {
                return steps;
            }
            let mut current = interaction.clone();
            current.focus = QuestionFocus {
                kind: FocusKind::Other,
                index: 0,
            };
            let target = QuestionFocus {
                kind: FocusKind::Option,
                index: payload.selected[0] as usize,
            };
            let mut keys = qoder_navigation_keys(&current, &target);
            keys.push("Enter".to_owned());
            steps.push(InputStep::keys(keys));
            return steps;
        }
        if !payload.selected.is_empty() {
            let target = QuestionFocus {
                kind: FocusKind::Option,
                index: payload.selected[0] as usize,
            };
            let mut keys = qoder_navigation_keys(interaction, &target);
            keys.push("Enter".to_owned());
            return vec![InputStep::keys(keys)];
        }
        let target = QuestionFocus {
            kind: FocusKind::Other,
            index: 0,
        };
        let mut keys = qoder_navigation_keys(interaction, &target);
        keys.push("Enter".to_owned());
        keys.push("Ctrl+U".to_owned());
        let mut steps = vec![InputStep::keys(keys)];
        if !payload.other_text.is_empty() {
            steps.push(InputStep::text(payload.other_text.clone()));
        }
        steps.push(InputStep::keys(vec!["Enter".to_owned()]));
        return steps;
    }
    plan_qoder_multi_input(interaction, payload)
}

/// `planQoderMultiInput`.
pub(crate) fn plan_qoder_multi_input(
    interaction: &Interaction,
    payload: &QuestionPayload,
) -> Vec<InputStep> {
    let mut current = interaction.clone();
    let mut steps: Vec<InputStep> = Vec::new();
    let mut notes_handled = false;
    if current.notes_active {
        steps.push(InputStep::keys(vec!["Ctrl+U".to_owned()]));
        if payload.other_selected && !payload.other_text.is_empty() {
            steps.push(InputStep::text(payload.other_text.clone()));
        }
        steps.push(InputStep::keys(vec!["Enter".to_owned()]));
        current.focus = QuestionFocus {
            kind: FocusKind::Other,
            index: 0,
        };
        current.notes_active = false;
        current.other.selected = true;
        current.other.text = payload.other_text.clone();
        notes_handled = true;
    }
    for index in 0..current.options.len() {
        let desired = payload.selected.contains(&(index as i64));
        if current.options[index].selected == desired {
            continue;
        }
        let target = QuestionFocus {
            kind: FocusKind::Option,
            index,
        };
        let mut keys = qoder_navigation_keys(&current, &target);
        keys.push("Enter".to_owned());
        steps.push(InputStep::keys(keys));
        current.focus = target;
        current.options[index].selected = desired;
    }
    let other_target = QuestionFocus {
        kind: FocusKind::Other,
        index: 0,
    };
    if payload.other_selected && !notes_handled {
        let mut keys = qoder_navigation_keys(&current, &other_target);
        keys.push("Enter".to_owned());
        keys.push("Ctrl+U".to_owned());
        steps.push(InputStep::keys(keys));
        if !payload.other_text.is_empty() {
            steps.push(InputStep::text(payload.other_text.clone()));
        }
        steps.push(InputStep::keys(vec!["Enter".to_owned()]));
        current.focus = other_target;
    } else if !payload.other_selected && current.other.selected {
        let mut keys = qoder_navigation_keys(&current, &other_target);
        keys.push("Enter".to_owned());
        steps.push(InputStep::keys(keys));
        current.focus = other_target;
    }
    let submit = QuestionFocus {
        kind: FocusKind::Submit,
        index: 0,
    };
    let mut keys = qoder_navigation_keys(&current, &submit);
    keys.push("Enter".to_owned());
    steps.push(InputStep::keys(keys));
    steps
}

/// `planOpenCodeInput`.
pub(crate) fn plan_opencode_input(
    interaction: &Interaction,
    payload: &QuestionPayload,
) -> Vec<InputStep> {
    if interaction.other.hidden {
        return vec![InputStep::keys(vec!["Enter".to_owned()])];
    }
    if interaction.kind == "multi_select" {
        return plan_opencode_multi_input(interaction, payload);
    }
    if interaction.notes_active {
        if payload.other_selected {
            let mut steps = vec![InputStep::keys(vec!["Ctrl+U".to_owned()])];
            if !payload.other_text.is_empty() {
                steps.push(InputStep::text(payload.other_text.clone()));
            }
            steps.push(InputStep::keys(vec!["Enter".to_owned()]));
            steps.push(InputStep::keys(vec!["Enter".to_owned()]));
            return steps;
        }
        let mut current = interaction.clone();
        current.focus = QuestionFocus {
            kind: FocusKind::Other,
            index: 0,
        };
        let target = QuestionFocus {
            kind: FocusKind::Option,
            index: payload.selected[0] as usize,
        };
        let mut keys = opencode_navigation_keys(&current, &target);
        keys.push("Enter".to_owned());
        return vec![
            InputStep::keys(vec!["Escape".to_owned()]),
            InputStep::keys(keys),
        ];
    }
    if !payload.selected.is_empty() {
        let target = QuestionFocus {
            kind: FocusKind::Option,
            index: payload.selected[0] as usize,
        };
        let mut keys = opencode_navigation_keys(interaction, &target);
        keys.push("Enter".to_owned());
        return vec![InputStep::keys(keys)];
    }
    let target = QuestionFocus {
        kind: FocusKind::Other,
        index: 0,
    };
    let mut keys = opencode_navigation_keys(interaction, &target);
    keys.push("Enter".to_owned());
    keys.push("Ctrl+U".to_owned());
    let mut steps = vec![InputStep::keys(keys)];
    if !payload.other_text.is_empty() {
        steps.push(InputStep::text(payload.other_text.clone()));
    }
    steps.push(InputStep::keys(vec!["Enter".to_owned()]));
    steps.push(InputStep::keys(vec!["Enter".to_owned()]));
    steps
}

/// `planOpenCodeMultiInput`.
pub(crate) fn plan_opencode_multi_input(
    interaction: &Interaction,
    payload: &QuestionPayload,
) -> Vec<InputStep> {
    let mut current = interaction.clone();
    let mut steps: Vec<InputStep> = Vec::new();
    if current.notes_active {
        if payload.other_selected {
            steps.push(InputStep::keys(vec!["Ctrl+U".to_owned()]));
            if !payload.other_text.is_empty() {
                steps.push(InputStep::text(payload.other_text.clone()));
            }
            steps.push(InputStep::keys(vec!["Enter".to_owned()]));
            current.other.selected = true;
            current.other.text = payload.other_text.clone();
        } else {
            steps.push(InputStep::keys(vec!["Escape".to_owned()]));
        }
        current.focus = QuestionFocus {
            kind: FocusKind::Other,
            index: 0,
        };
        current.notes_active = false;
    }
    for index in 0..current.options.len() {
        let desired = payload.selected.contains(&(index as i64));
        if current.options[index].selected == desired {
            continue;
        }
        let target = QuestionFocus {
            kind: FocusKind::Option,
            index,
        };
        let mut keys = opencode_navigation_keys(&current, &target);
        keys.push("Enter".to_owned());
        steps.push(InputStep::keys(keys));
        current.focus = target;
        current.options[index].selected = desired;
    }

    let other_target = QuestionFocus {
        kind: FocusKind::Other,
        index: 0,
    };
    if payload.other_selected
        && (!current.other.selected || current.other.text != payload.other_text)
    {
        let mut keys = opencode_navigation_keys(&current, &other_target);
        keys.push("Enter".to_owned());
        keys.push("Ctrl+U".to_owned());
        steps.push(InputStep::keys(keys));
        if !payload.other_text.is_empty() {
            steps.push(InputStep::text(payload.other_text.clone()));
        }
        steps.push(InputStep::keys(vec!["Enter".to_owned()]));
        current.focus = other_target;
    } else if !payload.other_selected && current.other.selected {
        let mut keys = opencode_navigation_keys(&current, &other_target);
        keys.push("Enter".to_owned());
        steps.push(InputStep::keys(keys));
        current.focus = other_target;
    }
    steps.push(InputStep::keys(vec!["Tab".to_owned()]));
    steps
}

/// `planOMPInput`.
pub(crate) fn plan_omp_input(
    interaction: &Interaction,
    payload: &QuestionPayload,
) -> Vec<InputStep> {
    if interaction.kind == "single_select" {
        if !payload.selected.is_empty() {
            let target = QuestionFocus {
                kind: FocusKind::Option,
                index: payload.selected[0] as usize,
            };
            let mut keys = navigation_keys(interaction, &target);
            keys.push("Enter".to_owned());
            return vec![InputStep::keys(keys)];
        }
        let target = QuestionFocus {
            kind: FocusKind::Option,
            index: interaction.all_option_count - 1,
        };
        let mut keys = navigation_keys(interaction, &target);
        keys.push("Enter".to_owned());
        keys.push("Ctrl+U".to_owned());
        let mut steps = vec![InputStep::keys(keys)];
        if !payload.other_text.is_empty() {
            steps.push(InputStep::text(payload.other_text.clone()));
        }
        steps.push(InputStep::keys(vec!["Enter".to_owned()]));
        return steps;
    }

    let mut current = interaction.clone();
    let mut steps: Vec<InputStep> = Vec::new();
    for index in 0..current.options.len() {
        let desired = payload.selected.contains(&(index as i64));
        if current.options[index].selected == desired {
            continue;
        }
        let target = QuestionFocus {
            kind: FocusKind::Option,
            index,
        };
        let mut keys = navigation_keys(&current, &target);
        keys.push("Enter".to_owned());
        steps.push(InputStep::keys(keys));
        current.focus = target;
    }
    if payload.other_selected {
        let target = QuestionFocus {
            kind: FocusKind::Option,
            index: current.all_option_count - 1,
        };
        let mut keys = navigation_keys(&current, &target);
        keys.push("Enter".to_owned());
        keys.push("Ctrl+U".to_owned());
        steps.push(InputStep::keys(keys));
        if !payload.other_text.is_empty() {
            steps.push(InputStep::text(payload.other_text.clone()));
        }
        steps.push(InputStep::keys(vec!["Enter".to_owned()]));
        return steps;
    }
    if current.question_total > 1 {
        steps.push(InputStep::keys(vec!["Right".to_owned()]));
        return steps;
    }
    if payload.selected.is_empty() {
        return steps;
    }
    let distance = current.options.len() - current.focus.index;
    let mut keys = Vec::with_capacity(distance + 1);
    for _ in 0..distance {
        keys.push("Down".to_owned());
    }
    keys.push("Enter".to_owned());
    steps.push(InputStep::keys(keys));
    steps
}

/// `planClaudeInput`.
pub(crate) fn plan_claude_input(
    interaction: &Interaction,
    payload: &QuestionPayload,
) -> Vec<InputStep> {
    if interaction.kind == "single_select" {
        if !payload.selected.is_empty() {
            let mut current = interaction.clone();
            let mut steps: Vec<InputStep> = Vec::new();
            if !current.other.hidden
                && !current.other.text.is_empty()
                && payload.other_text.is_empty()
            {
                let other_target = QuestionFocus {
                    kind: FocusKind::Option,
                    index: current.all_option_count - 1,
                };
                let mut keys = navigation_keys(&current, &other_target);
                keys.push("Ctrl+U".to_owned());
                steps.push(InputStep::keys(keys));
                current.focus = other_target;
            }
            let target = QuestionFocus {
                kind: FocusKind::Option,
                index: payload.selected[0] as usize,
            };
            let mut keys = navigation_keys(&current, &target);
            keys.push("Enter".to_owned());
            steps.push(InputStep::keys(keys));
            return steps;
        }
        let target = QuestionFocus {
            kind: FocusKind::Option,
            index: interaction.all_option_count - 1,
        };
        let mut keys = navigation_keys(interaction, &target);
        keys.push("Ctrl+U".to_owned());
        let mut steps = vec![InputStep::keys(keys)];
        if !payload.other_text.is_empty() {
            steps.push(InputStep::text(payload.other_text.clone()));
        }
        steps.push(InputStep::keys(vec!["Enter".to_owned()]));
        return steps;
    }

    let mut current = interaction.clone();
    let mut steps: Vec<InputStep> = Vec::new();
    for index in 0..current.options.len() {
        let desired = payload.selected.contains(&(index as i64));
        if current.options[index].selected == desired {
            continue;
        }
        let target = QuestionFocus {
            kind: FocusKind::Option,
            index,
        };
        let mut keys = navigation_keys(&current, &target);
        keys.push("Enter".to_owned());
        steps.push(InputStep::keys(keys));
        current.focus = target;
        current.options[index].selected = desired;
    }
    let other_target = QuestionFocus {
        kind: FocusKind::Option,
        index: current.all_option_count - 1,
    };
    if current.other.text != payload.other_text {
        let mut keys = navigation_keys(&current, &other_target);
        keys.push("Ctrl+U".to_owned());
        steps.push(InputStep::keys(keys));
        current.focus = other_target.clone();
        if !payload.other_text.is_empty() {
            steps.push(InputStep::text(payload.other_text.clone()));
        }
    }
    if current.other.selected != payload.other_selected {
        let mut keys = navigation_keys(&current, &other_target);
        keys.push("Enter".to_owned());
        steps.push(InputStep::keys(keys));
        current.focus = other_target;
    }
    let submit = QuestionFocus {
        kind: FocusKind::Submit,
        index: 0,
    };
    let mut keys = navigation_keys(&current, &submit);
    keys.push("Enter".to_owned());
    steps.push(InputStep::keys(keys));
    steps
}

/// `navigationKeys` — Up/Down travel between focus positions (option rows,
/// then submit, then chat for multi-select Claude).
pub(crate) fn navigation_keys(interaction: &Interaction, target: &QuestionFocus) -> Vec<String> {
    let position = |focus: &QuestionFocus| -> i64 {
        match focus.kind {
            FocusKind::Option => focus.index as i64,
            FocusKind::Submit => {
                if interaction.kind == "multi_select" {
                    interaction.all_option_count as i64
                } else {
                    0
                }
            }
            FocusKind::Chat => {
                let mut position = interaction.all_option_count as i64;
                if interaction.kind == "multi_select" {
                    position += 1;
                }
                position
            }
            FocusKind::Other => 0,
        }
    };
    let mut distance = position(target) - position(&interaction.focus);
    let mut key = "Down";
    if distance < 0 {
        key = "Up";
        distance = -distance;
    }
    vec![key.to_owned(); distance as usize]
}

/// `qoderNavigationKeys` — qoder's positions: options, then other, then
/// submit (multi-select gets one more slot).
pub(crate) fn qoder_navigation_keys(
    interaction: &Interaction,
    target: &QuestionFocus,
) -> Vec<String> {
    let position = |focus: &QuestionFocus| -> i64 {
        match focus.kind {
            FocusKind::Option => focus.index as i64,
            FocusKind::Submit => interaction.options.len() as i64,
            FocusKind::Other => {
                let mut position = interaction.options.len() as i64;
                if interaction.kind == "multi_select" {
                    position += 1;
                }
                position
            }
            FocusKind::Chat => 0,
        }
    };
    let mut distance = position(target) - position(&interaction.focus);
    let mut key = "Down";
    if distance < 0 {
        key = "Up";
        distance = -distance;
    }
    vec![key.to_owned(); distance as usize]
}

/// `openCodeNavigationKeys` — `other` is the row after the options.
pub(crate) fn opencode_navigation_keys(
    interaction: &Interaction,
    target: &QuestionFocus,
) -> Vec<String> {
    let position = |focus: &QuestionFocus| -> i64 {
        if focus.kind == FocusKind::Other {
            return interaction.options.len() as i64;
        }
        focus.index as i64
    };
    let mut distance = position(target) - position(&interaction.focus);
    let mut key = "Down";
    if distance < 0 {
        key = "Up";
        distance = -distance;
    }
    vec![key.to_owned(); distance as usize]
}

/// `approvalKeys` — move focus from the classified row to the chosen one,
/// then Enter.
pub(crate) fn approval_keys(target: usize, current: usize) -> Vec<String> {
    let mut distance = target as i64 - current as i64;
    let mut key = "Down";
    if distance < 0 {
        key = "Up";
        distance = -distance;
    }
    let mut keys = Vec::with_capacity(distance as usize + 1);
    for _ in 0..distance {
        keys.push(key.to_owned());
    }
    keys.push("Enter".to_owned());
    keys
}
