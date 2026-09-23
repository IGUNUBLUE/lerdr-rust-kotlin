package com.lerdr.app.session.feed

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material3.Checkbox
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.semantics.Role
import com.lerdr.core.designsystem.theme.LerdrTheme
import lerdr.core.model.Interaction
import lerdr.core.model.Option

/**
 * Structured question form — port of the oracle's `QuestionForm.svelte` +
 * `frontend/src/lib/questions.ts` onto `answer_question`/`navigate_question`/
 * `clarify_question` wire actions. The draft logic is kept in pure
 * functions so the ViewModel can own it (the oracle's module-level `drafts`
 * map survives form re-mounts; our `FeedLocal.questionDrafts` does the same).
 */

/** The oracle's `QuestionDraft` — checked option indices + the Other slot. */
@Immutable
data class QuestionDraft(
    val selected: Set<Int> = emptySet(),
    val otherSelected: Boolean = false,
    val otherText: String = "",
)

/** `questionDraftKey` — drafts are scoped to pane + interaction id. */
internal fun questionDraftKey(paneId: String, interaction: Interaction): String =
    "$paneId::${interaction.id}"

/** `createQuestionDraft` — initial selection mirrors the interaction model. */
internal fun createQuestionDraft(interaction: Interaction): QuestionDraft = QuestionDraft(
    selected = interaction.options.filter { it.selected }.map { it.index }.toSet(),
    otherSelected = interaction.other.selected,
    otherText = interaction.other.text,
)

/** `questionSubmitAllowed` — multi always; single needs one choice or valid Other. */
internal fun questionSubmitAllowed(interaction: Interaction, draft: QuestionDraft): Boolean {
    if (interaction.kindOrNull == Interaction.Kind.MULTI_SELECT) return true
    val otherAllowed = draft.otherSelected &&
        (draft.otherText.isNotBlank() || interaction.other.allowEmpty)
    return draft.selected.size == 1 || otherAllowed
}

/**
 * `shouldRestoreQuestionDraft` — a dirty draft beats the incoming baseline
 * when it still submits, or when the incoming one would not submit at all.
 */
internal fun shouldRestoreQuestionDraft(
    interaction: Interaction,
    cached: QuestionDraft?,
    incoming: QuestionDraft,
): Boolean {
    if (cached == null) return false
    return questionSubmitAllowed(interaction, cached) ||
        !questionSubmitAllowed(interaction, incoming)
}

/** `updateQuestionOption` — single-select clears the set and the Other slot. */
internal fun updateQuestionOption(
    interaction: Interaction,
    draft: QuestionDraft,
    index: Int,
    checked: Boolean,
): QuestionDraft {
    val selected = draft.selected.toMutableSet()
    var otherSelected = draft.otherSelected
    var otherText = draft.otherText
    if (interaction.kindOrNull != Interaction.Kind.MULTI_SELECT) {
        selected.clear()
        if (checked) selected += index
        otherSelected = false
        otherText = ""
    } else if (checked) {
        selected += index
    } else {
        selected -= index
    }
    return QuestionDraft(selected = selected, otherSelected = otherSelected, otherText = otherText)
}

/** `updateQuestionOther` — selecting Other on single-select clears options. */
internal fun updateQuestionOther(
    interaction: Interaction,
    draft: QuestionDraft,
    selected: Boolean,
    text: String = draft.otherText,
): QuestionDraft {
    val choices = draft.selected.toMutableSet()
    if (interaction.kindOrNull != Interaction.Kind.MULTI_SELECT && selected) choices.clear()
    return QuestionDraft(
        selected = choices,
        otherSelected = selected,
        otherText = if (interaction.kindOrNull == Interaction.Kind.MULTI_SELECT && !selected) {
            ""
        } else {
            text
        },
    )
}

/**
 * Oracle `changeOther` — typing selects Other (always on single-select,
 * while non-empty on multi); clearing multi Other unselects it.
 */
internal fun changeQuestionOtherText(
    interaction: Interaction,
    draft: QuestionDraft,
    text: String,
): QuestionDraft = updateQuestionOther(
    interaction,
    draft,
    interaction.kindOrNull != Interaction.Kind.MULTI_SELECT || text.isNotEmpty(),
    text,
)

/** Oracle `progress` — "Question X of Y" only for a sane 1-based range. */
internal fun questionProgress(interaction: Interaction): String {
    val current = interaction.questionIndex
    val total = interaction.questionTotal
    if (current < 1 || current > total) return ""
    return "Question $current of $total"
}

/**
 * The form body — checkbox list on `multi_select`, radio group on
 * `single_select`, the Other row when `other.hidden` is false, then the
 * Previous / Submit / Chat-about-this action row. [enabled] folds the
 * oracle's `responding` disable with the app's reader-role gate.
 */
@Composable
internal fun QuestionFormCard(
    interaction: Interaction,
    draft: QuestionDraft,
    enabled: Boolean,
    onDraftChange: (QuestionDraft) -> Unit,
    onSubmit: () -> Unit,
    onPrevious: () -> Unit,
    onClarify: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val spacing = LerdrTheme.spacing
    val multi = interaction.kindOrNull == Interaction.Kind.MULTI_SELECT
    Column(
        modifier = modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(spacing.small),
    ) {
        questionProgress(interaction).takeIf { it.isNotEmpty() }?.let { progress ->
            Text(
                progress,
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Column(modifier = if (multi) Modifier else Modifier.selectableGroup()) {
            interaction.options.forEachIndexed { position, option ->
                QuestionOptionRow(
                    option = option,
                    position = position,
                    multi = multi,
                    checked = option.index in draft.selected,
                    enabled = enabled,
                    onChecked = { checked ->
                        onDraftChange(
                            updateQuestionOption(interaction, draft, option.index, checked),
                        )
                    },
                )
            }
        }
        if (!interaction.other.hidden) {
            QuestionOtherRow(
                interaction = interaction,
                draft = draft,
                multi = multi,
                enabled = enabled,
                onDraftChange = onDraftChange,
            )
        }
        Row(horizontalArrangement = Arrangement.spacedBy(spacing.small)) {
            if (interaction.canGoBack) {
                OutlinedButton(onClick = onPrevious, enabled = enabled) {
                    Text("← Previous")
                }
            }
            FilledTonalButton(
                onClick = onSubmit,
                enabled = enabled && questionSubmitAllowed(interaction, draft),
            ) {
                Text(interaction.submitLabel.ifEmpty { "Submit" })
            }
            // The oracle gates this on `can_chat && !other` where `other` is
            // never absent on the wire — its condition is dead code. The
            // spec'd gate is `can_chat` alone.
            if (interaction.canChat) {
                OutlinedButton(onClick = onClarify, enabled = enabled) {
                    Text("Chat about this")
                }
            }
        }
        Text(
            when {
                !enabled -> "Waiting for agent…"
                multi -> "Selections are sent when you submit."
                else -> "Choose one answer."
            },
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/** One option row — summary lines (answered questions) beat the description. */
@Composable
private fun QuestionOptionRow(
    option: Option,
    position: Int,
    multi: Boolean,
    checked: Boolean,
    enabled: Boolean,
    onChecked: (Boolean) -> Unit,
) {
    val spacing = LerdrTheme.spacing
    Row(
        verticalAlignment = Alignment.Top,
        modifier = Modifier
            .fillMaxWidth()
            .then(
                if (multi) {
                    Modifier.toggleable(
                        value = checked,
                        enabled = enabled,
                        role = Role.Checkbox,
                        onValueChange = onChecked,
                    )
                } else {
                    // A radio never unchecks itself — re-click is a no-op.
                    Modifier.selectable(
                        selected = checked,
                        enabled = enabled,
                        role = Role.RadioButton,
                        onClick = { onChecked(true) },
                    )
                },
            )
            .padding(vertical = spacing.extraSmall),
    ) {
        if (multi) {
            Checkbox(
                checked = checked,
                onCheckedChange = null,
                enabled = enabled,
            )
        } else {
            RadioButton(
                selected = checked,
                onClick = null,
                enabled = enabled,
            )
        }
        Column(modifier = Modifier.padding(start = spacing.extraSmall)) {
            Text(
                option.label.ifEmpty { "Option ${position + 1}" },
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = androidx.compose.ui.text.font.FontWeight.SemiBold,
            )
            if (option.summary.isNotEmpty()) {
                option.summary.forEachIndexed { row, entry ->
                    Text(
                        "${row + 1}. ${entry.question}: ${entry.answer}",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            } else if (option.description.isNotEmpty()) {
                Text(
                    option.description,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

/** The Other row — toggle + free text; focus/typing selects it. */
@Composable
private fun QuestionOtherRow(
    interaction: Interaction,
    draft: QuestionDraft,
    multi: Boolean,
    enabled: Boolean,
    onDraftChange: (QuestionDraft) -> Unit,
) {
    val spacing = LerdrTheme.spacing
    Column(verticalArrangement = Arrangement.spacedBy(spacing.extraSmall)) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .fillMaxWidth()
                .then(
                    if (multi) {
                        Modifier.toggleable(
                            value = draft.otherSelected,
                            enabled = enabled,
                            role = Role.Checkbox,
                            onValueChange = { selected ->
                                onDraftChange(
                                    updateQuestionOther(interaction, draft, selected),
                                )
                            },
                        )
                    } else {
                        Modifier.selectable(
                            selected = draft.otherSelected,
                            enabled = enabled,
                            role = Role.RadioButton,
                            onClick = {
                                onDraftChange(
                                    updateQuestionOther(interaction, draft, true),
                                )
                            },
                        )
                    },
                ),
        ) {
            if (multi) {
                Checkbox(
                    checked = draft.otherSelected,
                    onCheckedChange = null,
                    enabled = enabled,
                )
            } else {
                RadioButton(
                    selected = draft.otherSelected,
                    onClick = null,
                    enabled = enabled,
                )
            }
            Text(
                interaction.other.label.ifEmpty { "Other" },
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.padding(start = spacing.extraSmall),
            )
        }
        OutlinedTextField(
            value = draft.otherText,
            onValueChange = { text ->
                if (enabled) {
                    onDraftChange(changeQuestionOtherText(interaction, draft, text))
                }
            },
            placeholder = {
                Text(interaction.other.placeholder.ifEmpty { "Other answer" })
            },
            enabled = enabled,
            singleLine = false,
            maxLines = 3,
            modifier = Modifier
                .fillMaxWidth()
                .padding(start = spacing.medium)
                .onFocusChanged { state ->
                    // Oracle `onfocus` — focusing Other selects it.
                    if (state.isFocused && !draft.otherSelected && enabled) {
                        onDraftChange(updateQuestionOther(interaction, draft, true))
                    }
                },
        )
    }
}
