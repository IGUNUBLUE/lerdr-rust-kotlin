package com.lerdr.app.session

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import lerdr.core.store.Agent
import lerdr.core.store.RelayStatus

/** The browser's two lists — flat file tree vs. changed files (oracle tabs). */
enum class FilesSection(val label: String) {
    FILES("Files"),
    CHANGES("Changes"),
}

/** Which payload the preview pane is showing. */
enum class FilesPreviewKind {
    FILE,
    DIFF,
}

/** One breadcrumb segment — [dir] is "" for the workspace root. */
data class FilesBreadcrumb(
    val label: String,
    val dir: String,
)

/** Everything Files mode renders — tree browser, git changes, preview pane. */
data class FilesUiState(
    val paneId: String,
    val title: String = "",
    val breadcrumb: String = "",
    /** Normalized agent identity ("claude", "codex"…) — top-bar logo. */
    val provider: String? = null,
    val statusLabel: String = "",
    /** Transport liveness for this agent's relay — false renders "offline". */
    val connected: Boolean = false,
    val section: FilesSection = FilesSection.FILES,
    /** True during the initial tree+git fetch. */
    val loading: Boolean = true,
    /** Tree-load failure — the workspace list itself could not be read. */
    val workspaceError: String? = null,
    /** Display name for the workspace root breadcrumb. */
    val rootLabel: String = "Workspace",
    val tree: WorkspaceTree? = null,
    val git: WorkspaceGitStatus? = null,
    /** Why the Changes tab is empty when [WorkspaceGitStatus.available] is false. */
    val gitReason: String? = null,
    /** The open directory — "" is the workspace root. */
    val currentDir: String = "",
    val filter: String = "",
    /** Path of the row last tapped — stays highlighted behind the preview. */
    val selectedPath: String = "",
    val previewKind: FilesPreviewKind = FilesPreviewKind.FILE,
    val previewFile: WorkspaceFilePreview? = null,
    val previewDiff: WorkspaceGitDiff? = null,
    val previewLoading: Boolean = false,
    val previewError: String? = null,
) {
    /** The preview pane owns the content area while any preview state is live. */
    val previewVisible: Boolean
        get() = previewLoading || previewFile != null || previewDiff != null || previewError != null
}

// ── pure tree/diff helpers (unit-tested, reused by the screen) ───────────

/** Parent directory of a workspace-relative path; "" for root entries. */
internal fun parentPathOf(path: String): String = path.substringBeforeLast('/', "")

/** Direct children of [dir] — the backend already sorts dirs-first per parent. */
internal fun childrenOf(
    entries: List<WorkspaceTreeEntry>,
    dir: String,
): List<WorkspaceTreeEntry> = entries.filter { parentPathOf(it.path) == dir }

/**
 * Breadcrumb chain for [currentDir] — root crumb plus one per segment,
 * each carrying the cumulative directory it navigates to.
 */
internal fun breadcrumbsOf(rootLabel: String, currentDir: String): List<FilesBreadcrumb> {
    val crumbs = mutableListOf(FilesBreadcrumb(label = rootLabel, dir = ""))
    if (currentDir.isEmpty()) return crumbs
    var cumulative = ""
    for (segment in currentDir.split('/')) {
        cumulative = if (cumulative.isEmpty()) segment else "$cumulative/$segment"
        crumbs += FilesBreadcrumb(label = segment, dir = cumulative)
    }
    return crumbs
}

/** `statusLabel` — the oracle's porcelain-XY → human label mapping. */
internal fun gitStatusLabel(status: String): String = when {
    status == "??" -> "New"
    status.contains('D') -> "Deleted"
    status.contains('R') -> "Renamed"
    status.contains('A') -> "Added"
    status.contains('M') -> "Modified"
    else -> status.trim().ifEmpty { "Changed" }
}

/** `diffLineTone` — the oracle's per-line unified-diff classification. */
enum class DiffLineTone {
    META,
    FILE,
    HUNK,
    ADDITION,
    DELETION,
    NOTE,
    CONTEXT,
}

internal fun diffLineToneOf(line: String): DiffLineTone = when {
    line.startsWith("diff --git ") || line.startsWith("index ") ||
        line.startsWith("new file mode ") || line.startsWith("deleted file mode ") ||
        line.startsWith("similarity index ") || line.startsWith("rename from ") ||
        line.startsWith("rename to ") || line.startsWith("Binary files ") -> DiffLineTone.META
    line.startsWith("--- ") || line.startsWith("+++ ") -> DiffLineTone.FILE
    line.startsWith("@@") -> DiffLineTone.HUNK
    line.startsWith("+") -> DiffLineTone.ADDITION
    line.startsWith("-") -> DiffLineTone.DELETION
    line.startsWith("\\ ") -> DiffLineTone.NOTE
    else -> DiffLineTone.CONTEXT
}

/**
 * Files-mode mutation point — owns workspace loads and preview fetches for
 * the screen's lifetime. Mirrors the oracle's `WorkspaceInspector`:
 * generation counters drop stale results, tree + git load in parallel with
 * `allSettled` semantics (a git failure degrades to `available=false` while
 * a tree failure errors the whole browser), and the workspace reloads when
 * the agent's `cwd` changes under it.
 */
class FilesViewModel(
    private val paneId: String,
    private val sessions: SessionRepository,
) : ViewModel() {

    private val relayId = paneId.substringBefore("::")

    /** Stale-load guards — the oracle's workspaceGeneration/previewGeneration. */
    private var workspaceGeneration = 0
    private var previewGeneration = 0

    private data class FilesLocal(
        val section: FilesSection = FilesSection.FILES,
        val loading: Boolean = true,
        val workspaceError: String? = null,
        val tree: WorkspaceTree? = null,
        val git: WorkspaceGitStatus? = null,
        val gitReason: String? = null,
        val currentDir: String = "",
        val filter: String = "",
        val selectedPath: String = "",
        val previewKind: FilesPreviewKind = FilesPreviewKind.FILE,
        val previewFile: WorkspaceFilePreview? = null,
        val previewDiff: WorkspaceGitDiff? = null,
        val previewLoading: Boolean = false,
        val previewError: String? = null,
    )

    private val local = MutableStateFlow(FilesLocal())

    val uiState: StateFlow<FilesUiState> = combine(
        sessions.agent(paneId),
        sessions.connection(relayId),
        local,
    ) { agent, connection, local ->
        FilesUiState(
            paneId = paneId,
            title = agent?.name ?: agent?.agent ?: paneId.substringAfter("::"),
            provider = agent?.agent?.takeIf { it.isNotEmpty() },
            breadcrumb = breadcrumbOf(agent),
            statusLabel = agent?.status ?: "",
            connected = connection?.status == RelayStatus.CONNECTED,
            section = local.section,
            loading = local.loading,
            workspaceError = local.workspaceError,
            rootLabel = agent?.cwd?.substringAfterLast('/')
                ?.takeIf { it.isNotEmpty() }
                ?: "Workspace",
            tree = local.tree,
            git = local.git,
            gitReason = local.gitReason,
            currentDir = local.currentDir,
            filter = local.filter,
            selectedPath = local.selectedPath,
            previewKind = local.previewKind,
            previewFile = local.previewFile,
            previewDiff = local.previewDiff,
            previewLoading = local.previewLoading,
            previewError = local.previewError,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), FilesUiState(paneId))

    init {
        // `loadedIdentity` — the oracle keys the listing to pane+cwd and
        // reloads when the agent moves directories.
        viewModelScope.launch {
            sessions.agent(paneId)
                .filterNotNull()
                .map { it.cwd.orEmpty() }
                .distinctUntilChanged()
                .collect { loadWorkspace() }
        }
    }

    /**
     * `loadWorkspace` — tree + git status in parallel. A rejected tree errors
     * the browser; a rejected git status degrades to "not a repo" with the
     * failure as the reason (the oracle's `Promise.allSettled` split).
     */
    fun loadWorkspace() {
        val generation = ++workspaceGeneration
        previewGeneration += 1
        local.value = local.value.copy(
            loading = true,
            workspaceError = null,
            tree = null,
            git = null,
            gitReason = null,
            currentDir = "",
            filter = "",
            selectedPath = "",
            previewFile = null,
            previewDiff = null,
            previewLoading = false,
            previewError = null,
        )
        viewModelScope.launch {
            val treeResult = async { runCatching { sessions.workspaceTree(paneId) } }
            val gitResult = async { runCatching { sessions.workspaceGitStatus(paneId) } }
            val tree = treeResult.await()
            val git = gitResult.await()
            if (generation != workspaceGeneration) return@launch
            local.value = if (tree.isFailure) {
                local.value.copy(
                    loading = false,
                    workspaceError = tree.exceptionOrNull()?.message
                        ?: "Workspace is unavailable",
                )
            } else {
                local.value.copy(
                    loading = false,
                    tree = tree.getOrNull(),
                    git = git.getOrNull() ?: WorkspaceGitStatus(available = false),
                    gitReason = when {
                        git.isFailure -> git.exceptionOrNull()?.message
                            ?: "Git status could not be read."
                        git.getOrNull()?.available == false ->
                            "This workspace is not inside a Git repository."
                        else -> null
                    },
                )
            }
        }
    }

    /** `showFile` — file tap in the tree; loads the bounded preview. */
    fun showFile(path: String) {
        val generation = ++previewGeneration
        local.value = local.value.copy(
            selectedPath = path,
            previewKind = FilesPreviewKind.FILE,
            previewFile = null,
            previewDiff = null,
            previewLoading = true,
            previewError = null,
        )
        viewModelScope.launch {
            try {
                val file = sessions.workspaceFile(paneId, path)
                if (generation == previewGeneration) {
                    local.value = local.value.copy(
                        previewFile = file,
                        previewLoading = false,
                    )
                }
            } catch (failure: Exception) {
                if (generation == previewGeneration) {
                    local.value = local.value.copy(
                        previewLoading = false,
                        previewError = failure.message ?: "Preview failed",
                    )
                }
            }
        }
    }

    /** `showDiff` — changed-file tap in Changes; loads the unified diff. */
    fun showDiff(path: String) {
        val generation = ++previewGeneration
        local.value = local.value.copy(
            selectedPath = path,
            previewKind = FilesPreviewKind.DIFF,
            previewFile = null,
            previewDiff = null,
            previewLoading = true,
            previewError = null,
        )
        viewModelScope.launch {
            try {
                val diff = sessions.workspaceGitDiff(paneId, path)
                if (generation == previewGeneration) {
                    local.value = local.value.copy(
                        previewDiff = diff,
                        previewLoading = false,
                    )
                }
            } catch (failure: Exception) {
                if (generation == previewGeneration) {
                    local.value = local.value.copy(
                        previewLoading = false,
                        previewError = failure.message ?: "Diff could not be read",
                    )
                }
            }
        }
    }

    /** `selectSection` — tab switch clears the open preview (oracle parity). */
    fun selectSection(section: FilesSection) {
        if (section == local.value.section) return
        previewGeneration += 1
        local.value = local.value.copy(
            section = section,
            selectedPath = "",
            previewFile = null,
            previewDiff = null,
            previewLoading = false,
            previewError = null,
        )
    }

    /** Drill into [dir] ("" = root); clears the filter so the listing shows. */
    fun openDir(dir: String) {
        local.value = local.value.copy(currentDir = dir, filter = "")
    }

    /** Back out of the preview pane to the list (selection stays highlighted). */
    fun closePreview() {
        previewGeneration += 1
        local.value = local.value.copy(
            previewFile = null,
            previewDiff = null,
            previewLoading = false,
            previewError = null,
        )
    }

    fun onFilterChange(query: String) {
        local.value = local.value.copy(filter = query)
    }

    private fun breadcrumbOf(agent: Agent?): String {
        if (agent == null) return ""
        val project = agent.project ?: agent.cwd?.substringAfterLast('/')
        return listOfNotNull(
            project?.takeIf { it.isNotEmpty() },
            agent.sessionName?.takeIf { it.isNotEmpty() },
            agent.relayLabel.takeIf { it.isNotEmpty() },
        ).joinToString(" · ")
    }
}
