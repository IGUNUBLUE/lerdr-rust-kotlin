package com.lerdr.app.ui.session

import android.graphics.Bitmap
import androidx.activity.ComponentActivity
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onAllNodesWithContentDescription
import androidx.compose.ui.test.onRoot
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.session.FilesContent
import com.lerdr.app.session.FilesPreviewKind
import com.lerdr.app.session.FilesSection
import com.lerdr.app.session.FilesUiState
import com.lerdr.app.session.WorkspaceEntryKind
import com.lerdr.app.session.WorkspaceFilePreview
import com.lerdr.app.session.WorkspaceGitDiff
import com.lerdr.app.session.WorkspaceGitFile
import com.lerdr.app.session.WorkspaceGitStatus
import com.lerdr.app.session.WorkspacePreviewKind
import com.lerdr.app.session.WorkspaceTree
import com.lerdr.app.session.WorkspaceTreeEntry
import com.lerdr.core.designsystem.theme.LerdrTheme
import java.io.ByteArrayOutputStream
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi coverage for Files mode — tree browser states (loaded, drilled
 * in, empty, error, offline), both preview kinds, and the git surfaces.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class FilesScreenScreenshotTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val options = RoborazziOptions(
        // Dump (the JVM default) paints a semantics-tree overlay whose
        // node text jitters run-to-run — force a plain bitmap capture.
        captureType = RoborazziOptions.CaptureType.Screenshot(),
        compareOptions = RoborazziOptions.CompareOptions(changeThreshold = 0.02f),
    )

    private val tree = WorkspaceTree(
        root = "/home/u/lerdr",
        entries = listOf(
            WorkspaceTreeEntry("app", "app", WorkspaceEntryKind.DIRECTORY),
            WorkspaceTreeEntry("docs", "docs", WorkspaceEntryKind.DIRECTORY),
            WorkspaceTreeEntry("app/src", "src", WorkspaceEntryKind.DIRECTORY),
            WorkspaceTreeEntry("app/src/f.kt", "f.kt", WorkspaceEntryKind.FILE, 4_096),
            WorkspaceTreeEntry("app/src/g.kt", "g.kt", WorkspaceEntryKind.FILE, 128),
            WorkspaceTreeEntry("docs/04-app-design.md", "04-app-design.md", WorkspaceEntryKind.FILE, 9_200),
            WorkspaceTreeEntry("AGENTS.md", "AGENTS.md", WorkspaceEntryKind.FILE, 2_048),
            WorkspaceTreeEntry("icon.png", "icon.png", WorkspaceEntryKind.FILE, 65_536),
            WorkspaceTreeEntry("README.md", "README.md", WorkspaceEntryKind.FILE, 512),
        ),
    )

    private val git = WorkspaceGitStatus(
        available = true,
        branch = "main",
        ahead = 2,
        behind = 1,
        files = listOf(
            WorkspaceGitFile("app/src/f.kt", status = "M "),
            WorkspaceGitFile("docs/04-app-design.md", status = " M"),
            WorkspaceGitFile("new.txt", status = "??"),
            WorkspaceGitFile("old.kt", originalPath = "older.kt", status = "R "),
            WorkspaceGitFile("gone.kt", status = "D "),
        ),
    )

    private fun baseState() = FilesUiState(
        paneId = "r1::%1",
        title = "claude",
        breadcrumb = "lerdr · main · workstation",
        statusLabel = "working",
        connected = true,
        loading = false,
        rootLabel = "lerdr",
        tree = tree,
        git = git,
    )

    private fun show(uiState: FilesUiState) {
        composeRule.setContent {
            LerdrTheme {
                FilesContent(
                    uiState = uiState,
                    onOpenFeed = {},
                    onOpenTerminal = {},
                    onBack = {},
                    onSelectSection = {},
                    onOpenDir = {},
                    onShowFile = {},
                    onShowDiff = {},
                    onClosePreview = {},
                    onFilterChange = {},
                    onRefresh = {},
                )
            }
        }
    }

    @Test
    fun files_treeLoaded() {
        show(baseState())
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun files_treeSubdirectory() {
        show(baseState().copy(currentDir = "app"))
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun files_emptyDirectory() {
        show(
            baseState().copy(
                currentDir = "docs",
                tree = tree.copy(
                    entries = tree.entries.filter { !it.path.startsWith("docs/") },
                ),
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun files_filterMatches() {
        show(baseState().copy(filter = "kt"))
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun files_textPreview() {
        show(
            baseState().copy(
                selectedPath = "app/src/f.kt",
                previewFile = WorkspaceFilePreview(
                    path = "app/src/f.kt",
                    mediaType = "text/plain",
                    kind = WorkspacePreviewKind.TEXT,
                    text = "package com.lerdr.app\n\nfun main() {\n    println(\"hello\")\n}\n",
                    size = 74,
                ),
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun files_imagePreview() {
        val png = ByteArrayOutputStream().also { out ->
            Bitmap.createBitmap(48, 48, Bitmap.Config.ARGB_8888)
                .apply { eraseColor(0xFF274777.toInt()) }
                .compress(Bitmap.CompressFormat.PNG, 100, out)
        }.toByteArray()
        val dataUrl = "data:image/png;base64," +
            android.util.Base64.encodeToString(png, android.util.Base64.NO_WRAP)
        show(
            baseState().copy(
                selectedPath = "icon.png",
                previewFile = WorkspaceFilePreview(
                    path = "icon.png",
                    mediaType = "image/png",
                    kind = WorkspacePreviewKind.IMAGE,
                    dataUrl = dataUrl,
                    size = png.size.toLong(),
                ),
            ),
        )
        // The decode runs off-thread — wait for the Image node to land.
        composeRule.waitUntil(5_000) {
            composeRule.onAllNodesWithContentDescription("Preview of icon.png")
                .fetchSemanticsNodes().isNotEmpty()
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun files_changesList() {
        show(baseState().copy(section = FilesSection.CHANGES))
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun files_diffPreview() {
        show(
            baseState().copy(
                section = FilesSection.CHANGES,
                selectedPath = "app/src/f.kt",
                previewKind = FilesPreviewKind.DIFF,
                previewDiff = WorkspaceGitDiff(
                    path = "app/src/f.kt",
                    diff = "diff --git a/app/src/f.kt b/app/src/f.kt\n" +
                        "index 1234567..89abcde 100644\n" +
                        "--- a/app/src/f.kt\n" +
                        "+++ b/app/src/f.kt\n" +
                        "@@ -1,3 +1,4 @@\n" +
                        " package com.lerdr.app\n" +
                        "-fun main() {\n" +
                        "+fun main(args: Array<String>) {\n" +
                        "     println(\"hello\")\n" +
                        "\\ No newline at end of file\n",
                ),
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun files_gitUnavailable() {
        show(
            baseState().copy(
                section = FilesSection.CHANGES,
                git = WorkspaceGitStatus(available = false),
                gitReason = "This workspace is not inside a Git repository.",
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun files_workspaceError() {
        show(
            baseState().copy(
                tree = null,
                git = null,
                workspaceError = "Workspace is unavailable",
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun files_offline() {
        show(
            baseState().copy(
                connected = false,
                statusLabel = "",
                tree = null,
                git = null,
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    @Test
    fun files_previewError() {
        show(
            baseState().copy(
                selectedPath = "secret.pem",
                previewError = "Workspace preview only supports regular files",
            ),
        )
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }
}
