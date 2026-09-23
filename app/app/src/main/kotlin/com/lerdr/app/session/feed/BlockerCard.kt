package com.lerdr.app.session.feed

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import com.lerdr.core.designsystem.theme.LerdrTheme
import lerdr.core.model.BlockedMessage
import lerdr.core.model.Interaction
import lerdr.core.store.Agent
import lerdr.core.store.attentionKind

/**
 * The pinned blocker card — triage parity with the oracle's
 * `AgentList`/`TerminalView` blocked state. The header carries the ⚠
 * warning mark; approval choices are color-coded with
 * [approvalButtonTone] (the oracle's `agents.ts` port); a question
 * interaction mounts [QuestionFormCard]; anything else falls back to the
 * oracle's "switch to Terminal" hint.
 */

/** Oracle `approvalButtonTone` buckets. */
internal enum class ApprovalTone { APPROVE, TRUST, DENY }

private val DENY_WORDS = Regex("\\b(no|deny|reject|cancel|exit)\\b")
private val TRUST_WORDS =
    Regex("\\b(always|trust|don't ask|dont ask|configure|edit|amend)\\b")

/**
 * `approvalButtonTone` — the LAST option is always the deny tone (the
 * oracle treats position as the reject slot); deny keywords anywhere and
 * trust keywords map next; everything else approves.
 */
internal fun approvalButtonTone(option: String, index: Int, total: Int): ApprovalTone {
    val value = option.replace(Regex("\\s+"), " ").trim().lowercase()
    if (index == total - 1 || DENY_WORDS.containsMatchIn(value)) return ApprovalTone.DENY
    if (TRUST_WORDS.containsMatchIn(value)) return ApprovalTone.TRUST
    return ApprovalTone.APPROVE
}

/**
 * The blocker card. [interaction] is the effective question (the
 * ViewModel's command-result override wins over the stale store copy);
 * [enabled] folds `responding` + the reader-role gate into one switch —
 * mutating affordances stay visible but inert like the oracle.
 */
@Composable
internal fun FeedBlockerCard(
    agent: Agent,
    interaction: Interaction?,
    draft: QuestionDraft,
    enabled: Boolean,
    onRespond: (Int, String) -> Unit,
    onDraftChange: (QuestionDraft) -> Unit,
    onSubmitQuestion: () -> Unit,
    onPreviousQuestion: () -> Unit,
    onClarifyQuestion: () -> Unit,
    onOpenTerminal: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val colors = LerdrTheme.extendedColors
    val spacing = LerdrTheme.spacing
    val kind = attentionKind(agent)
    val approval = kind == BlockedMessage.ATTENTION_APPROVAL
    val options = agent.options.orEmpty()
    val prompt = agent.prompt ?: interaction?.question ?: agent.command ?: ""

    Card(
        colors = CardDefaults.cardColors(
            containerColor = colors.attentionContainer,
            contentColor = colors.onAttentionContainer,
        ),
        shape = MaterialTheme.shapes.large,
        modifier = modifier.fillMaxWidth(),
    ) {
        Column(
            modifier = Modifier.padding(spacing.medium),
            verticalArrangement = Arrangement.spacedBy(spacing.small),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Icon(
                    Icons.Filled.Warning,
                    contentDescription = null,
                    tint = colors.attention,
                    modifier = Modifier
                        .padding(end = spacing.extraSmall)
                        .size(16.dp),
                )
                Text(
                    if (approval) "APPROVAL NEEDED" else "QUESTION",
                    style = MaterialTheme.typography.labelMedium,
                    color = colors.attention,
                )
            }
            if (prompt.isNotEmpty()) {
                Text(prompt, style = MaterialTheme.typography.titleSmall)
            }
            when {
                approval && options.isNotEmpty() -> ApprovalButtons(
                    options = options,
                    enabled = enabled,
                    onRespond = onRespond,
                )
                interaction != null -> QuestionFormCard(
                    interaction = interaction,
                    draft = draft,
                    enabled = enabled,
                    onDraftChange = onDraftChange,
                    onSubmit = onSubmitQuestion,
                    onPrevious = onPreviousQuestion,
                    onClarify = onClarifyQuestion,
                )
                else -> Column(verticalArrangement = Arrangement.spacedBy(spacing.extraSmall)) {
                    Text(
                        "Switch to Terminal to handle the pending agent interaction.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    OutlinedTerminalButton(onOpenTerminal)
                }
            }
        }
    }
}

@Composable
private fun OutlinedTerminalButton(onOpenTerminal: () -> Unit) {
    FilledTonalButton(onClick = onOpenTerminal) {
        Text("Open Terminal")
    }
}

/** One button per choice, tone-colored by [approvalButtonTone]. */
@Composable
private fun ApprovalButtons(
    options: List<String>,
    enabled: Boolean,
    onRespond: (Int, String) -> Unit,
) {
    val colors = LerdrTheme.extendedColors
    val spacing = LerdrTheme.spacing
    Row(
        horizontalArrangement = Arrangement.spacedBy(spacing.small),
        modifier = Modifier.fillMaxWidth(),
    ) {
        options.forEachIndexed { index, label ->
            when (approvalButtonTone(label, index, options.size)) {
                ApprovalTone.APPROVE -> Button(
                    onClick = { onRespond(index, label) },
                    enabled = enabled,
                    colors = ButtonDefaults.buttonColors(
                        containerColor = colors.working,
                        contentColor = colors.onWorking,
                    ),
                    modifier = Modifier.weight(1f),
                ) { Text(label, maxLines = 1) }
                ApprovalTone.TRUST -> FilledTonalButton(
                    onClick = { onRespond(index, label) },
                    enabled = enabled,
                    colors = ButtonDefaults.filledTonalButtonColors(
                        containerColor = MaterialTheme.colorScheme.secondaryContainer,
                        contentColor = MaterialTheme.colorScheme.onSecondaryContainer,
                    ),
                    modifier = Modifier.weight(1f),
                ) { Text(label, maxLines = 1) }
                ApprovalTone.DENY -> Button(
                    onClick = { onRespond(index, label) },
                    enabled = enabled,
                    colors = ButtonDefaults.buttonColors(
                        containerColor = colors.danger,
                        contentColor = colors.onDanger,
                    ),
                    modifier = Modifier.weight(1f),
                ) { Text(label, maxLines = 1) }
            }
        }
    }
}
