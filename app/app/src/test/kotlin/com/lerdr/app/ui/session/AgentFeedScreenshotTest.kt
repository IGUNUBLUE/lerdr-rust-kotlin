package com.lerdr.app.ui.session

import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.session.AgentFeedContent
import com.lerdr.app.session.AttachmentBatch
import com.lerdr.app.session.AttachmentIssue
import com.lerdr.app.session.AttachmentItem
import com.lerdr.app.session.AttachmentItemState
import com.lerdr.app.session.AttachmentUploads
import com.lerdr.app.session.FeedUiState
import com.lerdr.core.designsystem.theme.LerdrTheme
import kotlinx.serialization.json.JsonPrimitive
import lerdr.core.conversation.ConversationEntry
import lerdr.core.conversation.ConversationRole
import lerdr.core.model.UploadAttachment
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi coverage for Feed mode — composer attachment surfaces: the attach
 * affordance, the tray's per-file chips in each lifecycle state, progress,
 * batch issues, and the restart affordance.
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
        breadcrumb = "lerdr · main · workstation",
        statusLabel = "idle",
        connected = true,
        historyAvailable = true,
        entries = entries,
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
                    onAnswerOption = {},
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
