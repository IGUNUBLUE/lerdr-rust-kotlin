package com.lerdr.app.session.feed

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import com.lerdr.core.designsystem.theme.LerdrTheme
import kotlin.math.roundToInt
import lerdr.core.conversation.ConversationBrowseProgress
import lerdr.core.conversation.ConversationBrowseState
import lerdr.core.conversation.ConversationDiagnostics

/**
 * Conversation-history diagnostics + preparation surface — the port of the
 * oracle's `conversation-warning` block (`ConversationHistory.svelte`):
 * one centered status line per warning, rendered in the oracle's template
 * order above the transcript, plus the recover affordances the controller
 * (`conversation-history.ts`) backs with `get_conversation_history`
 * re-requests. There are no dedicated wire commands — Cancel only stops the
 * client-side poll loop, Continue/Retry re-issue the stored cursor, and
 * "Reload history" re-browses cursorless.
 */

/** Oracle `HISTORY_PREPARATION_INTERVAL_MS` — delay between preparing polls. */
internal const val HISTORY_PREPARATION_INTERVAL_MS = 1_000L

/** Oracle `HISTORY_MAX_PREPARATION_POLLS` — unchanged-progress polls before a stall pause. */
internal const val HISTORY_MAX_PREPARATION_POLLS = 30

/** Oracle `HISTORY_WIRE_PAGE_SIZE` — the history view's request limit. */
internal const val HISTORY_WIRE_PAGE_SIZE = 200

/** Oracle `sourceChangedNotice` — errors that resolve through a cursorless reload. */
internal val HISTORY_RELOAD_CODES = setOf("source_changed", "invalid_cursor", "cursor_expired")

/** Oracle `recoverHistory` label split — work-guard codes read "Continue". */
internal val HISTORY_CONTINUE_CODES = setOf(
    "work_deadline", "work_limit", "stalled", "preparation_stalled",
)

/**
 * Oracle `continuationMessage()` — the `continuation_incomplete` copy.
 * `resolution_limit` softens to "could not be fully checked"; every other
 * reason (including `partial_link` and missing) reads "continues in another
 * session".
 */
internal fun continuationWarningText(reason: String?): String = when (reason) {
    "resolution_limit" ->
        "This conversation may continue, but its continuation chain could not be fully checked. Reload to try again."
    else ->
        "This conversation continues in another session, but part of that history is unavailable. Reload to try again."
}

/** Oracle oversized-records warning — singular/plural "record(s) were". */
internal fun oversizedRecordsText(count: Int): String =
    "$count oversized " + (if (count == 1) "record was" else "records were") +
        " skipped from the full history."

/** Oracle omitted-tools/payloads warning — counts appended only when present. */
internal fun omittedActivityText(diagnostics: ConversationDiagnostics): String = buildString {
    append("Some tool activity is shortened to keep this history page within its response limit")
    if (diagnostics.omittedTools > 0) {
        append(" (").append(diagnostics.omittedTools).append(" tool")
        if (diagnostics.omittedTools != 1) append('s')
        append(" omitted)")
    }
    if (diagnostics.omittedPayloads > 0) {
        append("; ").append(diagnostics.omittedPayloads).append(" payload")
        if (diagnostics.omittedPayloads != 1) append('s')
        append(" shortened")
    }
    append('.')
}

/**
 * Oracle preparing row — `Preparing history ({phase || 'scanning'})` plus
 * ` — {percent}% scanned` when the page reports `source_bytes`, always
 * terminated by the oracle's ellipsis.
 */
internal fun preparationStatusText(progress: ConversationBrowseProgress?): String =
    buildString {
        append("Preparing history (")
        append(progress?.phase?.takeIf { it.isNotEmpty() } ?: "scanning")
        append(')')
        val source = progress?.sourceBytes ?: 0L
        if (source > 0L) {
            val scanned = progress?.scannedBytes ?: 0L
            val percent = minOf(100, (scanned.toDouble() / source * 100).roundToInt())
            append(" — ").append(percent).append("% scanned")
        }
        append('…')
    }

/** Oracle recover-affordance label — `Continue` for work-guard stalls, else `Retry`. */
internal fun historyRecoverLabel(code: String): String =
    if (code in HISTORY_CONTINUE_CODES) "Continue" else "Retry"

/** Oracle `sourceChangedNotice` — true when the error resolves by reloading. */
internal fun isHistorySourceChanged(error: String?, code: String): Boolean =
    error != null && code in HISTORY_RELOAD_CODES

/**
 * Oracle `mergeDiagnostics` — cursorful pages merge into the accumulated
 * window report: counts keep the max, flags OR, and `continuation_reason`
 * prefers the newer page.
 */
internal fun mergeHistoryDiagnostics(
    previous: ConversationDiagnostics,
    next: ConversationDiagnostics,
): ConversationDiagnostics = ConversationDiagnostics(
    oversizedRecords = maxOf(previous.oversizedRecords, next.oversizedRecords),
    corruptRecords = maxOf(previous.corruptRecords, next.corruptRecords),
    omittedTools = maxOf(previous.omittedTools, next.omittedTools),
    omittedPayloads = maxOf(previous.omittedPayloads, next.omittedPayloads),
    planCorrupt = previous.planCorrupt || next.planCorrupt,
    sourceTruncated = previous.sourceTruncated || next.sourceTruncated,
    continuationIncomplete =
        previous.continuationIncomplete || next.continuationIncomplete,
    continuationReason = next.continuationReason ?: previous.continuationReason,
)

/**
 * The warning block — oracle template order: source-changed notice,
 * preparation row, continuation warning, oversized records, omitted tool
 * activity, corrupt records, the generic error + recover affordance, then
 * `emptyHistoryText` for a healthy-but-empty authoritative window.
 */
@Composable
internal fun FeedHistoryWarnings(
    browseState: ConversationBrowseState,
    browseProgress: ConversationBrowseProgress?,
    diagnostics: ConversationDiagnostics,
    preparationPaused: Boolean,
    hasMoreHistory: Boolean,
    historyLoading: Boolean,
    entriesEmpty: Boolean,
    historyError: String?,
    historyErrorCode: String,
    historyErrorRetryable: Boolean,
    onReloadHistory: () -> Unit,
    onRecoverHistory: () -> Unit,
    onCancelPreparation: () -> Unit,
    onContinuePreparation: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val sourceChanged = historyError != null && historyErrorCode in HISTORY_RELOAD_CODES
    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        modifier = modifier.fillMaxWidth(),
    ) {
        if (sourceChanged) {
            HistoryWarning(
                text = historyError,
                error = true,
                action = "Reload history",
                onAction = onReloadHistory,
            )
        }
        // Oracle: `nextCursor && (preparationPolls >= max || state === 'preparing')`
        // — `hasMoreHistory` tracks the non-empty nextCursor.
        if (hasMoreHistory &&
            (preparationPaused || browseState == ConversationBrowseState.PREPARING)
        ) {
            if (preparationPaused) {
                HistoryWarning(
                    text = "Preparation is paused.",
                    error = false,
                    action = "Continue",
                    onAction = onContinuePreparation,
                )
            } else {
                HistoryWarning(
                    text = preparationStatusText(browseProgress),
                    error = false,
                    action = "Cancel",
                    onAction = onCancelPreparation,
                )
            }
        }
        if (diagnostics.continuationIncomplete) {
            HistoryWarning(
                text = continuationWarningText(diagnostics.continuationReason),
                error = true,
                action = "Reload history",
                onAction = onReloadHistory,
            )
        }
        if (diagnostics.oversizedRecords > 0) {
            HistoryWarning(
                text = oversizedRecordsText(diagnostics.oversizedRecords),
                error = false,
            )
        }
        if (diagnostics.omittedTools > 0 || diagnostics.omittedPayloads > 0) {
            HistoryWarning(
                text = omittedActivityText(diagnostics),
                error = false,
            )
        }
        if (diagnostics.corruptRecords > 0 || diagnostics.planCorrupt) {
            HistoryWarning(
                text = "Some records could not be decoded. Valid turns are shown, but the source may be damaged.",
                error = true,
            )
        }
        if (historyError != null && !sourceChanged) {
            HistoryWarning(
                text = historyError,
                error = true,
                action = if (historyErrorRetryable) {
                    historyRecoverLabel(historyErrorCode)
                } else {
                    null
                },
                onAction = onRecoverHistory,
            )
        }
        // Oracle `emptyHistoryText` — `!historyBusy && !error` and an
        // authoritative empty window; a damaged/truncated source reads
        // "No readable …" instead of "No conversation messages …".
        val historyBusy = historyLoading || browseState == ConversationBrowseState.PREPARING
        if (entriesEmpty && !historyBusy && historyError == null) {
            HistoryWarning(
                text = if (diagnostics.sourceTruncated ||
                    diagnostics.oversizedRecords > 0 ||
                    diagnostics.corruptRecords > 0 ||
                    diagnostics.continuationIncomplete
                ) {
                    "No readable conversation messages are available in the loaded history."
                } else {
                    "No conversation messages have been recorded yet."
                },
                error = false,
            )
        }
    }
}

/**
 * One `conversation-warning` line — small centered status text, error rows
 * tinted danger, with the oracle's optional inline action button.
 */
@Composable
private fun HistoryWarning(
    text: String,
    error: Boolean,
    modifier: Modifier = Modifier,
    action: String? = null,
    onAction: () -> Unit = {},
) {
    val spacing = LerdrTheme.spacing
    Column(
        horizontalAlignment = Alignment.CenterHorizontally,
        modifier = modifier
            .fillMaxWidth()
            .semantics {
                // role="status" vs role="alert" in the oracle template.
                liveRegion = if (error) LiveRegionMode.Assertive else LiveRegionMode.Polite
            }
            .padding(horizontal = spacing.medium),
    ) {
        Text(
            text,
            style = MaterialTheme.typography.labelMedium,
            color = if (error) {
                LerdrTheme.extendedColors.danger
            } else {
                LerdrTheme.extendedColors.attention
            },
            textAlign = TextAlign.Center,
        )
        if (action != null) {
            TextButton(onClick = onAction) {
                Text(action)
            }
        }
    }
}
