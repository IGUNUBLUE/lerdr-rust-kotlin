package com.lerdr.app.ui.session

import androidx.activity.ComponentActivity
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.runtime.State
import androidx.compose.runtime.mutableStateOf
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.isFocused
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.text.TextLayoutResult
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.test.performTouchInput
import androidx.compose.ui.test.swipeDown
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.session.AgentFeedContent
import com.lerdr.app.session.AttachmentBatch
import com.lerdr.app.session.AttachmentIssue
import com.lerdr.app.session.AttachmentItem
import com.lerdr.app.session.AttachmentItemState
import com.lerdr.app.session.AttachmentUploads
import com.lerdr.app.session.FeedUiState
import com.lerdr.app.session.feed.QuestionDraft
import com.lerdr.app.session.feed.SlashCommand
import com.lerdr.core.designsystem.theme.LerdrTheme
import kotlinx.serialization.json.JsonPrimitive
import lerdr.core.conversation.ConversationBrowseProgress
import lerdr.core.conversation.ConversationBrowseState
import lerdr.core.conversation.ConversationDiagnostics
import lerdr.core.conversation.ConversationEntry
import lerdr.core.conversation.ConversationRole
import lerdr.core.conversation.ConversationTool
import lerdr.core.model.BlockedMessage
import lerdr.core.model.Interaction
import lerdr.core.model.Option
import lerdr.core.model.Other
import lerdr.core.model.UploadAttachment
import lerdr.core.store.Agent
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi coverage for Feed mode — markdown bodies, tool cards, the
 * blocker card's approval/question triage, find-in-conversation, the slash
 * popover, reader gating, the error snackbar, and the composer attachment
 * surfaces.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class AgentFeedScreenshotTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val options = RoborazziOptions(
        // Dump (the JVM default) paints a semantics-tree overlay whose
        // node text jitters run-to-run — force a plain bitmap capture.
        captureType = RoborazziOptions.CaptureType.Screenshot(),
        compareOptions = RoborazziOptions.CompareOptions(changeThreshold = 0.02f),
    )

    private val entries = listOf(
        ConversationEntry(
            id = "e1",
            timestamp = "2026-01-01T10:00:00Z",
            role = ConversationRole.USER,
            text = "Summarize the sprint diff.",
        ),
        ConversationEntry(
            id = "e2",
            timestamp = "2026-01-01T10:00:12Z",
            role = ConversationRole.ASSISTANT,
            text = "I will read the changed files first.",
        ),
    )

    /** A markdown body + tool row — the assistant-side render path. */
    private val markdownEntry = ConversationEntry(
        id = "e3",
        timestamp = "2026-01-01T10:00:20Z",
        role = ConversationRole.ASSISTANT,
        text = "## Findings\n\nThe diff touches **three** files:\n\n" +
            "- `Feed.kt`\n- `Tools.kt`\n\n```kotlin\nfun main() = println(\"hi\")\n```\n\n" +
            "> Note: review the tests too.",
        tools = listOf(
            ConversationTool(
                id = "t1",
                name = "Bash",
                input = """{"command":"git diff --stat"}""",
                output = "3 files changed, 42 insertions(+)",
            ),
            ConversationTool(
                id = "t2",
                name = "Read",
                input = """{"file_path":"/src/Missing.kt"}""",
                output = "",
                error = true,
            ),
        ),
    )

    private fun agent(
        status: String = "idle",
        attentionKind: String? = null,
        prompt: String? = null,
        options: List<String>? = null,
        interaction: Interaction? = null,
    ) = Agent(
        relayId = "r1",
        relayLabel = "workstation",
        rawPaneId = "%1",
        paneId = "r1::%1",
        agent = "claude",
        name = "claude",
        status = status,
        attentionKind = attentionKind,
        prompt = prompt,
        options = options,
        interaction = interaction,
        conversationHistoryAvailable = true,
    )

    private fun item(
        clientId: String,
        name: String,
        state: AttachmentItemState,
        bytes: Long = 4_096,
        progress: Float = 0f,
        issue: AttachmentIssue? = null,
    ) = AttachmentItem(
        clientId = clientId,
        name = name,
        mediaType = "text/plain",
        bytes = bytes,
        order = 0,
        state = state,
        uploadedBytes = (bytes * progress).toLong(),
        progress = progress,
        issue = issue,
        attachment = if (state == AttachmentItemState.READY) {
            UploadAttachment(ref = "att_${clientId}", name = name, mediaType = "text/plain", bytes = bytes)
        } else {
            null
        },
    )

    private fun baseState() = FeedUiState(
        paneId = "r1::%1",
        title = "claude",
        provider = "claude",
        breadcrumb = "lerdr · main · workstation",
        statusLabel = "idle",
        connected = true,
        historyAvailable = true,
        entries = entries,
        canControl = true,
        canCopyResponse = true,
        canAttach = true,
    )

    private fun show(
        uiState: FeedUiState,
        listState: LazyListState? = null,
        draft: String = "",
        liveState: State<FeedUiState>? = null,
    ) {
        composeRule.setContent {
            LerdrTheme {
                AgentFeedContent(
                    uiState = liveState?.value ?: uiState,
                    composerValue = TextFieldValue(draft),
                    onComposerChange = {},
                    onOpenTerminal = {},
                    onOpenFiles = {},
                    onBack = {},
                    onDraftChange = {},
                    onSendPrompt = {},
                    onRespond = { _, _ -> },
                    onQuestionDraftChange = {},
                    onSubmitQuestion = {},
                    onNavigateQuestion = {},
                    onClarifyQuestion = {},
                    onCopyResponse = { entryText, onCopied -> onCopied(entryText) },
                    onClearError = {},
                    onLoadOlder = {},
                    onReloadHistory = {},
                    onRecoverHistory = {},
                    onCancelPreparation = {},
                    onContinuePreparation = {},
                    onPickAttachments = {},
                    onRemoveAttachment = {},
                    onClearAttachments = {},
                    onRestartAttachments = {},
                    listState = listState ?: rememberLazyListState(),
                )
            }
        }
    }

    @Test
    fun feed_idle() {
        show(baseState())
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun feed_markdownTools() {
        show(baseState().copy(entries = entries + markdownEntry))
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun feed_toolExpanded() {
        show(baseState().copy(entries = entries + markdownEntry))
        composeRule.onNodeWithText("Bash").performClick()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    @Config(qualifiers = "w411dp-h891dp")
    fun feed_unknownAttention() {
        show(
            baseState().copy(
                entries = emptyList(),
                statusLabel = "blocked",
                blocked = agent(
                    status = "blocked",
                    attentionKind = BlockedMessage.ATTENTION_UNKNOWN,
                    prompt = "Provider failed; inspect the native terminal.",
                ),
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun feed_approvalTriage() {
        show(
            baseState().copy(
                statusLabel = "blocked",
                blocked = agent(
                    status = "blocked",
                    attentionKind = BlockedMessage.ATTENTION_APPROVAL,
                    prompt = "Allow Bash: git push --force?",
                    options = listOf("Yes", "Always allow push", "No"),
                ),
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun feed_nativeApprovalOptionsReadable() {
        val labels = listOf(
            "Yes, proceed (y)",
            "Yes, and don't ask again for commands that start\n" +
                "with `cat -- /home/l/.local/state/lerdr-audit/\n" +
                "physical-ixsij1wu/providers/work/lerdr-audit-\n" +
                "owned-note.txt` (p)",
            "No, and tell Codex what to do differently (esc)",
        )
        show(
            baseState().copy(
                entries = emptyList(),
                statusLabel = "blocked",
                blocked = agent(
                    status = "blocked",
                    attentionKind = BlockedMessage.ATTENTION_APPROVAL,
                    prompt = "May I read the isolated audit note?",
                    options = labels,
                ),
            ),
        )
        labels.forEach { label ->
            val layouts = mutableListOf<TextLayoutResult>()
            composeRule.onNodeWithText(label, useUnmergedTree = true)
                .performSemanticsAction(SemanticsActions.GetTextLayoutResult) { it(layouts) }
            val layout = layouts.single()
            assertThat(layout.getLineEnd(layout.lineCount - 1, visibleEnd = true))
                .isEqualTo(label.length)
            assertThat(layout.didOverflowHeight).isFalse()
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun feed_questionForm() {
        show(
            baseState().copy(
                statusLabel = "blocked",
                blocked = agent(
                    status = "blocked",
                    attentionKind = BlockedMessage.ATTENTION_QUESTION,
                ),
                blockedInteraction = Interaction(
                    id = "q1",
                    kind = "single_select",
                    question = "Which branch should I target?",
                    options = listOf(
                        Option(index = 0, label = "main", description = "The default branch"),
                        Option(index = 1, label = "release/2.1", description = "The hotfix line"),
                    ),
                    other = Other(placeholder = "Another branch"),
                    submitLabel = "Submit",
                    canChat = true,
                    questionIndex = 1,
                    questionTotal = 2,
                ),
                questionDraft = QuestionDraft(selected = setOf(0)),
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun feed_findOpen() {
        show(baseState().copy(entries = entries + markdownEntry))
        composeRule.onNodeWithContentDescription("Session actions").performClick()
        composeRule.onNodeWithText("Find in conversation").performClick()
        composeRule.mainClock.advanceTimeBy(300)
        composeRule.waitForIdle()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun feed_find() {
        show(baseState().copy(entries = entries + markdownEntry))
        composeRule.onNodeWithContentDescription("Session actions").performClick()
        composeRule.onNodeWithText("Find in conversation").performClick()
        // The find field requests focus on open — target it via focus.
        composeRule.onNode(isFocused()).performTextInput("diff")
        // Lerdr debounces the filter 250 ms — step past it, then let
        // the auto-reveal scroll settle.
        composeRule.mainClock.advanceTimeBy(300)
        composeRule.waitForIdle()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun feed_slashMenu() {
        show(
            baseState().copy(
                slashCommands = listOf(
                    SlashCommand("/clear", "Clear the conversation"),
                    SlashCommand("/close", "Close the pane", source = "project"),
                    SlashCommand("/help", "Show help"),
                ),
            ),
            draft = "/cl",
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun feed_readerMode() {
        show(
            baseState().copy(
                canControl = false,
                canCopyResponse = false,
                canAttach = false,
                statusLabel = "blocked",
                blocked = agent(
                    status = "blocked",
                    attentionKind = BlockedMessage.ATTENTION_APPROVAL,
                    prompt = "Allow Bash: git push --force?",
                    options = listOf("Yes", "No"),
                ),
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun feed_snackbar() {
        // Freeze the clock — auto-advance would run the snackbar's own
        // timeout to completion before the capture.
        composeRule.mainClock.autoAdvance = false
        try {
            show(baseState().copy(lastError = "Prompt failed"))
            // Composition + the LaunchedEffect + the show animation — but
            // less than the snackbar's ~4 s duration.
            composeRule.mainClock.advanceTimeBy(2_000)
            composeRule.onRoot().captureRoboImage(roborazziOptions = options)
        } finally {
            composeRule.mainClock.autoAdvance = true
        }
    }

    @Test
    fun feed_attachmentsSelected() {
        show(
            baseState().copy(
                attachments = AttachmentBatch(
                    items = listOf(
                        item("c1", "notes.md", AttachmentItemState.SELECTED, bytes = 1_024),
                        item("c2", "diff.patch", AttachmentItemState.SELECTED, bytes = 12_288),
                    ),
                    canUpload = true,
                ),
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun feed_attachmentsUploading() {
        show(
            baseState().copy(
                attachments = AttachmentBatch(
                    items = listOf(
                        item("c1", "notes.md", AttachmentItemState.UPLOADING, bytes = 1_048_576, progress = 0.42f),
                        item("c2", "diff.patch", AttachmentItemState.SELECTED, bytes = 12_288),
                    ),
                    uploading = true,
                ),
                uploadStatus = "Uploading 2 attachments…",
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun feed_attachmentReady() {
        show(
            baseState().copy(
                attachments = AttachmentBatch(
                    items = listOf(item("c1", "notes.md", AttachmentItemState.READY, bytes = 1_024)),
                ),
                uploadStatus = "Attached notes.md",
            ),
            draft = "Attachment: att_c1\nSummarize this.",
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun feed_attachmentInterrupted() {
        show(
            baseState().copy(
                attachments = AttachmentBatch(
                    items = listOf(
                        item(
                            "c1",
                            "notes.md",
                            AttachmentItemState.INTERRUPTED,
                            bytes = 1_048_576,
                            progress = 0.42f,
                            issue = AttachmentIssue(AttachmentUploads.UPLOAD_STATE_UNKNOWN),
                        ),
                    ),
                ),
                uploadStatus = "Upload progress is uncertain. Restart it from the beginning; it will not resume automatically.",
                uploadError = true,
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun feed_attachmentRejected() {
        show(
            baseState().copy(
                attachments = AttachmentBatch(
                    items = listOf(
                        item(
                            "c1",
                            "archive.zip",
                            AttachmentItemState.REJECTED,
                            bytes = 65_536,
                            issue = AttachmentIssue(AttachmentUploads.UNKNOWN_MIME),
                        ),
                    ),
                    issue = AttachmentIssue(
                        AttachmentUploads.BATCH_LIMIT,
                        args = mapOf("max" to JsonPrimitive(8)),
                    ),
                ),
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    // ── conversation-history warnings + preparation ─────────────────

    @Test
    fun feed_historyDiagnostics() {
        show(
            baseState().copy(
                hasMoreHistory = true,
                historyDiagnostics = ConversationDiagnostics(
                    oversizedRecords = 2,
                    corruptRecords = 1,
                    omittedTools = 3,
                    omittedPayloads = 1,
                    continuationIncomplete = true,
                    continuationReason = "invalid_link",
                ),
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun feed_historyPreparing() {
        show(
            baseState().copy(
                hasMoreHistory = true,
                browseState = ConversationBrowseState.PREPARING,
                browseProgress = ConversationBrowseProgress(
                    phase = "indexing",
                    scannedBytes = 512,
                    sourceBytes = 2_048,
                ),
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun feed_historyPreparationPaused() {
        show(
            baseState().copy(
                hasMoreHistory = true,
                preparationPaused = true,
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun feed_historyErrorContinue() {
        show(
            baseState().copy(
                hasMoreHistory = true,
                browseState = ConversationBrowseState.FAILED,
                historyError =
                    "History loading stalled without finding more messages. Continue to retry.",
                historyErrorCode = "stalled",
                historyErrorRetryable = true,
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun feed_historySourceChanged() {
        show(
            baseState().copy(
                hasMoreHistory = true,
                historyError =
                    "The conversation source changed while history was being browsed.",
                historyErrorCode = "source_changed",
                historyErrorRetryable = false,
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun feed_historyUnavailable() {
        show(
            baseState().copy(
                entries = emptyList(),
                historyPageAvailable = false,
                historyUnavailableReason =
                    "Conversation history is not available for this agent.",
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    /**
     * Pinned tail must bottom-align: a last entry taller than the viewport
     * (streaming reply) lands its *end* on the fold, not its top.
     */
    @Test
    fun feed_tallEntryPinsToTail() {
        val tall = ConversationEntry(
            id = "eTall",
            timestamp = "2026-01-01T10:01:00Z",
            role = ConversationRole.ASSISTANT,
            text = (1..40).joinToString("\n\n") { "Streaming paragraph $it of the reply." } +
                "\n\nTAIL-OF-ENTRY",
        )
        val listState = LazyListState()
        show(baseState().copy(entries = entries + tall), listState)
        composeRule.waitForIdle()
        // Semantics bounds clip at the list's edge, so they can't prove
        // the tail is on screen — assert on the measured layout instead:
        // the final item must be bottom-aligned inside the viewport.
        composeRule.runOnIdle {
            val info = listState.layoutInfo
            val last = info.visibleItemsInfo.last()
            assertThat(last.index).isEqualTo(info.totalItemsCount - 1)
            assertThat(last.offset + last.size).isAtMost(info.viewportEndOffset)
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    /**
     * Re-entry path: the screen composes with an empty history while the
     * relay round-trip loads, then the accumulated entries arrive in one
     * shot. The feed must still land bottom-aligned on the newest entry.
     */
    @Test
    fun feed_lateArrivingEntriesPinToTail() {
        val tall = ConversationEntry(
            id = "eTall",
            timestamp = "2026-01-01T10:01:00Z",
            role = ConversationRole.ASSISTANT,
            text = (1..40).joinToString("\n\n") { "Streaming paragraph $it of the reply." } +
                "\n\nTAIL-OF-ENTRY",
        )
        val listState = LazyListState()
        val uiState = mutableStateOf(baseState().copy(entries = emptyList()))
        composeRule.setContent {
            LerdrTheme {
                AgentFeedContent(
                    uiState = uiState.value,
                    composerValue = TextFieldValue(),
                    onComposerChange = {},
                    onOpenTerminal = {},
                    onOpenFiles = {},
                    onBack = {},
                    onDraftChange = {},
                    onSendPrompt = {},
                    onRespond = { _, _ -> },
                    onQuestionDraftChange = {},
                    onSubmitQuestion = {},
                    onNavigateQuestion = {},
                    onClarifyQuestion = {},
                    onCopyResponse = { entryText, onCopied -> onCopied(entryText) },
                    onClearError = {},
                    onLoadOlder = {},
                    onReloadHistory = {},
                    onRecoverHistory = {},
                    onCancelPreparation = {},
                    onContinuePreparation = {},
                    onPickAttachments = {},
                    onRemoveAttachment = {},
                    onClearAttachments = {},
                    onRestartAttachments = {},
                    listState = listState,
                )
            }
        }
        composeRule.waitForIdle()
        composeRule.runOnIdle {
            uiState.value = baseState().copy(entries = entries + tall)
        }
        composeRule.waitForIdle()
        composeRule.runOnIdle {
            val info = listState.layoutInfo
            val last = info.visibleItemsInfo.last()
            assertThat(last.index).isEqualTo(info.totalItemsCount - 1)
            assertThat(last.offset + last.size).isAtMost(info.viewportEndOffset)
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun feed_olderPrependKeepsExpandedRowWhenHeaderDisappears() {
        val anchor = markdownEntry.copy(
            id = "anchor-tool",
            text = "Inspect the current record.",
            tools = listOf(markdownEntry.tools.first()),
        )
        val later = (1..40).map { index ->
            entries.last().copy(id = "later-$index", text = "Later turn $index.")
        }
        val initial = baseState().copy(entries = listOf(anchor) + later, hasMoreHistory = true)
        val uiState = mutableStateOf(initial)
        val listState = LazyListState()
        composeRule.setContent {
            LerdrTheme {
                AgentFeedContent(
                    uiState = uiState.value,
                    composerValue = TextFieldValue(),
                    onComposerChange = {},
                    onOpenTerminal = {},
                    onOpenFiles = {},
                    onBack = {},
                    onDraftChange = {},
                    onSendPrompt = {},
                    onRespond = { _, _ -> },
                    onQuestionDraftChange = {},
                    onSubmitQuestion = {},
                    onNavigateQuestion = {},
                    onClarifyQuestion = {},
                    onCopyResponse = { entryText, onCopied -> onCopied(entryText) },
                    onClearError = {},
                    onLoadOlder = {
                        uiState.value = uiState.value.copy(historyLoading = true)
                    },
                    onReloadHistory = {},
                    onRecoverHistory = {},
                    onCancelPreparation = {},
                    onContinuePreparation = {},
                    onPickAttachments = {},
                    onRemoveAttachment = {},
                    onClearAttachments = {},
                    onRestartAttachments = {},
                    listState = listState,
                )
            }
        }
        composeRule.waitForIdle()
        composeRule.onRoot().performTouchInput {
            swipeDown(startY = height * 0.35f, endY = height * 0.70f, durationMillis = 700)
        }
        composeRule.waitForIdle()
        composeRule.runOnIdle { listState.requestScrollToItem(0) }
        composeRule.waitForIdle()
        composeRule.onNodeWithContentDescription("Expand").performClick()
        composeRule.waitForIdle()
        var anchorOffset = 0
        composeRule.runOnIdle {
            assertThat(listState.layoutInfo.visibleItemsInfo.first().key).isEqualTo("load-older")
            anchorOffset = listState.layoutInfo.visibleItemsInfo.single {
                it.key == anchor.id
            }.offset
        }
        composeRule.onNodeWithText("Load older turns").performClick()
        composeRule.waitForIdle()
        composeRule.runOnIdle {
            uiState.value = initial.copy(
                entries = listOf(
                    entries.first().copy(
                        id = "older-turn",
                        text = (1..20).joinToString("\n\n") { "Earlier paragraph $it." },
                    ),
                ) + initial.entries,
                hasMoreHistory = false,
            )
        }
        composeRule.waitForIdle()
        composeRule.runOnIdle {
            assertThat(listState.layoutInfo.visibleItemsInfo.single {
                it.key == anchor.id
            }.offset).isEqualTo(anchorOffset)
        }
        composeRule.onNodeWithContentDescription("Collapse").assertIsDisplayed()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun feed_scrollBackWithinTallLastTurnDoesNotRepin() {
        val tall = entries.last().copy(
            id = "long-final-response",
            text = (1..80).joinToString("\n\n") { "Long response paragraph $it." },
        )
        val listState = LazyListState()
        val uiState = mutableStateOf(baseState().copy(entries = entries + tall))
        show(uiState.value, listState, liveState = uiState)
        composeRule.waitForIdle()
        composeRule.onRoot().performTouchInput {
            swipeDown(startY = height * 0.35f, endY = height * 0.70f, durationMillis = 700)
        }
        composeRule.waitForIdle()
        composeRule.runOnIdle {
            uiState.value = uiState.value.copy(
                entries = entries + tall.copy(text = tall.text + "\n\nA new streamed paragraph."),
            )
        }
        composeRule.waitForIdle()
        composeRule.runOnIdle {
            val info = listState.layoutInfo
            val last = info.visibleItemsInfo.last()
            assertThat(last.key).isEqualTo(tall.id)
            assertThat(last.offset + last.size).isGreaterThan(info.viewportEndOffset + 64)
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }
}
