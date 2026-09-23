package com.lerdr.app.ui.session

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.isFocused
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
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

    private fun show(uiState: FeedUiState) {
        composeRule.setContent {
            LerdrTheme {
                AgentFeedContent(
                    uiState = uiState,
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
                    onRetryHistory = {},
                    onPickAttachments = {},
                    onRemoveAttachment = {},
                    onClearAttachments = {},
                    onRestartAttachments = {},
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
    fun feed_find() {
        show(baseState().copy(entries = entries + markdownEntry))
        composeRule.onNodeWithContentDescription("Find in conversation").performClick()
        // The find field requests focus on open — target it via focus.
        composeRule.onNode(isFocused()).performTextInput("diff")
        // The oracle debounces the filter 250 ms — step past it, then let
        // the auto-reveal scroll settle.
        composeRule.mainClock.advanceTimeBy(300)
        composeRule.waitForIdle()
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun feed_slashMenu() {
        show(
            baseState().copy(
                composerDraft = "/cl",
                slashCommands = listOf(
                    SlashCommand("/clear", "Clear the conversation"),
                    SlashCommand("/close", "Close the pane", source = "project"),
                    SlashCommand("/help", "Show help"),
                ),
            ),
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
                composerDraft = "Attachment: att_c1\nSummarize this.",
                attachments = AttachmentBatch(
                    items = listOf(item("c1", "notes.md", AttachmentItemState.READY, bytes = 1_024)),
                ),
                uploadStatus = "Attached notes.md",
            ),
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
}
