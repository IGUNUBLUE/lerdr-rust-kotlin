package com.lerdr.app.session.feed

import com.google.common.truth.Truth.assertThat
import lerdr.core.conversation.ConversationBrowseProgress
import lerdr.core.conversation.ConversationDiagnostics
import org.junit.Test

/**
 * The oracle's `conversation-warning` copy — `continuationMessage`,
 * oversized/omitted diagnostics text, the preparing row, the recover label,
 * `sourceChangedNotice`, and `mergeDiagnostics` — as pure functions.
 */
class FeedHistoryWarningsTest {

    @Test
    fun `continuation warning softens only for resolution_limit`() {
        assertThat(continuationWarningText("resolution_limit"))
            .isEqualTo(
                "This conversation may continue, but its continuation chain " +
                    "could not be fully checked. Reload to try again.",
            )
        // Every other reason — including null/unknown — is the harder wording.
        for (reason in listOf(
            "invalid_link", "ambiguous_link", "cycle", "missing_source",
            "partial_link", "surprise", null,
        )) {
            assertThat(continuationWarningText(reason))
                .isEqualTo(
                    "This conversation continues in another session, but part " +
                        "of that history is unavailable. Reload to try again.",
                )
        }
    }

    @Test
    fun `oversized warning pluralizes the record count`() {
        assertThat(oversizedRecordsText(1))
            .isEqualTo("1 oversized record was skipped from the full history.")
        assertThat(oversizedRecordsText(3))
            .isEqualTo("3 oversized records were skipped from the full history.")
    }

    @Test
    fun `omitted activity text appends counts only when present`() {
        assertThat(
            omittedActivityText(
                ConversationDiagnostics(omittedTools = 1, omittedPayloads = 1),
            ),
        ).isEqualTo(
            // The payload clause sits outside the tools parenthesis.
            "Some tool activity is shortened to keep this history page within " +
                "its response limit (1 tool omitted); 1 payload shortened.",
        )
        assertThat(
            omittedActivityText(
                ConversationDiagnostics(omittedTools = 2),
            ),
        ).isEqualTo(
            "Some tool activity is shortened to keep this history page within " +
                "its response limit (2 tools omitted).",
        )
        assertThat(
            omittedActivityText(
                ConversationDiagnostics(omittedPayloads = 5),
            ),
        ).isEqualTo(
            "Some tool activity is shortened to keep this history page within " +
                "its response limit; 5 payloads shortened.",
        )
    }

    @Test
    fun `preparing text falls back to scanning and renders the percent`() {
        assertThat(preparationStatusText(null))
            .isEqualTo("Preparing history (scanning)…")
        assertThat(
            preparationStatusText(
                ConversationBrowseProgress(
                    phase = "indexing",
                    scannedBytes = 512,
                    sourceBytes = 2_048,
                ),
            ),
        ).isEqualTo("Preparing history (indexing) — 25% scanned…")
        // No source_bytes → no percent suffix.
        assertThat(
            preparationStatusText(
                ConversationBrowseProgress(
                    phase = "indexing",
                    scannedBytes = 512,
                    sourceBytes = 0,
                ),
            ),
        ).isEqualTo("Preparing history (indexing)…")
        // The oracle caps the percentage at 100.
        assertThat(
            preparationStatusText(
                ConversationBrowseProgress(
                    phase = "scanning",
                    scannedBytes = 4_096,
                    sourceBytes = 2_048,
                ),
            ),
        ).isEqualTo("Preparing history (scanning) — 100% scanned…")
    }

    @Test
    fun `recover label is Continue for work-guard codes else Retry`() {
        for (code in listOf("work_deadline", "work_limit", "stalled", "preparation_stalled")) {
            assertThat(historyRecoverLabel(code)).isEqualTo("Continue")
        }
        assertThat(historyRecoverLabel("cursor_expired")).isEqualTo("Retry")
        assertThat(historyRecoverLabel("")).isEqualTo("Retry")
    }

    @Test
    fun `source changed codes resolve through a cursorless reload`() {
        for (code in listOf("source_changed", "invalid_cursor", "cursor_expired")) {
            assertThat(isHistorySourceChanged("boom", code)).isTrue()
        }
        assertThat(isHistorySourceChanged("boom", "query_failed")).isFalse()
        assertThat(isHistorySourceChanged(null, "source_changed")).isFalse()
    }

    @Test
    fun `diagnostics merge keeps max counts and prefers the newer reason`() {
        val merged = mergeHistoryDiagnostics(
            ConversationDiagnostics(
                oversizedRecords = 2,
                corruptRecords = 1,
                omittedTools = 4,
                omittedPayloads = 1,
                continuationIncomplete = true,
                continuationReason = "invalid_link",
            ),
            ConversationDiagnostics(
                oversizedRecords = 5,
                corruptRecords = 0,
                omittedTools = 0,
                omittedPayloads = 3,
                planCorrupt = true,
                sourceTruncated = true,
            ),
        )
        assertThat(merged.oversizedRecords).isEqualTo(5)
        assertThat(merged.corruptRecords).isEqualTo(1)
        assertThat(merged.omittedTools).isEqualTo(4)
        assertThat(merged.omittedPayloads).isEqualTo(3)
        assertThat(merged.planCorrupt).isTrue()
        assertThat(merged.sourceTruncated).isTrue()
        assertThat(merged.continuationIncomplete).isTrue()
        assertThat(merged.continuationReason).isEqualTo("invalid_link")

        // A newer reason wins when present.
        val reasoned = mergeHistoryDiagnostics(
            merged,
            ConversationDiagnostics(
                continuationIncomplete = true,
                continuationReason = "cycle",
            ),
        )
        assertThat(reasoned.continuationReason).isEqualTo("cycle")
    }
}
